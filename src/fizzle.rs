use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::{Framebuffer, Index, Palette, Rect, Sprite, TRANSPARENT};


/// Tallest the candle ever gets: this many wax slices stacked between the
/// holder and the flaming top. Charge scales the slice count down to zero.
const MAX_MIDDLE: usize = 4;
/// Below this charge the stub candle is swapped for the dedicated low art.
const LOW_THRESHOLD: u8 = 15;
/// At or above this charge, a not-discharging candle shows the "full" top.
const FULL_THRESHOLD: u8 = 95;
const REFRESH: Duration = Duration::from_secs(10);

pub struct Fizzle {
    base: Sprite,
    middle: Sprite,
    top_lit: Sprite,
    top_unlit: Sprite,
    top_full: Sprite,
    low: Sprite,
    width: usize,
    height: usize,
    charge: u8,
    discharging: bool,
    last_check: Instant,
    battery_read_failing: bool,
}

impl Fizzle {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let base = crate::assets::candle_base();
        let middle = crate::assets::candle_middle();
        let top_lit = crate::assets::candle_top_lit();
        let top_unlit = crate::assets::candle_top_unlit();
        let top_full = crate::assets::candle_top_full();
        let low = crate::assets::candle_low();

        let width = base.width.max(low.width);
        // Reserve room for the tallest possible candle so shorter ones just
        // leave empty space at the top and the holder stays bottom-aligned.
        let height = base.height + MAX_MIDDLE * middle.height + top_lit.height;

        let (charge, discharging) = read_battery().unwrap_or((100, false));

        Ok(Self {
            base,
            middle,
            top_lit,
            top_unlit,
            top_full,
            low,
            width,
            height,
            charge,
            discharging,
            last_check: Instant::now(),
            battery_read_failing: false,
        })
    }

    pub(crate) const fn width(&self) -> usize {
        self.width
    }

    pub(crate) const fn height(&self) -> usize {
        self.height
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    pub(crate) fn update(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_check) < REFRESH {
            return false;
        }
        self.last_check = now;

        let Some((charge, discharging)) = read_battery() else {
            if !self.battery_read_failing {
                eprintln!("fizzle: battery sysfs reads failing, keeping last known reading");
                self.battery_read_failing = true;
            }
            return false;
        };
        if self.battery_read_failing {
            eprintln!("fizzle: battery sysfs reads recovered");
            self.battery_read_failing = false;
        }
        if charge == self.charge && discharging == self.discharging {
            return false;
        }
        self.charge = charge;
        self.discharging = discharging;
        true
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        if self.charge < LOW_THRESHOLD {
            let y = self.height.saturating_sub(self.low.height);
            fb.draw_sprite(&self.low, 0, y as isize, palette);
            return;
        }

        let base_top = (self.height - self.base.height) as isize;
        fb.draw_sprite(&self.base, 0, base_top, palette);

        // Grow the wax upward from the holder one pixel row at a time, tiling
        // the middle slice so the height interpolates perfectly smoothly.
        let mut y = base_top;
        for row in 0..self.wax_height() {
            y -= 1;
            let src_y = row % self.middle.height;
            fb.draw_sprite_region(
                &self.middle,
                Rect::new(0, src_y, self.middle.width, 1),
                0,
                y,
                palette,
            );
        }

        let top = if self.discharging {
            &self.top_lit
        } else if self.charge >= FULL_THRESHOLD {
            &self.top_full
        } else {
            &self.top_unlit
        };
        y -= top.height as isize;
        fb.draw_sprite(top, 0, y, palette);
    }

    /// Charge mapped onto the wax column height in pixels: zero at
    /// `LOW_THRESHOLD`, the full `MAX_MIDDLE` slices' worth at 100%.
    fn wax_height(&self) -> usize {
        let span = 100 - LOW_THRESHOLD as usize;
        let above = (self.charge as usize).saturating_sub(LOW_THRESHOLD as usize);
        let max_wax = MAX_MIDDLE * self.middle.height;
        (above * max_wax / span).min(max_wax)
    }
}

/// Read charge percentage and whether the battery is discharging from sysfs.
/// The device directory is cached after the first hit; the scan only reruns
/// if reading from the cached device fails (e.g. it was unplugged).
fn read_battery() -> Option<(u8, bool)> {
    static DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    let mut cached = DIR.lock().ok()?;
    if let Some(dir) = cached.as_ref()
        && let Some(reading) = read_battery_at(dir)
    {
        return Some(reading);
    }
    let dir = battery_dir()?;
    let reading = read_battery_at(&dir);
    *cached = reading.is_some().then_some(dir);
    reading
}

fn read_battery_at(dir: &std::path::Path) -> Option<(u8, bool)> {
    let capacity = fs::read_to_string(dir.join("capacity")).ok()?;
    let charge = capacity.trim().parse::<u8>().ok()?.min(100);
    let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
    let discharging = status.trim().eq_ignore_ascii_case("Discharging");
    Some((charge, discharging))
}

/// The `/sys/class/power_supply` entry for the system battery. Entries whose
/// `type` is exactly "Battery" and whose name starts with "BAT" are preferred
/// (this excludes wireless mouse/keyboard batteries, which report a capacity
/// but a different `type`); if none match, fall back to the first entry that
/// reports a charge capacity at all, so odd systems still work.
fn battery_dir() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    let paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("capacity").exists())
        .collect();

    paths
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("BAT"))
                && fs::read_to_string(path.join("type"))
                    .is_ok_and(|contents| contents.trim() == "Battery")
        })
        .or_else(|| paths.first())
        .cloned()
}

impl crate::widget::Widget for Fizzle {
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
}
