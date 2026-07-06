use std::error::Error;
use std::f32::consts::{FRAC_PI_2, PI};
use std::fs;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::localtime::local_time;
use crate::palette_color;
use crate::text::{BitmapFont, draw_text_centered};
use crate::{CursorKind, Framebuffer, Index, Palette, Sprite, TRANSPARENT};

mod player;

const DISPLAY_X: usize = 6;
const DISPLAY_Y: usize = 23;
const DIGIT_W: usize = 12;
const DIGIT_GAP: usize = 3;
const COLON_W: usize = 4;
const CLOCK_CLEAR_W: usize = 64;
const CLOCK_CLEAR_H: usize = 25;
const CLOCK_EIGHT_SRC_X: usize = 21;
const CLOCK_EIGHT_SRC_Y: usize = 23;
const CLOCK_COLON_SRC_X: usize = 36;
const CLOCK_COLON_SRC_Y: usize = 26;
const CLOCK_COLON_H: usize = 22;

const TUNER_ALL_Y: usize = 5;
const TUNER_X: usize = 100;
const TUNER_Y: usize = TUNER_ALL_Y + 12;
const TUNER_W: usize = 94;
const TUNER_H: usize = 26;
const TUNER_MARK_Y: usize = TUNER_ALL_Y + 23;
const TUNER_MARK_SIZE: usize = 5;
const LABEL_ABOVE_Y: usize = TUNER_ALL_Y + 12;
const LABEL_BELOW_Y: usize = TUNER_ALL_Y + 27;

const MEDIA_BUTTON_X: usize = 185;
const MEDIA_BUTTON_Y: usize = 4;
const MEDIA_BUTTON_W: usize = 13;
const MEDIA_BUTTON_H: usize = 12;
const MEDIA_BUTTON_GAP: usize = 1;
const MEDIA_BUTTON_COUNT: usize = 4;

const KNOB_X: usize = 216;
const KNOB_Y: usize = 34;
const KNOB_RADIUS: usize = 18;
const KNOB_MARKER_SRC_X: usize = 214;
const KNOB_MARKER_SRC_Y: usize = 41;
const KNOB_MARKER_SRC_W: usize = 4;
const KNOB_MARKER_SRC_H: usize = 4;
const MIN_VOLUME: u8 = 0;
const MAX_VOLUME: u8 = 100;

const CLOCK_REFRESH: Duration = Duration::from_millis(250);
/// How often a dragged/scrolled volume gets pushed to `wpctl`/`pactl`; see
/// `Wavey::push_volume`.
const VOLUME_PUSH_THROTTLE: Duration = Duration::from_millis(100);
// Song title display: TITLE_X/TITLE_Y position the text's top-left corner,
// TITLE_W is the fixed text window width (longer titles marquee through it);
// the black box extends TITLE_BOX_PAD around the window on every side.
const TITLE_X: usize = TUNER_X - 20;
const TITLE_Y: usize = 47;
const TITLE_W: usize = TUNER_W + 24;
const TITLE_BOX_PAD: usize = 2;
const MARQUEE_STEP: Duration = Duration::from_millis(60);
const MARQUEE_SEP: &str = "  ~  ";

#[derive(Clone, Copy)]
enum MediaButton {
    PlayPause,
    Stop,
    Back,
    Forward,
}

/// What's currently playing, collapsing what used to be three independently
/// (and manually) synchronized fields — `playing: bool`, `current_title`,
/// `current_url` — into one. `Playing.title` can legitimately be empty: right
/// after a station starts, before the title poller's first metadata reading
/// comes back.
enum Playback {
    Stopped,
    Playing { title: String, url: Option<String> },
}

impl Playback {
    const fn is_playing(&self) -> bool {
        matches!(self, Self::Playing { .. })
    }

    fn title(&self) -> &str {
        match self {
            Self::Playing { title, .. } => title,
            Self::Stopped => "",
        }
    }

    fn url(&self) -> Option<&str> {
        match self {
            Self::Playing { url, .. } => url.as_deref(),
            Self::Stopped => None,
        }
    }
}

