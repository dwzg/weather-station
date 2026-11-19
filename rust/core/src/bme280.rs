//! Bosch BME280: forced mode, 4x oversampling, no IIR filter, and the integer
//! compensation formulas of the datasheet (section 4.2.3 and 8.2).
//!
//! Every wait has a bound, so a sensor that stops answering mid-way turns into
//! an error instead of a hang.

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

pub const ADDR: u8 = 0x76;
const CHIP_ID: u8 = 0x60;
const REG_ID: u8 = 0xD0;
const REG_RESET: u8 = 0xE0;
const REG_CTRL_HUM: u8 = 0xF2;
const REG_STATUS: u8 = 0xF3;
const REG_CTRL_MEAS: u8 = 0xF4;
const REG_CONFIG: u8 = 0xF5;
const REG_DATA: u8 = 0xF7;
const REG_CALIB_A: u8 = 0x88; // 26 bytes, 0x88..=0xA1
const REG_CALIB_B: u8 = 0xE1; // 7 bytes, 0xE1..=0xE7
const OSRS_X4: u8 = 0b011;
/// Temperature and pressure oversampling x4, sleep mode (forced mode adds 0b01).
const CTRL_MEAS_SLEEP: u8 = (OSRS_X4 << 5) | (OSRS_X4 << 2);
const MODE_FORCED: u8 = 0b01;
/// Datasheet 9.1: 1.25 + 2.3 * 4 + (2.3 * 4 + 0.575) * 2 ms with x4 everywhere.
const MEASURE_MAX_MS: u32 = 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No answer on the bus, or a transfer failed.
    Bus,
    /// Something answers at the address, but it is not a BME280.
    WrongChip(u8),
    /// The sensor did not finish in time.
    Timeout,
    /// It answered with the "skipped" marker instead of a value.
    NoData,
}

/// Trimming parameters, burnt into each sensor at the factory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Calibration {
    pub t1: u16,
    pub t2: i16,
    pub t3: i16,
    pub p1: u16,
    pub p2: i16,
    pub p3: i16,
    pub p4: i16,
    pub p5: i16,
    pub p6: i16,
    pub p7: i16,
    pub p8: i16,
    pub p9: i16,
    pub h1: u8,
    pub h2: i16,
    pub h3: u8,
    pub h4: i16,
    pub h5: i16,
    pub h6: i8,
}

impl Calibration {
    /// From registers 0x88..=0xA1 and 0xE1..=0xE7.
    pub fn parse(a: &[u8; 26], b: &[u8; 7]) -> Self {
        let u = |i: usize| u16::from_le_bytes([a[i], a[i + 1]]);
        let s = |i: usize| i16::from_le_bytes([a[i], a[i + 1]]);
        Calibration {
            t1: u(0),
            t2: s(2),
            t3: s(4),
            p1: u(6),
            p2: s(8),
            p3: s(10),
            p4: s(12),
            p5: s(14),
            p6: s(16),
            p7: s(18),
            p8: s(20),
            p9: s(22),
            h1: a[25],
            h2: i16::from_le_bytes([b[0], b[1]]),
            h3: b[2],
            h4: (i16::from(b[3] as i8) << 4) | i16::from(b[4] & 0x0F),
            h5: (i16::from(b[5] as i8) << 4) | i16::from(b[4] >> 4),
            h6: b[6] as i8,
        }
    }

    /// Fine temperature (shared by the other two) and temperature in 0.01 degC.
    pub fn temperature(&self, adc_t: i32) -> (i32, i32) {
        let (adc, t1) = (i64::from(adc_t), i64::from(self.t1));
        let var1 = (((adc >> 3) - (t1 << 1)) * i64::from(self.t2)) >> 11;
        let var2 = (((((adc >> 4) - t1) * ((adc >> 4) - t1)) >> 12) * i64::from(self.t3)) >> 14;
        let t_fine = (var1 + var2) as i32;
        (t_fine, (t_fine * 5 + 128) >> 8)
    }

    /// Pressure in Pa as unsigned Q24.8 (divide by 256), None if the
    /// calibration would divide by zero.
    pub fn pressure_q24_8(&self, adc_p: i32, t_fine: i32) -> Option<u32> {
        let mut var1 = i64::from(t_fine) - 128_000;
        let mut var2 = var1 * var1 * i64::from(self.p6);
        var2 += (var1 * i64::from(self.p5)) << 17;
        var2 += i64::from(self.p4) << 35;
        var1 = ((var1 * var1 * i64::from(self.p3)) >> 8) + ((var1 * i64::from(self.p2)) << 12);
        var1 = (((1i64 << 47) + var1) * i64::from(self.p1)) >> 33;
        if var1 == 0 {
            return None;
        }
        let mut p = 1_048_576 - i64::from(adc_p);
        p = (((p << 31) - var2) * 3125) / var1;
        var1 = (i64::from(self.p9) * (p >> 13) * (p >> 13)) >> 25;
        var2 = (i64::from(self.p8) * p) >> 19;
        p = ((p + var1 + var2) >> 8) + (i64::from(self.p7) << 4);
        u32::try_from(p).ok()
    }

