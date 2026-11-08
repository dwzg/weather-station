#include "reading.h"

#include "crc.h"

namespace ws {
namespace {

void put16(uint8_t* p, uint16_t v) {
  p[0] = static_cast<uint8_t>(v);
  p[1] = static_cast<uint8_t>(v >> 8);
}
void put32(uint8_t* p, uint32_t v) {
  for (int i = 0; i < 4; ++i) p[i] = static_cast<uint8_t>(v >> (8 * i));
}
uint16_t get16(const uint8_t* p) { return static_cast<uint16_t>(p[0] | (p[1] << 8)); }
uint32_t get32(const uint8_t* p) {
  return static_cast<uint32_t>(p[0]) | (static_cast<uint32_t>(p[1]) << 8) |
         (static_cast<uint32_t>(p[2]) << 16) | (static_cast<uint32_t>(p[3]) << 24);
}

}  // namespace

void encodeReading(const Reading& r, uint8_t out[kRecordSize]) {
  put32(out, r.epoch);
  put16(out + 4, static_cast<uint16_t>(r.tempCenti));
  put16(out + 6, r.humCenti);
  put32(out + 8, r.stationDeciPa);
  out[12] = r.source;
  out[13] = kRecordVersion;
  put16(out + 14, crc16(out, 14));
}

bool decodeReading(const uint8_t in[kRecordSize], Reading& out) {
  if (get16(in + 14) != crc16(in, 14)) return false;
  if (in[13] != kRecordVersion) return false;
  out.epoch = get32(in);
  out.tempCenti = static_cast<int16_t>(get16(in + 4));
  out.humCenti = get16(in + 6);
  out.stationDeciPa = get32(in + 8);
  out.source = in[12];
  return out.epoch != 0;
}

}  // namespace ws
