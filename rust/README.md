# Weather station firmware — ESP32-S2 (Rust)

The same station on a **Wemos Lolin S2 mini** (ESP32-S2FN4R2, 4 MB flash),
written in Rust on ESP-IDF. It does what the ESP8266 firmware in `../src` does
(same 5-minute grid, same flash record format, same upload rules, same
self-healing, same status page) and keeps its rules in a crate that is tested
on the host.

| Directory | |
| --- | --- |
| `core/` | `ws-core`: everything that is not hardware — queue and record format, POSIX time zones, pressure, upload policy and batching, BME280/SHT4x drivers (over `embedded-hal`), crash counting, OTA authentication. `cargo test` on any machine. |
| `firmware/` | The ESP32-S2 glue: Wi-Fi, SNTP, LittleFS, I2C, HTTPS, web server, OTA, RTC memory. |

> **Status:** Builds for the ESP32-S2 (Rust 1.100.0.0 for Xtensa, ESP-IDF 5.5.5)
> without warnings, and clippy is clean. Not yet run on hardware.

## Wiring

| S2 mini | BME280 (0x76) | SHT45 (0x44, optional) |
| --- | --- | --- |
| 3V3 | VIN | VIN |
| GND | GND | GND |
| 33 | SDA | SDA |
| 35 | SCL | SCL |

GPIO 33/35 are the board's default I2C pins (`config.rs`).

## Build, test, flash

```bash
# once: the Xtensa Rust toolchain and the flashing tools
cargo install espup ldproxy espflash
espup install                 # then: . ~/export-esp.sh

cd rust/core && cargo test    # host tests (queue, time, pressure, sensors, uploader, OTA auth)

python3 tools/make-secrets.py # also writes rust/firmware/secrets.env (never committed)
cd rust/firmware
cargo build --release         # first build downloads ESP-IDF to ~/.espressif (~10 min)
```

**First flash over USB.** Hold **0**, press **RST**, release **0**; the board is
then in download mode. Then:

```bash
cargo run --release           # espflash: bootloader, partition table, app; then the console
```

`espflash` also writes `partitions.csv` and the ESP-IDF bootloader, which has
app rollback enabled. Press **RST** afterwards. The console runs over the S2's
own USB (`espflash monitor`).

**Updates over Wi-Fi.**

```bash
espflash save-image --chip esp32s2 target/xtensa-esp32s2-espidf/release/weather-station ws.bin
tools/ota-s2.py 192.168.178.57 ws.bin          # password from ~/.config/homelab/weather-station/ota.pass
```

The station hands out a one-time nonce (`GET /ota/nonce`). Before writing
anything it checks an HMAC-SHA256 over the nonce, the image's SHA-256 and its
length; before booting the image it checks the image against that SHA-256. The
key is the SHA-256 of the OTA password; only the key is in the firmware.

A new image boots in *pending verify*. It confirms itself with its first
accepted upload, or after 10 minutes in normal mode with an empty queue. If it
resets before that (a crash, a watchdog, or the 3-hour no-upload restart), the
bootloader goes back to the previous image.

## Flash layout (`firmware/partitions.csv`)

| | |
| --- | --- |
| `ota_0`, `ota_1` | 1.5 MB each. The app is 1.18 MB (75 %). |
| `storage` | 896 KB LittleFS for the queue. 60 days need 120 of its 224 blocks. |

## Differences from the ESP8266 firmware

| | ESP8266 (`../src`) | ESP32-S2 (`rust/`) |
| --- | --- | --- |
| BME280 | Adafruit library | own driver, datasheet integer formulas, every wait bounded; tested against the datasheet example |
| Time zone | libc `TZ` | own POSIX TZ rules (`timefmt.rs`), the same on host and device |
| Upload policy | in `uploader.cpp`, untested | `ws-core::uploader` behind a `Transport` trait; batch, window, reject/halt and backoff rules have tests |
| TLS | BearSSL, 8 KB buffer, heap checks | mbedTLS with the same four Let's Encrypt roots; refuses to start below 60 KB free heap |
| Updates | ArduinoOTA (espota) | `POST /ota` with HMAC (`tools/ota-s2.py`), rollback |
| Clock across resets | RTC memory | ESP-IDF keeps the system time across software resets; RTC memory as fallback |
| Loop watchdog | `os_timer` | `esp_timer` task; a stall counts as a crash |
| Status JSON | | same fields. `upload.last_status` is 0 when there was no HTTP answer (was a negative HTTPClient code). `heap` has `free`, `min_free`, `max_block`. New: `ota.unconfirmed`, `upload.last_batch_ms`. Dropped: the BearSSL figures. |

The pressure offset (−3.2 hPa) belongs to the BME280, so it moves with the
sensor. The sensor sits in its own enclosure, away from the board, so the
S2's own warmth does not reach it.
