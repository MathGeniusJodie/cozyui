//! Minimal wrapper over libc `localtime_r` for wall-clock fields.

use std::time::{SystemTime, UNIX_EPOCH};

/// The wall-clock fields cozyui uses, copied out of `libc::tm` so callers
/// never touch the platform struct (whose layout varies across libcs).
#[derive(Default)]
pub struct Tm {
    pub(crate) tm_min: i32,
    pub(crate) tm_hour: i32,
    pub(crate) tm_mday: i32,
    pub(crate) tm_mon: i32,
    pub(crate) tm_year: i32,
    pub(crate) tm_wday: i32,
}

/// Current local time broken into fields, or `None` if the conversion fails.
pub fn local_time() -> Option<Tm> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as libc::time_t;
    let mut out: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&raw const seconds, &raw mut out) };
    (!result.is_null()).then_some(Tm {
        tm_min: out.tm_min,
        tm_hour: out.tm_hour,
        tm_mday: out.tm_mday,
        tm_mon: out.tm_mon,
        tm_year: out.tm_year,
        tm_wday: out.tm_wday,
    })
}

/// Unix seconds for the given local civil date and time-of-day, resolved via
/// `mktime` so callers get true local time (DST-aware) instead of doing
/// their own epoch arithmetic. `mon` is 0-based (0 = January), matching
/// [`Tm::tm_mon`]; an out-of-range `mday` (e.g. day 32) is normalized by
/// `mktime` by rolling into the next month, which callers can rely on.
/// `None` if the conversion fails.
pub fn epoch_for_civil(
    year: i32,
    mon: i32,
    mday: i32,
    hour: i32,
    min: i32,
    sec: i32,
) -> Option<i64> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = mon;
    tm.tm_mday = mday;
    tm.tm_hour = hour;
    tm.tm_min = min;
    tm.tm_sec = sec;
    tm.tm_isdst = -1;
    // `mktime` returns -1 both on failure and for the one valid instant that
    // is actually -1 (1969-12-31 23:59:59 UTC in date-only offsets). Clear
    // errno first so a -1 result can be disambiguated: `mktime` only sets
    // errno on genuine failure, so if it's still 0 the -1 was the real epoch
    // value, not an error.
    unsafe {
        *libc::__errno_location() = 0;
    }
    let result = unsafe { libc::mktime(&mut tm) };
    if result == -1 && unsafe { *libc::__errno_location() } != 0 {
        return None;
    }
    Some(result as i64)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub(crate) const fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) as i64 / 400;
    let yoe = (y as i64) - era * 400;
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as i64;
    let doy = (153 * mp + 2) / 5 + (d as i64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`: returns (year, month, day).
pub(crate) const fn civil_from_days(z: i64) -> (i32, i32, i32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m as i32, d as i32)
}
