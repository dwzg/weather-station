//! Sensirion SHT4x (SHT40/41/45) at 0x44: frame decoding and the few commands we use.

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

use crate::crc::crc8_sensirion;

pub const ADDR: u8 = 0x44;
const MEASURE_HIGH: u8 = 0xFD; // high repeatability, 8.3 ms
const SERIAL: u8 = 0x89;
const SOFT_RESET: u8 = 0x94;
const HEAT_110MW_1S: u8 = 0x2F; // heater, then a measurement

/// Decodes a 6-byte frame (T msb, T lsb, crc, RH msb, RH lsb, crc) into
/// (temperature, humidity) in 0.01 units. None on a CRC error. Humidity is
/// clamped to 0..100 % as the datasheet advises.
pub fn decode(f: &[u8; 6]) -> Option<(i32, i32)> {
    if crc8_sensirion(&f[0..2]) != f[2] || crc8_sensirion(&f[3..5]) != f[5] {
        return None;
    }
    let t_raw = i64::from(u16::from_be_bytes([f[0], f[1]]));
    let h_raw = i64::from(u16::from_be_bytes([f[3], f[4]]));
    // T = -45 + 175 * raw / 65535, RH = -6 + 125 * raw / 65535 (rounded to 0.01)
    let t = -4500 + (17_500 * t_raw + 32_767) / 65_535;
    let h = -600 + (12_500 * h_raw + 32_767) / 65_535;
    Some((t as i32, h.clamp(0, 10_000) as i32))
}

fn command<I: I2c, D: DelayNs>(i2c: &mut I, delay: &mut D, cmd: u8, wait_ms: u32) -> Option<[u8; 6]> {
    i2c.write(ADDR, &[cmd]).ok()?;
    delay.delay_ms(wait_ms);
    let mut f = [0u8; 6];
    i2c.read(ADDR, &mut f).ok()?;
    Some(f)
}

/// One high-repeatability measurement: (temperature, humidity) in 0.01 units.
pub fn measure<I: I2c, D: DelayNs>(i2c: &mut I, delay: &mut D) -> Option<(i32, i32)> {
    decode(&command(i2c, delay, MEASURE_HIGH, 10)?)
}

/// Soft reset, then the serial number. None if no SHT4x answers.
pub fn probe<I: I2c, D: DelayNs>(i2c: &mut I, delay: &mut D) -> Option<u32> {
    i2c.write(ADDR, &[SOFT_RESET]).ok()?;
    delay.delay_ms(2);
    let f = command(i2c, delay, SERIAL, 2)?;
    decode(&f)?; // same framing and CRCs as a measurement
    Some(u32::from_be_bytes([f[0], f[1], f[3], f[4]]))
}

/// 110 mW for 1 s, then the sensor measures (that reading is hot: discarded).
pub fn heat<I: I2c, D: DelayNs>(i2c: &mut I, delay: &mut D) -> bool {
    command(i2c, delay, HEAT_110MW_1S, 1200).and_then(|f| decode(&f)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: u16, h: u16) -> [u8; 6] {
        let [a, b] = t.to_be_bytes();
        let [c, d] = h.to_be_bytes();
        [a, b, crc8_sensirion(&[a, b]), c, d, crc8_sensirion(&[c, d])]
    }

    #[test]
    fn decodes_and_clamps() {
        let mut f = frame(0x6666, 0xCCCC);
        assert_eq!(decode(&f), Some((2500, 9400))); // -45 + 175 * 0.4, -6 + 125 * 0.8
        f[5] ^= 1;
        assert_eq!(decode(&f), None);
        assert_eq!(decode(&frame(0, 0xFFFF)).unwrap().1, 10_000);
        assert_eq!(decode(&frame(0, 0)).unwrap(), (-4500, 0));
    }
}
