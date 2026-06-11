use std::error::Error;
use std::f32::consts::{FRAC_PI_2, PI};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::bitmap_font::BitmapFont;
use crate::palette_color;
use crate::poco_font;
use crate::{Framebuffer, Palette, Rgba, Sprite};

const WAVEY_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/wavey.png");
const STATIONS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/radio_stations.txt");

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

const TUNER_ALL_Y: usize = 7;
const TUNER_X: usize = 102;
const TUNER_Y: usize = TUNER_ALL_Y + 12;
const TUNER_W: usize = 96;
const TUNER_H: usize = 26;
const TUNER_MARK_Y: usize = TUNER_ALL_Y + 23;
const TUNER_MARK_SIZE: usize = 5;
const LABEL_ABOVE_Y: usize = TUNER_ALL_Y + 9;
const LABEL_BELOW_Y: usize = TUNER_ALL_Y + 24;

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
const TITLE_GAP: usize = 3;
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

pub(crate) struct Wavey {
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
    last_title_check: Instant,
    current_title: String,
    playing: bool,
}

impl Wavey {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let volume = read_system_volume().unwrap_or(50);
        let mut wavey = Self {
            image: Sprite::load_native(WAVEY_PATH, palette)?,
            font: BitmapFont::load(&poco_font::POCO_SPEC)?,
            stations: load_stations(STATIONS_PATH),
            station: 0,
            volume,
            clock_24h: false,
            dragging_knob: false,
            last_clock_text: clock_text(false),
            last_clock_check: Instant::now(),
            last_volume_check: Instant::now(),
            last_title_check: Instant::now(),
            current_title: String::new(),
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
        let Some(opts) = self.mpv_property("script-opts") else {
            return;
        };

        self.playing = true;
        if let Some(station) =
            station_from_script_opts(&opts).filter(|&station| station < self.stations.len())
        {
            self.station = station;
        }
    }

    pub(crate) fn width(&self) -> usize {
        self.image.width
    }

    pub(crate) fn height(&self) -> usize {
        self.image.height + TITLE_GAP + self.font.cell_h()
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK).transparent()
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.draw_sprite(&self.image, 0, 0, 1, palette);
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

        if now.duration_since(self.last_title_check) >= TITLE_REFRESH {
            self.last_title_check = now;
            let title = if self.playing {
                self.current_mpv_title().unwrap_or_default()
            } else {
                String::new()
            };
            if title != self.current_title {
                self.current_title = title;
                dirty = true;
            }
        }