pub struct Wavey {
    image: Sprite,
    font: BitmapFont,
    /// Never empty: `player::load_stations` falls back to `default_stations`.
    stations: Vec<player::Station>,
    station: usize,
    volume: u8,
    clock_24h: bool,
    dragging_knob: bool,
    last_clock_text: String,
    last_clock_check: Instant,
    /// Receives system-volume readings from the off-thread poller, so the
    /// blocking `wpctl`/`pactl` calls never run on the UI thread.
    volume_updates: mpsc::Receiver<u8>,
    /// One-shot result of the off-thread probe for an mpv left running by a
    /// previous cozyui instance (blocking IPC, so not done in `load`).
    resume_probe: mpsc::Receiver<Option<usize>>,
    title_updates: mpsc::Receiver<player::TitleUpdate>,
    /// What's currently playing, if anything: cached by the poller thread so
    /// a title click never does blocking mpv IPC on the UI thread. `title`
    /// can legitimately be empty while `Playing`, before the poller's first
    /// metadata reading arrives.
    playback: Playback,
    marquee_offset: usize,
    last_marquee_step: Instant,
    /// The volume value last written (or about to be written) to the OS,
    /// throttled separately from `volume` itself so a knob drag's flood of
    /// motion ticks doesn't spawn one detached `wpctl`/`pactl` per tick. See
    /// `push_volume`.
    pushed_volume: crate::util::Refresh<u8>,
}

