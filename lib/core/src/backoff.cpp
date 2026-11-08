#include "backoff.h"

namespace ws {

uint32_t Backoff::next(uint32_t random) {
  uint64_t base = min_;
  for (uint32_t i = 0; i < failures_ && base < max_; ++i) base *= 2;
  if (base > max_) base = max_;
  if (failures_ < 32) ++failures_;
  // jitter in [-25 %, +25 %)
  const uint64_t span = base / 2;
  const uint64_t jitter = span ? random % span : 0;
  uint64_t d = base - base / 4 + jitter;
  if (d < 1) d = 1;
  return static_cast<uint32_t>(d);
}

}  // namespace ws
