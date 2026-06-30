//! Hunger bar widget. A row of apples that depletes as the next mealtime
//! approaches: full when a meal was just eaten, empty when the next meal is
//! due. Once empty it stays empty until clicked ("eaten"), at which point it
//! refills and counts down to the following meal.

use std::error::Error;
use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::localtime;
use crate::palette_color;
use crate::pixolde_bold_font;
use crate::text::BitmapFont;
use crate::{Framebuffer, Index, Palette, Sprite, TRANSPARENT};

const APPLE_100_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/apple-100.png");
const APPLE_75_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/apple-75.png");
const APPLE_50_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/apple-50.png");
const APPLE_25_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/apple-25.png");

/// Where the last-eaten meal time is persisted so an "eaten" acknowledgement
/// survives restarts.
const STATE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/hunger.txt");

/// Number of apples in the bar; each apple has four bite levels, so the bar
/// resolves the countdown into `APPLES * 4` steps.
const APPLES: usize = 5;
const APPLE_GAP: usize = 0;
const BAR_TEXT_GAP: usize = 4;

const REFRESH: Duration = Duration::from_secs(1);

/// Eating window, in minutes from local midnight: 7:00 AM to 8:00 PM.
const WINDOW_START_MIN: i64 = 7 * 60;
const WINDOW_END_MIN: i64 = 20 * 60;
/// Five meals spread across the window (including both endpoints).
const MEALS_PER_DAY: i64 = 5;

#[derive(Clone, PartialEq, Eq)]
struct HungerView {
    /// Quarter-apple units remaining, `0..=APPLES * 4`. Zero means the meal is
    /// due and the bar is waiting to be eaten (clicked).
    fill: usize,
    label: String,
}

impl HungerView {
    /// The meal is due: the bar has emptied and is waiting for a click.
    const fn is_empty(&self) -> bool {
        self.fill == 0
    }
}

pub struct Hunger {
    apple_100: Sprite,
    apple_75: Sprite,
    apple_50: Sprite,
    apple_25: Sprite,
    font: BitmapFont,
    width: usize,
    height: usize,
    /// The meal (Unix seconds) the bar is counting down to, and the length of
    /// the countdown window. These only change when the user eats (clicks), so
    /// the per-second update derives the view from them without rescanning the
    /// meal schedule.
    target: f64,
    span: f64,
    view: HungerView,
    last_check: Instant,
}

impl Hunger {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let apple_100 = Sprite::load_native(APPLE_100_PATH, palette)?;
        let apple_75 = Sprite::load_native(APPLE_75_PATH, palette)?;
        let apple_50 = Sprite::load_native(APPLE_50_PATH, palette)?;
        let apple_25 = Sprite::load_native(APPLE_25_PATH, palette)?;
        let font = BitmapFont::load_with_fallback(
            &pixolde_bold_font::PIXOLDE_BOLD_SPEC,
            &crate::fusion_pixel_12_font::FUSION_PIXEL_12_SPEC,
        )?;

        let width = APPLES * apple_100.width + (APPLES - 1) * APPLE_GAP;
        let height = apple_100.height + BAR_TEXT_GAP + font.cell_h();

        let now = now_secs();
        // Default: assume every past meal has already been eaten, so the bar
        // simply counts down to the next upcoming meal.
        let eaten_through = load_state().unwrap_or_else(|| prev_meal(now));
        let (target, span) = meal_window(eaten_through);
        let view = render_view(target, span, now);

