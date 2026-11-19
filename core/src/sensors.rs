//! BME280 for pressure (and temperature/humidity while no SHT4x is fitted),
//! optional SHT4x for temperature and humidity. Both are found at runtime, so
//! fitting the SHT45 needs no new firmware.

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;
use log::{info, warn};

use crate::bme280::{self, Bme280};
use crate::median::median;
use crate::reading::{Reading, SOURCE_BME280, SOURCE_SHT4X};
use crate::sht4x;

/// The I2C bus, with the one thing embedded-hal has no word for.
pub trait Bus: I2c {
    /// A sensor that was reset mid-transfer can hold SDA low forever. Clock it
    /// out (up to 9 pulses), issue a STOP (I2C spec section 3.1.16), and set
    /// the controller up again. True if SDA was stuck.
    fn recover(&mut self) -> bool;
}

#[derive(Clone, Debug, Default)]
pub struct SensorStatus {
    pub bme280: bool,
    pub sht4x: bool,
    pub sht4x_serial: u32,
    /// Measurements that produced no reading.
    pub failures: u32,
    pub bus_recoveries: u32,
    pub heater_pulses: u32,
}

const SAMPLES: usize = 3;
const HEATER_ABOVE_CENTI: u16 = 9500;
const REPROBE_MS: u64 = 10 * 60 * 1000;

#[derive(Default)]
pub struct Sensors {
    bme: Option<Bme280>,
    st: SensorStatus,
    sht_humid: bool,
    last_probe_ms: u64,
}

impl Sensors {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> &SensorStatus {
        &self.st
    }

    fn recover<B: Bus>(&mut self, bus: &mut B) {
        if bus.recover() {
            self.st.bus_recoveries += 1;
            warn!("i2c: bus recovered (SDA was stuck low)");
        }
    }

    fn init_bme<B: Bus, D: DelayNs>(&mut self, bus: &mut B, delay: &mut D) {
        self.bme = Bme280::init(bus, delay, bme280::ADDR).ok();
        self.st.bme280 = self.bme.is_some();
    }

    fn probe_sht<B: Bus, D: DelayNs>(&mut self, bus: &mut B, delay: &mut D, now_ms: u64) -> bool {
        self.last_probe_ms = now_ms;
        match sht4x::probe(bus, delay) {
            Some(serial) => {
                self.st.sht4x_serial = serial;
                true
            }
            None => false,
        }
    }

    pub fn begin<B: Bus, D: DelayNs>(&mut self, bus: &mut B, delay: &mut D, now_ms: u64) {
        self.recover(bus);
        self.init_bme(bus, delay);
        self.st.sht4x = self.probe_sht(bus, delay, now_ms);
        info!(
            "sensors: BME280 {}, SHT4x {}",
            if self.st.bme280 { "ok" } else { "MISSING" },
            if self.st.sht4x { "ok" } else { "not fitted" }
        );
        if self.st.sht4x {
            info!("sensors: SHT4x serial {:08x}", self.st.sht4x_serial);
        }
    }

