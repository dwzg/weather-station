#pragma once
#include <stddef.h>
#include <stdint.h>

namespace ws {

// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF). Guards queue records and the cursor.
uint16_t crc16(const uint8_t* data, size_t len);

// CRC-8 as used by Sensirion sensors (poly 0x31, init 0xFF).
uint8_t crc8Sensirion(const uint8_t* data, size_t len);

// CRC-32 (IEEE). Guards the state block kept in RTC memory.
uint32_t crc32(const uint8_t* data, size_t len);

}  // namespace ws
