#pragma once
#include <stdint.h>

#include "backoff.h"

struct WifiStats {
  uint32_t connects = 0;
  uint32_t drops = 0;
  uint32_t roams = 0;
  uint32_t upSinceMs = 0;
  uint32_t downSinceMs = 0;
};

// Keeps the station on the strongest access point of its network. Instead of
// the SDK's auto-reconnect (which sticks to whichever AP it saw first) it scans,
// joins the best BSSID and re-checks now and then while the signal is weak.
class WifiLink {
 public:
  void begin(const char* hostname, const char* ssid, const char* pass);
  // `quiet` asks it not to start a blocking scan (a measurement is due).
  void loop(bool quiet);
  bool up() const { return state_ == State::Up; }
  const WifiStats& stats() const { return stats_; }

 private:
  enum class State { Down, Joining, Up };
  bool joinBest(bool onlyIfBetter);

  const char* ssid_ = nullptr;
  const char* pass_ = nullptr;
  State state_ = State::Down;
  uint32_t stateMs_ = 0;
  uint32_t downAtMs_ = 0;  // retry waitMs_ after this (elapsed-time form: safe across millis() wrap)
  uint32_t waitMs_ = 0;
  uint32_t lastRoamCheckMs_ = 0;
  uint32_t weakSinceMs_ = 0;
  ws::Backoff backoff_{5000, 5UL * 60 * 1000};
  WifiStats stats_;
};
