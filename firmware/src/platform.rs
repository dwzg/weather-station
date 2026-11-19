//! Small wrappers around ESP-IDF: time, randomness, heap, reset reason.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use esp_idf_svc::sys;
use ws_core::boot::ResetKind;

/// Anything earlier means "no clock yet".
pub const SANE_EPOCH: u32 = 1_700_000_000;

static CLOCK_TRUSTED: AtomicBool = AtomicBool::new(false);
static NTP_SYNCED: AtomicBool = AtomicBool::new(false);
static LAST_NTP: AtomicU32 = AtomicU32::new(0);

/// Monotonic milliseconds since boot (64-bit: no wrap to worry about).
pub fn millis() -> u64 {
    (unsafe { sys::esp_timer_get_time() } / 1000) as u64
}

/// Wall clock, seconds since 1970 (garbage until the clock is valid).
pub fn epoch() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)
}

pub fn set_epoch(t: u32) {
    let tv = sys::timeval { tv_sec: t as _, tv_usec: 0 };
    unsafe { sys::settimeofday(&tv, std::ptr::null()) };
}

/// True once the clock came from NTP or was carried across a reset.
pub fn clock_valid() -> bool {
    CLOCK_TRUSTED.load(Ordering::Relaxed) && epoch() > SANE_EPOCH
}

pub fn trust_clock() {
    CLOCK_TRUSTED.store(true, Ordering::Relaxed);
}

pub fn ntp_synced(now: u32) -> bool {
    LAST_NTP.store(now, Ordering::Relaxed);
    trust_clock();
    !NTP_SYNCED.swap(true, Ordering::Relaxed)
}

pub fn clock_from_ntp() -> bool {
    NTP_SYNCED.load(Ordering::Relaxed)
}

pub fn last_ntp() -> u32 {
    LAST_NTP.load(Ordering::Relaxed)
}

pub fn random() -> u32 {
    unsafe { sys::esp_random() }
}

pub fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    unsafe { sys::esp_fill_random(b.as_mut_ptr().cast(), b.len()) };
    b
}

pub fn free_heap() -> u32 {
    unsafe { sys::esp_get_free_heap_size() }
}

pub fn min_free_heap() -> u32 {
    unsafe { sys::esp_get_minimum_free_heap_size() }
}

pub fn largest_block() -> u32 {
    unsafe { sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_8BIT) as u32 }
}

#[allow(non_upper_case_globals)]
pub fn reset_reason() -> (ResetKind, &'static str) {
    match unsafe { sys::esp_reset_reason() } {
        sys::esp_reset_reason_t_ESP_RST_POWERON => (ResetKind::PowerOn, "power-on"),
        sys::esp_reset_reason_t_ESP_RST_EXT => (ResetKind::PowerOn, "reset pin"),
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => (ResetKind::PowerOn, "brownout"),
        sys::esp_reset_reason_t_ESP_RST_SW => (ResetKind::Software, "software restart"),
        sys::esp_reset_reason_t_ESP_RST_PANIC => (ResetKind::Panic, "panic"),
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => (ResetKind::Watchdog, "interrupt watchdog"),
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => (ResetKind::Watchdog, "task watchdog"),
        sys::esp_reset_reason_t_ESP_RST_WDT => (ResetKind::Watchdog, "watchdog"),
        sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => (ResetKind::Other, "deep sleep"),
        sys::esp_reset_reason_t_ESP_RST_SDIO => (ResetKind::Other, "sdio"),
        _ => (ResetKind::Other, "other"),
    }
}

/// True while a freshly installed image waits to be confirmed (rollback armed).
pub fn running_image_unconfirmed() -> bool {
    let mut state: sys::esp_ota_img_states_t = 0;
    // SAFETY: plain out-parameter; the running partition is static.
    let ok =
        unsafe { sys::esp_ota_get_state_partition(sys::esp_ota_get_running_partition(), &mut state) } == sys::ESP_OK;
    ok && state == sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY
}

pub fn restart_now() -> ! {
    unsafe { sys::esp_restart() }
}

/// Resets through the panic handler: no shutdown handlers (esp_restart() runs
/// them on the calling task, and a wedged driver can block one, e.g. Wi-Fi
/// stop). Reported as a panic on the next boot, so it counts as a crash.
pub fn abort_now(why: &core::ffi::CStr) -> ! {
    unsafe { sys::esp_system_abort(why.as_ptr()) }
}
