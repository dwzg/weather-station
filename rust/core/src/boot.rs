//! What survives a reset (RTC memory), and what the station makes of a reset.
//!
//! The state carries the clock across a crash or update, so readings continue
//! while the network is down, and counts crashes, so a bad build degrades
//! instead of looping.

use crate::crc::crc32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtcState {
    /// Wall clock when last saved.
    pub epoch: u32,
    /// Last slot measured, so a quick reboot does not measure twice.
    pub last_slot: u32,
    /// Consecutive unplanned resets.
    pub crashes: u32,
    /// Since power-on.
    pub boot_count: u32,
    /// Set right before a planned restart (update, self-heal).
    pub intentional: u32,
}

/// Change whenever RtcState changes.
const MAGIC: u32 = 0x5753_5433; // "WST3"
pub const RTC_WORDS: usize = 7;

impl RtcState {
    /// magic | five fields | crc32 over the first six words.
    pub fn to_words(&self) -> [u32; RTC_WORDS] {
        let mut w = [MAGIC, self.epoch, self.last_slot, self.crashes, self.boot_count, self.intentional, 0];
        w[6] = crc32(&words_bytes(&w[..6]));
        w
    }

    /// None if the words are not a valid state (power-on garbage, older layout).
    pub fn from_words(w: &[u32; RTC_WORDS]) -> Option<Self> {
        (w[0] == MAGIC && w[6] == crc32(&words_bytes(&w[..6]))).then_some(RtcState {
            epoch: w[1],
            last_slot: w[2],
            crashes: w[3],
            boot_count: w[4],
            intentional: w[5],
        })
    }
}

fn words_bytes(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Why the chip reset, as far as it matters here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetKind {
    /// Power-on, reset button, brownout: RTC memory is garbage or the outage length unknown.
    PowerOn,
    /// esp_restart(): planned, unless the RTC flag says otherwise.
    Software,
    /// Panic, abort, out of memory: the chip reports these itself.
    Panic,
    /// Interrupt, task or RTC watchdog.
    Watchdog,
    /// Anything else (deep sleep wake-up, USB reset while flashing, ...).
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Measure and buffer, but do not upload.
    NoUpload,
    /// Wi-Fi, updates and the status page only.
    Safe,
}

impl Mode {
    pub fn describe(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::NoUpload => "no-upload (after crashes)",
            Mode::Safe => "safe (after crashes)",
        }
    }
}

pub struct Boot {
    pub state: RtcState,
    pub crashed: bool,
    pub mode: Mode,
    /// The clock and last slot in `state` can be trusted.
    pub state_kept: bool,
}

/// Works out the state after a reset. Thresholds: unplanned resets in a row
/// before uploads stop, and before only Wi-Fi, updates and status run.
pub fn on_boot(loaded: Option<RtcState>, reset: ResetKind, crashes_no_upload: u32, crashes_safe: u32) -> Boot {
    let kept = loaded.filter(|_| reset != ResetKind::PowerOn);
    let mut state = kept.unwrap_or_default();
    let crashed = match reset {
        ResetKind::Panic | ResetKind::Watchdog => true,
        // Our own loop watchdog restarts in software with the flag cleared.
        ResetKind::Software => kept.is_some() && state.intentional == 0,
        ResetKind::PowerOn | ResetKind::Other => false,
    };
    if crashed {
        state.crashes += 1;
    }
    state.intentional = 0;
    state.boot_count += 1;
    let mode = if state.crashes >= crashes_safe {
        Mode::Safe
    } else if state.crashes >= crashes_no_upload {
        Mode::NoUpload
    } else {
        Mode::Normal
    };
    Boot { state, crashed, mode, state_kept: kept.is_some() }
}

/// The 5-minute slot to measure now, if any: the current one, if it has not been
/// measured and is less than `grace_s` old. A slot missed by more (boot, no clock
/// yet) is left as a gap rather than filled with a late value.
pub fn due_slot(now: u32, last_slot: u32, slot_s: u32, grace_s: u32) -> Option<u32> {
    let slot = now - now % slot_s;
    (slot != last_slot && now - slot <= grace_s).then_some(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_words_roundtrip_and_reject_garbage() {
        let s = RtcState { epoch: 1_794_128_400, last_slot: 1_794_128_100, crashes: 2, boot_count: 9, intentional: 1 };
        let mut w = s.to_words();
        assert_eq!(RtcState::from_words(&w), Some(s));
        w[3] ^= 1;
        assert_eq!(RtcState::from_words(&w), None);
        assert_eq!(RtcState::from_words(&[0; RTC_WORDS]), None);
    }

    #[test]
    fn crash_counting() {
        let s = RtcState { epoch: 5, crashes: 2, boot_count: 3, ..Default::default() };
        let b = on_boot(Some(s), ResetKind::Panic, 3, 5);
        assert!(b.crashed && b.state_kept);
        assert_eq!((b.state.crashes, b.state.boot_count, b.mode), (3, 4, Mode::NoUpload));
        // A planned restart is not a crash, an unflagged one is.
        let planned = RtcState { intentional: 1, ..s };
        assert!(!on_boot(Some(planned), ResetKind::Software, 3, 5).crashed);
        assert!(on_boot(Some(s), ResetKind::Software, 3, 5).crashed);
        // Without valid RTC memory a software reset says nothing.
        assert!(!on_boot(None, ResetKind::Software, 3, 5).crashed);
        let five = RtcState { crashes: 4, ..s };
        assert_eq!(on_boot(Some(five), ResetKind::Watchdog, 3, 5).mode, Mode::Safe);
        // Power-on forgets everything.
        let b = on_boot(Some(five), ResetKind::PowerOn, 3, 5);
        assert_eq!(
            (b.state, b.mode, b.state_kept),
            (RtcState { boot_count: 1, ..Default::default() }, Mode::Normal, false)
        );
    }

    #[test]
    fn slots() {
        let t = 1_794_128_400; // a slot boundary
        assert_eq!(due_slot(t, 0, 300, 60), Some(t));
        assert_eq!(due_slot(t + 60, 0, 300, 60), Some(t));
        assert_eq!(due_slot(t + 61, 0, 300, 60), None, "too late: a gap");
        assert_eq!(due_slot(t + 10, t, 300, 60), None, "already measured");
        assert_eq!(due_slot(t + 300, t, 300, 60), Some(t + 300));
    }
}
