#include "sht4x.h"

#include "crc.h"

namespace ws {

bool decodeSht4x(const uint8_t f[6], int32_t& tempCenti, int32_t& humCenti) {
  if (crc8Sensirion(f, 2) != f[2] || crc8Sensirion(f + 3, 2) != f[5]) return false;
  const int64_t tRaw = (f[0] << 8) | f[1];
  const int64_t hRaw = (f[3] << 8) | f[4];
  // T = -45 + 175 * raw / 65535, RH = -6 + 125 * raw / 65535 (rounded to 0.01)
  tempCenti = static_cast<int32_t>(-4500 + (17500 * tRaw + 32767) / 65535);
  int32_t h = static_cast<int32_t>(-600 + (12500 * hRaw + 32767) / 65535);
  humCenti = h < 0 ? 0 : (h > 10000 ? 10000 : h);
  return true;
}

}  // namespace ws
