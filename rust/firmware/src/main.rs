//! Balcony weather station on a Wemos Lolin S2 mini (ESP32-S2): measures every
//! 5 minutes, keeps every reading in flash until the weather app has confirmed
//! it, and sends it there directly. The rules live in ws-core; this is the glue.

mod bus;
mod config;
mod logger;
mod net;
mod platform;
mod rtc;
mod store;
mod web;
mod wifi;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SntpConf};
use esp_idf_svc::sys;
use esp_idf_svc::timer::EspTaskTimerService;
use log::{info, warn};
use ws_core::boot::{self, Mode, RtcState};
use ws_core::json::JsonObject;
use ws_core::queue::{MemStore, ReadingQueue, SegmentStore};
use ws_core::reading::{Reading, SOURCE_SHT4X};
use ws_core::sensors::Sensors;
use ws_core::timefmt::TimeZone;
use ws_core::upload::{app_accepts, format_centi, sea_level_centi_hpa, Calibration};
use ws_core::uploader::{Env, UploadConfig, Uploader};

use crate::bus::I2cBus;
use crate::store::LittleFsStore;
use crate::wifi::WifiLink;

const FW_VERSION: &str = env!("FW_VERSION");
const CAL: Calibration =
    Calibration { altitude_m: config::ALTITUDE_M, pressure_offset_hpa: config::PRESSURE_OFFSET_HPA };

/// Low 32 bits of millis() at the last loop pass (the S2 has no 64-bit atomics;
/// wrapping_sub keeps the comparison right across the 49-day wrap).
static LAST_LOOP_MS: AtomicU32 = AtomicU32::new(0);

fn loop_alive() {
    LAST_LOOP_MS.store(platform::millis() as u32, Ordering::Relaxed);
}

/// The flash, or the RAM fallback when the flash is unusable.
enum Store {
    Flash(LittleFsStore),
    Ram(MemStore),
}

impl Store {
    fn inner(&mut self) -> &mut dyn SegmentStore {
        match self {
            Store::Flash(s) => s,
            Store::Ram(s) => s,
        }
    }
}

impl SegmentStore for Store {
    fn list_segments(&mut self) -> Option<Vec<u32>> {
        self.inner().list_segments()
    }
    fn segment_size(&mut self, id: u32) -> Option<u32> {
        self.inner().segment_size(id)
    }
    fn append(&mut self, id: u32, data: &[u8]) -> bool {
        self.inner().append(id, data)
    }
    fn read(&mut self, id: u32, offset: u32, buf: &mut [u8]) -> bool {
        self.inner().read(id, offset, buf)
    }
    fn remove_segment(&mut self, id: u32) -> bool {
        self.inner().remove_segment(id)
    }
    fn load_cursor(&mut self, buf: &mut [u8]) -> bool {
        self.inner().load_cursor(buf)
    }
    fn save_cursor(&mut self, data: &[u8]) -> bool {
        self.inner().save_cursor(data)
    }
}

struct DeviceEnv;

impl Env for DeviceEnv {
    fn now_ms(&self) -> u64 {
        platform::millis()
    }
    fn epoch(&self) -> u32 {
        platform::epoch()
    }
    fn random(&mut self) -> u32 {
        platform::random()
    }
}

struct Station {
    tz: TimeZone,
    mode: Mode,
    reset_reason: &'static str,
    rtc: RtcState,
    last_slot: u32,
    failed_slot: u32,
    failed_at_ms: u64,
    last_rtc_save_ms: u64,
    clock_restored: bool,
    queue: Option<ReadingQueue<Store>>,
    queue_write_errors: u32,
    bus: Option<I2cBus>,
    sensors: Sensors,
    measured: u32,
    last_reading: Option<Reading>,
    uploader: Uploader,
    wifi: WifiLink,
    ota_unconfirmed: bool,
}

impl Station {
    fn save_rtc(&mut self) {
        self.rtc.epoch = if platform::clock_valid() { platform::epoch() } else { 0 };
        self.rtc.last_slot = self.last_slot;
        rtc::save(&self.rtc);
        self.last_rtc_save_ms = platform::millis();
    }

