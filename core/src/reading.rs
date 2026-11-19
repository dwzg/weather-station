//! One observation and its 16-byte flash record. The byte layout is the one the
//! station's former ESP8266 firmware (C++, in the git history) used.

use crate::crc::crc16;

/// Which sensor supplied temperature and humidity (pressure always comes from the BME280).
pub const SOURCE_BME280: u8 = 0;
pub const SOURCE_SHT4X: u8 = 1;

/// One observation for one 5-minute slot, as it is kept in the flash queue.
///
/// Everything is fixed-point so a reading has exactly one encoding. The pressure
/// is the raw station pressure as the sensor measured it: calibration and the
/// reduction to sea level are applied when the reading is sent, so a firmware
/// update with a better calibration also corrects readings still in the queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// Start of the slot, seconds since 1970-01-01 UTC.
    pub epoch: u32,
    /// Air temperature in 0.01 degC.
    pub temp_centi: i16,
    /// Relative humidity in 0.01 %.
    pub hum_centi: u16,
    /// Station pressure in 0.1 Pa (= 0.001 hPa).
    pub station_deci_pa: u32,
    pub source: u8,
}

pub const RECORD_SIZE: usize = 16;
/// If the format ever changes, bump this and keep decoding version 1:
/// otherwise every reading still queued at the update is skipped as corrupt.
pub const RECORD_VERSION: u8 = 1;

/// Layout (little endian): epoch u32 | temp i16 | hum u16 | pressure u32 | source u8 |
/// version u8 | crc16 over bytes 0..13.
pub fn encode(r: &Reading) -> [u8; RECORD_SIZE] {
    let mut out = [0u8; RECORD_SIZE];
    out[0..4].copy_from_slice(&r.epoch.to_le_bytes());
    out[4..6].copy_from_slice(&r.temp_centi.to_le_bytes());
    out[6..8].copy_from_slice(&r.hum_centi.to_le_bytes());
    out[8..12].copy_from_slice(&r.station_deci_pa.to_le_bytes());
    out[12] = r.source;
    out[13] = RECORD_VERSION;
    let c = crc16(&out[..14]);
    out[14..16].copy_from_slice(&c.to_le_bytes());
    out
}

/// None for a torn, corrupted or unknown record.
pub fn decode(b: &[u8; RECORD_SIZE]) -> Option<Reading> {
    if u16::from_le_bytes([b[14], b[15]]) != crc16(&b[..14]) || b[13] != RECORD_VERSION {
        return None;
    }
    let r = Reading {
        epoch: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        temp_centi: i16::from_le_bytes([b[4], b[5]]),
        hum_centi: u16::from_le_bytes([b[6], b[7]]),
        station_deci_pa: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
        source: b[12],
    };
    (r.epoch != 0).then_some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corruption() {
        let r = Reading {
            epoch: 1_794_128_400,
            temp_centi: -1234,
            hum_centi: 5000,
            station_deci_pa: 992_000,
            source: SOURCE_SHT4X,
        };
        let mut b = encode(&r);
        assert_eq!(decode(&b), Some(r));
        b[5] ^= 0x01;
        assert_eq!(decode(&b), None);
        assert_eq!(decode(&[0u8; RECORD_SIZE]), None);
    }

    #[test]
    fn same_bytes_as_the_esp8266_firmware() {
        // Encoded by the ESP8266 firmware's lib/core/src/reading.cpp (git history) for this reading.
        let r =
            Reading { epoch: 300, temp_centi: -5, hum_centi: 10000, station_deci_pa: 992_000, source: SOURCE_BME280 };
        let b = encode(&r);
        assert_eq!(&b[..14], &[0x2C, 0x01, 0, 0, 0xFB, 0xFF, 0x10, 0x27, 0x00, 0x23, 0x0F, 0x00, 0, 1]);
        assert_eq!(u16::from_le_bytes([b[14], b[15]]), crc16(&b[..14]));
    }
}
