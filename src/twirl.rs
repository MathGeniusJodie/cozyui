use std::error::Error;
use std::f32::consts::TAU;
use std::fs;
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::app_color;
use crate::palette_color;
use crate::text::BitmapFont;
use crate::util::{Fingerprint, SaveWorker, fingerprint, read_or_empty};
use crate::widget::Widget;
use crate::{CursorKind, Framebuffer, Index, Palette, Sprite, TRANSPARENT};

/// Config file naming the frogpoints markdown file that stores the wheel's
/// running total. The first non-blank, non-comment line is the path (`~`
/// expands to `$HOME`). Looked up in `$XDG_CONFIG_HOME/cozyui/` first, then
/// the source checkout.
const TWIRL_CONF_FILE: &str = "twirl.conf";
/// Path used when `twirl.conf` is missing or blank.
const DEFAULT_TOTAL_PATH: &str = "~/Desktop/RemoteVault/frogpoints.md";

/// Path to the frogpoints markdown file, configurable via `twirl.conf`.
/// Resolved once and cached; falls back to [`DEFAULT_TOTAL_PATH`] when the
/// config is missing or contains no usable path.
fn total_path() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| crate::paths::config_first_line(TWIRL_CONF_FILE, DEFAULT_TOTAL_PATH))
}

/// How often to stat the frogpoints file for external rewrites (sync
/// clients, editors); matches toodle's poll cadence.
const DISK_POLL_INTERVAL: Duration = Duration::from_secs(1);

const SHADOW_X_OFFSET: isize = 1;
const SHADOW_Y_OFFSET: isize = 4;

const SEGMENT_COUNT: usize = 6;
const SEGMENT_VALUES: [u64; SEGMENT_COUNT] = [1, 2, 4, 8, 16, 100];

const SEGMENT_LIGHT_COLORS: [Index; SEGMENT_COUNT] = [
    palette_color::CYAN,
    palette_color::LAVENDER,
    palette_color::ROSE,
    palette_color::ORANGE,
    palette_color::CREAM,
    palette_color::LIME,
];
const SEGMENT_DARK_COLORS: [Index; SEGMENT_COUNT] = [
    palette_color::BLUE,
    palette_color::PURPLE,
    palette_color::CRIMSON,
    palette_color::BROWN,
    palette_color::ORANGE,
    palette_color::GREEN,
];

const START_SPEED_MIN: f32 = 11.0;
const START_SPEED_RANGE: f32 = 5.0;
const FRICTION_PER_SECOND: f32 = 0.4;
const STOP_SPEED: f32 = 0.28;
const NUMBER_RADIUS: f32 = 55.0;
const POINTER_ANGLE: f32 = -std::f32::consts::FRAC_PI_2;
const CLICK_SAMPLE_RATE: u32 = 22_050;
const CLICK_DURATION_SECONDS: f32 = 0.012;
const CLICK_VOLUME: f32 = 1200.0;
const TOTAL_GAP: usize = 4;

/// Whether the wheel is spinning, and if so how fast. Replaces a `speed: f32`
/// that used `0.0` as a sentinel for "not spinning" (checked via two
/// separate comparisons against zero): a spinning speed is always positive
/// by construction, and the transition to `Idle` happens explicitly at the
/// one place speed decays past `STOP_SPEED`.
#[derive(Clone, Copy)]
enum Spin {
    Idle,
    Spinning(f32),
}

pub struct Twirl {
    wheel: Sprite,
    font: BitmapFont,
    angle: f32,
    spin: Spin,
    last_update: Instant,
    last_click_segment: usize,
    /// The running total, kept in lockstep with the frogpoints file.
    total: TotalStore,
    /// Per-pixel `atan2(dy, dx)` of each wheel pixel relative to the wheel
    /// center, precomputed once. The center angle never changes, so the
    /// render hot loop only needs to add `self.angle` instead of calling
    /// `atan2` per pixel per frame.
    pixel_base_angle: Vec<f32>,
}

/// A total store's sync bookkeeping, mirroring toodle's `SyncState`: `dirty`
/// (points earned in memory but not yet on disk) and whether a background
/// save is in flight are one enum because while a save is in flight the
/// in-flight value must be retained to become the new synced base once it
/// lands.
#[derive(Clone, Copy)]
enum SyncState {
    Idle { dirty: bool },
    Saving { dirty: bool, pending: u64 },
}

