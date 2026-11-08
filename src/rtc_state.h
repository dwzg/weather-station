#pragma once
#include <stdint.h>

// A few words kept in RTC memory, which survives every reset except power loss.
// It carries the clock across a crash or update so readings continue while the
// network is down, and counts crashes so a bad build degrades instead of looping.
struct RtcState {
  uint32_t epoch = 0;        // wall clock when last saved
  uint32_t lastSlot = 0;     // last slot measured, so a quick reboot does not measure twice
  uint32_t crashes = 0;      // consecutive unplanned resets
  uint32_t bootCount = 0;    // since power-on
  uint32_t intentional = 0;  // set right before a planned restart (update, self-heal)
};

bool rtcLoad(RtcState& s);
void rtcSave(const RtcState& s);
