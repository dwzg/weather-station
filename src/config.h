#pragma once
#include <stdint.h>

// Everything about this station that is not a secret.
namespace cfg {

constexpr char kHostname[] = "weather-station";
constexpr char kTimezone[] = "CET-1CEST,M3.5.0,M10.5.0/3";  // Europe/Berlin
constexpr char kNtp1[] = "ptbtime1.ptb.de";
constexpr char kNtp2[] = "ptbtime2.ptb.de";
constexpr char kNtp3[] = "de.pool.ntp.org";

// Readings are taken at the start of every 5-minute slot (:00, :05, ...), the
// grid the weather app expects. A slot missed by more than kSlotGraceS (boot,
// no clock yet) is left as a gap rather than filled with a late value.
constexpr uint32_t kSlotS = 300;
constexpr uint32_t kSlotGraceS = 60;

// Pressure calibration, see README "Pressure". Altitude of the sensor and the
// BME280's offset against ERA5 reanalysis and the DWD ICON model
// (2026-06-21..2026-11-07: -3.12 and -3.28 hPa).
constexpr double kAltitudeM = 298.0;
constexpr double kPressureOffsetHpa = -3.2;

// Flash queue: one segment (file) per day, 60 days at most: 17,280 readings,
// 276 KB of the 2 MB file system. Beyond that the oldest day is dropped.
constexpr uint16_t kRecordsPerSegment = 288;
constexpr uint16_t kMaxSegments = 60;

// Hardware
constexpr uint8_t kPinSda = 4;  // D2
constexpr uint8_t kPinScl = 5;  // D1
constexpr uint8_t kBme280Addr = 0x76;
constexpr uint8_t kSht4xAddr = 0x44;
constexpr float kWifiTxDbm = 20.0f;

// Upload
constexpr uint8_t kBatchMax = 40;            // readings per TLS connection
constexpr uint32_t kBatchBudgetMs = 25000;   // ... and at most this long
constexpr uint32_t kHttpTimeoutMs = 12000;
constexpr uint32_t kRetryMinMs = 15000;
constexpr uint32_t kRetryMaxMs = 10UL * 60 * 1000;
constexpr uint32_t kHaltMs = 30UL * 60 * 1000;  // after 401/403/404: someone has to fix the config
constexpr uint32_t kMinHeapBlockForTls = 22000;

// Self-healing
constexpr uint32_t kStableUptimeMs = 10UL * 60 * 1000;     // clears the crash counter
constexpr uint8_t kCrashesForSafeMode = 3;
constexpr uint32_t kSafeModeMaxMs = 60UL * 60 * 1000;      // then try normal mode again
constexpr uint32_t kNoUploadRestartS = 3UL * 3600;         // backlog, Wi-Fi up, nothing accepted
constexpr uint32_t kRtcSaveMs = 5000;

}  // namespace cfg
