// Balcony weather station: measures every 5 minutes, keeps every reading in
// flash until the weather app has confirmed it, and sends it there directly.
#include <ArduinoOTA.h>
#include <ESP8266WebServer.h>
#include <ESP8266WiFi.h>
#include <LittleFS.h>
#include <coredecls.h>  // settimeofday_cb
#include <sys/time.h>
#include <time.h>
#include <user_interface.h>

#include "config.h"
#include "fs_store.h"
#include "log.h"
#include "queue.h"
#include "ram_store.h"
#include "rtc_state.h"
#include "secrets.h"
#include "sensors.h"
#include "uploader.h"
#include "wifi_link.h"

#ifndef FW_VERSION
#define FW_VERSION "dev"
#endif

namespace {

constexpr time_t kSaneEpoch = 1700000000;  // anything earlier means "no clock yet"

LittleFsStore store;
RamStore ramStore;
ws::ReadingQueue flashQueue(store, cfg::kRecordsPerSegment, cfg::kMaxSegments);
ws::ReadingQueue ramQueue(ramStore, 36, 4);  // 12 h in RAM if the flash is unusable
ws::ReadingQueue* queue = nullptr;
Sensors sensors;
WifiLink wifi;
Uploader uploader;
ESP8266WebServer web(80);

RtcState rtc;
enum class Mode { Normal, NoUpload, Safe };
Mode mode = Mode::Normal;
bool fsOk = false;
bool clockFromNtp = false;
bool clockRestored = false;
uint32_t lastNtpEpoch = 0;
uint32_t lastSlot = 0;
uint32_t failedSlot = 0;  // slot whose measurement failed, retried every 10 s within the grace
uint32_t failedAtMs = 0;
os_timer_t loopWatchdog;
volatile uint32_t lastLoopMs = 0;
uint32_t lastRtcSaveMs = 0;
uint32_t queueWriteErrors = 0;
uint32_t measured = 0;
ws::Reading lastReading;
bool haveLastReading = false;
const char* resetReason = "";

bool clockValid() { return (clockFromNtp || clockRestored) && time(nullptr) > kSaneEpoch; }

void saveRtc() {
  rtc.epoch = clockValid() ? static_cast<uint32_t>(time(nullptr)) : 0;
  rtc.lastSlot = lastSlot;
  rtcSave(rtc);
  lastRtcSaveMs = millis();
}

[[noreturn]] void restart(const char* why) {
  logf("restart: %s", why);
  if (queue) queue->commit();
  rtc.intentional = 1;  // not a crash
  saveRtc();
  delay(100);
  ESP.restart();
  while (true) delay(1000);
}

// ---- status page ----------------------------------------------------------------

void jsonStr(String& s, const char* key, const char* v, bool comma = true) {
  s += '"';
  s += key;
  s += F("\":\"");
  for (const char* p = v; *p; ++p) {
    if (*p == '"' || *p == '\\') s += '\\';
    if (static_cast<uint8_t>(*p) >= 0x20) s += *p;
  }
  s += '"';
  if (comma) s += ',';
}
void jsonNum(String& s, const char* key, long v, bool comma = true) {
  s += '"';
  s += key;
  s += F("\":");
  s += v;
  if (comma) s += ',';
}
void jsonBool(String& s, const char* key, bool v, bool comma = true) {
  s += '"';
  s += key;
  s += F("\":");
  s += v ? F("true") : F("false");
  if (comma) s += ',';
}

void handleStatus() {
  String s;
  s.reserve(1400);
  const uint32_t up = millis();
  s += '{';
  jsonStr(s, "firmware", FW_VERSION);
  jsonNum(s, "uptime_s", up / 1000);
  jsonStr(s, "reset_reason", resetReason);
  jsonNum(s, "boot_count", rtc.bootCount);
  jsonStr(s, "mode", mode == Mode::Normal ? "normal" : (mode == Mode::NoUpload ? "no-upload (after crashes)" : "safe (after crashes)"));
  jsonNum(s, "crashes_in_a_row", rtc.crashes);
  char now[32] = "";
  if (clockValid()) {
    const time_t t = time(nullptr);
    struct tm tm;
    localtime_r(&t, &tm);
    strftime(now, sizeof now, "%Y-%m-%dT%H:%M:%S%z", &tm);
  }
  s += F("\"clock\":{");
  jsonStr(s, "now", now);
  jsonStr(s, "source", clockFromNtp ? "ntp" : (clockRestored ? "rtc (restored after reset)" : "none"));
  jsonNum(s, "last_ntp_sync", lastNtpEpoch, false);
  s += F("},\"wifi\":{");
  jsonBool(s, "up", wifi.up());
  jsonNum(s, "rssi_dbm", wifi.up() ? WiFi.RSSI() : 0);
  jsonStr(s, "bssid", wifi.up() ? WiFi.BSSIDstr().c_str() : "");
  jsonStr(s, "ip", wifi.up() ? WiFi.localIP().toString().c_str() : "");
  jsonNum(s, "connects", wifi.stats().connects);
  jsonNum(s, "drops", wifi.stats().drops);
  jsonNum(s, "roams", wifi.stats().roams, false);
  const SensorStatus& ss = sensors.status();
  s += F("},\"sensors\":{");
  jsonBool(s, "bme280", ss.bme280);
  jsonBool(s, "sht4x", ss.sht4x);
  jsonNum(s, "measured", measured);
  jsonNum(s, "failures", ss.failures);
  jsonNum(s, "bus_recoveries", ss.busRecoveries);
  jsonNum(s, "heater_pulses", ss.heaterPulses, haveLastReading);
  if (haveLastReading) {
    char ts[32];
    const time_t t = lastReading.epoch;
    struct tm tm;
    localtime_r(&t, &tm);
    strftime(ts, sizeof ts, "%Y-%m-%dT%H:%M:%S%z", &tm);
    char v[16];
    s += F("\"last\":{");
    jsonStr(s, "slot", ts);
    ws::formatCenti(lastReading.tempCenti, v, sizeof v);
    s += F("\"temperature\":");
    s += v;
    ws::formatCenti(lastReading.humCenti, v, sizeof v);
    s += F(",\"humidity\":");
    s += v;
    s += F(",\"station_pressure\":");
    s += String(lastReading.stationDeciPa / 1000.0, 3);
    ws::formatCenti(ws::seaLevelCentiHpa(lastReading, {cfg::kAltitudeM, cfg::kPressureOffsetHpa}), v, sizeof v);
    s += F(",\"sea_level_pressure\":");
    s += v;
    s += F(",\"source\":\"");
    s += lastReading.source == ws::kSourceSht4x ? F("sht4x") : F("bme280");
    s += F("\"}");
  }
  size_t fsUsed = 0, fsTotal = 0;
  if (fsOk) store.usage(fsUsed, fsTotal);
  s += F("},\"queue\":{");
  jsonStr(s, "storage", queue == &flashQueue ? "flash" : (queue ? "RAM (flash unusable)" : "none"));
  jsonNum(s, "pending", queue ? queue->pending() : 0);
  jsonNum(s, "segments", queue ? queue->segments() : 0);
  jsonNum(s, "dropped_full", queue ? queue->dropped() : 0);
  jsonNum(s, "corrupt_skipped", queue ? queue->corrupt() : 0);
  jsonNum(s, "read_errors", queue ? queue->readErrors() : 0);
  jsonNum(s, "unreadable_skipped", queue ? queue->unreadable() : 0);
  jsonNum(s, "write_errors", queueWriteErrors);
  jsonNum(s, "fs_used", fsUsed);
  jsonNum(s, "fs_total", fsTotal, false);
  const UploadStats& us = uploader.stats();
  s += F("},\"upload\":{");
  jsonNum(s, "accepted", us.accepted);
  jsonNum(s, "rejected", us.rejected);
  jsonNum(s, "failed_batches", us.failedBatches);
  jsonNum(s, "last_ok", us.lastOkEpoch);
  jsonNum(s, "last_status", us.lastStatus);
  jsonStr(s, "last_error", us.lastError);
  jsonNum(s, "retry_in_s", uploader.retryInMs() / 1000);
  jsonNum(s, "min_heap_in_tls", us.minHeapInTls);
  jsonNum(s, "tls_stack_max", us.tlsStackMax, false);
  s += F("},\"heap\":{");
  jsonNum(s, "free", ESP.getFreeHeap());
  jsonNum(s, "max_block", ESP.getMaxFreeBlockSize());
  jsonNum(s, "fragmentation_pct", ESP.getHeapFragmentation(), false);
  s += F("}}");
  web.sendHeader(F("Cache-Control"), F("no-store"));
  web.send(200, F("application/json"), s);
}

void handleLog() {
  String s;
  s.reserve(40 * 80);
  class StringPrint : public Print {
   public:
    explicit StringPrint(String& s) : s_(s) {}
    size_t write(uint8_t c) override {
      s_ += static_cast<char>(c);
      return 1;
    }
   private:
    String& s_;
  } out(s);
  logDump(out);
  web.sendHeader(F("Cache-Control"), F("no-store"));
  web.send(200, F("text/plain; charset=utf-8"), s);
}

// ---- setup / loop ----------------------------------------------------------------

void setupOta() {
  ArduinoOTA.setHostname(cfg::kHostname);
  ArduinoOTA.setPasswordHash(OTA_PASSWORD_MD5);
  ArduinoOTA.onStart([] {
    logf("ota: update starting");
    if (queue) queue->commit();
    saveRtc();
  });
  ArduinoOTA.onEnd([] {
    logf("ota: update written, restarting");
    rtc.intentional = 1;  // ArduinoOTA restarts right after this
    saveRtc();
  });
  ArduinoOTA.onError([](ota_error_t e) { logf("ota: failed (%d)", static_cast<int>(e)); });
  ArduinoOTA.begin(false);  // no mDNS: PlatformIO is pointed at the IP
}

void setupClock() {
  settimeofday_cb([](bool fromSntp) {
    if (!fromSntp) return;
    const bool first = !clockFromNtp;
    clockFromNtp = true;
    lastNtpEpoch = static_cast<uint32_t>(time(nullptr));
    if (first) logf("clock: NTP sync");
  });
  configTime(cfg::kTimezone, cfg::kNtp1, cfg::kNtp2, cfg::kNtp3);
  // A crash or update resets the CPU but not the RTC memory: carry the clock
  // across it so the station keeps measuring even without network. Off by the
  // reboot time (~1 s plus up to kRtcSaveMs) until the next NTP sync.
  if (rtc.epoch > kSaneEpoch) {
    struct timeval tv = {static_cast<time_t>(rtc.epoch + 2 + cfg::kRtcSaveMs / 2000), 0};
    settimeofday(&tv, nullptr);
    clockRestored = true;
    lastSlot = rtc.lastSlot;
    logf("clock: restored from RTC memory");
  }
}

void measureIfDue() {
  if (!clockValid()) return;
  const uint32_t now = static_cast<uint32_t>(time(nullptr));
  const uint32_t slot = now - now % cfg::kSlotS;
  if (slot == lastSlot || now - slot > cfg::kSlotGraceS) return;
  if (slot == failedSlot && millis() - failedAtMs < 10000) return;  // retry every 10 s within the grace

  ws::Reading r;
  if (!sensors.measure(slot, r)) {
    failedSlot = slot;
    failedAtMs = millis();
    return;
  }
  // A value the weather app refuses is not queued: refusals are how the
  // uploader recognises a broken app. Retried within the grace, then a gap.
  if (!ws::appAccepts(r, {cfg::kAltitudeM, cfg::kPressureOffsetHpa})) {
    logf("sensors: T=%d RH=%u p=%lu outside what the weather app accepts", r.tempCenti, r.humCenti,
         static_cast<unsigned long>(r.stationDeciPa));
    failedSlot = slot;
    failedAtMs = millis();
    return;
  }
  lastSlot = slot;
  ++measured;
  lastReading = r;
  haveLastReading = true;
  if (queue && !queue->push(r)) {
    ++queueWriteErrors;
    logf("queue: write failed (%lu so far)", static_cast<unsigned long>(queueWriteErrors));
  }
  sensors.maybeDeCondense();
  saveRtc();
}

void watchdogs() {
  const uint32_t up = millis();
  if (rtc.crashes && up > cfg::kStableUptimeMs) {
    rtc.crashes = 0;
    saveRtc();
    logf("boot: stable, crash counter cleared");
  }
  // Wi-Fi down for hours: the AP may be gone, or the SDK's Wi-Fi wedged. The
  // queue is in flash and the clock in RTC memory, so a restart costs nothing.
  if (!wifi.up() && up - wifi.stats().downSinceMs > cfg::kWifiDownRestartMs) restart("Wi-Fi down for 6 h");
  // Backlog, Wi-Fi up for a while, yet nothing accepted for hours: something in
  // the network stack is wedged. The queue is in flash and the clock in RTC
  // memory, so a restart costs nothing.
  if (mode == Mode::Normal && queue && queue->pending() > 0 && wifi.up() && clockValid() &&
      up - wifi.stats().upSinceMs > 30UL * 60 * 1000) {
    const uint32_t lastOk = uploader.stats().lastOkEpoch;
    const uint32_t since = lastOk ? static_cast<uint32_t>(time(nullptr)) - lastOk : up / 1000;
    if (since > cfg::kNoUploadRestartS) restart("no upload accepted for 3 h despite Wi-Fi");
  }
}

// Runs from the SDK's timer, so it still fires when loop() is stuck in a
// delay()-based wait that the hardware watchdogs do not catch (e.g. a library
// polling a dead I2C device forever). Busy loops trip the soft WDT anyway.
void checkLoopStalled(void*) {
  if (millis() - lastLoopMs < cfg::kLoopStallMs) return;
  if (clockValid()) rtc.epoch = static_cast<uint32_t>(time(nullptr));
  rtc.intentional = 0;  // counts as a crash on the next boot
  rtcSave(rtc);
  ESP.reset();
}

}  // namespace

