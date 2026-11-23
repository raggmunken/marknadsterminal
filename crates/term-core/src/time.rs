//! Minimal date handling, so that the core needs no date library.
//! Everything is UTC and proleptic Gregorian.

/// Days from 1970-01-01 to the given civil date. Howard Hinnant's algorithm.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

fn number(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `2026-11-01` or `2026-11-01T06:30:00`, read as UTC. A trailing `Z` and
/// fractions of a second are accepted and ignored.
pub fn iso_to_unix(text: &str) -> Option<i64> {
    let text = text.trim().trim_end_matches('Z');
    let (date, time) = match text.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let (year, month, day) = (number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?);
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut seconds = 0;
    if let Some(time) = time {
        let time = time.split('.').next()?;
        let mut t = time.split(':');
        let (h, m) = (number(t.next()?)?, number(t.next()?)?);
        let s = match t.next() {
            Some(s) => number(s)?,
            None => 0,
        };
        if h > 23 || m > 59 || s > 60 {
            return None;
        }
        seconds = h * 3600 + m * 60 + s;
    }
    Some(days_from_civil(year, month, day) * 86_400 + seconds)
}

/// `2026-11-01 06:30` in UTC.
pub fn unix_to_iso_minutes(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let rest = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rest / 3600, rest % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(iso_to_unix("1970-01-01"), Some(0));
        assert_eq!(iso_to_unix("2000-01-01T00:00:00Z"), Some(946_684_800));
        assert_eq!(iso_to_unix("2024-02-29T12:00:00"), Some(1_709_208_000));
        assert_eq!(iso_to_unix("2026-11-18 16:32:57.607850"), Some(iso_to_unix("2026-11-18T16:32:57").unwrap()));
        assert_eq!(iso_to_unix("1969-12-31T23:59:59"), Some(-1));
    }

    #[test]
    fn round_trip() {
        for unix in [0, 946_684_800, 1_709_208_000, 1_795_446_657, -86_400] {
            let text = unix_to_iso_minutes(unix);
            assert_eq!(iso_to_unix(&format!("{text}:00")), Some(unix - unix.rem_euclid(60)), "{text}");
        }
        assert_eq!(unix_to_iso_minutes(946_684_800), "2000-01-01 00:00");
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "2026", "2026-13-01", "2026-00-10", "2026-1-1-1", "igår", "2026-11-01T25:00", "2026-11-01T10"] {
            assert_eq!(iso_to_unix(bad), None, "{bad:?}");
        }
    }
}