    fn restart(&mut self, why: &str) -> ! {
        info!("restart: {why}");
        if let Some(q) = self.queue.as_mut() {
            q.commit();
        }
        self.rtc.intentional = 1; // not a crash
        self.save_rtc();
        FreeRtos::delay_ms(500); // lets a last HTTP response leave
        platform::restart_now()
    }

    /// A restart the station chooses itself: never in the middle of an update.
    fn planned_restart(&mut self, why: &str) {
        if !web::ota_in_progress() {
            self.restart(why);
        }
    }

    /// An update is waiting for the previous image: return to it.
    fn roll_back_for_update(&mut self) {
        info!("ota: returning to the previous image, so the update can be installed there");
        if let Some(q) = self.queue.as_mut() {
            q.commit();
        }
        self.rtc.intentional = 1;
        self.save_rtc();
        FreeRtos::delay_ms(500);
        // Only returns if there is no image to go back to.
        let err = unsafe { sys::esp_ota_mark_app_invalid_rollback_and_reboot() };
        let why = sys::EspError::from(err).map_or_else(|| "?".to_string(), |e| e.to_string());
        warn!("ota: cannot roll back ({why}); confirming this image so the update can go in");
        self.rtc.intentional = 0;
        self.save_rtc();
        self.confirm_image();
        web::clear_rollback();
    }

