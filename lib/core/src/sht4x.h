#pragma once
#include <stdint.h>

namespace ws {

// Decodes a 6-byte SHT4x measurement frame (T msb, T lsb, crc, RH msb, RH lsb, crc).
// Returns false on a CRC error. Humidity is clamped to 0..100 % as the datasheet advises.
bool decodeSht4x(const uint8_t frame[6], int32_t& tempCenti, int32_t& humCenti);

}  // namespace ws
