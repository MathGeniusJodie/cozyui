//! System-monitor gauges. Three analog dials — CPU, memory, and swap usage —
//! with car-speedometer needles sweeping 270 degrees from lower-left (0%) over
//! the top to lower-right (100%). The faces are placeholder circles with tick
//! marks until real sprites replace `draw_face`.

use std::error::Error;
use std::f32::consts::PI;
use std::fs;
use std::time::{Duration, Instant};

use crate::palette_color;
use crate::text::BitmapFont;
use crate::{Framebuffer, Index, Palette, TRANSPARENT};

const GAUGE_D: usize = 56;
const GAUGE_R: f32 = GAUGE_D as f32 / 2.0;
const GAUGE_GAP: usize = 8;
const GAUGE_COUNT: usize = 3;
const LABEL_GAP: usize = 3;

const LABELS: [&str; GAUGE_COUNT] = ["CPU", "MEM", "SWP"];

const NEEDLE_LEN: f32 = GAUGE_R - 6.0;

/// Needle sweep in y-down screen coordinates: 0% points lower-left, 100%
/// lower-right, sweeping clockwise over the top like a speedometer. The
/// remaining 90 degrees at the bottom is the dead zone the readout sits in.
const SWEEP_START: f32 = 3.0 * PI / 4.0;
const SWEEP: f32 = 3.0 * PI / 2.0;

const REFRESH: Duration = Duration::from_secs(1);

/// Integer usage percents; equality is the redraw test, so the needle only
/// repaints on a >=1% change.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GaugeView {
    cpu: u8,
    mem: u8,
    swap: u8,
}

pub struct Gauges {
    font: BitmapFont,
    height: usize,
    /// (total, idle) jiffies from the previous /proc/stat sample; CPU usage is
    /// computed from the delta between consecutive samples.
    cpu_prev: (u64, u64),
    view: GaugeView,
    last_check: Instant,
    cpu_read_failing: bool,
    mem_read_failing: bool,
}

impl Gauges {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let font = BitmapFont::load_with_fallback(
            &pixel_fonts::PIXOLDE_SPEC,
            &pixel_fonts::FUSION_PIXEL_8_SPEC,
        )?;
        let height = GAUGE_D + LABEL_GAP + font.cell_h();
        let cpu_prev = read_cpu_times().unwrap_or((0, 0));
        let (mem, swap) = read_mem_percents().unwrap_or((0, 0));
        Ok(Self {
            font,
            height,
            cpu_prev,
            view: GaugeView { cpu: 0, mem, swap },
            last_check: Instant::now(),
            cpu_read_failing: false,
            mem_read_failing: false,
        })
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn width(&self) -> usize {
        GAUGE_COUNT * GAUGE_D + (GAUGE_COUNT - 1) * GAUGE_GAP
    }

    pub(crate) const fn height(&self) -> usize {
        self.height
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    /// Re-sample /proc periodically; returns whether the view changed.
    pub(crate) fn update(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_check) < REFRESH {
            return false;
        }
        self.last_check = now;

        let cpu = match read_cpu_times() {
            Some(sample) => {
                if self.cpu_read_failing {
                    eprintln!("gauges: /proc/stat reads recovered");
                    self.cpu_read_failing = false;
                }
                let pct = cpu_percent(self.cpu_prev, sample).unwrap_or(self.view.cpu);
                self.cpu_prev = sample;
                pct
            }
            None => {
                if !self.cpu_read_failing {
                    eprintln!("gauges: /proc/stat reads failing, keeping last known CPU reading");
                    self.cpu_read_failing = true;
                }
                self.view.cpu
            }
        };
        let (mem, swap) = match read_mem_percents() {
            Some(pair) => {
                if self.mem_read_failing {
                    eprintln!("gauges: /proc/meminfo reads recovered");
                    self.mem_read_failing = false;
                }
                pair
            }
            None => {
                if !self.mem_read_failing {
                    eprintln!(
                        "gauges: /proc/meminfo reads failing, keeping last known mem/swap reading"
                    );
                    self.mem_read_failing = true;
                }
                (self.view.mem, self.view.swap)
            }
        };

        let view = GaugeView { cpu, mem, swap };
        if view == self.view {
            return false;
        }
        self.view = view;
        true
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let percents = [self.view.cpu, self.view.mem, self.view.swap];
        for (i, (&label, pct)) in LABELS.iter().zip(percents).enumerate() {
            let left = i * (GAUGE_D + GAUGE_GAP);
            let cx = left as f32 + GAUGE_R;
            let cy = GAUGE_R;

            draw_face(fb, cx, cy);
            draw_needle(fb, cx, cy, pct);

            // Percent readout inside the face, centered in the bottom dead
            // zone under the needle hub.
            let readout = format!("{pct}%");
            let readout_x = left + (GAUGE_D.saturating_sub(self.font.text_width(&readout))) / 2;
            let readout_y = GAUGE_D.saturating_sub(self.font.cell_h() + 4);
            self.font
                .draw_text(fb, &readout, readout_x, readout_y, palette_color::BLACK);

            let label_x = left + (GAUGE_D.saturating_sub(self.font.text_width(label))) / 2;
            self.font.draw_text(
                fb,
                label,
                label_x,
                GAUGE_D + LABEL_GAP,
                palette_color::CREAM,
            );
        }
    }
}

