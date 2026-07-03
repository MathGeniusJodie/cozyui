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
/// good enough for cosmetic randomness. The compare-exchange loop keeps the
/// step atomic, so concurrent draws never return the same value.
static RNG_STATE: AtomicU64 = AtomicU64::new(0);

fn next_random() -> u64 {
    let mut state = RNG_STATE.load(Ordering::Relaxed);
    loop {
        let mut next = if state == 0 {
            // Seeded nonzero; xorshift never reaches 0 from a nonzero state.
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
                | 1
        } else {
            state
        };
        next ^= next << 13;
        next ^= next >> 7;
        next ^= next << 17;
        match RNG_STATE.compare_exchange_weak(state, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(actual) => state = actual,
        }
    }
}

/// A pseudo-random value in `0.0..1.0`.
pub(crate) fn random_unit() -> f32 {
    // Top 24 bits: the full granularity an f32 mantissa can hold.
    (next_random() >> 40) as f32 / (1u32 << 24) as f32
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

/// Spawn `command`, reaping the child on a background thread so it never
/// lingers as a zombie until cozyui exits.
pub(crate) fn spawn_and_reap(command: &mut std::process::Command) -> io::Result<()> {
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
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