/// The frogpoints total plus everything needed to keep it in lockstep with
/// its backing file — twirl's counterpart of toodle's `SectionStore`. The
/// file is the source of truth and may be rewritten at any time by other
/// programs (sync clients, editors): `base` is the exact disk value the
/// total was last synced with (loaded, merged, or written), `fingerprint`
/// identifies that disk version so external rewrites are detected without
/// reading the file, and every write first folds in unseen external changes
/// — so a spin can never clobber an edit it has not seen.
struct TotalStore {
    /// The live total shown on the wheel. Always `>= base`: spins only add,
    /// and `base` is only ever set to a value `total` held at the time.
    total: u64,
    /// The disk value the total was last synced with.
    base: u64,
    fingerprint: Option<Fingerprint>,
    sync: SyncState,
    /// Saves run here so their fsyncs never hitch the UI thread — points are
    /// added from `update` (the frame the wheel stops) and from a mid-spin
    /// re-click. Flushed on shutdown (see [`Self::flush`]).
    worker: SaveWorker<()>,
    last_poll: Instant,
}

impl TotalStore {
    fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let (total, fingerprint) = read_disk_total(path)?;
        Ok(Self {
            total,
            base: total,
            fingerprint,
            sync: SyncState::Idle { dirty: false },
            worker: SaveWorker::spawn(),
            last_poll: Instant::now(),
        })
    }

    const fn total(&self) -> u64 {
        self.total
    }

    /// Add freshly earned points; they reach disk via [`Self::maintain`],
    /// usually within the same frame.
    fn add(&mut self, points: u64) {
        self.total = self.total.saturating_add(points);
        match &mut self.sync {
            SyncState::Idle { dirty } | SyncState::Saving { dirty, .. } => *dirty = true,
        }
    }

    fn is_dirty(&self) -> bool {
        match self.sync {
            SyncState::Idle { dirty } | SyncState::Saving { dirty, .. } => dirty,
        }
    }

    fn is_saving(&self) -> bool {
        matches!(self.sync, SyncState::Saving { .. })
    }

    /// Keep the total in lockstep with disk: absorb external edits to the
    /// backing file and write freshly earned points. Call regularly; returns
    /// whether the displayed total changed (redraw needed).
    fn maintain(&mut self, path: &str) -> io::Result<bool> {
        while let Some(((), result)) = self.worker.try_result() {
            self.complete_save(result);
        }
        let mut changed = false;
        if self.last_poll.elapsed() >= DISK_POLL_INTERVAL {
            self.last_poll = Instant::now();
            changed = self.absorb_external(path)?;
        }
        if self.is_dirty() && !self.is_saving() {
            changed |= self.begin_save(path)?;
        }
        Ok(changed)
    }

    /// Detect and fold in an external change to the backing file. Points
    /// earned in-app since the last sync are preserved by re-applying them on
    /// top of the new disk value (the counter equivalent of toodle's
    /// three-way line merge); if the merged total differs from disk it is
    /// re-flagged dirty so it gets written back. Returns whether the total
    /// changed.
    fn absorb_external(&mut self, path: &str) -> io::Result<bool> {
        if self.is_saving() {
            // Our own rename is in flight; the fingerprint is stale by
            // construction. complete_save re-arms syncing with the written
            // version, and any genuinely external rewrite after that still
            // mismatches it and is folded in on the next poll.
            return Ok(false);
        }
        if fingerprint(path)? == self.fingerprint {
            return Ok(false);
        }
        let (theirs, disk_fingerprint) = read_disk_total(path)?;
        let earned = self.total - self.base;
        let merged = theirs.saturating_add(earned);
        let changed = merged != self.total;
        self.total = merged;
        self.base = theirs;
        self.fingerprint = disk_fingerprint;
        self.sync = SyncState::Idle {
            dirty: merged != theirs,
        };
        Ok(changed)
    }

    /// Queue an async save, first absorbing any external change so the write
    /// is never based on stale disk state. Returns whether the absorb
    /// altered the total. Only called with no save in flight (`maintain` and
    /// `flush` both check), so the `Saving` state can't be clobbered.
    fn begin_save(&mut self, path: &str) -> io::Result<bool> {
        let changed = self.absorb_external(path)?;
        self.sync = SyncState::Saving {
            dirty: false,
            pending: self.total,
        };
        self.worker
            .submit((), path.to_string(), serialize_total(self.total));
        Ok(changed)
    }

    /// Fold in the save worker's outcome: on success the written value
    /// becomes the new synced base; on failure the store is re-flagged dirty
    /// so the next `maintain` retries.
    fn complete_save(&mut self, result: Result<Fingerprint, String>) {
        let SyncState::Saving { dirty, pending } = self.sync else {
            return;
        };
        match result {
            Ok(written) => {
                self.base = pending;
                self.fingerprint = Some(written);
                self.sync = SyncState::Idle { dirty };
            }
            Err(err) => {
                eprintln!("twirl background save failed: {err}");
                self.sync = SyncState::Idle { dirty: true };
            }
        }
    }

    /// Wait out any in-flight save and write pending points synchronously
    /// (shutdown); without this a spin that finished just before quitting
    /// would be silently dropped, since the worker's queue doesn't survive
    /// process exit.
    fn flush(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        while self.is_saving() {
            let Some(((), result)) = self.worker.wait_result() else {
                return Ok(());
            };
            self.complete_save(result);
        }
        if self.is_dirty() {
            self.absorb_external(path)?;
            let written = crate::util::atomic_write(path, serialize_total(self.total))?;
            self.base = self.total;
            self.fingerprint = Some(written);
            self.sync = SyncState::Idle { dirty: false };
        }
        Ok(())
    }
}

