//! Small helpers shared across widgets.

use std::fs;
use std::io::{self, ErrorKind, Write as _};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Identity of one on-disk file version. The inode is included so an atomic
/// rename-over (how sync tools and we ourselves write) is always detected even
/// if size and mtime happen to match.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(crate) struct Fingerprint {
    pub(crate) ino: u64,
    pub(crate) size: u64,
    pub(crate) mtime_s: i64,
    pub(crate) mtime_ns: i64,
}

/// The file's current fingerprint, or `None` if it does not exist.
pub(crate) fn fingerprint(path: &str) -> io::Result<Option<Fingerprint>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(Fingerprint {
            ino: meta.ino(),
            size: meta.size(),
            mtime_s: meta.mtime(),
            mtime_ns: meta.mtime_nsec(),
        })),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Directory for per-user runtime files (sockets, request bodies):
/// `$XDG_RUNTIME_DIR` when set (per-user, mode 0700) so files there can't be
/// observed or squatted by another user in the shared, world-writable
/// `temp_dir()`; falls back to `temp_dir()` when the runtime dir isn't
/// available (e.g. no session manager).
pub(crate) fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

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

/// A fixed-interval gate: `ready` reports whether `interval` has elapsed
/// since the last time it returned `true`, at which point it resets its own
/// clock. Several widgets poll some external state (clock, sysfs,
/// filesystem) on every render tick but only want to actually re-read it
/// once every so often; this factors out that "has it been long enough"
/// check, shared by both `Refresh<T>` below and widgets whose reads are
/// fallible or update more than one field at once (so they don't fit
/// `Refresh`'s single-value replace-on-change shape).
pub(crate) struct Throttle {
    last_check: Instant,
}

impl Throttle {
    /// Starts the clock at construction time, so the first `ready` call
    /// waits a full `interval` before firing — matching the existing
    /// widgets' first-tick behavior (they all seeded `last_check` with
    /// `Instant::now()` at load time).
    pub(crate) fn new() -> Self {
        Self {
            last_check: Instant::now(),
        }
    }

    pub(crate) fn ready(&mut self, interval: Duration) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_check) < interval {
            return false;
        }
        self.last_check = now;
        true
    }
}

/// A value that's recomputed on a fixed interval, only reporting a change
/// when the recomputed value actually differs from the cached one. Several
/// widgets poll some external state (clock, sysfs, filesystem) on every
/// render tick but only want to redraw when the derived view changes; this
/// factors out the common "elapsed check, recompute, compare, store" shape.
pub(crate) struct Refresh<T> {
    throttle: Throttle,
    value: T,
}

impl<T: PartialEq> Refresh<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            throttle: Throttle::new(),
            value,
        }
    }

    pub(crate) fn get(&self) -> &T {
        &self.value
    }

    /// Overwrites the cached value immediately, bypassing the throttle. For
    /// updates triggered directly (e.g. a click) that shouldn't wait for the
    /// next tick.
    pub(crate) fn set(&mut self, value: T) {
        self.value = value;
    }

    /// If `interval` has elapsed since the last check, recomputes via
    /// `compute` and updates the cached value if it changed. Returns whether
    /// the value changed; does nothing (and returns `false`) if `interval`
    /// hasn't elapsed yet.
    pub(crate) fn refresh(&mut self, interval: Duration, compute: impl FnOnce() -> T) -> bool {
        if !self.throttle.ready(interval) {
            return false;
        }
        let value = compute();
        if value == self.value {
            return false;
        }
        self.value = value;
        true
    }
}

/// Tracks a periodic operation's failure/recovery state so a caller logs the
/// transition ("started failing" / "recovered") exactly once per episode
/// instead of once per failing tick. Several widgets poll something that can
/// transiently fail (sysfs, /proc, file metadata) and used to each hand-roll
/// this as their own `bool` field (or, for a free function with no `self`, a
/// module-level `AtomicBool`) — this factors out the shared shape. Built on
/// an atomic so it works the same way as a plain struct field or a `static`.
pub(crate) struct FailureLog {
    failing: AtomicBool,
}

impl FailureLog {
    pub(crate) const fn new() -> Self {
        Self {
            failing: AtomicBool::new(false),
        }
    }

    /// Call on a successful read; logs `recovered_msg()` the first time this
    /// follows a failure, and does nothing otherwise. Takes a closure (rather
    /// than an already-formatted `&str`) so a caller building the message via
    /// `format!` only pays for it on the rare transition tick, not on every
    /// call.
    pub(crate) fn record_ok(&self, recovered_msg: impl FnOnce() -> String) {
        if self.failing.swap(false, Ordering::Relaxed) {
            eprintln!("{}", recovered_msg());
        }
    }

    /// Call on a failed read; logs `failing_msg()` only the first time this
    /// follows a success (or startup) — repeats are suppressed until the
    /// next `record_ok`. See `record_ok` for why this takes a closure.
    pub(crate) fn record_err(&self, failing_msg: impl FnOnce() -> String) {
        if !self.failing.swap(true, Ordering::Relaxed) {
            eprintln!("{}", failing_msg());
        }
    }
}

/// Spawns a background thread that repeatedly calls `poll` and sends any
/// `Some` result down the returned channel, sleeping `interval` after each
/// call (so a slow `poll` never overlaps the next). Ticks where `poll`
/// returns `None` are skipped (no send) but still wait out the interval.
/// The thread exits once the receiver is dropped.
pub(crate) fn spawn_poller<T: Send + 'static>(
    interval: Duration,
    mut poll: impl FnMut() -> Option<T> + Send + 'static,
) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            if let Some(value) = poll()
                && tx.send(value).is_err()
            {
                break;
            }
            std::thread::sleep(interval);
        }
    });
    rx
}

/// Write `contents` to `path` atomically: write to a unique temp file in the
/// same directory, fsync, rename over the destination, then fsync the
/// directory (without which a crash shortly after can revert the rename).
pub(crate) fn atomic_write(path: &str, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let temp_path = unique_temp_path(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()?;
        fs::rename(&temp_path, path)?;
        sync_parent_dir(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

/// fsync the directory containing `path`, making a just-committed rename (or
/// a new directory entry) in it durable.
pub(crate) fn sync_parent_dir(path: &str) -> io::Result<()> {
    let parent = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
