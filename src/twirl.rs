use std::error::Error;
use std::f32::consts::TAU;
use std::fs;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app_color;
use crate::bitmap_font::BitmapFont;
use crate::comicoro_font;
use crate::palette_color;
use crate::{Framebuffer, Index, Paint, Palette, Rgba, Sprite, TRANSPARENT};

const WHEEL_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/wheel.png");
const TOTAL_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/twirl_total.txt");
const SHADOW_X_OFFSET: isize = 1;
const SHADOW_Y_OFFSET: isize = 4;

const SEGMENT_COUNT: usize = 6;
const SEGMENT_NUMBERS: [&str; SEGMENT_COUNT] = [
    "1", "2", "4", "8", "16", "100"
];

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
const FRICTION_PER_SECOND: f32 = 0.78;
const STOP_SPEED: f32 = 0.28;
const NUMBER_RADIUS: f32 = 55.0;
const POINTER_ANGLE: f32 = -std::f32::consts::FRAC_PI_2;
const CLICK_SAMPLE_RATE: u32 = 22_050;
const CLICK_DURATION_SECONDS: f32 = 0.012;
const CLICK_VOLUME: f32 = 1200.0;
const TOTAL_GAP: usize = 4;

pub(crate) struct Twirl {
    wheel: Sprite,
    font: BitmapFont,
    angle: f32,
    speed: f32,
    last_update: Instant,
    last_click_segment: usize,
    total: u64,
}

