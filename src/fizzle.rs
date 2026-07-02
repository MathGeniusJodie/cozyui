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
            return false;
        };
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
fn read_battery() -> Option<(u8, bool)> {
    let dir = battery_dir()?;
    let capacity = fs::read_to_string(dir.join("capacity")).ok()?;
    let charge = capacity.trim().parse::<u8>().ok()?.min(100);
    let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
    let discharging = status.trim().eq_ignore_ascii_case("Discharging");
    Some((charge, discharging))
}

/// First `/sys/class/power_supply` entry that reports a charge capacity.
fn battery_dir() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("capacity").exists())
}
