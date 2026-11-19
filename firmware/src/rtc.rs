//! The state block in RTC slow memory, which survives every reset except power loss.

use std::ptr::{addr_of, addr_of_mut};

use ws_core::boot::{RtcState, RTC_WORDS};

#[link_section = ".rtc_noinit"]
static mut BLOCK: [u32; RTC_WORDS] = [0; RTC_WORDS];

pub fn load() -> Option<RtcState> {
    // SAFETY: plain words, only accessed through volatile copies; a torn read
    // (watchdog firing mid-write) fails the CRC.
    let w = unsafe { addr_of!(BLOCK).read_volatile() };
    RtcState::from_words(&w)
}

pub fn save(s: &RtcState) {
    unsafe { addr_of_mut!(BLOCK).write_volatile(s.to_words()) };
}