    fn confirm_image(&mut self) {
        self.ota_unconfirmed = false;
        match sys::esp!(unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() }) {
            Ok(()) => info!("ota: new firmware confirmed"),
            Err(e) => warn!("ota: could not confirm the firmware ({e})"),
        }
    }

    fn measure_if_due(&mut self) {
        if !platform::clock_valid() {
            return;
        }
        let Some(slot) = boot::due_slot(platform::epoch(), self.last_slot, config::SLOT_S, config::SLOT_GRACE_S) else {
            return;
        };
        let now_ms = platform::millis();
        if slot == self.failed_slot && now_ms - self.failed_at_ms < config::MEASURE_RETRY_MS {
            return; // retry every 10 s within the grace
        }
        let Some(bus) = self.bus.as_mut() else { return };
        let measured = self.sensors.measure(bus, &mut FreeRtos, slot, now_ms);
        // A value the weather app refuses is not queued: refusals are how the
        // uploader recognises a broken app. Retried within the grace, then a gap.
        let Some(r) = measured.filter(|r| {
            let ok = app_accepts(r, &CAL);
            if !ok {
                warn!(
                    "sensors: T={} RH={} p0={} is outside what the weather app accepts",
                    format_centi(i32::from(r.temp_centi)),
                    format_centi(i32::from(r.hum_centi)),
                    format_centi(sea_level_centi_hpa(r, &CAL))
                );
            }
            ok
        }) else {
            self.failed_slot = slot;
            self.failed_at_ms = now_ms;
            return;
        };
        self.last_slot = slot;
        self.measured += 1;
        self.last_reading = Some(r);
        if let Some(q) = self.queue.as_mut() {
            if !q.push(&r) {
                self.queue_write_errors += 1;
                warn!("queue: write failed ({} so far)", self.queue_write_errors);
            }
        }
        self.sensors.maybe_decondense(bus, &mut FreeRtos);
        self.save_rtc();
    }

    fn upload(&mut self) {
        let Some(q) = self.queue.as_mut() else { return };
        if web::ota_in_progress() {
            return; // leave the RAM and the radio to the update
        }
        let ready = self.wifi.up() && platform::clock_valid();
        self.uploader.poll(q, ready, &mut DeviceEnv, || net::Https::connect(env!("WS_API_URL"), env!("WS_API_KEY")));
    }

    fn watchdogs(&mut self) {
        let up = platform::millis();
        if self.rtc.crashes > 0 && up > config::STABLE_UPTIME_MS {
            self.rtc.crashes = 0;
            self.save_rtc();
            info!("boot: stable, crash counter cleared");
        }
        let pending = self.queue.as_ref().map_or(0, |q| q.pending());
        // A new image is confirmed once it has shown it can do its job: it has
        // measured, and the app has accepted a reading from it.
        if self.ota_unconfirmed && self.measured > 0 && self.uploader.stats().accepted > 0 {
            self.confirm_image();
        }
        // Wi-Fi down for hours: the AP may be gone, or the radio driver wedged.
        if !self.wifi.up() && up - self.wifi.stats().down_since_ms > config::WIFI_DOWN_RESTART_MS {
            self.planned_restart("Wi-Fi down for 6 h");
        }
        // Backlog, Wi-Fi up for a while, yet nothing accepted for hours: something
        // in the network stack is wedged. The queue is in flash and the clock in
        // RTC memory, so a restart costs nothing. (An unconfirmed update that
        // cannot upload is rolled back by this.)
        if self.mode == Mode::Normal
            && pending > 0
            && self.wifi.up()
            && platform::clock_valid()
            && up - self.wifi.stats().up_since_ms > 30 * 60 * 1000
        {
            let last_ok = self.uploader.stats().last_ok_epoch;
            let since = if last_ok > 0 { platform::epoch().saturating_sub(last_ok) } else { (up / 1000) as u32 };
            if since > config::NO_UPLOAD_RESTART_S {
                self.planned_restart("no upload accepted for 3 h despite Wi-Fi");
            }
        }
    }

    fn status_json(&self) -> String {
        let up_ms = platform::millis();
        let now = platform::epoch();
        let mut j = JsonObject::new();
        j.str("firmware", FW_VERSION)
            .raw("uptime_s", up_ms / 1000)
            .str("reset_reason", self.reset_reason)
            .raw("boot_count", self.rtc.boot_count)
            .str("mode", self.mode.describe())
            .raw("crashes_in_a_row", self.rtc.crashes);

        let source = if platform::clock_from_ntp() {
            "ntp"
        } else if self.clock_restored {
            "rtc (restored after reset)"
        } else {
            "none"
        };
        let now_s = if platform::clock_valid() { self.tz.format_iso(i64::from(now)) } else { String::new() };
        j.open("clock").str("now", &now_s).str("source", source).raw("last_ntp_sync", platform::last_ntp()).close();

        let (rssi, bssid) = self.wifi.ap().map_or((0, String::new()), |(r, b)| {
            (r, b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":"))
        });
        let ws = self.wifi.stats();
        j.open("wifi")
            .bool("up", self.wifi.up())
            .raw("rssi_dbm", rssi)
            .str("bssid", &bssid)
            .str("ip", &self.wifi.ip())
            .raw("connects", ws.connects)
            .raw("drops", ws.drops)
            .raw("roams", ws.roams)
            .close();

        let ss = self.sensors.status();
        j.open("sensors")
            .bool("bme280", ss.bme280)
            .bool("sht4x", ss.sht4x)
            .raw("measured", self.measured)
            .raw("failures", ss.failures)
            .raw("bus_recoveries", ss.bus_recoveries)
            .raw("heater_pulses", ss.heater_pulses);
        if let Some(r) = &self.last_reading {
            j.open("last")
                .str("slot", &self.tz.format_iso(i64::from(r.epoch)))
                .raw("temperature", format_centi(i32::from(r.temp_centi)))
                .raw("humidity", format_centi(i32::from(r.hum_centi)))
                .raw("station_pressure", format!("{}.{:03}", r.station_deci_pa / 1000, r.station_deci_pa % 1000))
                .raw("sea_level_pressure", format_centi(sea_level_centi_hpa(r, &CAL)))
                .str("source", if r.source == SOURCE_SHT4X { "sht4x" } else { "bme280" })
                .close();
        }
        j.close();

        let (storage, fs) = match self.queue.as_ref().map(|q| q.store()) {
            Some(Store::Flash(s)) => ("flash", s.usage()),
            Some(Store::Ram(_)) => ("RAM (flash unusable)", (0, 0)),
            None => ("none", (0, 0)),
        };
        let q = self.queue.as_ref();
        j.open("queue")
            .str("storage", storage)
            .raw("pending", q.map_or(0, |q| q.pending()))
            .raw("segments", q.map_or(0, |q| q.segments()))
            .raw("dropped_full", q.map_or(0, |q| q.dropped()))
            .raw("corrupt_skipped", q.map_or(0, |q| q.corrupt()))
            .raw("read_errors", q.map_or(0, |q| q.read_errors()))
            .raw("unreadable_skipped", q.map_or(0, |q| q.unreadable()))
            .raw("write_errors", self.queue_write_errors)
            .raw("fs_used", fs.0)
            .raw("fs_total", fs.1)
            .close();

        let us = self.uploader.stats();
        j.open("upload")
            .raw("accepted", us.accepted)
            .raw("rejected", us.rejected)
            .raw("failed_batches", us.failed_batches)
            .raw("last_ok", us.last_ok_epoch)
            .raw("last_status", us.last_status)
            .str("last_error", &us.last_error)
            .raw("retry_in_s", self.uploader.retry_in_ms(up_ms) / 1000)
            .raw("last_batch_ms", us.last_batch_ms)
            .close();

        j.open("heap")
            .raw("free", platform::free_heap())
            .raw("min_free", platform::min_free_heap())
            .raw("max_block", platform::largest_block())
            .close();
        j.open("ota").bool("unconfirmed", self.ota_unconfirmed).close();
        j.finish()
    }
}

