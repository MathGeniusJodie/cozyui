//! Small helpers shared across widgets.

use std::fs;
use std::io::{self, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (0.0 if the clock is before the epoch).
pub(crate) fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// xorshift64 state, seeded lazily from the nanosecond clock. Not secure;
/// good enough for cosmetic randomness, and (unlike reading the clock per
/// call) consecutive draws are decorrelated.
static RNG_STATE: AtomicU64 = AtomicU64::new(0);

fn next_random() -> u64 {
    let mut state = RNG_STATE.load(Ordering::Relaxed);
    if state == 0 {
        state = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
            | 1;
    }
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    RNG_STATE.store(state, Ordering::Relaxed);
    state
}

/// A pseudo-random value in `0.0..1.0`.
pub(crate) fn random_unit() -> f32 {
    (next_random() % 10_000) as f32 / 10_000.0
}

/// A pseudo-random index in `0..len` (returns 0 when `len` is 0).
pub(crate) fn random_index(len: usize) -> usize {
    next_random() as usize % len.max(1)
}

/// Suffix counter so overlapping temp files within one process never collide.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A per-process-unique temp path next to `path`.
pub(crate) fn unique_temp_path(path: &str) -> String {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{path}.tmp.{}.{seq}", std::process::id())
}

/// Single-quote `arg` for safe interpolation into a `sh -c` command line,
/// escaping any embedded single quote as `'\''`.
pub(crate) fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Write `contents` to `path` atomically: write to a unique temp file in the
/// same directory, fsync, then rename over the destination.
pub(crate) fn atomic_write(path: &str, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let temp_path = unique_temp_path(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()?;
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}
