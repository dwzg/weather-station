#pragma once
#include <stdint.h>

namespace ws {

// Exponential backoff with +-25 % jitter, so a device that lost the network
// does not retry in lock-step with the outage.
class Backoff {
 public:
  Backoff(uint32_t minMs, uint32_t maxMs) : min_(minMs), max_(maxMs) {}
  // `random` is any 32-bit random value; the caller supplies it so this stays testable.
  uint32_t next(uint32_t random);
  void reset() { failures_ = 0; }
  uint32_t failures() const { return failures_; }

 private:
  uint32_t min_, max_;
  uint32_t failures_ = 0;
};

}  // namespace ws