    /// Takes one reading for `slot_epoch`. None if any value is missing or implausible.
    pub fn measure<B: Bus, D: DelayNs>(
        &mut self,
        bus: &mut B,
        delay: &mut D,
        slot_epoch: u32,
        now_ms: u64,
    ) -> Option<Reading> {
        if self.bme.is_none() || (!self.st.sht4x && now_ms.saturating_sub(self.last_probe_ms) > REPROBE_MS) {
            self.recover(bus);
            if self.bme.is_none() {
                self.init_bme(bus, delay);
            }
            if !self.st.sht4x && self.probe_sht(bus, delay, now_ms) {
                self.st.sht4x = true;
                info!(
                    "sensors: SHT4x found (serial {:08x}), using it for temperature and humidity",
                    self.st.sht4x_serial
                );
            }
        }
        let Some(bme) = self.bme else {
            self.st.failures += 1;
            warn!("sensors: BME280 not responding");
            return None;
        };

        let mut t = [0i32; SAMPLES];
        let mut h = [0i32; SAMPLES];
        let mut p = [0i32; SAMPLES];
        for i in 0..SAMPLES {
            match bme.measure(bus, delay) {
                Ok(m) => {
                    t[i] = m.temp_centi;
                    h[i] = m.hum_centi;
                    p[i] = m.pressure_deci_pa as i32;
                }
                Err(e) => {
                    self.bme = None; // re-initialise next time
                    self.st.bme280 = false;
                    self.st.failures += 1;
                    warn!("sensors: BME280 measurement failed ({e:?})");
                    return None;
                }
            }
        }
        let (tm, hm, pm) = (median(&t)?, median(&h)?, median(&p)?);
        if !(300_000..1_100_000).contains(&pm) || !(-4000..8500).contains(&tm) || !(0..=10_000).contains(&hm) {
            self.bme = None;
            self.st.bme280 = false;
            self.st.failures += 1;
            warn!(
                "sensors: BME280 implausible (T={} RH={} p={})",
                crate::upload::format_centi(tm),
                crate::upload::format_centi(hm),
                crate::upload::format_centi(pm / 10)
            );
            return None;
        }
        let mut out = Reading {
            epoch: slot_epoch,
            temp_centi: tm as i16,
            hum_centi: hm as u16,
            station_deci_pa: pm as u32,
            source: SOURCE_BME280,
        };

        if self.st.sht4x {
            let mut ts = Vec::with_capacity(SAMPLES);
            let mut hs = Vec::with_capacity(SAMPLES);
            for _ in 0..SAMPLES {
                if let Some((a, b)) = sht4x::measure(bus, delay) {
                    ts.push(a);
                    hs.push(b);
                }
            }
            match (median(&ts), median(&hs)) {
                (Some(ts), Some(hs)) if (-4000..8500).contains(&ts) => {
                    out.temp_centi = ts as i16;
                    out.hum_centi = hs as u16;
                    out.source = SOURCE_SHT4X;
                }
                _ => {
                    self.st.sht4x = false; // probe again later; this slot uses the BME280
                    warn!("sensors: SHT4x read failed, falling back to BME280 for this slot");
                }
            }
        }
        self.sht_humid = out.source == SOURCE_SHT4X && out.hum_centi >= HEATER_ABOVE_CENTI;
        Some(out)
    }

