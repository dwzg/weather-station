#include "wifi_link.h"

#include <ESP8266WiFi.h>

#include "config.h"
#include "log.h"

namespace {
constexpr uint32_t kJoinTimeoutMs = 20000;
constexpr int32_t kWeakRssi = -75;
constexpr uint32_t kWeakForMs = 10UL * 60 * 1000;
constexpr uint32_t kRoamCheckEveryMs = 30UL * 60 * 1000;
constexpr int32_t kRoamGainDb = 8;
}  // namespace

void WifiLink::begin(const char* hostname, const char* ssid, const char* pass) {
  ssid_ = ssid;
  pass_ = pass;
  WiFi.persistent(false);  // never write credentials to flash on every join
  WiFi.mode(WIFI_STA);
  WiFi.setAutoReconnect(false);
  WiFi.setSleepMode(WIFI_NONE_SLEEP);  // mains powered: modem sleep only costs reliability
  WiFi.setOutputPower(cfg::kWifiTxDbm);
  WiFi.hostname(hostname);
  stats_.downSinceMs = millis();
}

bool WifiLink::joinBest(bool onlyIfBetter) {
  const int n = WiFi.scanNetworks(false, false);
  int best = -1;
  for (int i = 0; i < n; ++i) {
    if (WiFi.SSID(i) == ssid_ && (best < 0 || WiFi.RSSI(i) > WiFi.RSSI(best))) best = i;
  }
  if (best < 0) {
    WiFi.scanDelete();
    if (onlyIfBetter) return false;
    logf("wifi: '%s' not seen in scan (%d networks), joining blind", ssid_, n);
    WiFi.begin(ssid_, pass_);
    return true;
  }
  const int32_t rssi = WiFi.RSSI(best);
  uint8_t bssid[6];
  memcpy(bssid, WiFi.BSSID(best), 6);
  const int32_t channel = WiFi.channel(best);
  WiFi.scanDelete();
  if (onlyIfBetter) {
    if (memcmp(bssid, WiFi.BSSID(), 6) == 0 || rssi < WiFi.RSSI() + kRoamGainDb) return false;
    logf("wifi: roaming to %02x:%02x:%02x:%02x:%02x:%02x (%ld dBm vs %ld dBm)", bssid[0], bssid[1], bssid[2],
         bssid[3], bssid[4], bssid[5], static_cast<long>(rssi), static_cast<long>(WiFi.RSSI()));
    ++stats_.roams;
  } else {
    logf("wifi: joining %02x:%02x:%02x:%02x:%02x:%02x ch %ld, %ld dBm", bssid[0], bssid[1], bssid[2], bssid[3],
         bssid[4], bssid[5], static_cast<long>(channel), static_cast<long>(rssi));
  }
  WiFi.begin(ssid_, pass_, channel, bssid, true);
  return true;
}

void WifiLink::loop(bool quiet) {
  const uint32_t now = millis();
  const bool linked = WiFi.status() == WL_CONNECTED;

  switch (state_) {
    case State::Up:
      if (!linked) {
        ++stats_.drops;
        stats_.downSinceMs = now;
        logf("wifi: link lost after %lus", static_cast<unsigned long>((now - stats_.upSinceMs) / 1000));
        state_ = State::Down;
        downAtMs_ = now;
        waitMs_ = 1000;
        return;
      }
      if (WiFi.RSSI() < kWeakRssi) {
        if (!weakSinceMs_) weakSinceMs_ = now;
      } else {
        weakSinceMs_ = 0;
      }
      if (!quiet && weakSinceMs_ && now - weakSinceMs_ > kWeakForMs && now - lastRoamCheckMs_ > kRoamCheckEveryMs) {
        lastRoamCheckMs_ = now;
        if (joinBest(true)) {
          state_ = State::Joining;
          stateMs_ = now;
          stats_.downSinceMs = now;
        }
      }
      return;

    case State::Joining:
      // WiFi.begin() drops the old link asynchronously; give it a moment before trusting "connected".
      if (linked && now - stateMs_ > 500) {
        state_ = State::Up;
        stats_.upSinceMs = now;
        ++stats_.connects;
        backoff_.reset();
        weakSinceMs_ = 0;
        logf("wifi: up, %s, %ld dBm, down for %lus", WiFi.localIP().toString().c_str(),
             static_cast<long>(WiFi.RSSI()), static_cast<unsigned long>((now - stats_.downSinceMs) / 1000));
      } else if (now - stateMs_ > kJoinTimeoutMs) {
        WiFi.disconnect(false);
        state_ = State::Down;
        const uint32_t wait = backoff_.next(os_random());
        downAtMs_ = now;
        waitMs_ = wait;
        logf("wifi: join timed out (status %d), next try in %lus", static_cast<int>(WiFi.status()),
             static_cast<unsigned long>(wait / 1000));
      }
      return;

    case State::Down:
      if (linked) {  // came back by itself
        state_ = State::Joining;
        stateMs_ = now;
        return;
      }
      if (quiet || now - downAtMs_ < waitMs_) return;
      joinBest(false);
      state_ = State::Joining;
      stateMs_ = millis();
      return;
  }
}