        Ok(Self {
            apple_100,
            apple_75,
            apple_50,
            apple_25,
            font,
            width,
            height,
            target,
            span,
            view,
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
        let view = render_view(self.target, self.span, now_secs());
        if view == self.view {
            return false;
        }
        self.view = view;
        true
    }

    /// Click to "eat". Only acts when the bar has emptied (the meal is due);
    /// refills the bar to count down to the next meal. Returns whether the
    /// widget changed and needs redrawing.
    pub(crate) fn click(&mut self) -> bool {
        if !self.view.is_empty() {
            return false;
        }
        let now = now_secs();
        let eaten_through = prev_meal(now);
        if let Err(err) = save_state(eaten_through) {
            eprintln!("hunger save failed: {err}");
        }
        (self.target, self.span) = meal_window(eaten_through);
        self.view = render_view(self.target, self.span, now);
        true
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        let apple_w = self.apple_100.width;
        for slot in 0..APPLES {
            let x = (slot * (apple_w + APPLE_GAP)) as isize;
            // The leftmost apples are eaten last: slot 0 holds the lowest
            // quarter-units, so the bar drains from right to left.
            let level = self.view.fill.saturating_sub(slot * 4).min(4);
            match level {
                4 => fb.draw_sprite(&self.apple_100, x, 0, palette),
                3 => fb.draw_sprite(&self.apple_75, x, 0, palette),
                2 => fb.draw_sprite(&self.apple_50, x, 0, palette),
                1 => fb.draw_sprite(&self.apple_25, x, 0, palette),
                // An eaten slot draws nothing — just empty space.
                _ => {}
            }
        }

        let color = if self.view.is_empty() {
            palette_color::CRIMSON
        } else {
            palette_color::CREAM
        };
        let text_x = self
            .width
            .saturating_sub(self.font.text_width(&self.view.label))
            / 2;
        let text_y = self.apple_100.height + BAR_TEXT_GAP;
        self.font
            .draw_text(fb, &self.view.label, text_x, text_y, color);
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn load_state() -> Option<f64> {
    fs::read_to_string(STATE_PATH).ok()?.trim().parse().ok()
}

fn save_state(eaten_through: f64) -> Result<(), Box<dyn Error>> {
    fs::write(STATE_PATH, format!("{eaten_through}\n"))?;
    Ok(())
}

/// Unix seconds of local midnight today, derived from the current wall clock.
/// This is always anchored on the real "now": the time-of-day fields only make
/// sense for the current instant, so callers scan a range of days around it
/// rather than asking for the midnight of some other timestamp.
fn today_midnight() -> f64 {
    let now = now_secs();
    localtime::local_time().map_or_else(
        || (now as i64 / 86400 * 86400) as f64,
        |tm| {
            let since_midnight =
                i64::from(tm.tm_hour) * 3600 + i64::from(tm.tm_min) * 60 + i64::from(tm.tm_sec);
            (now as i64 - since_midnight) as f64
        },
    )
}

/// All meal times (Unix seconds) for the day `day_offset` days from today.
fn meals_for_day(midnight: f64, day_offset: i64) -> impl Iterator<Item = f64> {
    let day = midnight + (day_offset * 86400) as f64;
    let step = (WINDOW_END_MIN - WINDOW_START_MIN) / (MEALS_PER_DAY - 1);
    (0..MEALS_PER_DAY).map(move |i| day + ((WINDOW_START_MIN + i * step) * 60) as f64)
}

/// The earliest meal time strictly after `t`.
fn next_meal(t: f64) -> f64 {
    let midnight = today_midnight();
    (-2..=2)
        .flat_map(|d| meals_for_day(midnight, d))
        .find(|&m| m > t)
        .unwrap_or(t)
}

/// The latest meal time at or before `t`. Meal times are generated in
/// ascending order, so the last one passing the filter is the latest.
fn prev_meal(t: f64) -> f64 {
    let midnight = today_midnight();
    (-2..=2)
        .flat_map(|d| meals_for_day(midnight, d))
        .filter(|&m| m <= t)
        .last()
        .unwrap_or(t)
}

/// The meal the bar counts down to after `eaten_through`, and the length of
/// that countdown window.
fn meal_window(eaten_through: f64) -> (f64, f64) {
    let target = next_meal(eaten_through);
    (target, (target - eaten_through).max(1.0))
}

/// The bar's view for the current instant given its cached countdown window.
fn render_view(target: f64, span: f64, now: f64) -> HungerView {
    let remaining = target - now;
    if remaining <= 0.0 {
        return HungerView {
            fill: 0,
            label: "EAT!".to_string(),
        };
    }

    let fraction = (remaining / span).clamp(0.0, 1.0);
    // Round up so any time remaining keeps at least one bite on the bar.
    let fill = (fraction * (APPLES * 4) as f64).ceil() as usize;

    HungerView {
        fill,
        label: fmt_countdown(remaining),
    }
}

fn fmt_countdown(secs: f64) -> String {
    let total = secs.max(0.0) as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: f64 = 86400.0;

    /// Build a fixed Unix-day midnight (UTC) for deterministic meal math by
    /// going through the same path the widget uses.
    fn meals(midnight: f64) -> Vec<f64> {
        meals_for_day(midnight, 0).collect()
    }

    #[test]
    fn five_meals_span_the_window() {
        let m = meals(0.0);
        assert_eq!(m.len(), 5);
        assert_eq!(m[0], (7 * 3600) as f64); // 7:00
        assert_eq!(m[4], (20 * 3600) as f64); // 20:00
        // Evenly spaced 3h15m apart.
        assert_eq!(m[1] - m[0], 3.25 * 3600.0);
    }

    #[test]
    fn full_just_after_a_meal_empty_at_the_next() {
        // 1s after the first meal: bar is essentially full.
        let midnight = today_midnight();
        let first = meals_for_day(midnight, 0).next().unwrap();
        let (target, span) = meal_window(first);
        let view = render_view(target, span, first + 1.0);
        assert!(!view.is_empty());
        assert_eq!(view.fill, APPLES * 4);

        // At the next meal time the bar is empty and prompts to eat.
        let empty = render_view(target, span, target);
        assert!(empty.is_empty());
        assert_eq!(empty.fill, 0);
        assert_eq!(empty.label, "EAT!");
    }

    #[test]
    fn next_and_prev_meal_bracket_now() {
        let now = now_secs();
        assert!(prev_meal(now) <= now);
        assert!(next_meal(now) > now);
        assert!(next_meal(now) - prev_meal(now) <= DAY);
    }

    #[test]
    fn countdown_formats_hours_and_minutes() {
        assert_eq!(fmt_countdown(3661.0), "1:01:01");
        assert_eq!(fmt_countdown(125.0), "2:05");
    }

    #[test]
    fn meal_math_is_consistent_for_non_now_timestamps() {
        // Regression: next_meal/prev_meal must work when called with a meal
        // timestamp rather than the current instant. The two meals either side
        // of a meal time should be a clean window apart, not garbage.
        let midnight = today_midnight();
        let meal = meals_for_day(midnight, 0).nth(1).unwrap(); // 10:15 today
        assert_eq!(prev_meal(meal), meal);
        let after = next_meal(meal);
        assert_eq!(after - meal, 3.25 * 3600.0);
    }
}