    /// Runs the SHT4x heater after a measurement in near-saturated air, so
    /// condensation does not leave the humidity stuck at 100 %. Five minutes
    /// until the next slot are plenty for it to cool down again.
    pub fn maybe_decondense<B: Bus, D: DelayNs>(&mut self, bus: &mut B, delay: &mut D) {
        if !std::mem::take(&mut self.sht_humid) {
            return;
        }
        if sht4x::heat(bus, delay) {
            self.st.heater_pulses += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use embedded_hal::i2c::{ErrorKind, ErrorType, NoAcknowledgeSource, Operation};

    use super::*;
    use crate::bme280::tests::example;
    use crate::crc::crc8_sensirion;

    /// A BME280 at register level.
    struct SimBme {
        regs: [u8; 256],
        ptr: u8,
        adc: (u32, u32, u16), // pressure, temperature, humidity
        hang: bool,           // stays "measuring" forever
    }

    impl SimBme {
        fn new() -> Self {
            let c = example();
            let mut regs = [0u8; 256];
            regs[0xD0] = 0x60;
            let words = [c.t1 as i16, c.t2, c.t3, c.p1 as i16, c.p2, c.p3, c.p4, c.p5, c.p6, c.p7, c.p8, c.p9];
            for (i, w) in words.iter().enumerate() {
                regs[0x88 + 2 * i..0x8A + 2 * i].copy_from_slice(&w.to_le_bytes());
            }
            regs[0xA1] = c.h1;
            regs[0xE1..0xE3].copy_from_slice(&c.h2.to_le_bytes());
            regs[0xE3] = c.h3;
            regs[0xE4] = (c.h4 >> 4) as u8;
            regs[0xE5] = ((c.h5 as u8 & 0x0F) << 4) | (c.h4 as u8 & 0x0F);
            regs[0xE6] = (c.h5 >> 4) as u8;
            regs[0xE7] = c.h6 as u8;
            SimBme { regs, ptr: 0, adc: (415_148, 519_888, 28_000), hang: false }
        }

        /// [reg] sets the read pointer; [reg, value, reg, value, ...] writes.
        fn write(&mut self, b: &[u8]) {
            self.ptr = b[0];
            for pair in b.chunks_exact(2) {
                let (reg, v) = (pair[0], pair[1]);
                self.regs[reg as usize] = v;
                if reg == 0xF4 && v & 0b11 == 0b01 {
                    let (p, t, h) = self.adc;
                    self.regs[0xF7..0xFF].copy_from_slice(&[
                        (p >> 12) as u8,
                        (p >> 4) as u8,
                        ((p & 0xF) << 4) as u8,
                        (t >> 12) as u8,
                        (t >> 4) as u8,
                        ((t & 0xF) << 4) as u8,
                        (h >> 8) as u8,
                        h as u8,
                    ]);
                    self.regs[0xF3] = if self.hang { 0x08 } else { 0 };
                }
            }
        }

        fn read(&mut self, buf: &mut [u8]) {
            for b in buf {
                *b = self.regs[self.ptr as usize];
                self.ptr = self.ptr.wrapping_add(1);
            }
        }
    }

    /// An SHT4x that answers every command with a fixed frame.
    struct SimSht {
        t_raw: u16,
        h_raw: u16,
        last: u8,
        heats: u32,
        broken: bool, // NACKs measurements
    }

    impl SimSht {
        fn frame(a: u16, b: u16) -> [u8; 6] {
            let [a0, a1] = a.to_be_bytes();
            let [b0, b1] = b.to_be_bytes();
            [a0, a1, crc8_sensirion(&[a0, a1]), b0, b1, crc8_sensirion(&[b0, b1])]
        }
    }

    #[derive(Default)]
    struct SimBus {
        bme: Option<SimBme>,
        sht: Option<SimSht>,
        stuck: bool,
    }

    impl ErrorType for SimBus {
        type Error = ErrorKind;
    }

    impl I2c for SimBus {
        fn transaction(&mut self, addr: u8, ops: &mut [Operation<'_>]) -> Result<(), ErrorKind> {
            let nack = ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address);
            if self.stuck {
                return Err(ErrorKind::Bus);
            }
            for op in ops {
                match (addr, op) {
                    (0x76, Operation::Write(b)) => self.bme.as_mut().ok_or(nack)?.write(b),
                    (0x76, Operation::Read(b)) => self.bme.as_mut().ok_or(nack)?.read(b),
                    (0x44, Operation::Write(b)) => {
                        let s = self.sht.as_mut().ok_or(nack)?;
                        s.last = b[0];
                        if b[0] == 0x2F {
                            s.heats += 1;
                        }
                    }
                    (0x44, Operation::Read(b)) => {
                        let s = self.sht.as_mut().ok_or(nack)?;
                        if s.broken && s.last == 0xFD {
                            return Err(nack);
                        }
                        let f = if s.last == 0x89 {
                            SimSht::frame(0x1234, 0x5678)
                        } else {
                            SimSht::frame(s.t_raw, s.h_raw)
                        };
                        b.copy_from_slice(&f[..b.len()]);
                    }
                    _ => return Err(nack),
                }
            }
            Ok(())
        }
    }

    impl Bus for SimBus {
        fn recover(&mut self) -> bool {
            std::mem::take(&mut self.stuck)
        }
    }

    struct NoDelay;
    impl DelayNs for NoDelay {
        fn delay_ns(&mut self, _: u32) {}
    }

    fn sht(t_raw: u16, h_raw: u16) -> SimSht {
        SimSht { t_raw, h_raw, last: 0, heats: 0, broken: false }
    }

    #[test]
    fn bme280_alone() {
        let mut bus = SimBus { bme: Some(SimBme::new()), ..Default::default() };
        let mut s = Sensors::new();
        s.begin(&mut bus, &mut NoDelay, 0);
        assert!(s.status().bme280 && !s.status().sht4x);
        let r = s.measure(&mut bus, &mut NoDelay, 1_794_128_400, 1000).unwrap();
        assert_eq!(r.epoch, 1_794_128_400);
        assert_eq!(r.temp_centi, 2508);
        assert_eq!(r.station_deci_pa, 1_006_533);
        assert_eq!(r.source, SOURCE_BME280);
        assert!(r.hum_centi > 0 && r.hum_centi < 10_000);
    }

    #[test]
    fn sht4x_supplies_temperature_and_humidity() {
        let mut bus = SimBus { bme: Some(SimBme::new()), sht: Some(sht(0x6666, 0x8000)), ..Default::default() };
        let mut s = Sensors::new();
        s.begin(&mut bus, &mut NoDelay, 0);
        assert_eq!(s.status().sht4x_serial, 0x1234_5678);
        let r = s.measure(&mut bus, &mut NoDelay, 300, 1000).unwrap();
        assert_eq!((r.temp_centi, r.hum_centi, r.source), (2500, 5650, SOURCE_SHT4X));
        assert_eq!(r.station_deci_pa, 1_006_533, "pressure still from the BME280");
        s.maybe_decondense(&mut bus, &mut NoDelay);
        assert_eq!(bus.sht.as_ref().unwrap().heats, 0, "no heater below 95 %");
    }

    #[test]
    fn heater_after_saturated_reading() {
        let mut bus = SimBus { bme: Some(SimBme::new()), sht: Some(sht(0x6666, 0xFFFF)), ..Default::default() };
        let mut s = Sensors::new();
        s.begin(&mut bus, &mut NoDelay, 0);
        assert_eq!(s.measure(&mut bus, &mut NoDelay, 300, 0).unwrap().hum_centi, 10_000);
        s.maybe_decondense(&mut bus, &mut NoDelay);
        s.maybe_decondense(&mut bus, &mut NoDelay);
        assert_eq!(bus.sht.as_ref().unwrap().heats, 1, "one pulse per humid reading");
        assert_eq!(s.status().heater_pulses, 1);
    }

    #[test]
    fn sht4x_fitted_later_is_found() {
        let mut bus = SimBus { bme: Some(SimBme::new()), ..Default::default() };
        let mut s = Sensors::new();
        s.begin(&mut bus, &mut NoDelay, 0);
        bus.sht = Some(sht(0x6666, 0x8000));
        assert_eq!(
            s.measure(&mut bus, &mut NoDelay, 300, 5 * 60_000).unwrap().source,
            SOURCE_BME280,
            "not re-probed yet"
        );
        assert_eq!(s.measure(&mut bus, &mut NoDelay, 600, 11 * 60_000).unwrap().source, SOURCE_SHT4X);
    }

    #[test]
    fn broken_sht4x_falls_back_for_the_slot() {
        let mut bus = SimBus { bme: Some(SimBme::new()), sht: Some(sht(0x6666, 0x8000)), ..Default::default() };
        let mut s = Sensors::new();
        s.begin(&mut bus, &mut NoDelay, 0);
        bus.sht.as_mut().unwrap().broken = true;
        let r = s.measure(&mut bus, &mut NoDelay, 300, 1000).unwrap();
        assert_eq!((r.source, r.temp_centi), (SOURCE_BME280, 2508));
        assert!(!s.status().sht4x);
    }

    #[test]
    fn missing_or_hung_bme280_gives_no_reading_and_recovers() {
        let mut bus = SimBus::default();
        let mut s = Sensors::new();
        s.begin(&mut bus, &mut NoDelay, 0);
        assert!(!s.status().bme280);
        assert!(s.measure(&mut bus, &mut NoDelay, 300, 0).is_none());
        // Plugged in, with the bus stuck: recovered, initialised, measured.
        bus.bme = Some(SimBme::new());
        bus.stuck = true;
        assert!(s.measure(&mut bus, &mut NoDelay, 600, 0).is_some());
        assert_eq!(s.status().bus_recoveries, 1);
        // Stops answering mid-measurement: no reading, re-initialised next time.
        bus.bme.as_mut().unwrap().hang = true;
        assert!(s.measure(&mut bus, &mut NoDelay, 900, 0).is_none());
        assert!(!s.status().bme280);
        bus.bme.as_mut().unwrap().hang = false;
        assert!(s.measure(&mut bus, &mut NoDelay, 1200, 0).is_some());
        assert_eq!(s.status().failures, 2);
    }

    #[test]
    fn implausible_values_are_refused() {
        let mut bme = SimBme::new();
        bme.adc.1 = 0xFFFF0; // ~ 200 degC
        let mut bus = SimBus { bme: Some(bme), ..Default::default() };
        let mut s = Sensors::new();
        s.begin(&mut bus, &mut NoDelay, 0);
        assert!(s.measure(&mut bus, &mut NoDelay, 300, 0).is_none());
        assert_eq!(s.status().failures, 1);
    }
}