/// Placeholder gauge face: rim, dial, and tick marks. This is the single spot
/// the hand-drawn face sprite will replace later — swap the body for one
/// `fb.draw_sprite(...)` call and nothing else changes.
fn draw_face(fb: &mut Framebuffer, cx: f32, cy: f32) {
    let (cx_i, cy_i) = (cx as isize, cy as isize);
    crate::draw_filled_circle(fb, cx_i, cy_i, GAUGE_R as isize, palette_color::GUNMETAL);
    crate::draw_filled_circle(fb, cx_i, cy_i, GAUGE_R as isize - 2, palette_color::CREAM);

    for tick in 0..=4 {
        let theta = SWEEP_START + (tick as f32 / 4.0) * SWEEP;
        let (sin, cos) = theta.sin_cos();
        let color = if tick == 4 {
            palette_color::CRIMSON
        } else {
            palette_color::BLACK
        };
        let mut r = GAUGE_R - 6.0;
        while r <= GAUGE_R - 3.0 {
            set_px(fb, cx + r * cos, cy + r * sin, color);
            r += 0.5;
        }
    }
}

/// Draws the needle for `pct` as a tapered line from the hub out along the
/// sweep angle, with a small hub cap on top.
fn draw_needle(fb: &mut Framebuffer, cx: f32, cy: f32, pct: u8) {
    let theta = SWEEP_START + (f32::from(pct.min(100)) / 100.0) * SWEEP;
    let (sin, cos) = theta.sin_cos();
    let mut t = 0.0f32;
    while t <= NEEDLE_LEN {
        let (x, y) = (t.mul_add(cos, cx), t.mul_add(sin, cy));
        set_px(fb, x, y, palette_color::CRIMSON);
        // Thicken the inner half with a pixel perpendicular to the shaft so
        // the needle tapers toward the tip.
        if t < NEEDLE_LEN * 0.5 {
            set_px(fb, x - sin, y + cos, palette_color::CRIMSON);
        }
        t += 0.5;
    }
    crate::draw_filled_circle(fb, cx as isize, cy as isize, 2, palette_color::BLACK);
}

fn set_px(fb: &mut Framebuffer, x: f32, y: f32, color: Index) {
    let (x, y) = (x.round() as isize, y.round() as isize);
    if x >= 0 && y >= 0 {
        fb.set_pixel(x as usize, y as usize, color);
    }
}

/// (total, idle) jiffies from the aggregate first line of /proc/stat. Idle
/// includes iowait so waiting-on-disk doesn't read as load.
fn read_cpu_times() -> Option<(u64, u64)> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    cpu_times_from_line(text.lines().next()?)
}

fn cpu_times_from_line(line: &str) -> Option<(u64, u64)> {
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.parse().ok())
        .collect();
    let idle = vals.get(3)? + vals.get(4).copied().unwrap_or(0);
    Some((vals.iter().sum(), idle))
}

/// CPU usage over the interval between two (total, idle) samples. `None` when
/// no time has elapsed (caller keeps its previous reading).
fn cpu_percent(prev: (u64, u64), cur: (u64, u64)) -> Option<u8> {
    let total = cur.0.checked_sub(prev.0)?;
    if total == 0 {
        return None;
    }
    let idle = cur.1.saturating_sub(prev.1).min(total);
    Some((100 * (total - idle) / total) as u8)
}

/// (memory%, swap%) from /proc/meminfo. Swap reads 0 on swapless machines.
fn read_mem_percents() -> Option<(u8, u8)> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let total = meminfo_kb(&text, "MemTotal:")?;
    let available = meminfo_kb(&text, "MemAvailable:")?;
    let mem = percent_used(total, available);

    let swap_total = meminfo_kb(&text, "SwapTotal:")?;
    let swap_free = meminfo_kb(&text, "SwapFree:")?;
    Some((mem, percent_used(swap_total, swap_free)))
}

fn percent_used(total: u64, free: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    (100 * total.saturating_sub(free) / total) as u8
}

fn meminfo_kb(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find(|line| line.starts_with(key))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

impl crate::widget::Widget for Gauges {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meminfo_fields() {
        let text = "MemTotal:       16000000 kB\nMemAvailable:    4000000 kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n";
        assert_eq!(meminfo_kb(text, "MemTotal:"), Some(16_000_000));
        assert_eq!(meminfo_kb(text, "MemAvailable:"), Some(4_000_000));
        assert_eq!(meminfo_kb(text, "Missing:"), None);
    }

    #[test]
    fn percent_used_handles_zero_total() {
        assert_eq!(percent_used(0, 0), 0);
        assert_eq!(percent_used(16, 4), 75);
    }

    #[test]
    fn cpu_line_parses_total_and_idle() {
        // user nice system idle iowait irq softirq
        let line = "cpu  100 0 50 800 50 0 0";
        assert_eq!(cpu_times_from_line(line), Some((1000, 850)));
    }

    #[test]
    fn cpu_percent_uses_the_delta() {
        // 150 busy out of 1000 jiffies elapsed.
        assert_eq!(cpu_percent((1000, 850), (2000, 1700)), Some(15));
        // No time elapsed: keep the previous reading.
        assert_eq!(cpu_percent((2000, 1700), (2000, 1700)), None);
        // Counter went backwards (shouldn't happen): also punt.
        assert_eq!(cpu_percent((2000, 1700), (1000, 850)), None);
    }
}