fn main() {
    sys::link_patches();
    let tz = TimeZone::parse(config::TIMEZONE).expect("valid time zone");
    logger::init(tz);
    // run() only comes back with an error. Returning from main would end the
    // task and leave the station idle, with no watchdog. Restart instead,
    // counted as a crash, so a failure that persists ends in safe mode; wait
    // longer the more often it failed.
    let Err(e) = run(tz);
    log::error!("boot: start-up failed: {e:#}");
    let mut s = rtc::load().unwrap_or_default();
    if platform::clock_valid() {
        s.epoch = platform::epoch();
    }
    s.intentional = 0;
    rtc::save(&s);
    FreeRtos::delay_ms(5000 * s.crashes.clamp(1, 12));
    platform::restart_now();
}

fn run(tz: TimeZone) -> anyhow::Result<std::convert::Infallible> {
    let (kind, reset_reason) = platform::reset_reason();
    let b = boot::on_boot(rtc::load(), kind, config::CRASHES_NO_UPLOAD, config::CRASHES_SAFE_MODE);
    rtc::save(&b.state);

    // Runs from the timer task, so it still fires when the main loop is stuck
    // in a wait that the hardware watchdogs do not catch (e.g. a driver waiting
    // forever on a broken bus). It counts as a crash on the next boot.
    loop_alive();
    let timers = EspTaskTimerService::new()?;
    let loop_watchdog = timers.timer(|| {
        let stalled = (platform::millis() as u32).wrapping_sub(LAST_LOOP_MS.load(Ordering::Relaxed));
        if u64::from(stalled) < config::LOOP_STALL_MS {
            return;
        }
        let mut s = rtc::load().unwrap_or_default();
        if platform::clock_valid() {
            s.epoch = platform::epoch();
        }
        s.intentional = 0;
        rtc::save(&s);
        platform::restart_now();
    })?;
    loop_watchdog.every(Duration::from_secs(10))?;

    info!(
        "boot: weather-station {FW_VERSION}, reset: {reset_reason}, unplanned resets in a row: {}{}",
        b.state.crashes,
        match b.mode {
            Mode::Safe => " -> SAFE MODE (Wi-Fi, updates and status only)",
            Mode::NoUpload => " -> measuring and buffering, uploads off",
            Mode::Normal => "",
        }
    );

    // A crash or update resets the CPU but not the RTC: ESP-IDF keeps the system
    // time across it. If it did not, restore it from RTC memory, off by the reboot
    // time (~1 s plus up to RTC_SAVE_MS) until the next NTP sync.
    let mut clock_restored = false;
    if b.state_kept && platform::epoch() > platform::SANE_EPOCH {
        clock_restored = true;
        info!("clock: kept across the reset");
    } else if b.state_kept && b.state.epoch > platform::SANE_EPOCH {
        platform::set_epoch(b.state.epoch + 2 + (config::RTC_SAVE_MS / 2000) as u32);
        clock_restored = true;
        info!("clock: restored from RTC memory");
    }
    if clock_restored {
        platform::trust_clock();
    }
    let mut sntp_conf = SntpConf::default();
    for (slot, server) in sntp_conf.servers.iter_mut().zip(config::NTP_SERVERS) {
        *slot = server;
    }
    let _sntp = EspSntp::new_with_callback(&sntp_conf, |_| {
        if platform::ntp_synced(platform::epoch()) {
            info!("clock: NTP sync");
        }
    })?;

    let p = Peripherals::take()?;
    let wifi = WifiLink::new(
        p.modem,
        EspSystemEventLoop::take()?,
        EspDefaultNvsPartition::take()?,
        env!("WS_WIFI_SSID"),
        env!("WS_WIFI_PASS"),
    )?;
    let ota_key: [u8; 32] =
        ws_core::ota::from_hex(env!("WS_OTA_KEY")).and_then(|k| k.try_into().ok()).expect("OTA_KEY");
    let _server = web::start(ota_key)?;

    let uploader = Uploader::new(UploadConfig {
        cal: CAL,
        tz,
        slot_s: config::SLOT_S,
        batch_max: config::BATCH_MAX,
        batch_budget_ms: config::BATCH_BUDGET_MS,
        retry_min_ms: config::RETRY_MIN_MS,
        retry_max_ms: config::RETRY_MAX_MS,
        halt_ms: config::HALT_MS,
        stop_before_slot_s: config::STOP_BEFORE_SLOT_S,
        start_after_slot_s: config::START_AFTER_SLOT_S,
    });

    let mut st = Station {
        tz,
        mode: b.mode,
        reset_reason,
        rtc: b.state,
        last_slot: if b.state_kept { b.state.last_slot } else { 0 },
        failed_slot: 0,
        failed_at_ms: 0,
        last_rtc_save_ms: 0,
        clock_restored,
        queue: None,
        queue_write_errors: 0,
        bus: None,
        sensors: Sensors::new(),
        measured: 0,
        last_reading: None,
        uploader,
        wifi,
        ota_unconfirmed: platform::running_image_unconfirmed(),
    };
    if st.ota_unconfirmed {
        info!("ota: running a new image, confirming it once it uploads");
    }

    if st.mode != Mode::Safe {
        let flash = LittleFsStore::mount(config::FS_BASE, config::FS_PARTITION)
            .map(|s| ReadingQueue::new(Store::Flash(s), config::RECORDS_PER_SEGMENT, config::MAX_SEGMENTS))
            .and_then(|mut q| q.begin().then_some(q));
        st.queue = Some(match flash {
            Some(q) => {
                info!("queue: {} readings waiting in {} segments", q.pending(), q.segments());
                q
            }
            None => {
                warn!("queue: flash unusable, buffering up to 12 h in RAM (lost on reset)");
                let mut q = ReadingQueue::new(
                    Store::Ram(MemStore::default()),
                    config::RAM_RECORDS_PER_SEGMENT,
                    config::RAM_MAX_SEGMENTS,
                );
                q.begin();
                q
            }
        });
        loop_alive();
        let mut bus = I2cBus::new(p.i2c0, p.pins.gpio33, p.pins.gpio35);
        st.sensors.begin(&mut bus, &mut FreeRtos, platform::millis());
        st.bus = Some(bus);
    }

    let mut last_status_ms = 0;
    loop {
        loop_alive();
        let time_ok = platform::clock_valid();
        // Do not start a blocking Wi-Fi scan in the 5 s before a measurement.
        let quiet = time_ok && config::SLOT_S - platform::epoch() % config::SLOT_S < 5;
        st.wifi.poll(quiet);

        if st.mode != Mode::Safe {
            st.measure_if_due();
            if st.mode == Mode::Normal {
                st.upload();
            }
        }
        st.watchdogs();
        if st.mode != Mode::Normal && platform::millis() > config::DEGRADED_MAX_MS && !web::ota_in_progress() {
            st.rtc.crashes = 0;
            st.restart("an hour in degraded mode, trying normal mode again");
        }
        if web::restart_requested() {
            st.planned_restart("firmware update installed"); // once the response is out
        }
        if web::rollback_requested() && !web::ota_in_progress() {
            st.roll_back_for_update();
        }
        if platform::millis() - st.last_rtc_save_ms > config::RTC_SAVE_MS {
            st.save_rtc();
        }
        if platform::millis() - last_status_ms >= 1000 {
            last_status_ms = platform::millis();
            web::set_status(st.status_json());
        }
        FreeRtos::delay_ms(10);
    }
}
