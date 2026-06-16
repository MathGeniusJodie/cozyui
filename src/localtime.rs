//! Minimal FFI binding to libc `localtime_r` for wall-clock fields.

use std::time::{SystemTime, UNIX_EPOCH};

type TimeT = i64;

#[repr(C)]
#[derive(Default)]
pub struct Tm {
    pub(crate) tm_sec: i32,
    pub(crate) tm_min: i32,
    pub(crate) tm_hour: i32,
    pub(crate) tm_mday: i32,
    pub(crate) tm_mon: i32,
    pub(crate) tm_year: i32,
    pub(crate) tm_wday: i32,
    pub(crate) tm_yday: i32,
    pub(crate) tm_isdst: i32,
    pub(crate) tm_gmtoff: i64,
    pub(crate) tm_zone: *const i8,
}

unsafe extern "C" {
    fn localtime_r(timep: *const TimeT, result: *mut Tm) -> *mut Tm;
}

/// Current local time broken into fields, or `None` if the conversion fails.
pub fn local_time() -> Option<Tm> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as TimeT;
    let mut out = Tm::default();
    let result = unsafe { localtime_r(&raw const seconds, &raw mut out) };
    (!result.is_null()).then_some(out)
}