        dirty
    }

    pub(crate) fn click(&mut self, x: i16, y: i16) -> bool {
        if x < 0 || y < 0 {
            return false;
        }
        let x = x as usize;
        let y = y as usize;

        if let Some(button) = media_button_at(x, y) {
            self.press_media_button(button);
            return true;
        }

        if self.clock_contains(x, y) {
            self.clock_24h = !self.clock_24h;
            self.last_clock_text = clock_text(self.clock_24h);
            return true;
        }

        if self.knob_contains(x, y) {
            self.dragging_knob = true;
            self.set_volume_from_point(x, y, false);
            return true;
        }

        if self.tuner_contains(x, y) {
            self.station = self.station_at(x);
            self.play_station();
            return true;
        }

        false
    }

    pub(crate) fn release(&mut self) -> bool {
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

    pub(crate) fn shutdown(&mut self) {
        // The player lives in the "wavey" abduco session and keeps playing
        // across cozyui restarts; only the stop button kills it.
    }

    fn tuner_contains(&self, x: usize, y: usize) -> bool {
        (TUNER_X..TUNER_X + TUNER_W).contains(&x) && (TUNER_Y..TUNER_Y + TUNER_H).contains(&y)
    }

    fn clock_contains(&self, x: usize, y: usize) -> bool {
        (DISPLAY_X..DISPLAY_X + CLOCK_CLEAR_W).contains(&x)
            && (DISPLAY_Y..DISPLAY_Y + CLOCK_CLEAR_H).contains(&y)
    }

    fn knob_contains(&self, x: usize, y: usize) -> bool {
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
        let mut volume = (sweep / (2.0 * PI) * MAX_VOLUME as f32).round() as i16;

        if clamp_wrap {
            let delta = volume - self.volume as i16;
            if delta > 50 {
                volume = MIN_VOLUME as i16;
            } else if delta < -50 {
                volume = MAX_VOLUME as i16;
            }
        }

        self.set_volume(volume);
    }

    fn scroll_volume(&mut self, x: i16, y: i16, delta: i16) -> bool {
        if x < 0 || y < 0 || !self.knob_contains(x as usize, y as usize) {
            return false;
        }

        self.set_volume(self.volume as i16 + delta);
        true
    }

    fn set_volume(&mut self, volume: i16) {
        let volume = volume.clamp(MIN_VOLUME as i16, MAX_VOLUME as i16) as u8;
        if volume == self.volume {
            return;
        }
        self.volume = volume;
        set_system_volume(volume);
    }

    fn play_station(&mut self) {
        self.stop_player();
        let Some(station) = self.stations.get(self.station) else {
            return;
        };
        if station.mpv_args.trim().is_empty() {
            return;
        }

        let _ = fs::remove_file(mpv_ipc_path());
        self.current_title.clear();
        let command_line = format!(
            "mpv --input-terminal=no --input-ipc-server=\"$COZYUI_MPV_IPC\" \
             --script-opts=cozyui-wavey-station={} {}",
            self.station, station.mpv_args
        );
        ensure_player_session();
        queue_player_command(&command_line);
        self.playing = true;
    }

    fn current_mpv_title(&self) -> Option<String> {
        let chapter_title = self
            .mpv_property("chapter-metadata")
            .as_ref()
            .and_then(title_from_metadata);
        let media_title = self
            .mpv_property("media-title")
            .as_ref()
            .and_then(json_string);

        if let Some(chapter_title) = chapter_title {
            if let Some(media_title) = media_title.as_deref()
                && !media_title.eq_ignore_ascii_case(&chapter_title)
            {
                return clean_title(format!("{media_title} - {chapter_title}"));
            }
            return clean_title(chapter_title);
        }

        self.mpv_property("metadata")
            .as_ref()
            .and_then(track_line_from_metadata)
            .or(media_title)
            .and_then(clean_title)
    }

    fn mpv_property(&self, property: &str) -> Option<serde_json::Value> {
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
        for _ in 0..8 {
            let mut line = String::new();
            if reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let response = serde_json::from_str::<serde_json::Value>(&line).ok()?;
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

    fn draw_tuner(&self, fb: &mut Framebuffer, palette: &Palette) {
        if self.stations.is_empty() {
            return;
        }

        let text = palette.color(palette_color::LAVENDER);
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
                .draw_text(fb, &station.label, label_x, label_y, 1, text);
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
            palette.color(palette_color::ROSE),
        );
    }

    fn draw_knob(&self, fb: &mut Framebuffer, palette: &Palette) {
        clear_knob_marker(&self.image, fb, palette);
        copy_moved_knob_marker(&self.image, fb, volume_angle(self.volume), palette);
    }

    fn draw_title(&self, fb: &mut Framebuffer, palette: &Palette) {
        if self.current_title.is_empty() {
            return;
        }

        let text = fit_text(&self.font, &self.current_title, self.image.width);
        let text_w = self.font.text_width(&text);
        let x = self.image.width.saturating_sub(text_w) / 2;
        self.font.draw_text(
            fb,
            &text,
            x,
            self.image.height + TITLE_GAP,
            1,
            palette.color(palette_color::CREAM),
        );
    }
}

fn station_center(index: usize, count: usize) -> usize {
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
    let artist = ["artist", "album_artist", "albumartist"]
        .iter()
        .find_map(|key| metadata_value(object, key));
    let title = title_from_metadata(value);

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

fn clean_title(title: String) -> Option<String> {
    let title = deunicode::deunicode(&title);
    let title = title.split_whitespace().collect::<Vec<&str>>().join(" ");
    (!title.is_empty()).then_some(title)
}

fn fit_text(font: &BitmapFont, text: &str, max_width: usize) -> String {
    if font.text_width(text) <= max_width {
        return text.to_string();
    }

    let ellipsis = "...";
    let ellipsis_w = font.text_width(ellipsis);
    if ellipsis_w >= max_width {
        return String::new();
    }

    let mut fitted = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let next_w = font.advance(ch);
        if width + next_w + ellipsis_w > max_width {
            break;
        }
        fitted.push(ch);
        width += next_w;
    }
    fitted.push_str(ellipsis);
    fitted
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

fn mpv_ipc_path() -> PathBuf {
    // Fixed path so a restarted cozyui can find the mpv left running in the
    // "wavey" abduco session.
    std::env::temp_dir().join("cozyui-mpv-wavey.sock")
}

/// SIGKILL the wavey mpv (matched by its unique IPC socket argument) so the
/// session loop frees up immediately instead of waiting for a graceful quit.
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
    let runner = r#"while :; do cmd=$(cat "$COZYUI_MPV_FIFO") || exit; [ -n "$cmd" ] && eval "$cmd"; done"#;
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
    let spawned = Command::new("sh")
        .args(["-c", r#"printf '%s\n' "$COZYUI_MPV_CMD" > "$COZYUI_MPV_FIFO""#])
        .env("COZYUI_MPV_CMD", command)
        .env("COZYUI_MPV_FIFO", player_fifo_path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn volume_angle(volume: u8) -> f32 {
    FRAC_PI_2 + (volume as f32 / MAX_VOLUME as f32) * 2.0 * PI
}

fn clear_clock_art(fb: &mut Framebuffer, palette: &Palette) {
    let black = palette.color(palette_color::BLACK);
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
            if let Some(color) = palette.resolve(index, px, py) {
                fb.fill_rect(px, py, 1, 1, color);
            }
        }
    }
}

fn digit_mask(digit: char) -> Option<u8> {
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

fn clock_segment_rect(segment: usize) -> SourceRect {
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
                && let Some(color) = palette.resolve(palette_color::PLUM, x, y)
            {
                fb.fill_rect(x, y, 1, 1, color);
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
    let radius = ((marker_center_x - KNOB_X as f32).powi(2)
        + (marker_center_y - KNOB_Y as f32).powi(2))
    .sqrt();
    let dest_center_x = KNOB_X as f32 + target_angle.cos() * radius;
    let dest_center_y = KNOB_Y as f32 + target_angle.sin() * radius;
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
                && let Some(color) = palette.resolve(index, dest_x as usize, dest_y as usize)
            {
                fb.fill_rect(dest_x as usize, dest_y as usize, 1, 1, color);
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
        label: label.chars().take(5).collect(),
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
    if Command::new("wpctl")
        .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{volume}%")])
        .spawn()
        .is_ok()
    {
        return;
    }

    let _ = Command::new("pactl")
        .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{volume}%")])
        .spawn();
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
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as TimeT;
    let mut out = Tm::default();
    let result = unsafe { localtime_r(&seconds, &mut out) };
    if result.is_null() {
        return None;
    }
    Some((out.tm_hour as u8, out.tm_min as u8))
}

type TimeT = i64;

#[repr(C)]
#[derive(Default)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const i8,
}

unsafe extern "C" {
    fn localtime_r(timep: *const TimeT, result: *mut Tm) -> *mut Tm;
}

#[cfg(test)]
mod tests {
    use super::*;

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
