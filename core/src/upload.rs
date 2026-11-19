//! What the weather app expects, and how to read its answers.

use std::fmt::Write;

use crate::pressure::sea_level_pressure_dwd;
use crate::reading::Reading;
use crate::timefmt::TimeZone;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Calibration {
    /// Height of the sensor above sea level.
    pub altitude_m: f64,
    /// Added to the measured station pressure.
    pub pressure_offset_hpa: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Stored by the app: drop it from the queue.
    Accepted,
    /// The app will not take this reading (400, 422, other 4xx): drop it once the
    /// app has taken the newest one (the uploader asks), or it blocks the queue.
    Rejected,
    /// Transient (network, 5xx, 429...): keep it, try again later.
    Retry,
    /// Configuration problem (bad key, wrong URL): keep everything, wait long.
    Halt,
}

/// `status` is the HTTP status, or 0 when there was no HTTP answer at all.
pub fn classify_response(status: i32) -> Outcome {
    match status {
        200..=299 => Outcome::Accepted,
        400 | 422 => Outcome::Rejected,
        // Redirects (e.g. a trailing-slash 307), auth, wrong path or media type:
        // retrying cannot help, a person has to fix the URL, key or app.
        300..=399 | 401 | 403 | 404 | 405 | 413 | 415 => Outcome::Halt,
        // Timeout and rate limit: the same request can succeed later.
        408 | 429 => Outcome::Retry,
        // Any other 4xx (409, 410, ...) is about this request; retrying the same
        // reading forever would block the queue. The uploader drops it only if
        // the app takes the newest reading, and halts if it refuses that too.
        400..=499 => Outcome::Rejected,
        // No connection, TLS, timeout, 5xx, anything unexpected.
        _ => Outcome::Retry,
    }
}

/// Sea-level pressure in 0.01 hPa for a stored reading.
pub fn sea_level_centi_hpa(r: &Reading, cal: &Calibration) -> i32 {
    let station = f64::from(r.station_deci_pa) / 1000.0 + cal.pressure_offset_hpa;
    let p0 = sea_level_pressure_dwd(
        station,
        f64::from(r.temp_centi) / 100.0,
        f64::from(r.hum_centi) / 100.0,
        cal.altitude_m,
    );
    (p0 * 100.0).round() as i32
}

/// What the weather app accepts (dwzg/weatherpage, app/models.py: temperature
/// -90..60 degC, humidity 0..100 %, sea-level pressure 800..1100 hPa), with the
/// lower temperature bound at the sensors' -40 degC. A reading outside these is
/// not queued: the app would refuse it, and a refused reading is how the
/// station recognises a broken app.
pub fn app_accepts(r: &Reading, cal: &Calibration) -> bool {
    (-4000..=6000).contains(&i32::from(r.temp_centi))
        && (0..=10_000).contains(&r.hum_centi)
        && (80_000..=110_000).contains(&sea_level_centi_hpa(r, cal))
}

/// A signed fixed-point value with two decimals, e.g. -123 -> "-1.23".
pub fn format_centi(v: i32) -> String {
    let a = v.unsigned_abs();
    format!("{}{}.{:02}", if v < 0 { "-" } else { "" }, a / 100, a % 100)
}

/// The JSON the weather app expects, built with integer arithmetic only:
/// {"timestamp":"2026-11-08T10:00:00+01:00","temperature":7.12,"humidity":99.80,"pressure":1025.43}
pub fn build_body(r: &Reading, cal: &Calibration, tz: &TimeZone) -> String {
    let mut s = String::with_capacity(112);
    let _ = write!(
        s,
        "{{\"timestamp\":\"{}\",\"temperature\":{},\"humidity\":{},\"pressure\":{}}}",
        tz.format_iso(i64::from(r.epoch)),
        format_centi(i32::from(r.temp_centi)),
        format_centi(i32::from(r.hum_centi)),
        format_centi(sea_level_centi_hpa(r, cal))
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body() {
        let tz = TimeZone::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        let r = Reading { epoch: 1_794_128_400, temp_centi: -5, hum_centi: 10000, station_deci_pa: 992_000, source: 0 };
        let cal = Calibration { altitude_m: 298.0, pressure_offset_hpa: -3.2 };
        let p = format_centi(sea_level_centi_hpa(&r, &cal));
        assert_eq!(
            build_body(&r, &cal, &tz),
            format!("{{\"timestamp\":\"2026-11-08T10:00:00+01:00\",\"temperature\":-0.05,\"humidity\":100.00,\"pressure\":{p}}}")
        );
        // 988.8 hPa at -0.05 degC, 100 %, 298 m (checked with the formula in Python).
        assert_eq!(p, "1026.12");
    }

    #[test]
    fn app_ranges() {
        let cal = Calibration { altitude_m: 298.0, pressure_offset_hpa: -3.2 };
        let ok = Reading { epoch: 300, temp_centi: 1500, hum_centi: 5000, station_deci_pa: 980_000, source: 0 };
        assert!(app_accepts(&ok, &cal));
        assert!(app_accepts(&Reading { temp_centi: 6000, ..ok }, &cal));
        assert!(!app_accepts(&Reading { temp_centi: 6001, ..ok }, &cal), "the sensor allows up to 85 degC");
        assert!(!app_accepts(&Reading { temp_centi: -4001, ..ok }, &cal));
        assert!(!app_accepts(&Reading { hum_centi: 10_001, ..ok }, &cal));
        // 1070 hPa at the station is ~1106 hPa at sea level.
        assert!(!app_accepts(&Reading { station_deci_pa: 1_070_000, ..ok }, &cal));
        assert!(!app_accepts(&Reading { station_deci_pa: 760_000, ..ok }, &cal));
    }

    #[test]
    fn centi() {
        assert_eq!(format_centi(0), "0.00");
        assert_eq!(format_centi(-1), "-0.01");
        assert_eq!(format_centi(102_543), "1025.43");
        assert_eq!(format_centi(-32_768), "-327.68");
        assert_eq!(format_centi(i32::MIN), "-21474836.48");
    }

    #[test]
    fn classify() {
        assert_eq!(classify_response(200), Outcome::Accepted);
        assert_eq!(classify_response(201), Outcome::Accepted);
        assert_eq!(classify_response(422), Outcome::Rejected);
        assert_eq!(classify_response(400), Outcome::Rejected);
        assert_eq!(classify_response(401), Outcome::Halt);
        assert_eq!(classify_response(404), Outcome::Halt);
        assert_eq!(classify_response(307), Outcome::Halt);
        assert_eq!(classify_response(413), Outcome::Halt);
        assert_eq!(classify_response(429), Outcome::Retry);
        assert_eq!(classify_response(408), Outcome::Retry);
        assert_eq!(classify_response(406), Outcome::Rejected);
        assert_eq!(classify_response(409), Outcome::Rejected);
        assert_eq!(classify_response(410), Outcome::Rejected);
        assert_eq!(classify_response(499), Outcome::Rejected);
        assert_eq!(classify_response(502), Outcome::Retry);
        assert_eq!(classify_response(0), Outcome::Retry);
        assert_eq!(classify_response(-1), Outcome::Retry);
    }
}
