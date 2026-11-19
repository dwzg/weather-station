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
> without warnings, and clippy is clean. Runs in the station since 2026-11-19
> (in place of the ESP8266): measuring with the BME280, uploading, the LittleFS
> queue, the status page and an update over Wi-Fi with confirmation work. The
> SHT45 is not fitted; rollback and safe mode are not yet tried on hardware.

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

python3 tools/make-secrets.py # also writes rust/firmware/secrets.env (never committed; the build needs it)
cd rust/firmware
cargo build --release         # first build downloads ESP-IDF to ~/.espressif (~10 min)
```

The first build sets up ESP-IDF's tools and Python environment. Two things can
get in its way:

- **Python newer than 3.13.** ESP-IDF 5.5 needs `tree_sitter`, which has no
  wheel for Python 3.15; pip then compiles it and fails without the Python
  headers. Put a Python 3.13 first on `PATH` for the build, e.g.
  `uv python install 3.13` and a directory with a `python3` symlink to it.
- **Slow GitHub downloads.** The tools (~575 MB) come from GitHub releases;
  `export IDF_GITHUB_ASSETS=dl.espressif.com/github_assets` fetches them from
  Espressif's mirror instead.

**First flash over USB.** Hold **0**, press **RST**, release **0**; the board is
then in download mode. Flash with `esptool` (it comes with ESP-IDF):

```bash
PY=~/.espressif/python_env/idf5.5_py3.13_env/bin/python   # the env the build created
$PY ~/.espressif/esp-idf/v5.5.5/components/partition_table/gen_esp32part.py partitions.csv partitions.bin
espflash save-image --chip esp32s2 target/xtensa-esp32s2-espidf/release/weather-station ws.bin
head -c 8192 /dev/zero | tr '\0' '\377' > otadata.bin  # blank: boot ota_0
$PY -m esptool --chip esp32s2 --port /dev/ttyACM0 --before no_reset write_flash \
    0x1000 target/xtensa-esp32s2-espidf/release/bootloader.bin \
    0x8000 partitions.bin 0xe000 otadata.bin 0x20000 ws.bin
```

This writes the ESP-IDF bootloader (which has app rollback enabled), the
partition table from `partitions.csv`, a blank `otadata` and the app. Press
**RST** afterwards: `esptool`'s own reset leaves the S2's USB in a state in
which the app panics in a loop until a real reset.

Not `cargo run --release` / `espflash` (4.7) over the S2's own USB: its flasher
stub times out (`MemData`), and without the stub the ROM rejects its
`EraseRegion` and `FlashEnd` commands. After each such failure the ROM loader
stops answering until the next 0 + RST.

**Logs.** The last lines of the log are at `http://<station>/log`. The USB
console (`CONFIG_ESP_CONSOLE_USB_CDC`) did not enumerate on the first hardware
run (`device descriptor read … error -71`), even after a power-on reset; not
yet looked into.

**Updates over Wi-Fi.**

```bash
espflash save-image --chip esp32s2 target/xtensa-esp32s2-espidf/release/weather-station ws.bin
tools/ota-s2.py 192.168.178.57 ws.bin          # password from ~/.config/homelab/weather-station/ota.pass
```

The S2 replaced the ESP8266 in the station on 2026-11-19 and took over its
address: the FRITZ!Box always gives 192.168.178.57 to the S2's MAC
(`80:65:99:4d:c1:1a`). The ESP8266 used the same hostname, `weather-station`.

`ota-s2.py` then waits until the new image has confirmed itself (see below,
up to ~10 min) and exits 0 only then; `--no-wait` stops once the station runs
the new image. It recognises the restart by the status page's boot count and
uptime at the address it was given; if the update changes the address (as a
new DHCP lease can), it reports "did not restart" although the update may have
gone through. Check the status page.

Without `secrets.env` the build stops. `WS_EXAMPLE_SECRETS=1` builds with
`secrets.example.env` instead (CI does): no network access, all-zero OTA key.

The station hands out a one-time nonce (`GET /ota/nonce`). Before writing
anything it checks an HMAC-SHA256 over the nonce, the image's SHA-256 and its
length; before booting the image it checks the image against that SHA-256. The
key is the SHA-256 of the OTA password; only the key is in the firmware.

A new image boots in *pending verify*. It confirms itself once it has measured
and the app has accepted a reading. If it resets before that (a crash, a
watchdog, the 3-hour no-upload or 6-hour no-Wi-Fi restart), the bootloader goes
back to the previous image. ESP-IDF installs no update while the running image
is unconfirmed, so an authenticated update to such an image first sends the
station back to the previous one; `ota-s2.py` waits and sends the update again.
Planned restarts wait while an update is being received.

A start-up error (e.g. the Wi-Fi driver refusing to start) restarts the station
and counts as a crash, so a persistent one ends in safe mode.

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
