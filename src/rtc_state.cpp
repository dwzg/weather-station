#include "rtc_state.h"

#include <Esp.h>

#include "crc.h"

namespace {
// User RTC memory words 0..31 (0x60001200) hold the OTA bootloader's command
// block (eboot); start behind it. Change kMagic whenever RtcState changes.
constexpr uint32_t kOffsetWords = 32;
constexpr uint32_t kMagic = 0x57535432;  // "WST2"

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
