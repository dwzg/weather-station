# Weather station firmware

Firmware for the balcony weather station: a **Wemos Lolin S2 mini**
(ESP32-S2FN4R2, 4 MB flash) with a BME280, and optionally an SHT45, written in
Rust on ESP-IDF. Every five minutes it takes a reading, keeps it in flash, and
sends it straight to the weather page
([dwzg/weatherpage](https://github.com/dwzg/weatherpage)) over HTTPS. A reading
leaves the flash only after the page has confirmed it, so Wi-Fi or internet
outages of up to 60 days lose nothing.

| Directory | |
| --- | --- |
| `core/` | `ws-core`: everything that is not hardware — queue and record format, POSIX time zones, pressure, upload policy and batching, BME280/SHT4x drivers (over `embedded-hal`), crash counting, OTA authentication. `cargo test` on any machine. |
| `firmware/` | The ESP32-S2 glue: Wi-Fi, SNTP, LittleFS, I2C, HTTPS, web server, OTA, RTC memory. |
| `tools/` | `make-secrets.py` (writes `firmware/secrets.env`), `ota.py` (updates over Wi-Fi). |
| `certs/` | The Let's Encrypt roots the firmware trusts. |

Keep the rules in `core/` and the hardware in `firmware/`: what is in `core/`
is tested on the host.

> **Status:** Builds for the ESP32-S2 (Rust 1.100.0.0 for Xtensa, ESP-IDF 5.5.5)
> without warnings, and clippy is clean. Runs in the station since 2026-11-19:
> measuring with the BME280, uploading, the LittleFS queue, the status page and
> an update over Wi-Fi with confirmation work. The SHT45 is not fitted;
> rollback and safe mode are not yet tried on hardware.

## What it does

| | |
| --- | --- |
| **Schedule** | One reading per 5-minute slot (:00, :05, …), the grid the weather app expects. A slot that cannot be measured within 60 s stays a gap; nothing is interpolated. |
| **Sensors** | BME280 in forced mode (sleeps between readings, so it does not warm itself), median of 3 samples. An SHT45 at 0x44 is detected automatically, also later at runtime, and then supplies temperature and humidity; its heater runs for 1 s after a reading above 95 % RH to drive off condensation. |
| **Pressure** | Station pressure, corrected by the calibrated sensor offset, reduced to sea level with the DWD formula (uses the actual temperature and humidity). See [Pressure](#pressure). |
| **Buffer** | LittleFS, one file per day, 16-byte records with CRC; the delivery cursor is replaced atomically. Survives power loss mid-write; corrupt records are skipped, and so is a record whose flash cannot be read 5 times in a row, but only if the records after it can be (a failing flash loses nothing). Readings outside what the app accepts (−40…60 °C, 0…100 %, 800…1100 hPa at sea level) are not queued: that slot is a gap. Holds 60 days, then drops the oldest day. If the flash is unusable it buffers 12 h in RAM. |
| **Upload** | `POST /api/weather`, oldest first, up to 40 per keep-alive TLS connection, never within 70 s of the next measurement. The certificate is verified against Let's Encrypt's roots (ISRG X1, X2, YR, YE in `certs/`), so renewals need no update. 2xx → delivered. 400, 422 and other 4xx → the station sends the newest reading as a check: if the app takes that, the refused reading is bad data and is dropped; if it refuses that too, the app (or the firmware's request) is broken and everything is kept, pause 30 min. 3xx/401/403/404/405/413/415 → pause 30 min. 408, 429, 5xx, no answer → exponential backoff 15 s – 10 min. The app upserts on the timestamp, so a reading sent twice is harmless. |
| **Key** | The station holds `INGEST_API_KEY`, which can only add readings — not export or delete. |
| **Time** | NTP (PTB), time zone Europe/Berlin, own POSIX TZ rules (`core/src/timefmt.rs`). Timestamps carry their UTC offset (`2026-11-08T10:00:00+01:00`), so the repeated hour in October is unambiguous. The clock survives crashes and updates (ESP-IDF keeps it across resets, checked against a copy in RTC memory), so the station keeps measuring offline after a reset. |
| **Wi-Fi** | Scans and joins the strongest access point of the network, re-checks after 10 min of weak signal and roams if another AP is ≥ 8 dB better. Backoff 5 s – 5 min between attempts. |
| **Self-healing** | A loop watchdog resets the station if the main loop stalls for 3 min (e.g. a driver waiting forever on a broken I2C bus). Unplanned resets (panics, watchdogs) are counted in RTC memory: after 3 in a row it keeps measuring and buffering but stops uploading, after 5 it runs only Wi-Fi, updates and the status page; after an hour it tries normal mode again. Restarts itself if nothing was accepted for 3 h with Wi-Fi up, or Wi-Fi has been down for 6 h. A start-up error (e.g. the Wi-Fi driver refusing to start) also restarts it and counts as a crash. |
| **Status** | `http://<ip>/` — JSON with the last reading, queue, upload, Wi-Fi, sensor, heap and update state. `http://<ip>/log` — the last 40 log lines. Read-only, no secrets. |
| **Updates** | Over Wi-Fi with an HMAC-authenticated `POST /ota` (`tools/ota.py`); a new image must prove itself, or the bootloader goes back to the previous one. See [Updates over Wi-Fi](#updates-over-wi-fi). |

## Wiring

| S2 mini | BME280 (0x76) | SHT45 (0x44, optional) |
| --- | --- | --- |
| 3V3 | VIN | VIN |
| GND | GND | GND |
| 33 | SDA | SDA |
| 35 | SCL | SCL |

GPIO 33/35 are the board's default I2C pins (`firmware/src/config.rs`). The
sensor sits in its own enclosure, away from the board, so the S2's own warmth
does not reach it. A radiation shield matters more for temperature than the
choice of sensor.

## Build, test, flash

```bash
# once: the Xtensa Rust toolchain and the flashing tools
cargo install espup ldproxy espflash
espup install                 # then: . ~/export-esp.sh

cd core && cargo test         # host tests (queue, time, pressure, sensors, uploader, OTA auth)

python3 tools/make-secrets.py # firmware/secrets.env from ~/.config/homelab (never committed; the build needs it)
cd firmware
cargo build --release         # first build downloads ESP-IDF to ~/.espressif (~10 min)
```

Without `secrets.env` the build stops. `WS_EXAMPLE_SECRETS=1` builds with
`secrets.example.env` instead (CI does): no network access, all-zero OTA key.

The first build sets up ESP-IDF's tools and Python environment. Two things can
get in its way:

- **Python newer than 3.13.** ESP-IDF 5.5 needs `tree_sitter`, which has no
  wheel for Python 3.15; pip then compiles it and fails without the Python
  headers. Put a Python 3.13 first on `PATH` for the build, e.g.
  `uv python install 3.13` and a directory with a `python3` symlink to it.
- **Slow GitHub downloads.** The tools (~575 MB) come from GitHub releases;
  `export IDF_GITHUB_ASSETS=dl.espressif.com/github_assets` fetches them from
  Espressif's mirror instead.

### First flash over USB

Hold **0**, press **RST**, release **0**; the board is then in download mode.
Flash with `esptool` (it comes with ESP-IDF), from `firmware/`:

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

### Updates over Wi-Fi

From `firmware/`:

```bash
espflash save-image --chip esp32s2 target/xtensa-esp32s2-espidf/release/weather-station ws.bin
../tools/ota.py <station-ip> ws.bin            # OTA password from $OTA_PASSWORD or the secrets folder
```

Give the station a fixed address in the router (a DHCP reservation for its
MAC address); its hostname is `weather-station`.

`ota.py` then waits until the new image has confirmed itself (see below, up to
~10 min) and exits 0 only then; `--no-wait` stops once the station runs the new
image. It recognises the restart by the status page's boot count and uptime at
the address it was given; if the update changes the address (as a new DHCP
lease can), it reports "did not restart" although the update may have gone
through. Check the status page.

The station hands out a one-time nonce (`GET /ota/nonce`). Before writing
anything it checks an HMAC-SHA256 over the nonce, the image's SHA-256 and its
length; before booting the image it checks the image against that SHA-256. The
key is the SHA-256 of the OTA password; only the key is in the firmware. The
image travels over plain HTTP and holds the Wi-Fi password, the API key and the
OTA key: this trusts the home network.

A new image boots in *pending verify*. It confirms itself once it has measured
and the app has accepted a reading. If it resets before that (a crash, a
watchdog, the 3-hour no-upload or 6-hour no-Wi-Fi restart), the bootloader goes
back to the previous image. ESP-IDF installs no update while the running image
is unconfirmed, so an authenticated update to such an image first sends the
station back to the previous one; `ota.py` waits and sends the update again.
Planned restarts wait while an update is being received.

### Flash layout (`firmware/partitions.csv`)

| | |
| --- | --- |
| `ota_0`, `ota_1` | 1.5 MB each. The app is 1.19 MB (75 %). |
| `storage` | 896 KB LittleFS for the queue. 60 days need 120 of its 224 blocks. |

## Pressure

The BME280 measures station pressure. Two corrections, both in
`firmware/src/config.rs`:

1. **Offset −3.2 hPa.** Compared against ERA5 reanalysis (2026-06-21 – 11-07,
   n = 3284 hours: −3.12 hPa) and the DWD ICON model (past 60 days, n = 1396:
   −3.28 hPa) for the station's location. Measured while the sensor was on the
   ESP8266 board; the offset belongs to the sensor, so it moved with it, but
   re-check it after the move to the S2.
2. **Reduction to sea level with the DWD formula** at 298 m. The standard-atmosphere
   formula assumes 15 °C and drifted by ~2 hPa between summer and November
   here; the DWD formula, which uses the measured temperature and humidity,
   has a third less scatter against the reference and no seasonal trend.

Tasmota used an approximate `pow()` and a −21.6 hPa fudge, which put its values
~1.1 hPa above the exact standard-atmosphere result. The archive up to the
switch away from Tasmota was converted to the new calculation once (backup
kept), so the series has no step.

Re-check the offset now and then; if it moves, the sensor is ageing.

## History

The station ran Tasmota, with readings relayed through Home Assistant and
GitHub Actions to the app. On 2026-11-08 an ESP8266 firmware (C++, PlatformIO,
on a Wemos D1 mini) replaced that and sent readings to the app directly. This
Rust firmware was written as its port to the ESP32-S2 and took over the station
on 2026-11-19, with the same record format, upload rules and status page. The
C++ firmware is in the git history up to commit `023749e`; `core/` still
carries its test vectors (record bytes, queue tests).