impl Twirl {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            wheel: Sprite::load_native(WHEEL_PATH, palette)?,
            font: BitmapFont::load(&comicoro_font::COMICORO_SPEC)?,
            angle: 0.0,
            speed: 0.0,
            last_update: Instant::now(),
            last_click_segment: 0,
            total: load_total(TOTAL_PATH)?,
        })
    }

    pub(crate) fn width(&self) -> usize {
        self.wheel.width + SHADOW_X_OFFSET as usize
    }

    pub(crate) fn height(&self) -> usize {
        self.wheel.height + TOTAL_GAP + self.font.cell_h() + SHADOW_Y_OFFSET as usize
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK).transparent()
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.draw_sprite_silhouette(
            &self.wheel,
            SHADOW_X_OFFSET,
            SHADOW_Y_OFFSET,
            1,
            palette,
            Paint::Solid(app_color::BACKGROUND_SHADOW),
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
                if let Some(color) = palette.resolve(index, x, y) {
                    fb.set_pixel(x, y, color);
                }
            }
        }

        self.draw_numbers(fb, palette);
        self.draw_total(fb, palette);
    }

    pub(crate) fn update(&mut self) -> Result<bool, Box<dyn Error>> {
        let now = Instant::now();
        let dt = now.duration_since(self.last_update);
        self.last_update = now;

        if self.speed <= 0.0 {
            return Ok(false);
        }

        let previous_segment = self.pointer_segment();
        self.angle = normalize_angle(self.angle + self.speed * dt.as_secs_f32());
        self.speed *= FRICTION_PER_SECOND.powf(dt.as_secs_f32());

        let current_segment = self.pointer_segment();
        if current_segment != previous_segment && current_segment != self.last_click_segment {
            self.last_click_segment = current_segment;
            play_click();
        }

        if self.speed < STOP_SPEED {
            self.speed = 0.0;
            self.add_landed_value()?;
            play_jingle();
        }

        Ok(true)
    }

    pub(crate) fn click(&mut self, x: i16, y: i16) {
        if x < 0 || y < 0 {
            return;
        }
        let x = x as usize;
        let y = y as usize;
        if x >= self.width() || y >= self.height() {
            return;
        }

        let center_x = self.wheel.width as f32 / 2.0;
        let center_y = self.wheel.height as f32 / 2.0;
        let dx = x as f32 + 0.5 - center_x;
        let dy = y as f32 + 0.5 - center_y;
        if dx * dx + dy * dy > center_x.min(center_y).powi(2) {
            return;
        }

        self.spin();
    }

    pub(crate) fn spin(&mut self) {
        self.speed = START_SPEED_MIN + random_unit() * START_SPEED_RANGE;
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

    fn draw_numbers(&self, fb: &mut Framebuffer, palette: &Palette) {
        let center_x = self.wheel.width as f32 / 2.0;
        let center_y = self.wheel.height as f32 / 2.0;
        let color = palette.color(palette_color::BLACK);
        for (segment, number) in SEGMENT_NUMBERS.iter().enumerate() {
            let angle = segment_center_angle(segment) - self.angle;
            let text_w = self.font.text_width(number);
            let text_h = self.font.cell_h();
            let x = center_x + angle.cos() * NUMBER_RADIUS - text_w as f32 / 2.0;
            let y = center_y + angle.sin() * NUMBER_RADIUS - text_h as f32 / 2.0;
            self.font.draw_text(
                fb,
                number,
                x.max(0.0) as usize,
                y.max(0.0) as usize,
                1,
                color,
            );
        }
    }

    fn draw_total(&self, fb: &mut Framebuffer, palette: &Palette) {
        let total = self.total.to_string();
        let text_w = self.font.text_width(&total);
        let x = self.wheel.width.saturating_sub(text_w) / 2;
        let y = self.wheel.height + TOTAL_GAP;
        self.font
            .draw_text(fb, &total, x, y, 1, palette.color(palette_color::CREAM));
    }

    fn add_landed_value(&mut self) -> Result<(), Box<dyn Error>> {
        self.total = self
            .total
            .saturating_add(segment_value(self.pointer_segment())?);
        save_total(TOTAL_PATH, self.total)
    }

    fn pointer_segment(&self) -> usize {
        segment_for_angle(POINTER_ANGLE + self.angle)
    }

    fn segment_at(&self, x: usize, y: usize) -> usize {
        let center_x = self.wheel.width as f32 / 2.0;
        let center_y = self.wheel.height as f32 / 2.0;
        let dx = x as f32 + 0.5 - center_x;
        let dy = y as f32 + 0.5 - center_y;
        segment_for_angle(dy.atan2(dx) + self.angle)
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

fn random_unit() -> f32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .subsec_nanos();
    (nanos % 10_000) as f32 / 10_000.0
}

fn segment_value(segment: usize) -> Result<u64, Box<dyn Error>> {
    Ok(SEGMENT_NUMBERS[segment].parse()?)
}

fn load_total(path: &str) -> Result<u64, Box<dyn Error>> {
    if !std::path::Path::new(path).exists() {
        return Ok(0);
    }

    let text = fs::read_to_string(path)?;
    Ok(text.trim().parse()?)
}

fn save_total(path: &str, total: u64) -> Result<(), Box<dyn Error>> {
    fs::write(path, format!("{total}\n"))?;
    Ok(())
}

fn play_click() {
    play_wav("cozyui-twirl-click.wav", synth_click());
}

fn play_jingle() {
    play_wav("cozyui-twirl-jingle.wav", synth_jingle());
}

fn play_wav(name: &str, samples: Vec<i16>) {
    let path = std::env::temp_dir().join(name);
    if fs::write(&path, wav_bytes(&samples)).is_err() {
        return;
    }

    for player in AUDIO_PLAYERS {
        let mut command = Command::new(player.command);
        for arg in player.args {
            command.arg(arg);
        }
        command.arg(&path);
        if command.spawn().is_ok() {
            break;
        }
    }
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
            let sample = ((noise >> 16) as f32 / 32_768.0) * 2.0 - 1.0;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_for_angle_wraps_around() {
        assert_eq!(segment_for_angle(0.0), 0);
        assert_eq!(segment_for_angle(TAU), 0);
        assert_eq!(segment_for_angle(-0.01), SEGMENT_COUNT - 1);
    }

    #[test]
    fn wav_bytes_writes_mono_pcm_header() {
        let bytes = wav_bytes(&[0, 1, -1]);

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 50);
    }

    #[test]
    fn segment_value_uses_customizable_numbers() {
        assert_eq!(segment_value(0).unwrap(), 1);
        assert_eq!(segment_value(SEGMENT_COUNT - 1).unwrap(), 100);
    }
}
