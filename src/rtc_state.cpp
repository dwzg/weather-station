#include "rtc_state.h"

#include <Esp.h>

#include "crc.h"

namespace {
// User RTC memory words 64..127 are used by the OTA bootloader (eboot); stay below.
constexpr uint32_t kOffsetWords = 32;
constexpr uint32_t kMagic = 0x57535431;  // "WST1"

struct Block {
  uint32_t magic;
  RtcState state;
  uint32_t crc;
};
}  // namespace

bool rtcLoad(RtcState& s) {
  Block b;
  if (!ESP.rtcUserMemoryRead(kOffsetWords, reinterpret_cast<uint32_t*>(&b), sizeof b)) return false;
  if (b.magic != kMagic || b.crc != ws::crc32(reinterpret_cast<const uint8_t*>(&b), offsetof(Block, crc))) {
    return false;
  }
  s = b.state;
  return true;
}

void rtcSave(const RtcState& s) {
  Block b{kMagic, s, 0};
  b.crc = ws::crc32(reinterpret_cast<const uint8_t*>(&b), offsetof(Block, crc));
  ESP.rtcUserMemoryWrite(kOffsetWords, reinterpret_cast<uint32_t*>(&b), sizeof b);
}
