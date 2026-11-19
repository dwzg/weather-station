//! Logging to the USB console and, for this firmware's own lines, a small RAM
//! ring shown at /log.

use std::collections::VecDeque;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};
use ws_core::timefmt::TimeZone;

use crate::platform;

const LINES: usize = 40;
const WIDTH: usize = 100;

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static TZ: Mutex<Option<TimeZone>> = Mutex::new(None);

struct Logger;

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= Level::Info
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let ours = r.target().starts_with("weather_station") || r.target().starts_with("ws_core");
        let tz = TZ.lock().ok().and_then(|g| *g);
        let stamp = match tz {
            Some(tz) if platform::clock_valid() => tz.format_short(i64::from(platform::epoch())),
            _ => format!("+{}s", platform::millis() / 1000),
        };
        let mut line =
            if ours { format!("{stamp} {}", r.args()) } else { format!("{stamp} [{}] {}", r.target(), r.args()) };
        if line.len() > WIDTH {
            let cut = (0..=WIDTH).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
            line.truncate(cut);
        }
        println!("{line}");
        if ours {
            if let Ok(mut ring) = RING.lock() {
                if ring.len() == LINES {
                    ring.pop_front();
                }
                ring.push_back(line);
            }
        }
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

pub fn init(tz: TimeZone) {
    *TZ.lock().unwrap() = Some(tz);
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Info);
}

pub fn dump() -> String {
    let ring = RING.lock().unwrap();
    let mut s = String::with_capacity(ring.len() * 80);
    for l in ring.iter() {
        s.push_str(l);
        s.push('\n');
    }
    s
}
