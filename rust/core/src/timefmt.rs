//! Calendar arithmetic and a POSIX time-zone rule ("CET-1CEST,M3.5.0,M10.5.0/3"),
//! so local time is computed the same way on the device and in the tests,
//! without depending on the C library's TZ handling.

use std::fmt::Write;

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month 1..12, day 1..31) for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

/// A broken-down local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// Local time minus UTC, seconds.
    pub offset: i32,
}

/// When daylight saving time starts or ends: day `wday` (0 = Sunday) of week
/// `week` (1..5, 5 = last) of `month`, at `time` seconds after local midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Transition {
    month: u32,
    week: u32,
    wday: u32,
    time: i32,
}

impl Transition {
    /// Day number (since 1970) of the transition in year `y`.
    fn day(&self, y: i64) -> i64 {
        let first = days_from_civil(y, self.month, 1);
        let first_wday = (first + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
        let mut d = 1 + (self.wday + 7 - first_wday) % 7 + (self.week - 1) * 7;
        while d > days_in_month(y, self.month) {
            d -= 7;
        }
        first + i64::from(d) - 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dst {
    offset: i32,
    start: Transition,
    end: Transition,
}

/// A POSIX TZ rule with "M" transitions, e.g. "CET-1CEST,M3.5.0,M10.5.0/3".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeZone {
    std_offset: i32,
    dst: Option<Dst>,
}

impl TimeZone {
    pub const UTC: TimeZone = TimeZone { std_offset: 0, dst: None };

    /// Parses a POSIX TZ string. Only the "Mm.w.d" form of transitions is
    /// supported (the one every European and American zone uses).
    pub fn parse(s: &str) -> Option<TimeZone> {
        let mut p = Parser { s: s.as_bytes(), i: 0 };
        p.name()?;
        // POSIX offsets are west-positive: "CET-1" is one hour east of UTC.
        let std_offset = -p.offset()?;
        if p.done() {
            return Some(TimeZone { std_offset, dst: None });
        }
        p.name()?;
        let dst_offset = if p.peek() == Some(b',') { std_offset + 3600 } else { -p.offset()? };
        p.expect(b',')?;
        let start = p.transition()?;
        p.expect(b',')?;
        let end = p.transition()?;
        p.done().then_some(TimeZone { std_offset, dst: Some(Dst { offset: dst_offset, start, end }) })
    }

    /// Local time minus UTC at instant `t`, in seconds.
    pub fn utc_offset(&self, t: i64) -> i32 {
        let Some(dst) = self.dst else { return self.std_offset };
        let (y, _, _) = civil_from_days((t + i64::from(self.std_offset)).div_euclid(86_400));
        // Start is given in standard time, end in daylight time.
        let start = dst.start.day(y) * 86_400 + i64::from(dst.start.time) - i64::from(self.std_offset);
        let end = dst.end.day(y) * 86_400 + i64::from(dst.end.time) - i64::from(dst.offset);
        let in_dst = if start < end { start <= t && t < end } else { !(end <= t && t < start) };
        if in_dst {
            dst.offset
        } else {
            self.std_offset
        }
    }

    pub fn local(&self, t: i64) -> LocalTime {
        let offset = self.utc_offset(t);
        let lt = t + i64::from(offset);
        let (year, month, day) = civil_from_days(lt.div_euclid(86_400));
        let secs = lt.rem_euclid(86_400) as u32;
        LocalTime { year, month, day, hour: secs / 3600, minute: secs / 60 % 60, second: secs % 60, offset }
    }

    /// ISO-8601 local time with an explicit offset, e.g. "2026-11-08T10:00:00+01:00".
    /// The offset is what lets the weather app tell the two 02:30s of the autumn
    /// DST change apart.
    pub fn format_iso(&self, t: i64) -> String {
        let l = self.local(t);
        let sign = if l.offset < 0 { '-' } else { '+' };
        let off_min = l.offset.unsigned_abs() / 60;
        let mut s = String::with_capacity(25);
        let _ = write!(
            s,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
            l.year,
            l.month,
            l.day,
            l.hour,
            l.minute,
            l.second,
            sign,
            off_min / 60,
            off_min % 60
        );
        s
    }

    /// "11-08 10:00:00", the log line prefix.
    pub fn format_short(&self, t: i64) -> String {
        let l = self.local(t);
        format!("{:02}-{:02} {:02}:{:02}:{:02}", l.month, l.day, l.hour, l.minute, l.second)
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn done(&self) -> bool {
        self.i == self.s.len()
    }
    fn expect(&mut self, c: u8) -> Option<()> {
        (self.peek()? == c).then(|| self.i += 1)
    }
    fn name(&mut self) -> Option<()> {
        let start = self.i;
        if self.peek() == Some(b'<') {
            while self.peek()? != b'>' {
                self.i += 1;
            }
            self.i += 1;
            return Some(());
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        (self.i - start >= 3).then_some(())
    }
    fn number(&mut self) -> Option<i32> {
        let start = self.i;
        let mut v: i32 = 0;
        while let Some(c) = self.peek().filter(u8::is_ascii_digit) {
            v = v.checked_mul(10)?.checked_add(i32::from(c - b'0'))?;
            self.i += 1;
        }
        (self.i > start).then_some(v)
    }
    /// [+-]hh[:mm[:ss]] in seconds.
    fn offset(&mut self) -> Option<i32> {
        let sign = match self.peek() {
            Some(b'-') => {
                self.i += 1;
                -1
            }
            Some(b'+') => {
                self.i += 1;
                1
            }
            _ => 1,
        };
        let mut v = self.number()? * 3600;
        if self.peek() == Some(b':') {
            self.i += 1;
            v += self.number()? * 60;
            if self.peek() == Some(b':') {
                self.i += 1;
                v += self.number()?;
            }
        }
        Some(sign * v)
    }
    fn transition(&mut self) -> Option<Transition> {
        self.expect(b'M')?;
        let month = self.number()? as u32;
        self.expect(b'.')?;
        let week = self.number()? as u32;
        self.expect(b'.')?;
        let wday = self.number()? as u32;
        let time = if self.peek() == Some(b'/') {
            self.i += 1;
            self.offset()?
        } else {
            7200
        };
        ((1..=12).contains(&month) && (1..=5).contains(&week) && wday <= 6).then_some(Transition {
            month,
            week,
            wday,
            time,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin() -> TimeZone {
        TimeZone::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    #[test]
    fn iso_local_with_offset() {
        let tz = berlin();
        assert_eq!(tz.format_iso(1_794_128_400), "2026-11-08T10:00:00+01:00"); // 2026-11-08 09:00:00Z
        assert_eq!(tz.format_iso(1_784_538_000), "2026-07-20T11:00:00+02:00"); // 2026-07-20 09:00:00Z
    }

    #[test]
    fn dst_fold_is_unambiguous() {
        let tz = berlin();
        assert_eq!(tz.format_iso(1_792_888_200), "2026-10-25T02:30:00+02:00"); // 00:30Z, first 02:30
        assert_eq!(tz.format_iso(1_792_891_800), "2026-10-25T02:30:00+01:00"); // 01:30Z, second 02:30
    }

    #[test]
    fn dst_transitions_to_the_second() {
        let tz = berlin();
        // 2026-03-29 01:00:00Z: 02:00 CET becomes 03:00 CEST.
        let spring = days_from_civil(2026, 3, 29) * 86_400 + 3600;
        assert_eq!(tz.utc_offset(spring - 1), 3600);
        assert_eq!(tz.utc_offset(spring), 7200);
        assert_eq!(tz.format_iso(spring), "2026-03-29T03:00:00+02:00");
        // 2026-10-25 01:00:00Z: 03:00 CEST becomes 02:00 CET.
        let autumn = days_from_civil(2026, 10, 25) * 86_400 + 3600;
        assert_eq!(tz.utc_offset(autumn - 1), 7200);
        assert_eq!(tz.utc_offset(autumn), 3600);
        // Around new year, and a year whose last Sunday of March is the 31st.
        assert_eq!(tz.format_iso(days_from_civil(2027, 1, 1) * 86_400 - 1), "2027-01-01T00:59:59+01:00");
        let s2024 = days_from_civil(2024, 3, 31) * 86_400 + 3600;
        assert_eq!(tz.utc_offset(s2024), 7200);
        assert_eq!(tz.utc_offset(s2024 - 86_400), 3600);
    }

    #[test]
    fn other_rules() {
        assert_eq!(TimeZone::parse("UTC0").unwrap().format_iso(0), "1970-01-01T00:00:00+00:00");
        let ny = TimeZone::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(ny.format_iso(1_784_538_000), "2026-07-20T05:00:00-04:00");
        // Southern hemisphere: DST across new year.
        let syd = TimeZone::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(syd.utc_offset(days_from_civil(2026, 1, 15) * 86_400), 11 * 3600);
        assert_eq!(syd.utc_offset(days_from_civil(2026, 7, 15) * 86_400), 10 * 3600);
        let india = TimeZone::parse("<+0530>-5:30").unwrap();
        assert_eq!(india.format_iso(0), "1970-01-01T05:30:00+05:30");
        assert!(TimeZone::parse("CET-1CEST,J60,M10.5.0").is_none());
        assert!(TimeZone::parse("").is_none());
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 11, 8), 20_765);
        for z in [-800_000, -1, 0, 59, 60, 20_765, 1_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    #[test]
    fn short_format() {
        assert_eq!(berlin().format_short(1_794_128_400), "11-08 10:00:00");
    }
}