    /// Relative humidity in %RH as unsigned Q22.10 (divide by 1024), 0..=100 %.
    pub fn humidity_q22_10(&self, adc_h: i32, t_fine: i32) -> u32 {
        let x = i64::from(t_fine) - 76_800;
        let mut v = (((i64::from(adc_h) << 14) - (i64::from(self.h4) << 20) - (i64::from(self.h5) * x) + 16_384) >> 15)
            * (((((((x * i64::from(self.h6)) >> 10) * (((x * i64::from(self.h3)) >> 11) + 32_768)) >> 10)
                + 2_097_152)
                * i64::from(self.h2)
                + 8192)
                >> 14);
        v -= ((((v >> 15) * (v >> 15)) >> 7) * i64::from(self.h1)) >> 4;
        (v.clamp(0, 419_430_400) >> 12) as u32
    }
}

/// One compensated measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement {
    /// 0.01 degC
    pub temp_centi: i32,
    /// 0.01 %RH, 0..=10000
    pub hum_centi: i32,
    /// 0.1 Pa
    pub pressure_deci_pa: u32,
}

impl Calibration {
    /// Compensates raw ADC values (20-bit pressure and temperature, 16-bit humidity).
    pub fn compensate(&self, adc_p: i32, adc_t: i32, adc_h: i32) -> Option<Measurement> {
        let (t_fine, temp_centi) = self.temperature(adc_t);
        let p = self.pressure_q24_8(adc_p, t_fine)?;
        let h = self.humidity_q22_10(adc_h, t_fine);
        Some(Measurement {
            temp_centi,
            hum_centi: ((u64::from(h) * 100 + 512) / 1024) as i32,
            pressure_deci_pa: ((u64::from(p) * 10 + 128) / 256) as u32,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Bme280 {
    addr: u8,
    cal: Calibration,
}

fn read<I: I2c>(i2c: &mut I, addr: u8, reg: u8, buf: &mut [u8]) -> Result<(), Error> {
    i2c.write_read(addr, &[reg], buf).map_err(|_| Error::Bus)
}

fn write<I: I2c>(i2c: &mut I, addr: u8, reg: u8, v: u8) -> Result<(), Error> {
    i2c.write(addr, &[reg, v]).map_err(|_| Error::Bus)
}

/// Polls the status register until `mask` clears, for at most `max_ms`.
fn wait_clear<I: I2c, D: DelayNs>(i2c: &mut I, delay: &mut D, addr: u8, mask: u8, max_ms: u32) -> Result<(), Error> {
    for _ in 0..=max_ms / 2 {
        let mut s = [0u8];
        read(i2c, addr, REG_STATUS, &mut s)?;
        if s[0] & mask == 0 {
            return Ok(());
        }
        delay.delay_ms(2);
    }
    Err(Error::Timeout)
}

impl Bme280 {
    /// Checks the chip id, resets the sensor, reads its calibration and sets
    /// forced mode: it then sleeps between readings, so it does not warm itself
    /// up (the datasheet's "weather monitoring" setting, with more oversampling).
    pub fn init<I: I2c, D: DelayNs>(i2c: &mut I, delay: &mut D, addr: u8) -> Result<Self, Error> {
        let mut id = [0u8];
        read(i2c, addr, REG_ID, &mut id)?;
        if id[0] != CHIP_ID {
            return Err(Error::WrongChip(id[0]));
        }
        write(i2c, addr, REG_RESET, 0xB6)?;
        delay.delay_ms(5); // start-up time 2 ms
        wait_clear(i2c, delay, addr, 0x01, 50)?; // im_update: NVM copy running
        let mut a = [0u8; 26];
        let mut b = [0u8; 7];
        read(i2c, addr, REG_CALIB_A, &mut a)?;
        read(i2c, addr, REG_CALIB_B, &mut b)?;
        // ctrl_hum only takes effect after a write to ctrl_meas.
        write(i2c, addr, REG_CTRL_HUM, OSRS_X4)?;
        write(i2c, addr, REG_CONFIG, 0x00)?; // filter off
        write(i2c, addr, REG_CTRL_MEAS, CTRL_MEAS_SLEEP)?;
        Ok(Bme280 { addr, cal: Calibration::parse(&a, &b) })
    }

    pub fn calibration(&self) -> &Calibration {
        &self.cal
    }

    /// Triggers one forced measurement and waits for it (about 31 ms).
    pub fn measure<I: I2c, D: DelayNs>(&self, i2c: &mut I, delay: &mut D) -> Result<Measurement, Error> {
        write(i2c, self.addr, REG_CTRL_MEAS, CTRL_MEAS_SLEEP | MODE_FORCED)?;
        delay.delay_ms(MEASURE_MAX_MS);
        wait_clear(i2c, delay, self.addr, 0x08, 100)?; // measuring
        let mut d = [0u8; 8];
        read(i2c, self.addr, REG_DATA, &mut d)?;
        let adc_p = (i32::from(d[0]) << 12) | (i32::from(d[1]) << 4) | (i32::from(d[2]) >> 4);
        let adc_t = (i32::from(d[3]) << 12) | (i32::from(d[4]) << 4) | (i32::from(d[5]) >> 4);
        let adc_h = (i32::from(d[6]) << 8) | i32::from(d[7]);
        if adc_p == 0x80000 || adc_t == 0x80000 || adc_h == 0x8000 {
            return Err(Error::NoData);
        }
        self.cal.compensate(adc_p, adc_t, adc_h).ok_or(Error::NoData)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The example in the BMP280 datasheet (section 3.12), whose temperature and
    /// pressure formulas the BME280 shares; humidity values of a real BME280.
    pub(crate) fn example() -> Calibration {
        Calibration {
            t1: 27504,
            t2: 26435,
            t3: -1000,
            p1: 36477,
            p2: -10685,
            p3: 3024,
            p4: 2855,
            p5: 140,
            p6: -7,
            p7: 15500,
            p8: -14600,
            p9: 6000,
            h1: 75,
            h2: 362,
            h3: 0,
            h4: 313,
            h5: 50,
            h6: 30,
        }
    }

    /// The datasheet's double-precision humidity formula (section 8.1).
    fn humidity_f64(c: &Calibration, adc_h: i32, t_fine: i32) -> f64 {
        let x = f64::from(t_fine) - 76_800.0;
        let mut h = (f64::from(adc_h) - (f64::from(c.h4) * 64.0 + f64::from(c.h5) / 16_384.0 * x))
            * (f64::from(c.h2) / 65_536.0
                * (1.0 + f64::from(c.h6) / 67_108_864.0 * x * (1.0 + f64::from(c.h3) / 67_108_864.0 * x)));
        h *= 1.0 - f64::from(c.h1) * h / 524_288.0;
        h.clamp(0.0, 100.0)
    }

    #[test]
    fn datasheet_example() {
        let c = example();
        let (t_fine, t) = c.temperature(519_888);
        assert_eq!(t_fine, 128_422);
        assert_eq!(t, 2508); // 25.08 degC
        assert_eq!(c.pressure_q24_8(415_148, t_fine), Some(25_767_233)); // 100653.25 Pa (the double formula: 100653.258)
        let m = c.compensate(415_148, 519_888, 0).unwrap();
        assert_eq!(m.pressure_deci_pa, 1_006_533);
        assert_eq!(m.temp_centi, 2508);
    }

    #[test]
    fn humidity_matches_the_floating_point_formula() {
        let c = example();
        let (t_fine, _) = c.temperature(519_888);
        for adc_h in [20_000, 25_000, 28_000, 30_000, 32_000] {
            let int = f64::from(c.humidity_q22_10(adc_h, t_fine)) / 1024.0;
            let float = humidity_f64(&c, adc_h, t_fine);
            assert!((int - float).abs() < 0.05, "adc {adc_h}: {int} vs {float}");
        }
        assert_eq!(c.humidity_q22_10(0, t_fine), 0);
        assert_eq!(c.humidity_q22_10(65_535, t_fine), 100 << 10);
    }

    #[test]
    fn calibration_registers() {
        let c = example();
        let mut a = [0u8; 26];
        for (i, v) in
            [c.t1 as i16, c.t2, c.t3, c.p1 as i16, c.p2, c.p3, c.p4, c.p5, c.p6, c.p7, c.p8, c.p9].iter().enumerate()
        {
            a[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        a[25] = c.h1;
        let [h2l, h2h] = c.h2.to_le_bytes();
        let b = [
            h2l,
            h2h,
            c.h3,
            (c.h4 >> 4) as u8,
            ((c.h5 as u8 & 0x0F) << 4) | (c.h4 as u8 & 0x0F),
            (c.h5 >> 4) as u8,
            c.h6 as u8,
        ];
        assert_eq!(Calibration::parse(&a, &b), c);
        // Negative H4/H5 (the sign lives in the MSB register).
        let n = Calibration::parse(&a, &[0, 0, 0, 0xFF, 0x8F, 0xFF, 0]);
        assert_eq!((n.h4, n.h5), (-1, -8));
    }
}