fn serialize_total(total: u64) -> String {
    format!("{total}\n")
}

/// The total as stored on disk, plus the fingerprint of that disk version.
/// The fingerprint is statted *before* the read: if the file changes again
/// mid-read, the stale fingerprint recorded here guarantees the next poll
/// re-syncs. A missing or empty file is zero (nothing recorded yet);
/// unparseable content is quarantined to `<path>.bad` rather than silently
/// overwritten, and counts as zero from then on.
fn read_disk_total(path: &str) -> io::Result<(u64, Option<Fingerprint>)> {
    let disk_fingerprint = fingerprint(path)?;
    let text = read_or_empty(path)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok((0, disk_fingerprint));
    }
    match trimmed.parse() {
        Ok(total) => Ok((total, disk_fingerprint)),
        Err(err) => {
            eprintln!("twirl: failed to parse {path}: {err}");
            let bad_path = format!("{path}.bad");
            match fs::rename(path, &bad_path) {
                Ok(()) => eprintln!("twirl: renamed corrupt {path} to {bad_path}"),
                Err(err) => {
                    eprintln!("twirl: failed to rename corrupt {path} to {bad_path}: {err}")
                }
            }
            // Re-stat: the quarantine rename just changed (or emptied) the
            // path, and recording the pre-rename fingerprint would make the
            // next poll re-read the corrupt content forever.
            Ok((0, fingerprint(path)?))
        }
    }
}

