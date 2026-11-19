# Weather station firmware

Firmware for the balcony weather station: a Wemos D1 mini (ESP8266) with a
BME280, and optionally an SHT45. A port to the ESP32-S2 (Lolin S2 mini) in
Rust is in [`rust/`](rust/README.md). Every five minutes it takes a reading, keeps
it in flash, and sends it straight to the weather page
([dwzg/weatherpage](https://github.com/dwzg/weatherpage)) over HTTPS. A reading
leaves the flash only after the page has confirmed it, so Wi-Fi or internet
outages of up to 60 days lose nothing.

It replaced Tasmota plus the old relay (Home Assistant → GitHub Actions → app).

## What it does

| | |
| --- | --- |
| **Schedule** | One reading per 5-minute slot (:00, :05, …), the grid the weather app expects. A slot that cannot be measured within 60 s stays a gap; nothing is interpolated. |
| **Sensors** | BME280 in forced mode (sleeps between readings, so it does not warm itself), median of 3 samples. An SHT45 at 0x44 is detected automatically, also later at runtime, and then supplies temperature and humidity; its heater runs for 1 s after a reading above 95 % RH to drive off condensation. |
| **Pressure** | Station pressure, corrected by the calibrated sensor offset, reduced to sea level with the DWD formula (uses the actual temperature and humidity). See [Pressure](#pressure). |
| **Buffer** | LittleFS, one file per day, 16-byte records with CRC; the delivery cursor is replaced atomically. Survives power loss mid-write; corrupt records are skipped, and so is a record whose flash cannot be read 5 times in a row, but only if the records after it can be (a failing flash loses nothing). Readings outside what the app accepts (−40…60 °C, 0…100 %, 800…1100 hPa at sea level) are not queued: that slot is a gap. Holds 60 days, then drops the oldest day. |
| **Upload** | `POST /api/weather`, oldest first, up to 40 per keep-alive TLS connection, never within 70 s of the next measurement. The certificate is verified against Let's Encrypt's roots (ISRG X1, X2, YR, YE in `certs/`), so renewals need no update. 2xx → delivered; 400/422 → the station sends the newest reading as a check: if the app takes that, the refused reading is bad data and is dropped; if it refuses that too, the app (or the firmware's request) is broken and everything is kept, pause 30 min; 3xx/401/403/404/413/415 → pause 30 min; anything else → exponential backoff 15 s – 10 min. 8 KB TLS receive buffer: the server's largest record is the 4.3 KB certificate chain. The app upserts on the timestamp, so a reading sent twice is harmless. |
| **Key** | The station holds `INGEST_API_KEY`, which can only add readings — not export or delete. |
| **Time** | NTP (PTB), time zone Europe/Berlin. Timestamps carry their UTC offset (`2026-11-08T10:00:00+01:00`), so the repeated hour in October is unambiguous. The clock survives crashes and updates in RTC memory, so it keeps measuring offline after a reset. |
| **Wi-Fi** | Scans and joins the strongest access point of the network, re-checks after 10 min of weak signal and roams if another AP is ≥ 8 dB better. Backoff 5 s – 5 min between attempts. |
| **Self-healing** | A loop watchdog resets the station if the main loop stalls for 3 min (e.g. a library waiting forever on a broken I2C bus). Unplanned resets are counted in RTC memory (exceptions, watchdogs, and also abort/out-of-memory, which look like software restarts): after 3 in a row it keeps measuring and buffering but stops uploading, after 5 it runs only Wi-Fi, OTA and the status page; after an hour it tries normal mode again. Restarts itself if nothing was accepted for 3 h with Wi-Fi up, or Wi-Fi has been down for 6 h. If the flash is unusable it buffers 12 h in RAM. |
| **Status** | `http://<ip>/` — JSON with the last reading, queue, upload, Wi-Fi, sensor and heap state. `http://<ip>/log` — the last 40 log lines. Read-only, no secrets. |
| **Updates** | ArduinoOTA with a password (only its MD5 is in the firmware). |

## Build, test, flash

PlatformIO (`pip install platformio`):

```bash
pio test -e native                    # unit tests of lib/core (queue, time, pressure, upload rules)
python3 tools/make-secrets.py         # include/secrets.h from ~/.config/homelab (never committed)
pio run -e station                    # build
OTA_PASSWORD=$(cat ~/.config/homelab/weather-station/ota.pass) pio run -e ota -t upload   # update over Wi-Fi
pio run -e station -t upload          # first flash / recovery over USB
pio device monitor                    # serial log, 115200 baud
```

`lib/core` holds everything that is not hardware (queue, record format, time
formatting, pressure, upload policy, backoff) and builds on the host; `src/`
is the ESP8266 glue. Keep it that way: the ESP32-S2 (Rust) port in
[`rust/`](rust/README.md) follows the same rules and carries the same test
cases, so a change to one belongs in the other too.

## ESP32-S2 (Rust)

[`rust/`](rust/README.md) holds the same firmware for a Wemos Lolin S2 mini,
written in Rust on ESP-IDF. It keeps the record format, the 5-minute grid, the
upload rules and the status page, and adds host tests for the BME280 driver and
the upload policy, plus signed updates with automatic rollback. Build, flash
and wiring are in its README.

## Pressure

The BME280 measures station pressure. Two corrections, both in `src/config.h`:

1. **Offset −3.2 hPa.** Compared against ERA5 reanalysis (2026-06-21 – 11-07,
   n = 3284 hours: −3.12 hPa) and the DWD ICON model (past 60 days, n = 1396:
   −3.28 hPa) for the station's location.
2. **Reduction to sea level with the DWD formula** at 298 m. The standard-atmosphere
   formula assumes 15 °C and drifted by ~2 hPa between summer and November
   here; the DWD formula, which uses the measured temperature and humidity,
   has a third less scatter against the reference and no seasonal trend.

Tasmota used an approximate `pow()` and a −21.6 hPa fudge, which put its values
~1.1 hPa above the exact standard-atmosphere result. The archive up to the
switch to this firmware was converted to the new calculation once (backup
kept), so the series has no step.

Re-check the offset now and then; if it moves, the sensor is ageing.

## Wiring

| D1 mini | BME280 (0x76) | SHT45 (0x44, optional) |
| --- | --- | --- |
| 3V3 | VIN | VIN |
| G | GND | GND |
| D2 (GPIO4) | SDA | SDA |
| D1 (GPIO5) | SCL | SCL |

A radiation shield matters more for temperature than the choice of sensor.
