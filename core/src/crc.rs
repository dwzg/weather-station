//! Checksums used by the record format, the queue cursor, the RTC block and the SHT4x.

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF). Guards queue records and the cursor.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// CRC-8 as used by Sensirion sensors (poly 0x31, init 0xFF).
pub fn crc8_sensirion(data: &[u8]) -> u8 {
    let mut crc: u8 = 0xFF;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x31 } else { crc << 1 };
        }
    }
    crc
}

/// CRC-32 (IEEE). Guards the state block kept in RTC memory.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_values() {
        let s = b"123456789";
        assert_eq!(crc16(s), 0x29B1); // CRC-16/CCITT-FALSE check value
        assert_eq!(crc32(s), 0xCBF4_3926); // CRC-32 check value
        assert_eq!(crc8_sensirion(&[0xBE, 0xEF]), 0x92); // example from the Sensirion datasheet
    }
}