impl Twirl {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let wheel = crate::assets::wheel();
        let pixel_base_angle = Self::compute_pixel_base_angles(&wheel);
        Ok(Self {
            wheel,
            font: BitmapFont::load_with_fallback(
                &pixel_fonts::COMICORO_SPEC,
                &pixel_fonts::FUSION_PIXEL_10_SPEC,
            )?,
            angle: 0.0,
            spin: Spin::Idle,
            last_update: Instant::now(),
            last_click_segment: 0,
            total: TotalStore::load(total_path())?,
            pixel_base_angle,
        })
    }

    fn compute_pixel_base_angles(wheel: &Sprite) -> Vec<f32> {
        let radius = wheel.width as f32 / 2.0;
        let (center_x, center_y) = (radius, radius);
        let mut angles = Vec::with_capacity(wheel.width * wheel.height);
        for y in 0..wheel.height {
            for x in 0..wheel.width {
                let dx = x as f32 + 0.5 - center_x;
                let dy = y as f32 + 0.5 - center_y;
                angles.push(dy.atan2(dx));
            }
        }
        angles
    }

    /// Center of the wheel's circular face. The face is a `wheel.width`-diameter
    /// circle anchored at the top of the (taller) sprite, so its center sits at
    /// `wheel.width / 2` from the top rather than at the sprite's mid-height.
    fn wheel_center(&self) -> (f32, f32) {
        let radius = self.wheel.width as f32 / 2.0;
        (radius, radius)
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.draw_sprite_silhouette(
            &self.wheel,
            SHADOW_X_OFFSET,
            SHADOW_Y_OFFSET,
            palette,
            app_color::BACKGROUND_SHADOW_PAINT,
        );

        for y in 0..self.wheel.height {
            for x in 0..self.wheel.width {
                let source = self.wheel.at(x, y);
                let index = match source {
                    palette_color::LIME => self.segment_index(x, y, true),
                    palette_color::GREEN => self.segment_index(x, y, false),
                    TRANSPARENT => continue,
                    other => other,
                };
                if let Some(color) = palette.resolve_index(index, x as isize, y as isize) {
                    fb.set_pixel(x as isize, y as isize, color);
                }
            }
        }

        self.draw_numbers(fb, palette);
        self.draw_total(fb, palette);
    }

    /// Whether `(x, y)` (already known to be inside the widget) falls within
    /// the spinnable wheel's circle. Shared by `click` and `cursor_at` so the
    /// clickable region and the hand cursor can never drift apart.
    fn wheel_contains(&self, x: usize, y: usize) -> bool {
        let (center_x, center_y) = self.wheel_center();
        let dx = x as f32 + 0.5 - center_x;
        let dy = y as f32 + 0.5 - center_y;
        dx * dx + dy * dy <= center_x.min(center_y).powi(2)
    }

    pub(crate) fn click(&mut self, x: isize, y: isize) {
        if x < 0 || y < 0 {
            return;
        }
        let x = x as usize;
        let y = y as usize;
        if x >= self.width() || y >= self.height() || !self.wheel_contains(x, y) {
            return;
        }

        self.spin();
    }

    pub(crate) fn spin(&mut self) {
        // Spinning while already spinning shouldn't discard the in-progress
        // spin, but awarding the current pointer segment would let players time
        // their re-spin to pick a result. Award a random segment instead.
        if matches!(self.spin, Spin::Spinning(_)) {
            self.add_random_value();
        }
        self.spin =
            Spin::Spinning(crate::util::random_unit().mul_add(START_SPEED_RANGE, START_SPEED_MIN));
        self.last_update = Instant::now();
        self.last_click_segment = self.pointer_segment();
        play_click();
    }

    fn segment_index(&self, x: usize, y: usize, light: bool) -> Index {
        let segment = self.segment_at(x, y);
        if light {
            SEGMENT_LIGHT_COLORS[segment]
        } else {
            SEGMENT_DARK_COLORS[segment]
        }
    }

    fn draw_numbers(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let (center_x, center_y) = self.wheel_center();
        let color = palette_color::BLACK;
        for (segment, value) in SEGMENT_VALUES.iter().enumerate() {
            let number = value.to_string();
            let angle = segment_center_angle(segment) - self.angle;
            let text_w = self.font.text_width(&number);
            let text_h = self.font.cell_h();
            let x = angle.cos().mul_add(NUMBER_RADIUS, center_x) - text_w as f32 / 2.0;
            let y = angle.sin().mul_add(NUMBER_RADIUS, center_y) - text_h as f32 / 2.0;
            self.font
                .draw_text(fb, &number, x as isize, y as isize, color);
        }
    }

    fn draw_total(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let total = self.total.total().to_string();
        let text_w = self.font.text_width(&total);
        let x = (self.wheel.width.saturating_sub(text_w) / 2).saturating_sub(5);
        let y = self.wheel.width + TOTAL_GAP + 2;
        self.font
            .draw_text(fb, &total, x as isize, y as isize, palette_color::CREAM);
    }

    fn add_landed_value(&mut self) {
        self.add_segment_value(self.pointer_segment());
    }

    fn add_random_value(&mut self) {
        let segment = (crate::util::random_unit() * SEGMENT_COUNT as f32) as usize % SEGMENT_COUNT;
        self.add_segment_value(segment);
    }

    fn add_segment_value(&mut self, segment: usize) {
        self.total.add(SEGMENT_VALUES[segment]);
    }

    /// Write any pending points now, synchronously (shutdown), waiting out
    /// in-flight background saves first.
    pub(crate) fn flush_saves(&mut self) -> Result<(), Box<dyn Error>> {
        self.total.flush(total_path())
    }

    /// Advance the spin physics one frame; returns whether the wheel is
    /// animating (was spinning this frame).
    fn step_spin(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_update);
        self.last_update = now;

        let Spin::Spinning(mut speed) = self.spin else {
            return false;
        };

        let previous_segment = self.pointer_segment();
        self.angle = normalize_angle(speed.mul_add(dt.as_secs_f32(), self.angle));
        speed *= FRICTION_PER_SECOND.powf(dt.as_secs_f32());

        let current_segment = self.pointer_segment();
        if current_segment != previous_segment && current_segment != self.last_click_segment {
            self.last_click_segment = current_segment;
            play_click();
        }

        if speed < STOP_SPEED {
            self.spin = Spin::Idle;
            self.add_landed_value();
            play_jingle();
        } else {
            self.spin = Spin::Spinning(speed);
        }

        true
    }

    fn pointer_segment(&self) -> usize {
        segment_for_angle(POINTER_ANGLE + self.angle)
    }

    fn segment_at(&self, x: usize, y: usize) -> usize {
        let base = self.pixel_base_angle[y * self.wheel.width + x];
        segment_for_angle(base + self.angle)
    }
}

