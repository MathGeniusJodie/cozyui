use std::error::Error;
use std::f32::consts::{FRAC_PI_2, PI};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::localtime::local_time;
use crate::palette_color;
use crate::text::BitmapFont;
use crate::{CursorKind, Framebuffer, Index, Palette, Sprite, TRANSPARENT};

const STATIONS_FILE: &str = "radio_stations.txt";

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
const VOLUME_REFRESH: Duration = Duration::from_secs(3);
const TITLE_REFRESH: Duration = Duration::from_secs(1);
// Song title display: TITLE_X/TITLE_Y position the text's top-left corner,
// TITLE_W is the fixed text window width (longer titles marquee through it);
// the black box extends TITLE_BOX_PAD around the window on every side.
const TITLE_X: usize = TUNER_X - 20;
const TITLE_Y: usize = 47;
const TITLE_W: usize = TUNER_W + 24;
const TITLE_BOX_PAD: usize = 2;
const MARQUEE_STEP: Duration = Duration::from_millis(60);
const MARQUEE_SEP: &str = "  ~  ";
const TITLE_READ_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Clone)]
struct Station {
    label: String,
    mpv_args: String,
}

#[derive(Clone, Copy)]
enum MediaButton {
    PlayPause,
    Stop,
    Back,
    Forward,
}

pub struct Wavey {
    image: Sprite,
    font: BitmapFont,
    stations: Vec<Station>,
    station: usize,
    volume: u8,
    clock_24h: bool,
    dragging_knob: bool,
    last_clock_text: String,
    last_clock_check: Instant,
    last_volume_check: Instant,
    title_updates: mpsc::Receiver<TitleUpdate>,
    current_title: String,
    /// Stream URL matching `current_title`, cached by the poller thread so a
    /// title click never does blocking mpv IPC on the UI thread.
    current_url: Option<String>,
    marquee_offset: usize,
    last_marquee_step: Instant,
    playing: bool,
}