impl Wavey {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let wavey = Self {
            image: crate::assets::wavey(),
            font: BitmapFont::load_with_fallback(
                &pixel_fonts::POCO_SPEC,
                &pixel_fonts::FUSION_PIXEL_8_SPEC,
            )?,
            stations: player::load_stations(&crate::paths::config_file(player::STATIONS_FILE)),
            station: 0,
            // Placeholder until the volume poller's first reading arrives.
            volume: 50,
            clock_24h: false,
            dragging_knob: false,
            last_clock_text: clock_text(false),
            last_clock_check: Instant::now(),
            volume_updates: player::spawn_volume_poller(),
            resume_probe: player::spawn_resume_probe(),
            title_updates: player::spawn_title_poller(),
            playback: Playback::Stopped,
            marquee_offset: 0,
            last_marquee_step: Instant::now(),
            // Seeded to match the placeholder `volume` above so the pair
            // starts in sync and the first real reading (below) doesn't
            // read as a user-driven change worth pushing back to the OS.
            pushed_volume: crate::util::Refresh::new(50),
        };
        Ok(wavey)
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.draw_sprite(&self.image, 0, 0, palette);
        self.draw_clock(fb, palette);
        self.draw_tuner(fb, palette);
        self.draw_knob(fb, palette);
        self.draw_title(fb, palette);
    }

    pub(crate) fn update(&mut self) -> bool {
        let now = Instant::now();
        let mut dirty = false;

        if now.duration_since(self.last_clock_check) >= CLOCK_REFRESH {
            self.last_clock_check = now;
            let text = clock_text(self.clock_24h);
            if text != self.last_clock_text {
                self.last_clock_text = text;
                dirty = true;
            }
        }

        // Reconnect to an mpv left running by a previous cozyui instance,
        // once the off-thread probe reports one.
        if let Ok(station) = self.resume_probe.try_recv() {
            self.playback = Playback::Playing {
                title: String::new(),
                url: None,
            };
            if let Some(station) = station.filter(|&station| station < self.stations.len()) {
                self.station = station;
            }
            dirty = true;
        }

        if let Some(volume) = self.volume_updates.try_iter().last()
            && !self.dragging_knob
            && volume != self.volume
            // Skip the external reading while a local change (e.g. a scroll)
            // is still waiting on the throttled push below: the poller can
            // race that push and report the stale pre-push OS volume, which
            // would otherwise silently overwrite the not-yet-pushed value.
            && self.volume == *self.pushed_volume.get()
        {
            self.volume = volume;
            // This reading already reflects what the OS has, so record it as
            // pushed rather than letting `push_volume` below mistake it for
            // a user-driven change and write it straight back.
            self.pushed_volume.set(volume);
            dirty = true;
        }
        self.push_volume(false);

        // A reading that arrives after `stop_player` (e.g. a stale in-flight
        // poll) is simply dropped: there's no title/url to update once
        // `playback` is `Stopped`.
        if let Some(update) = self.title_updates.try_iter().last()
            && let Playback::Playing { title, url } = &mut self.playback
        {
            *url = update.url;
            if update.title != *title {
                *title = update.title;
                self.marquee_offset = 0;
                dirty = true;
            }
        }

        if self.font.text_width(self.playback.title()) > TITLE_W
            && now.duration_since(self.last_marquee_step) >= MARQUEE_STEP
        {
            self.last_marquee_step = now;
            let loop_w =
                self.font.text_width(self.playback.title()) + self.font.text_width(MARQUEE_SEP);
            self.marquee_offset = (self.marquee_offset + 1) % loop_w;
            dirty = true;
        }

        dirty
    }

    /// Returns text to copy to the clipboard when the click asks for it.
    pub(crate) fn click(&mut self, x: isize, y: isize) -> Option<String> {
        if x < 0 || y < 0 {
            return None;
        }
        let x = x as usize;
        let y = y as usize;

        if let Some(button) = media_button_at(x, y) {
            self.press_media_button(button);
            return None;
        }

        if self.clock_contains(x, y) {
            self.clock_24h = !self.clock_24h;
            self.last_clock_text = clock_text(self.clock_24h);
            return None;
        }

        if self.knob_contains(x, y) {
            self.dragging_knob = true;
            self.set_volume_from_point(x, y, false);
            return None;
        }

        if self.tuner_contains(x, y) {
            self.station = self.station_at(x);
            self.play_station();
            return None;
        }

        if self.title_contains(x, y) {
            return self.title_copy_text();
        }

        None
    }

    fn title_contains(&self, x: usize, y: usize) -> bool {
        !self.playback.title().is_empty()
            && (TITLE_X.saturating_sub(TITLE_BOX_PAD)..TITLE_X + TITLE_W + TITLE_BOX_PAD)
                .contains(&x)
            && (TITLE_Y.saturating_sub(TITLE_BOX_PAD)..TITLE_Y + self.font.cell_h() + TITLE_BOX_PAD)
                .contains(&y)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn title_copy_text(&self) -> Option<String> {
        let title = self.playback.title();
        match self.playback.url() {
            Some(url) => Some(format!("{title}\n{url}")),
            None => Some(title.to_string()),
        }
    }

    pub(crate) fn release(&mut self) -> bool {
        let was_dragging = self.dragging_knob;
        self.dragging_knob = false;
        if was_dragging {
            self.push_volume(true);
        }
        was_dragging
    }

    pub(crate) fn scroll_up(&mut self, x: isize, y: isize) -> bool {
        self.scroll_volume(x, y, 5)
    }

    pub(crate) fn scroll_down(&mut self, x: isize, y: isize) -> bool {
        self.scroll_volume(x, y, -5)
    }

    pub(crate) fn shutdown(&mut self) {
        // The player lives in the "wavey" abduco session and keeps playing
        // across cozyui restarts; only the stop button kills it. A
        // scroll-driven volume change right before quitting is otherwise only
        // queued for the throttled per-tick push, never flushed — force it
        // now so it isn't silently dropped.
        self.push_volume(true);
    }

    #[allow(clippy::unused_self)]
    fn tuner_contains(&self, x: usize, y: usize) -> bool {
        (TUNER_X..TUNER_X + TUNER_W).contains(&x) && (TUNER_Y..TUNER_Y + TUNER_H).contains(&y)
    }

    #[allow(clippy::unused_self)]
    fn clock_contains(&self, x: usize, y: usize) -> bool {
        (DISPLAY_X..DISPLAY_X + CLOCK_CLEAR_W).contains(&x)
            && (DISPLAY_Y..DISPLAY_Y + CLOCK_CLEAR_H).contains(&y)
    }

    #[allow(clippy::unused_self)]
    const fn knob_contains(&self, x: usize, y: usize) -> bool {
        let dx = x as isize - KNOB_X as isize;
        let dy = y as isize - KNOB_Y as isize;
        dx * dx + dy * dy <= (KNOB_RADIUS * KNOB_RADIUS) as isize
    }

    fn station_at(&self, x: usize) -> usize {
        let rel = x.saturating_sub(TUNER_X).min(TUNER_W.saturating_sub(1));
        (rel * self.stations.len() / TUNER_W).min(self.stations.len() - 1)
    }

    fn set_volume_from_point(&mut self, x: usize, y: usize, clamp_wrap: bool) {
        let dx = x as f32 + 0.5 - KNOB_X as f32;
        let dy = y as f32 + 0.5 - KNOB_Y as f32;
        let angle = dy.atan2(dx);
        let sweep = (angle - FRAC_PI_2).rem_euclid(2.0 * PI);
        let mut volume = (sweep / (2.0 * PI) * f32::from(MAX_VOLUME)).round() as i16;

        if clamp_wrap {
            let delta = volume - i16::from(self.volume);
            if delta > 50 {
                volume = i16::from(MIN_VOLUME);
            } else if delta < -50 {
                volume = i16::from(MAX_VOLUME);
            }
        }

        self.set_volume(volume);
    }

    fn scroll_volume(&mut self, x: isize, y: isize, delta: i16) -> bool {
        if x < 0 || y < 0 || !self.knob_contains(x as usize, y as usize) {
            return false;
        }

        self.set_volume(i16::from(self.volume) + delta);
        true
    }

    fn set_volume(&mut self, volume: i16) {
        let volume = volume.clamp(i16::from(MIN_VOLUME), i16::from(MAX_VOLUME)) as u8;
        if volume == self.volume {
            return;
        }
        self.volume = volume;
    }

    /// Reconciles the OS volume with `self.volume`, throttled (`force =
    /// false`, from the per-tick `update`) so a knob drag's flood of motion
    /// events collapses to at most one `wpctl`/`pactl` spawn per
    /// `VOLUME_PUSH_THROTTLE`, instead of one per event with no ordering
    /// guarantee between them. `force = true` (from `release`) bypasses the
    /// throttle so letting go of the knob is never left waiting out the
    /// window before the final value lands.
    fn push_volume(&mut self, force: bool) {
        let volume = self.volume;
        let changed = if force {
            let changed = *self.pushed_volume.get() != volume;
            self.pushed_volume.set(volume);
            changed
        } else {
            self.pushed_volume.refresh(VOLUME_PUSH_THROTTLE, || volume)
        };
        if changed {
            player::set_system_volume(volume);
        }
    }

    fn play_station(&mut self) {
        let Some(station) = self.stations.get(self.station) else {
            return;
        };
        let mpv_args = station.mpv_args.clone();
        self.stop_player();

        let ipc_path = player::mpv_ipc_path();
        if let Err(err) = fs::remove_file(&ipc_path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "wavey: failed to remove mpv IPC socket {}: {err}",
                ipc_path.display()
            );
        }
        let script_opts =
            crate::util::shell_quote(&format!("cozyui-wavey-station={}", self.station));
        // Tokens containing `*` are left unquoted so the shell can glob-expand
        // them, since mpv doesn't glob-expand its own arguments. That is only
        // safe when the token is made up entirely of characters that can't
        // form shell metacharacters, so `is_safe_glob_token` restricts the
        // exception to a conservative allowlist; anything else (even if it
        // contains `*`) falls back to single-quoting, which just means the
        // glob won't expand rather than reopening shell injection.
        let quoted_args = mpv_args
            .split_whitespace()
            .map(|token| {
                if is_safe_glob_token(token) {
                    token.to_string()
                } else {
                    crate::util::shell_quote(token)
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let command_line = format!(
            "mpv --input-terminal=no --input-ipc-server=\"$COZYUI_MPV_IPC\" \
             --script-opts={script_opts} {quoted_args}"
        );
        player::start_player(command_line);
        self.playback = Playback::Playing {
            title: String::new(),
            url: None,
        };
    }

    fn press_media_button(&mut self, button: MediaButton) {
        match button {
            MediaButton::PlayPause => {
                if self.playback.is_playing() {
                    self.send_mpv_command(&["cycle", "pause"]);
                } else {
                    self.play_station();
                }
            }
            MediaButton::Stop => self.stop_player(),
            MediaButton::Back => self.send_mpv_command(&["playlist-prev", "force"]),
            MediaButton::Forward => self.send_mpv_command(&["playlist-next", "force"]),
        }
    }

    #[allow(clippy::needless_pass_by_ref_mut)]
    fn send_mpv_command(&mut self, command: &[&str]) {
        if !self.playback.is_playing() {
            return;
        }
        player::send_command(command);
    }

    fn stop_player(&mut self) {
        player::kill_player();
        let _ = fs::remove_file(player::mpv_ipc_path());
        self.playback = Playback::Stopped;
    }

    fn draw_clock(&self, fb: &mut Framebuffer, palette: &Palette) {
        clear_clock_art(fb, palette);
        let mut x = DISPLAY_X;
        for ch in self.last_clock_text.chars() {
            if ch == ':' {
                copy_clock_colon(&self.image, fb, x, DISPLAY_Y + 4, palette);
                x += COLON_W + DIGIT_GAP;
            } else {
                copy_clock_digit(&self.image, fb, ch, x, DISPLAY_Y, palette);
                x += DIGIT_W + DIGIT_GAP;
            }
        }
    }

    fn draw_tuner(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let text = palette_color::LAVENDER;
        let count = self.stations.len();
        for (index, station) in self.stations.iter().enumerate() {
            let center = station_center(index, count);
            let label_w = self.font.text_width(&station.label);
            let label_x = center.saturating_sub(label_w / 2);
            let label_y = if index % 2 == 0 {
                LABEL_ABOVE_Y
            } else {
                LABEL_BELOW_Y
            };
            self.font
                .draw_text(fb, &station.label, label_x as isize, label_y as isize, text);
        }

        let marker_x = if self.playback.is_playing() {
            station_center(self.station, count)
        } else {
            TUNER_X.saturating_sub(TUNER_MARK_SIZE + 2)
        };
        fb.fill_rect(
            marker_x.saturating_sub(TUNER_MARK_SIZE / 2) as isize,
            TUNER_MARK_Y as isize,
            TUNER_MARK_SIZE,
            TUNER_MARK_SIZE,
            palette_color::ROSE,
        );
    }

    fn draw_knob(&self, fb: &mut Framebuffer, palette: &Palette) {
        clear_knob_marker(&self.image, fb, palette);
        copy_moved_knob_marker(&self.image, fb, volume_angle(self.volume), palette);
    }

    fn draw_title(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let title = self.playback.title();
        if title.is_empty() {
            return;
        }

        let y = TITLE_Y;
        fb.fill_rect(
            TITLE_X.saturating_sub(TITLE_BOX_PAD) as isize,
            y.saturating_sub(TITLE_BOX_PAD) as isize,
            TITLE_W + TITLE_BOX_PAD * 2,
            self.font.cell_h() + TITLE_BOX_PAD * 2,
            palette_color::BLACK,
        );

        let cream = palette_color::CREAM;
        let text_w = self.font.text_width(title);
        if text_w <= TITLE_W {
            draw_text_centered(
                fb,
                &self.font,
                title,
                TITLE_X as isize,
                TITLE_W,
                y as isize,
                cream,
            );
            return;
        }

        // Two copies around the separator cover the window for any pixel
        // offset within the loop (title width + separator width).
        let looped = format!("{title}{MARQUEE_SEP}{title}");
        self.font.draw_text_clipped(
            fb,
            &looped,
            TITLE_X as isize - self.marquee_offset as isize,
            y as isize,
            cream,
            TITLE_X as isize,
            TITLE_W,
        );
    }
}

impl crate::widget::Widget for Wavey {
    fn width(&self) -> usize {
        self.image.width
    }

    fn height(&self) -> usize {
        self.image
            .height
            .max(TITLE_Y + self.font.cell_h() + TITLE_BOX_PAD)
    }

    fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        Self::render(self, fb, palette);
    }

    fn update(&mut self) -> Result<bool, Box<dyn Error>> {
        Ok(Self::update(self))
    }

    fn click(
        &mut self,
        x: isize,
        y: isize,
        _state: u16,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        Ok(crate::widget::ClickOutcome {
            copy_text: Self::click(self, x, y),
            ..Default::default()
        })
    }

    fn motion(&mut self, x: isize, y: isize) -> bool {
        if !self.dragging_knob || x < 0 || y < 0 {
            return false;
        }

        self.set_volume_from_point(x as usize, y as usize, true);
        true
    }

    fn scroll(&mut self, x: isize, y: isize, direction: crate::widget::ScrollDirection) -> bool {
        match direction {
            crate::widget::ScrollDirection::Up => self.scroll_up(x, y),
            crate::widget::ScrollDirection::Down => self.scroll_down(x, y),
        }
    }

    /// Hand over everything clickable: media buttons, clock, volume knob,
    /// tuner, and the copyable title.
    fn cursor_at(&self, x: isize, y: isize) -> CursorKind {
        if x < 0 || y < 0 {
            return CursorKind::Pointer;
        }
        let x = x as usize;
        let y = y as usize;
        if media_button_at(x, y).is_some()
            || self.clock_contains(x, y)
            || self.knob_contains(x, y)
            || self.tuner_contains(x, y)
            || self.title_contains(x, y)
        {
            CursorKind::Hand
        } else {
            CursorKind::Pointer
        }
    }
}

const fn station_center(index: usize, count: usize) -> usize {
    TUNER_X + ((index * 2 + 1) * TUNER_W / (count * 2))
}

fn media_button_at(x: usize, y: usize) -> Option<MediaButton> {
    if !(MEDIA_BUTTON_Y..MEDIA_BUTTON_Y + MEDIA_BUTTON_H).contains(&y) {
        return None;
    }

    let total_w = MEDIA_BUTTON_COUNT * MEDIA_BUTTON_W + (MEDIA_BUTTON_COUNT - 1) * MEDIA_BUTTON_GAP;
    if !(MEDIA_BUTTON_X..MEDIA_BUTTON_X + total_w).contains(&x) {
        return None;
    }

    let rel_x = x - MEDIA_BUTTON_X;
    let slot_w = MEDIA_BUTTON_W + MEDIA_BUTTON_GAP;
    if rel_x % slot_w >= MEDIA_BUTTON_W {
        return None;
    }

    match rel_x / slot_w {
        0 => Some(MediaButton::PlayPause),
        1 => Some(MediaButton::Stop),
        2 => Some(MediaButton::Back),
        3 => Some(MediaButton::Forward),
        _ => None,
    }
}

fn volume_angle(volume: u8) -> f32 {
    ((f32::from(volume) / f32::from(MAX_VOLUME)) * 2.0).mul_add(PI, FRAC_PI_2)
}

fn clear_clock_art(fb: &mut Framebuffer, _palette: &Palette) {
    let black = palette_color::BLACK;
    fb.fill_rect(
        DISPLAY_X as isize,
        DISPLAY_Y as isize,
        CLOCK_CLEAR_W,
        CLOCK_CLEAR_H,
        black,
    );
}

fn copy_clock_digit(
    image: &Sprite,
    fb: &mut Framebuffer,
    digit: char,
    dest_x: usize,
    dest_y: usize,
    palette: &Palette,
) {
    let Some(mask) = digit_mask(digit) else {
        return;
    };

    for segment in 0..7 {
        if mask & (1 << segment) == 0 {
            continue;
        }
        let rect = clock_segment_rect(segment);
        copy_clock_pixels(
            image,
            fb,
            rect,
            (CLOCK_EIGHT_SRC_X, CLOCK_EIGHT_SRC_Y),
            dest_x,
            dest_y,
            palette,
        );
    }
}

fn copy_clock_colon(
    image: &Sprite,
    fb: &mut Framebuffer,
    dest_x: usize,
    dest_y: usize,
    palette: &Palette,
) {
    copy_clock_pixels(
        image,
        fb,
        SourceRect {
            x: CLOCK_COLON_SRC_X,
            y: CLOCK_COLON_SRC_Y,
            w: COLON_W,
            h: CLOCK_COLON_H,
        },
        (CLOCK_COLON_SRC_X, CLOCK_COLON_SRC_Y),
        dest_x,
        dest_y,
        palette,
    );
}

fn copy_clock_pixels(
    image: &Sprite,
    fb: &mut Framebuffer,
    src: SourceRect,
    anchor: (usize, usize),
    dest_x: usize,
    dest_y: usize,
    palette: &Palette,
) {
    for y in 0..src.h {
        for x in 0..src.w {
            let index = image.at(src.x + x, src.y + y);
            if !matches!(index, palette_color::CRIMSON | palette_color::ROSE) {
                continue;
            }
            let px = (dest_x + src.x + x - anchor.0) as isize;
            let py = (dest_y + src.y + y - anchor.1) as isize;
            if let Some(color) = palette.resolve_index(index, px, py) {
                fb.set_pixel(px, py, color);
            }
        }
    }
}

const fn digit_mask(digit: char) -> Option<u8> {
    Some(match digit {
        '0' => 0b011_1111,
        '1' => 0b000_0110,
        '2' => 0b101_1011,
        '3' => 0b100_1111,
        '4' => 0b110_0110,
        '5' => 0b110_1101,
        '6' => 0b111_1101,
        '7' => 0b000_0111,
        '8' => 0b111_1111,
        '9' => 0b110_1111,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct SourceRect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

const fn clock_segment_rect(segment: usize) -> SourceRect {
    match segment {
        0 => SourceRect {
            x: CLOCK_EIGHT_SRC_X + 2,
            y: CLOCK_EIGHT_SRC_Y,
            w: DIGIT_W - 4,
            h: 3,
        },
        1 => SourceRect {
            x: CLOCK_EIGHT_SRC_X + 9,
            y: CLOCK_EIGHT_SRC_Y + 2,
            w: 3,
            h: 11,
        },
        2 => SourceRect {
            x: CLOCK_EIGHT_SRC_X + 9,
            y: CLOCK_EIGHT_SRC_Y + 12,
            w: 3,
            h: 11,
        },
        3 => SourceRect {
            x: CLOCK_EIGHT_SRC_X + 2,
            y: CLOCK_EIGHT_SRC_Y + 22,
            w: DIGIT_W - 4,
            h: 3,
        },
        4 => SourceRect {
            x: CLOCK_EIGHT_SRC_X,
            y: CLOCK_EIGHT_SRC_Y + 12,
            w: 3,
            h: 11,
        },
        5 => SourceRect {
            x: CLOCK_EIGHT_SRC_X,
            y: CLOCK_EIGHT_SRC_Y + 2,
            w: 3,
            h: 11,
        },
        6 => SourceRect {
            x: CLOCK_EIGHT_SRC_X + 2,
            y: CLOCK_EIGHT_SRC_Y + 11,
            w: DIGIT_W - 4,
            h: 3,
        },
        _ => SourceRect {
            x: CLOCK_EIGHT_SRC_X,
            y: CLOCK_EIGHT_SRC_Y,
            w: 0,
            h: 0,
        },
    }
}

fn clear_knob_marker(image: &Sprite, fb: &mut Framebuffer, palette: &Palette) {
    for y in KNOB_MARKER_SRC_Y..KNOB_MARKER_SRC_Y + KNOB_MARKER_SRC_H {
        for x in KNOB_MARKER_SRC_X..KNOB_MARKER_SRC_X + KNOB_MARKER_SRC_W {
            if image.at(x, y) == palette_color::LAVENDER
                && let Some(color) =
                    palette.resolve_index(palette_color::PLUM, x as isize, y as isize)
            {
                fb.set_pixel(x as isize, y as isize, color);
            }
        }
    }
}

fn copy_moved_knob_marker(
    image: &Sprite,
    fb: &mut Framebuffer,
    target_angle: f32,
    palette: &Palette,
) {
    let marker_center_x = KNOB_MARKER_SRC_X as f32 + KNOB_MARKER_SRC_W as f32 / 2.0 - 0.5;
    let marker_center_y = KNOB_MARKER_SRC_Y as f32 + KNOB_MARKER_SRC_H as f32 / 2.0 - 0.5;
    let radius = (marker_center_x - KNOB_X as f32).hypot(marker_center_y - KNOB_Y as f32);
    let dest_center_x = target_angle.cos().mul_add(radius, KNOB_X as f32);
    let dest_center_y = target_angle.sin().mul_add(radius, KNOB_Y as f32);
    let dest_left = (dest_center_x - KNOB_MARKER_SRC_W as f32 / 2.0 + 0.5).round() as isize;
    let dest_top = (dest_center_y - KNOB_MARKER_SRC_H as f32 / 2.0 + 0.5).round() as isize;

    for y in KNOB_MARKER_SRC_Y..KNOB_MARKER_SRC_Y + KNOB_MARKER_SRC_H {
        for x in KNOB_MARKER_SRC_X..KNOB_MARKER_SRC_X + KNOB_MARKER_SRC_W {
            let index = image.at(x, y);
            if index != palette_color::LAVENDER {
                continue;
            }

            let dest_x = dest_left + (x - KNOB_MARKER_SRC_X) as isize;
            let dest_y = dest_top + (y - KNOB_MARKER_SRC_Y) as isize;
            if dest_x >= 0
                && dest_y >= 0
                && let Some(color) = palette.resolve_index(index, dest_x, dest_y)
            {
                fb.set_pixel(dest_x, dest_y, color);
            }
        }
    }
}

fn clock_text(clock_24h: bool) -> String {
    let Some((hour, minute)) = local_hour_minute() else {
        return "00:00".to_string();
    };
    if !clock_24h {
        let hour = match hour % 12 {
            0 => 12,
            hour => hour,
        };
        return format!("{hour:02}:{minute:02}");
    }
    format!("{hour:02}:{minute:02}")
}

fn local_hour_minute() -> Option<(u8, u8)> {
    let tm = local_time()?;
    Some((tm.tm_hour as u8, tm.tm_min as u8))
}

/// Whether `token` is safe to pass unquoted to the shell so a `*` in it can
/// glob-expand. Requires both that the token contains a `*` (otherwise there
/// is nothing to expand and quoting it is strictly safer) and that every
/// character is drawn from a conservative allowlist (ASCII alphanumerics
/// plus `*`, `/`, `.`, `_`, `-`, `~`) that cannot form shell metacharacters,
/// so a station config line can never smuggle in `;`, `|`, backticks,
/// `$(...)`, quotes, whitespace, etc. by hiding them behind an unrelated `*`.
fn is_safe_glob_token(token: &str) -> bool {
    token.contains('*')
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'*' | b'/' | b'.' | b'_' | b'-' | b'~'))
}

#[cfg(test)]
mod glob_token_tests {
    use super::is_safe_glob_token;

    #[test]
    fn plain_url_token_is_not_safe_glob() {
        assert!(!is_safe_glob_token("https://example.com/stream"));
    }

    #[test]
    fn home_relative_glob_stays_raw() {
        assert!(is_safe_glob_token("~/Music/*.mp3"));
    }

    #[test]
    fn glob_with_semicolon_is_rejected() {
        assert!(!is_safe_glob_token("*;rm"));
    }

    #[test]
    fn glob_with_command_substitution_is_rejected() {
        assert!(!is_safe_glob_token("*$(x)"));
    }

    #[test]
    fn glob_with_quote_is_rejected() {
        assert!(!is_safe_glob_token("*'a'"));
    }
}