fn segment_center_angle(segment: usize) -> f32 {
    (segment as f32 + 0.5) * TAU / SEGMENT_COUNT as f32
}

fn segment_for_angle(angle: f32) -> usize {
    let angle = normalize_angle(angle);
    (angle / (TAU / SEGMENT_COUNT as f32)).floor() as usize % SEGMENT_COUNT
}

fn normalize_angle(angle: f32) -> f32 {
    angle.rem_euclid(TAU)
}

fn play_click() {
    play_wav("cozyui-twirl-click.wav", synth_click());
}

fn play_jingle() {
    play_wav("cozyui-twirl-jingle.wav", synth_jingle());
}

fn play_wav(name: &str, samples: Vec<i16>) {
    // Unique path per playback: a fixed name in shared /tmp is race-able, and
    // the file must survive until the player has read it anyway. The runtime
    // dir keeps these out of shared /tmp, consistent with the rest of the
    // codebase.
    let Some(base) = crate::util::runtime_dir()
        .join(name)
        .to_str()
        .map(str::to_owned)
    else {
        return;
    };
    let path = crate::util::unique_temp_path(&base);
    if fs::write(&path, wav_bytes(&samples)).is_err() {
        return;
    }

    // Waiting doubles as reaping (no zombies) and tells us when the wav file
    // can be deleted; blocking is fine off the UI thread.
    std::thread::spawn(move || {
        for player in AUDIO_PLAYERS {
            let mut command = Command::new(player.command);
            for arg in player.args {
                command.arg(arg);
            }
            command.arg(&path);
            if let Ok(mut child) = command.spawn() {
                let _ = child.wait();
                break;
            }
        }
        let _ = fs::remove_file(&path);
    });
}

struct AudioPlayer {
    command: &'static str,
    args: &'static [&'static str],
}

const AUDIO_PLAYERS: &[AudioPlayer] = &[
    AudioPlayer {
        command: "pw-play",
        args: &[],
    },
    AudioPlayer {
        command: "paplay",
        args: &[],
    },
    AudioPlayer {
        command: "play",
        args: &["-q"],
    },
    AudioPlayer {
        command: "ffplay",
        args: &["-nodisp", "-autoexit", "-loglevel", "quiet"],
    },
    AudioPlayer {
        command: "afplay",
        args: &[],
    },
];

fn synth_click() -> Vec<i16> {
    let len = (CLICK_SAMPLE_RATE as f32 * CLICK_DURATION_SECONDS) as usize;
    let mut noise = 0x1234_abcd_u32;
    (0..len)
        .map(|i| {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let envelope = (1.0 - i as f32 / len as f32).powi(4);
            let sample = ((noise >> 16) as f32 / 32_768.0).mul_add(2.0, -1.0);
            (sample * envelope * CLICK_VOLUME) as i16
        })
        .collect()
}

fn synth_jingle() -> Vec<i16> {
    let notes = [440.0, 660.0, 550.0, 880.0];
    let note_len = (CLICK_SAMPLE_RATE as f32 * 0.09) as usize;
    let mut samples = Vec::with_capacity(note_len * notes.len());
    for note in notes {
        for i in 0..note_len {
            let t = i as f32 / CLICK_SAMPLE_RATE as f32;
            let envelope = 1.0 - i as f32 / note_len as f32;
            let wave = (t * note * TAU).sin() * envelope;
            samples.push((wave * 7000.0) as i16);
        }
    }
    samples
}

fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&CLICK_SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(CLICK_SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

impl crate::widget::Widget for Twirl {
    fn width(&self) -> usize {
        self.wheel.width + SHADOW_X_OFFSET as usize
    }

    fn height(&self) -> usize {
        self.wheel.height + SHADOW_Y_OFFSET as usize
    }

    fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        Self::render(self, fb, palette);
    }

    fn update(&mut self) -> Result<bool, Box<dyn Error>> {
        let spinning = self.step_spin();
        // Housekeeping mirrors toodle's maintain(): a spin that just landed
        // is saved this same frame, and external edits to the frogpoints
        // file are folded in.
        let total_changed = self.total.maintain(total_path())?;
        Ok(spinning || total_changed)
    }

    fn click(
        &mut self,
        x: isize,
        y: isize,
        _shift: bool,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        Self::click(self, x, y);
        Ok(crate::widget::ClickOutcome::default())
    }

    /// Hand inside the spinnable wheel; the shadow margin is inert.
    fn hit_test(&self, x: isize, y: isize) -> Option<crate::CursorKind> {
        if x < 0 || y < 0 {
            return None;
        }
        let x = x as usize;
        let y = y as usize;
        if x < self.width() && y < self.height() && self.wheel_contains(x, y) {
            Some(CursorKind::Hand)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_for_angle_wraps_around() {
        assert_eq!(segment_for_angle(0.0), 0);
        assert_eq!(segment_for_angle(TAU), 0);
        assert_eq!(segment_for_angle(-0.01), SEGMENT_COUNT - 1);
    }

    fn temp_total_path(dir: &std::path::Path) -> String {
        dir.join("frogpoints.md").to_str().unwrap().to_string()
    }

    fn unique_temp_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cozyui-twirl-test-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn store_loads_missing_and_empty_files_as_zero() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = temp_total_path(&dir);

        assert_eq!(TotalStore::load(&path).unwrap().total(), 0);
        fs::write(&path, "\n").unwrap();
        assert_eq!(TotalStore::load(&path).unwrap().total(), 0);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_absorbs_external_rewrite_when_clean() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = temp_total_path(&dir);

        fs::write(&path, "10\n").unwrap();
        let mut store = TotalStore::load(&path).unwrap();

        fs::write(&path, "25\n").unwrap();
        assert!(store.absorb_external(&path).unwrap());
        assert_eq!(store.total(), 25);
        assert!(!store.is_dirty());

        // Nothing further changed: no-op.
        assert!(!store.absorb_external(&path).unwrap());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_merges_external_change_into_unsaved_points() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = temp_total_path(&dir);

        fs::write(&path, "10\n").unwrap();
        let mut store = TotalStore::load(&path).unwrap();
        store.add(4);

        // An external writer rewrites the file before our points are saved:
        // its new baseline and our earned points both survive.
        fs::write(&path, "100\n").unwrap();
        assert!(store.absorb_external(&path).unwrap());
        assert_eq!(store.total(), 104);
        // Merged result differs from disk, so it stays dirty until saved.
        assert!(store.is_dirty());

        store.flush(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "104\n");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_ignores_its_own_writes() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = temp_total_path(&dir);

        let mut store = TotalStore::load(&path).unwrap();
        store.add(7);
        store.flush(&path).unwrap();

        assert!(!store.absorb_external(&path).unwrap());
        assert!(!store.is_dirty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "7\n");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_flush_folds_in_unseen_external_change() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = temp_total_path(&dir);

        fs::write(&path, "1\n").unwrap();
        let mut store = TotalStore::load(&path).unwrap();
        store.add(2);

        // An external writer bumps the total before our save lands.
        fs::write(&path, "50\n").unwrap();
        store.flush(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "52\n");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_quarantines_corrupt_content_and_keeps_earned_points() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = temp_total_path(&dir);

        fs::write(&path, "10\n").unwrap();
        let mut store = TotalStore::load(&path).unwrap();
        store.add(4);

        fs::write(&path, "not a number\n").unwrap();
        assert!(store.absorb_external(&path).unwrap());

        // The corrupt content is preserved next to the file, the new baseline
        // is zero, and the in-app points survive on top of it.
        assert_eq!(
            fs::read_to_string(format!("{path}.bad")).unwrap(),
            "not a number\n"
        );
        assert_eq!(store.total(), 4);
        assert!(store.is_dirty());

        store.flush(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "4\n");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_maintain_saves_earned_points_in_background() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = temp_total_path(&dir);

        let mut store = TotalStore::load(&path).unwrap();
        store.add(16);
        assert!(!store.maintain(&path).unwrap());
        assert!(store.is_saving());

        // The flush path waits out the background save it did not start.
        store.flush(&path).unwrap();
        assert!(!store.is_dirty());
        assert!(!store.is_saving());
        assert_eq!(fs::read_to_string(&path).unwrap(), "16\n");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wav_bytes_writes_mono_pcm_header() {
        let bytes = wav_bytes(&[0, 1, -1]);

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 50);
    }
}