impl Wavey {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let volume = read_system_volume().unwrap_or(50);
        let mut wavey = Self {
            image: crate::assets::wavey(),
            font: BitmapFont::load_with_fallback(
                &pixel_fonts::POCO_SPEC,
                &pixel_fonts::FUSION_PIXEL_8_SPEC,
            )?,
            stations: load_stations(&crate::paths::config_file(STATIONS_FILE)),
            station: 0,
            volume,
            clock_24h: false,
            dragging_knob: false,
            last_clock_text: clock_text(false),
            last_clock_check: Instant::now(),
            last_volume_check: Instant::now(),
            title_updates: spawn_title_poller(),
            current_title: String::new(),
            current_url: None,
            marquee_offset: 0,
            last_marquee_step: Instant::now(),
            playing: false,
        };
        wavey.resume_running_player();
        Ok(wavey)
    }

    /// Reconnect to an mpv left running in the "wavey" abduco session by a
    /// previous cozyui instance. The station index is stamped onto mpv via
    /// --script-opts at launch, so the running player itself records which
    /// station it is; querying any property doubles as the liveness check.
    fn resume_running_player(&mut self) {
        let Some(opts) = mpv_property("script-opts") else {
            return;
        };

        self.playing = true;
        if let Some(station) =
            station_from_script_opts(&opts).filter(|&station| station < self.stations.len())
        {
            self.station = station;
        }
    }

    pub(crate) const fn width(&self) -> usize {
        self.image.width
    }

    pub(crate) fn height(&self) -> usize {
        self.image
            .height
            .max(TITLE_Y + self.font.cell_h() + TITLE_BOX_PAD)
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
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

        if now.duration_since(self.last_volume_check) >= VOLUME_REFRESH && !self.dragging_knob {
            self.last_volume_check = now;
            if let Some(volume) = read_system_volume()
                && volume != self.volume
            {
                self.volume = volume;
                dirty = true;
            }
        }

        if let Some(update) = self.title_updates.try_iter().last() {
            let (title, url) = if self.playing {
                (update.title, update.url)
            } else {
                (String::new(), None)
            };
            self.current_url = url;
            if title != self.current_title {
                self.current_title = title;
                self.marquee_offset = 0;
                dirty = true;
            }
        }

        if self.font.text_width(&self.current_title) > TITLE_W
            && now.duration_since(self.last_marquee_step) >= MARQUEE_STEP
        {
            self.last_marquee_step = now;
            let loop_w =
                self.font.text_width(&self.current_title) + self.font.text_width(MARQUEE_SEP);
            self.marquee_offset = (self.marquee_offset + 1) % loop_w;
            dirty = true;
        }

        dirty
    }

    /// Returns text to copy to the clipboard when the click asks for it.
    pub(crate) fn click(&mut self, x: i16, y: i16) -> Option<String> {
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

    /// Hand over everything clickable: media buttons, clock, volume knob,
    /// tuner, and the copyable title.
    pub(crate) fn cursor_at(&self, x: i16, y: i16) -> CursorKind {
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

    fn title_contains(&self, x: usize, y: usize) -> bool {
        !self.current_title.is_empty()
            && (TITLE_X.saturating_sub(TITLE_BOX_PAD)..TITLE_X + TITLE_W + TITLE_BOX_PAD)
                .contains(&x)
            && (TITLE_Y.saturating_sub(TITLE_BOX_PAD)..TITLE_Y + self.font.cell_h() + TITLE_BOX_PAD)
                .contains(&y)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn title_copy_text(&self) -> Option<String> {
        let title = self.current_title.clone();
        match &self.current_url {
            Some(url) => Some(format!("{title}\n{url}")),
            None => Some(title),
        }
    }

    pub(crate) const fn release(&mut self) -> bool {
        let was_dragging = self.dragging_knob;
        self.dragging_knob = false;
        was_dragging
    }

    pub(crate) fn motion(&mut self, x: i16, y: i16) -> bool {
        if !self.dragging_knob || x < 0 || y < 0 {
            return false;
        }

        self.set_volume_from_point(x as usize, y as usize, true);
        true
    }

    pub(crate) fn scroll_up(&mut self, x: i16, y: i16) -> bool {
        self.scroll_volume(x, y, 5)
    }

    pub(crate) fn scroll_down(&mut self, x: i16, y: i16) -> bool {
        self.scroll_volume(x, y, -5)
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    pub(crate) const fn shutdown(&mut self) {
        // The player lives in the "wavey" abduco session and keeps playing
        // across cozyui restarts; only the stop button kills it.
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
        if self.stations.is_empty() {
            return 0;
        }

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

    fn scroll_volume(&mut self, x: i16, y: i16, delta: i16) -> bool {
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
        set_system_volume(volume);
    }

    fn play_station(&mut self) {
        let Some(station) = self.stations.get(self.station) else {
            return;
        };
        if station.mpv_args.trim().is_empty() {
            eprintln!("wavey: selected station has no mpv_args, leaving playback untouched");
            return;
        }
        let mpv_args = station.mpv_args.clone();
        self.stop_player();

        let ipc_path = mpv_ipc_path();
        if let Err(err) = fs::remove_file(&ipc_path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "wavey: failed to remove mpv IPC socket {}: {err}",
                ipc_path.display()
            );
        }
        self.current_title.clear();
        let script_opts =
            crate::util::shell_quote(&format!("cozyui-wavey-station={}", self.station));
        let quoted_args = mpv_args
            .split_whitespace()
            .map(crate::util::shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
        let command_line = format!(
            "mpv --input-terminal=no --input-ipc-server=\"$COZYUI_MPV_IPC\" \
             --script-opts={script_opts} {quoted_args}"
        );
        start_player(command_line);
        self.playing = true;
    }

    fn press_media_button(&mut self, button: MediaButton) {
        match button {
            MediaButton::PlayPause => {
                if self.playing {
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
        if !self.playing {
            return;
        }

        let Ok(mut stream) = UnixStream::connect(mpv_ipc_path()) else {
            return;
        };
        let message = serde_json::json!({ "command": command }).to_string();
        if stream.write_all(message.as_bytes()).is_ok() {
            let _ = stream.write_all(b"\n");
        }
    }

    fn stop_player(&mut self) {
        kill_player();
        let _ = fs::remove_file(mpv_ipc_path());
        self.playing = false;
        self.current_title.clear();
        self.current_url = None;
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
        if self.stations.is_empty() {
            return;
        }

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
                .draw_text(fb, &station.label, label_x, label_y, text);
        }

        let marker_x = if self.playing {
            station_center(self.station, count)
        } else {
            TUNER_X.saturating_sub(TUNER_MARK_SIZE + 2)
        };
        fb.fill_rect(
            marker_x.saturating_sub(TUNER_MARK_SIZE / 2),
            TUNER_MARK_Y,
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
        if self.current_title.is_empty() {
            return;
        }

        let y = TITLE_Y;
        fb.fill_rect(
            TITLE_X.saturating_sub(TITLE_BOX_PAD),
            y.saturating_sub(TITLE_BOX_PAD),
            TITLE_W + TITLE_BOX_PAD * 2,
            self.font.cell_h() + TITLE_BOX_PAD * 2,
            palette_color::BLACK,
        );

        let cream = palette_color::CREAM;
        let text_w = self.font.text_width(&self.current_title);
        if text_w <= TITLE_W {
            let x = TITLE_X + (TITLE_W - text_w) / 2;
            self.font.draw_text(fb, &self.current_title, x, y, cream);
            return;
        }

        // Two copies around the separator cover the window for any pixel
        // offset within the loop (title width + separator width).
        let looped = format!("{}{MARQUEE_SEP}{}", self.current_title, self.current_title);
        self.font.draw_text_clipped(
            fb,
            &looped,
            TITLE_X as isize - self.marquee_offset as isize,
            y,
            cream,
            TITLE_X,
            TITLE_W,
        );
    }
}

impl crate::widget::Widget for Wavey {
    fn width(&self) -> usize {
        self.width()
    }

    fn height(&self) -> usize {
        self.height()
    }

    fn fill_color(&self, palette: &Palette) -> Index {
        self.fill_color(palette)
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        Self::render(self, fb, palette);
    }

    fn update(&mut self) -> Result<bool, Box<dyn Error>> {
        Ok(Self::update(self))
    }

    fn click(
        &mut self,
        x: i16,
        y: i16,
        _state: u16,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        Ok(crate::widget::ClickOutcome {
            copy_text: Self::click(self, x, y),
            ..Default::default()
        })
    }

    fn motion(&mut self, x: i16, y: i16) -> bool {
        Self::motion(self, x, y)
    }

    fn scroll(&mut self, x: i16, y: i16, direction: crate::widget::ScrollDirection) -> bool {
        match direction {
            crate::widget::ScrollDirection::Up => self.scroll_up(x, y),
            crate::widget::ScrollDirection::Down => self.scroll_down(x, y),
        }
    }

    fn cursor_at(&self, x: i16, y: i16) -> CursorKind {
        self.cursor_at(x, y)
    }
}

const fn station_center(index: usize, count: usize) -> usize {
    TUNER_X + ((index * 2 + 1) * TUNER_W / (count * 2))
}

fn title_from_metadata(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    ["title", "icy-title", "icy_title"]
        .iter()
        .find_map(|key| metadata_value(object, key))
}

fn track_line_from_metadata(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let title = title_from_metadata(value);
    // Untagged web tracks (e.g. SoundCloud) have no artist tag, only the
    // uploading account; use it unless the title already names the artist
    // dash-style, as YouTube channel uploads usually do.
    let artist = ["artist", "album_artist", "albumartist"]
        .iter()
        .find_map(|key| metadata_value(object, key))
        .or_else(|| {
            if title.as_deref().is_some_and(|title| title.contains(" - ")) {
                return None;
            }
            metadata_value(object, "uploader")
        });

    match (artist, title) {
        (Some(artist), Some(title)) => {
            if title.to_lowercase().contains(&artist.to_lowercase()) {
                Some(title)
            } else {
                Some(format!("{artist} - {title}"))
            }
        }
        (Some(artist), None) => Some(artist),
        (None, Some(title)) => Some(title),
        (None, None) => None,
    }
}

fn metadata_value(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    object.iter().find_map(|(candidate, value)| {
        if candidate.eq_ignore_ascii_case(key) {
            json_string(value)
        } else {
            None
        }
    })
}

fn json_string(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .filter(|text| !text.trim().is_empty())
}

#[allow(clippy::needless_pass_by_value)]
fn clean_title(title: String) -> Option<String> {
    let title = deunicode::deunicode(&title);
    let title = title.split_whitespace().collect::<Vec<&str>>().join(" ");
    (!title.is_empty()).then_some(title)
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

/// The currently playing track's display title, fetched from mpv over IPC.
/// Blocking (socket round trips with read timeouts) — call off the UI thread.
fn current_mpv_title() -> Option<String> {
    let chapter_title = mpv_property("chapter-metadata")
        .as_ref()
        .and_then(title_from_metadata);
    let media_title = mpv_property("media-title").as_ref().and_then(json_string);

    if let Some(chapter_title) = chapter_title {
        if let Some(media_title) = media_title.as_deref()
            && !media_title.eq_ignore_ascii_case(&chapter_title)
        {
            return clean_title(format!("{media_title} - {chapter_title}"));
        }
        return clean_title(chapter_title);
    }

    mpv_property("metadata")
        .as_ref()
        .and_then(track_line_from_metadata)
        .or(media_title)
        .and_then(clean_title)
}

fn mpv_property(property: &str) -> Option<serde_json::Value> {
    let mut stream = UnixStream::connect(mpv_ipc_path()).ok()?;
    let _ = stream.set_read_timeout(Some(TITLE_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(TITLE_READ_TIMEOUT));

    let message = serde_json::json!({
        "command": ["get_property", property],
        "request_id": 1
    })
    .to_string();
    stream.write_all(message.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;

    let mut reader = BufReader::new(stream);
    for _ in 0..64 {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let Ok(response) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if response
            .get("request_id")
            .and_then(serde_json::Value::as_i64)
            != Some(1)
        {
            continue;
        }
        if response.get("error").and_then(serde_json::Value::as_str) != Some("success") {
            return None;
        }
        return response.get("data").cloned();
    }

    None
}

/// One poller result: the display title and the stream URL it belongs to.
struct TitleUpdate {
    title: String,
    url: Option<String>,
}

/// Poll mpv for the playing title (and its URL, for title-click copies) off
/// the UI thread; a wedged mpv then can't stall rendering. The thread exits
/// once the receiver is dropped.
fn spawn_title_poller() -> mpsc::Receiver<TitleUpdate> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let title = current_mpv_title().unwrap_or_default();
            let url = (!title.is_empty())
                .then(|| mpv_property("path").as_ref().and_then(json_string))
                .flatten();
            if tx.send(TitleUpdate { title, url }).is_err() {
                return;
            }
            thread::sleep(TITLE_REFRESH);
        }
    });
    rx
}

fn mpv_ipc_path() -> PathBuf {
    // Fixed (non-pid) path on purpose: a restarted cozyui reconnects to an
    // mpv left running in the persistent player session. kill_player anchors
    // its pkill pattern on this exact path, so it can only ever match the
    // wavey mpv, not unrelated mpv processes.
    std::env::temp_dir().join("cozyui-mpv-wavey.sock")
}

/// SIGKILL the wavey mpv (matched by its IPC socket argument)
/// so the session loop frees up immediately instead of waiting for a
/// graceful quit. The pattern anchors on the full socket path, so it can
/// only match the wavey mpv, never an unrelated mpv.
fn kill_player() {
    let pattern = format!("mpv .*{}", mpv_ipc_path().display());
    let _ = Command::new("pkill")
        .args(["-9", "-f", &pattern])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn station_from_script_opts(opts: &serde_json::Value) -> Option<usize> {
    opts.get("cozyui-wavey-station")?.as_str()?.parse().ok()
}

fn player_fifo_path() -> PathBuf {
    std::env::temp_dir().join("cozyui-mpv-wavey.cmd")
}

/// Session setup plus command hand-off, on a background thread: the `mkfifo`/
/// `abduco` `.status()` calls block until those children exit, which must not
/// stall the UI thread (`play_station` runs inside `click`). The ordering
/// matters — the FIFO and its reader loop must exist before the command write,
/// or the shell redirection would create a plain file — so both steps share
/// one thread.
fn start_player(command_line: String) {
    std::thread::spawn(move || {
        ensure_player_session();
        queue_player_command(&command_line);
    });
}

/// One persistent "wavey" abduco session running a shell loop that reads mpv
/// command lines from the FIFO and runs them. The loop outlives each mpv, so
/// the session name never has to be recycled and `abduco -a wavey` works from
/// any terminal whenever something is (or was) playing.
fn ensure_player_session() {
    let fifo = player_fifo_path();
    let _ = Command::new("mkfifo")
        .arg(&fifo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Fails fast with "session exists" noise when already running; that is
    // the expected steady state, so the output is discarded. -f reclaims the
    // name from a dead session (e.g. a force-killed mpv from an older cozyui)
    // that would otherwise block creation forever.
    let runner =
        r#"while :; do cmd=$(cat "$COZYUI_MPV_FIFO") || exit; [ -n "$cmd" ] && eval "$cmd"; done"#;
    let _ = Command::new("abduco")
        .args(["-f", "-n", "wavey", "sh", "-c", runner])
        .env("COZYUI_MPV_FIFO", &fifo)
        .env("COZYUI_MPV_IPC", mpv_ipc_path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Writing to the FIFO blocks until the session loop is back at `cat` (i.e.
/// the previous mpv has exited), so the write happens in a throwaway child
/// that gets reaped off the UI thread.
fn queue_player_command(command: &str) {
    let _ = crate::util::spawn_and_reap(
        Command::new("sh")
            .args([
                "-c",
                r#"printf '%s\n' "$COZYUI_MPV_CMD" > "$COZYUI_MPV_FIFO""#,
            ])
            .env("COZYUI_MPV_CMD", command)
            .env("COZYUI_MPV_FIFO", player_fifo_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

fn volume_angle(volume: u8) -> f32 {
    ((f32::from(volume) / f32::from(MAX_VOLUME)) * 2.0).mul_add(PI, FRAC_PI_2)
}

fn clear_clock_art(fb: &mut Framebuffer, _palette: &Palette) {
    let black = palette_color::BLACK;
    fb.fill_rect(DISPLAY_X, DISPLAY_Y, CLOCK_CLEAR_W, CLOCK_CLEAR_H, black);
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
            let px = dest_x + src.x + x - anchor.0;
            let py = dest_y + src.y + y - anchor.1;
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
                && let Some(color) = palette.resolve_index(palette_color::PLUM, x, y)
            {
                fb.set_pixel(x, y, color);
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
                && let Some(color) = palette.resolve_index(index, dest_x as usize, dest_y as usize)
            {
                fb.set_pixel(dest_x as usize, dest_y as usize, color);
            }
        }
    }
}

fn load_stations(path: &str) -> Vec<Station> {
    fs::read_to_string(path)
        .ok()
        .map(|text| {
            text.lines()
                .filter_map(parse_station)
                .collect::<Vec<Station>>()
        })
        .filter(|stations| !stations.is_empty())
        .unwrap_or_else(default_stations)
}

fn parse_station(line: &str) -> Option<Station> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (label, mpv_args) = line.split_once('|')?;
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    Some(Station {
        label: label.chars().take(6).collect(),
        mpv_args: mpv_args.trim().to_string(),
    })
}

fn default_stations() -> Vec<Station> {
    [
        (
            "FM",
            "--no-video --really-quiet https://somafm.com/groovesalad.pls",
        ),
        (
            "POP",
            "--no-video --really-quiet https://somafm.com/poptron.pls",
        ),
        (
            "DNB",
            "--no-video --really-quiet https://somafm.com/deepspaceone.pls",
        ),
        (
            "JAZZ",
            "--no-video --really-quiet https://somafm.com/sonicuniverse.pls",
        ),
        (
            "LPS",
            "--no-video --really-quiet https://somafm.com/lush.pls",
        ),
    ]
    .into_iter()
    .map(|(label, mpv_args)| Station {
        label: label.to_string(),
        mpv_args: mpv_args.to_string(),
    })
    .collect()
}

fn read_system_volume() -> Option<u8> {
    read_wpctl_volume().or_else(read_pactl_volume)
}

fn read_wpctl_volume() -> Option<u8> {
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let value = text
        .split_whitespace()
        .find_map(|word| word.parse::<f32>().ok())?;
    Some((value * 100.0).round().clamp(0.0, 100.0) as u8)
}

fn read_pactl_volume() -> Option<u8> {
    let output = Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find_map(|word| word.strip_suffix('%')?.parse::<u8>().ok())
        .map(|volume| volume.min(100))
}

fn set_system_volume(volume: u8) {
    // Off-thread so the UI never blocks, and waited on so (a) no zombies and
    // (b) a wpctl that exists but fails at runtime still falls back to pactl.
    std::thread::spawn(move || {
        let wpctl = Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{volume}%")])
            .status();
        if wpctl.is_ok_and(|status| status.success()) {
            return;
        }

        let _ = Command::new("pactl")
            .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{volume}%")])
            .status();
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_line_falls_back_to_uploader_when_untagged() {
        let soundcloud = serde_json::json!({
            "title": "Double Spire (Unbroken Edit)",
            "uploader": "UnbrokenOne",
        });
        let tagged = serde_json::json!({
            "title": "Ask Me",
            "artist": "Duck Sauce",
            "uploader": "ducksaucenyc",
        });
        let youtube_upload = serde_json::json!({
            "title": "Soichi Terada - Double Spire",
            "uploader": "TheDailyDose",
        });

        assert_eq!(
            track_line_from_metadata(&soundcloud).as_deref(),
            Some("UnbrokenOne - Double Spire (Unbroken Edit)")
        );
        assert_eq!(
            track_line_from_metadata(&tagged).as_deref(),
            Some("Duck Sauce - Ask Me")
        );
        assert_eq!(
            track_line_from_metadata(&youtube_upload).as_deref(),
            Some("Soichi Terada - Double Spire")
        );
    }

    #[test]
    fn station_from_script_opts_reads_stamped_index() {
        let opts = serde_json::json!({"cozyui-wavey-station": "2", "other": "x"});

        assert_eq!(station_from_script_opts(&opts), Some(2));
        assert_eq!(station_from_script_opts(&serde_json::json!({})), None);
        assert_eq!(
            station_from_script_opts(&serde_json::json!({"cozyui-wavey-station": "nope"})),
            None
        );
    }
}