void setup() {
  Serial.begin(115200);
  delay(50);
  const rst_info* ri = ESP.getResetInfoPtr();
  static const String reason = ESP.getResetReason();
  resetReason = reason.c_str();

  const bool haveRtc = rtcLoad(rtc);
  if (!haveRtc || ri->reason == REASON_DEFAULT_RST || ri->reason == REASON_EXT_SYS_RST) {
    // Power-on or reset button: RTC memory is garbage or the outage length is unknown.
    rtc = RtcState{};
  }
  // Exceptions and watchdogs report themselves; abort(), panics, out-of-memory
  // and our own loop watchdog all come back as a plain software restart, which
  // is a crash unless we flagged the restart as planned.
  const bool crashed = ri->reason == REASON_WDT_RST || ri->reason == REASON_EXCEPTION_RST ||
                       ri->reason == REASON_SOFT_WDT_RST ||
                       (ri->reason == REASON_SOFT_RESTART && haveRtc && !rtc.intentional);
  if (crashed) ++rtc.crashes;
  rtc.intentional = 0;
  ++rtc.bootCount;
  mode = rtc.crashes >= cfg::kCrashesSafeMode ? Mode::Safe
         : rtc.crashes >= cfg::kCrashesNoUpload ? Mode::NoUpload
                                                : Mode::Normal;
  rtcSave(rtc);

  lastLoopMs = millis();
  os_timer_setfn(&loopWatchdog, checkLoopStalled, nullptr);
  os_timer_arm(&loopWatchdog, 10000, true);

  logf("boot: weather-station %s, reset: %s, unplanned resets in a row: %lu%s", FW_VERSION, resetReason,
       static_cast<unsigned long>(rtc.crashes),
       mode == Mode::Safe       ? " -> SAFE MODE (Wi-Fi, OTA and status only)"
       : mode == Mode::NoUpload ? " -> measuring and buffering, uploads off"
                                : "");

  setupClock();
  wifi.begin(cfg::kHostname, WIFI_SSID, WIFI_PASS);
  setupOta();
  web.on("/", HTTP_GET, handleStatus);
  web.on("/log", HTTP_GET, handleLog);
  web.onNotFound([] { web.send(404, F("text/plain"), F("not found\n")); });
  web.begin();

  if (mode == Mode::Safe) return;

  fsOk = store.begin() && flashQueue.begin();
  if (fsOk) {
    queue = &flashQueue;
    logf("queue: %lu readings waiting in %u segments", static_cast<unsigned long>(queue->pending()),
         static_cast<unsigned>(queue->segments()));
  } else {
    ramQueue.begin();
    queue = &ramQueue;
    logf("queue: flash unusable, buffering up to 12 h in RAM (lost on reset)");
  }
  lastLoopMs = millis();
  sensors.begin();
  uploader.begin(mode == Mode::Normal ? queue : nullptr, {cfg::kAltitudeM, cfg::kPressureOffsetHpa});
}

void loop() {
  lastLoopMs = millis();
  const bool timeOk = clockValid();
  // Do not start a blocking Wi-Fi scan in the 5 s before a measurement.
  const bool quiet = timeOk && (cfg::kSlotS - static_cast<uint32_t>(time(nullptr) % cfg::kSlotS)) < 5;
  wifi.loop(quiet);
  ArduinoOTA.handle();
  web.handleClient();

  if (mode != Mode::Safe) {
    measureIfDue();
    uploader.loop(wifi.up(), timeOk);
    watchdogs();
  }
  if (mode != Mode::Normal && millis() > cfg::kDegradedMaxMs) {
    rtc.crashes = 0;
    restart("an hour in degraded mode, trying normal mode again");
  }

  if (millis() - lastRtcSaveMs > cfg::kRtcSaveMs) saveRtc();
  delay(5);
}
