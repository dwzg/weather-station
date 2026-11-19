//! Everything about this station that is not a secret.

use std::time::Duration;

pub const HOSTNAME: &str = "weather-station";
pub const TIMEZONE: &str = "CET-1CEST,M3.5.0,M10.5.0/3"; // Europe/Berlin
pub const NTP_SERVERS: [&str; 3] = ["ptbtime1.ptb.de", "ptbtime2.ptb.de", "de.pool.ntp.org"];

/// Readings are taken at the start of every 5-minute slot (:00, :05, ...), the
/// grid the weather app expects. A slot missed by more than SLOT_GRACE_S (boot,
/// no clock yet) is left as a gap rather than filled with a late value.
pub const SLOT_S: u32 = 300;
pub const SLOT_GRACE_S: u32 = 60;
/// A failed measurement is retried this often within the grace.
pub const MEASURE_RETRY_MS: u64 = 10_000;

/// Pressure calibration, see README "Pressure": altitude of the sensor and the
/// BME280's offset against ERA5 reanalysis and the DWD ICON model
/// (2026-06-21..2026-11-07: -3.12 and -3.28 hPa). Measured with the sensor on
/// the ESP8266 board; re-check it after the move to the S2.
pub const ALTITUDE_M: f64 = 298.0;
pub const PRESSURE_OFFSET_HPA: f64 = -3.2;

/// Flash queue: one segment (file) per day, 60 days at most: 17,280 readings.
/// Beyond that the oldest day is dropped.
pub const RECORDS_PER_SEGMENT: u16 = 288;
pub const MAX_SEGMENTS: u16 = 60;
/// RAM fallback if the flash is unusable: 12 h.
pub const RAM_RECORDS_PER_SEGMENT: u16 = 36;
pub const RAM_MAX_SEGMENTS: u16 = 4;
pub const FS_BASE: &str = "/fs";
pub const FS_PARTITION: &str = "storage";

// Hardware: the Lolin S2 mini's default I2C pins.
pub const PIN_SDA: i32 = 33;
pub const PIN_SCL: i32 = 35;
pub const I2C_HZ: u32 = 100_000;
pub const I2C_TIMEOUT_MS: u64 = 100;
/// 20 dBm, in units of 0.25 dBm.
pub const WIFI_TX_QUARTER_DBM: i8 = 80;

// Upload
pub const BATCH_MAX: u32 = 40; // readings per TLS connection
pub const BATCH_BUDGET_MS: u64 = 25_000; // ... and at most this long
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
pub const RETRY_MIN_MS: u32 = 15_000;
pub const RETRY_MAX_MS: u32 = 10 * 60 * 1000;
pub const HALT_MS: u32 = 30 * 60 * 1000; // after 401/403/404: someone has to fix the config
pub const STOP_BEFORE_SLOT_S: u32 = 70;
pub const START_AFTER_SLOT_S: u32 = 3;
/// Don't start TLS below this much free heap (mbedTLS needs ~45 KB for a handshake).
pub const TLS_MIN_FREE_HEAP: u32 = 60_000;
pub const TLS_MIN_BLOCK: u32 = 20_000;

// Self-healing
pub const STABLE_UPTIME_MS: u64 = 10 * 60 * 1000; // clears the crash counter
pub const CRASHES_NO_UPLOAD: u32 = 3; // then: measure and buffer, but do not upload
pub const CRASHES_SAFE_MODE: u32 = 5; // then: Wi-Fi, updates and status page only
pub const DEGRADED_MAX_MS: u64 = 60 * 60 * 1000; // then try normal mode again
pub const LOOP_STALL_MS: u64 = 180_000; // loop not run for this long: reset (worst batch ~80 s)
pub const NO_UPLOAD_RESTART_S: u32 = 3 * 3600; // backlog, Wi-Fi up, nothing accepted
/// Wi-Fi down this long: restart, in case the radio driver is wedged. The queue
/// is in flash and ESP-IDF keeps the clock, so a restart costs nothing.
pub const WIFI_DOWN_RESTART_MS: u64 = 6 * 3600 * 1000;
pub const RTC_SAVE_MS: u64 = 5000;

// Updates
pub const OTA_NONCE_TTL_MS: u64 = 120_000;
