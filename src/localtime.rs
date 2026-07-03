//! Minimal wrapper over libc `localtime_r` for wall-clock fields.

use std::time::{SystemTime, UNIX_EPOCH};

/// The wall-clock fields cozyui uses, copied out of `libc::tm` so callers
/// never touch the platform struct (whose layout varies across libcs).
#[derive(Default)]
pub struct Tm {
    pub(crate) tm_sec: i32,
    pub(crate) tm_min: i32,
    pub(crate) tm_hour: i32,
    pub(crate) tm_mday: i32,
    pub(crate) tm_mon: i32,
    pub(crate) tm_year: i32,
    pub(crate) tm_wday: i32,
}

/// Current local time broken into fields, or `None` if the conversion fails.
pub fn local_time() -> Option<Tm> {
    let seconds =
        SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as libc::time_t;
    let mut out: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&raw const seconds, &raw mut out) };
    (!result.is_null()).then_some(Tm {
        tm_sec: out.tm_sec,
        tm_min: out.tm_min,
        tm_hour: out.tm_hour,
        tm_mday: out.tm_mday,
        tm_mon: out.tm_mon,
        tm_year: out.tm_year,
        tm_wday: out.tm_wday,
    })
}
