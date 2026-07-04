//! Hunger bar widget. A row of apples that depletes as the next mealtime
//! approaches: full when a meal was just eaten, empty when the next meal is
//! due. Once empty it stays empty until clicked ("eaten"), at which point it
//! refills and counts down to the following meal.

use std::error::Error;
use std::fs;
use std::time::Duration;

use crate::localtime;
use crate::palette_color;
use crate::text::BitmapFont;
use crate::{Framebuffer, Index, Palette, Sprite, TRANSPARENT};

/// Where the last-eaten meal time is persisted so an "eaten" acknowledgement
/// survives restarts.
const STATE_FILE: &str = "hunger.txt";

fn state_path() -> String {
    crate::paths::config_file(STATE_FILE)
}

/// Number of apples in the bar; each apple has four bite levels, so the bar
/// resolves the countdown into `APPLES * 4` steps.
const APPLES: usize = 5;
const APPLE_GAP: usize = 0;
const BAR_TEXT_GAP: usize = 5;

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
    view: crate::util::Refresh<HungerView>,
}

impl Hunger {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let apple_100 = crate::assets::apple_100();
        let apple_75 = crate::assets::apple_75();
        let apple_50 = crate::assets::apple_50();
        let apple_25 = crate::assets::apple_25();
        let font = BitmapFont::load_with_fallback(
            &pixel_fonts::PIXOLDE_BOLD_SPEC,
            &pixel_fonts::FUSION_PIXEL_12_SPEC,
        )?;

        let width = APPLES * apple_100.width + (APPLES - 1) * APPLE_GAP;
        let height = apple_100.height + BAR_TEXT_GAP + font.cell_h();

        let now = crate::util::now_secs();
        // Default: assume every past meal has already been eaten, so the bar
        // simply counts down to the next upcoming meal. A persisted timestamp
        // from the future (e.g. saved under a skewed clock that was since
        // corrected) can't reflect a meal that's actually been eaten yet, and
        // would fall outside next_meal's search window, so it's rejected the
        // same as a missing/corrupt file.
        let eaten_through = load_state()
            .filter(|&value| value <= now)
            .unwrap_or_else(|| prev_meal(now));
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
            view: crate::util::Refresh::new(view),
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
        let (target, span) = (self.target, self.span);
        self.view.refresh(REFRESH, || {
            render_view(target, span, crate::util::now_secs())
        })
    }

    /// Click to "eat". Only acts when the bar has emptied (the meal is due);
    /// refills the bar to count down to the next meal. Returns whether the
    /// widget changed and needs redrawing.
    pub(crate) fn click(&mut self) -> bool {
        if !self.view.get().is_empty() {
            return false;
        }
        let now = crate::util::now_secs();
        let eaten_through = prev_meal(now);
        if let Err(err) = save_state(eaten_through) {
            eprintln!("hunger save failed: {err}");
            return false;
        }
        (self.target, self.span) = meal_window(eaten_through);
        self.view.set(render_view(self.target, self.span, now));
        true
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        let view = self.view.get();
        let apple_w = self.apple_100.width;
        for slot in 0..APPLES {
            let x = (slot * (apple_w + APPLE_GAP)) as isize;
            // The leftmost apples are eaten last: slot 0 holds the lowest
            // quarter-units, so the bar drains from right to left.
            let level = view.fill.saturating_sub(slot * 4).min(4);
            match level {
                4 => fb.draw_sprite(&self.apple_100, x, 0, palette),
                3 => fb.draw_sprite(&self.apple_75, x, 0, palette),
                2 => fb.draw_sprite(&self.apple_50, x, 0, palette),
                1 => fb.draw_sprite(&self.apple_25, x, 0, palette),
                // An eaten slot draws nothing — just empty space.
                _ => {}
            }
        }

        let color = if view.is_empty() {
            palette_color::CRIMSON
        } else {
            palette_color::CREAM
        };
        let text_x = self.width.saturating_sub(self.font.text_width(&view.label)) / 2;
        let text_y = self.apple_100.height + BAR_TEXT_GAP;
        self.font.draw_text(fb, &view.label, text_x, text_y, color);
    }
}

fn load_state() -> Option<f64> {
    match fs::read_to_string(state_path()) {
        Ok(text) => match text.trim().parse().ok() {
            Some(value) => Some(value),
            None => {
                eprintln!("hunger: state file exists but failed to parse; treating as first run");
                None
            }
        },
        Err(_) => None,
    }
}

fn save_state(eaten_through: f64) -> Result<(), Box<dyn Error>> {
    crate::util::atomic_write(&state_path(), format!("{eaten_through}\n"))?;
    Ok(())
}

/// Unix seconds of local midnight today, resolved via `mktime` so the
/// boundary lands on true local civil midnight even on a day DST starts or
/// ends, rather than drifting by the DST offset delta like naive epoch
/// arithmetic would. Falls back to a UTC day-aligned instant if local time
/// is unavailable. Only used by tests below; production code goes through
/// [`civil_day_midnight`] and [`meals_for_day`] directly.
#[cfg(test)]
fn today_midnight() -> f64 {
    let now = crate::util::now_secs();
    civil_day_midnight(0).unwrap_or_else(|| (now as i64 / 86400 * 86400) as f64)
}

/// Local midnight `day_offset` days from today. Built by taking today's
/// civil date (year/month/day) from [`localtime::local_time`] and shifting
/// `tm_mday` by `day_offset` directly, then resolving to epoch seconds via
/// [`localtime::epoch_for_civil`] (`mktime` normalizes an out-of-range
/// `mday`, e.g. day 32, by rolling into the next month, so this is safe).
/// This anchors day boundaries to true local civil time instead of adding
/// `day_offset * 86400` seconds, which drifts across a DST transition.
/// `None` if local time is unavailable.
#[cfg(test)]
fn civil_day_midnight(day_offset: i64) -> Option<f64> {
    let tm = localtime::local_time()?;
    localtime::epoch_for_civil(
        tm.tm_year + 1900,
        tm.tm_mon,
        tm.tm_mday + day_offset as i32,
        0,
        0,
        0,
    )
    .map(|epoch| epoch as f64)
}

/// All meal times (Unix seconds) for the day `day_offset` days from today.
/// Each meal's wall-clock minutes resolve through `mktime`, so 7:00 means
/// 7:00 on the local clock even on the day a DST transition inserts or
/// removes an hour before the window opens (adding fixed seconds to
/// midnight would shift every meal by the DST delta on that day).
fn meals_for_day(day_offset: i64) -> impl Iterator<Item = f64> {
    let tm = localtime::local_time();
    let step = (WINDOW_END_MIN - WINDOW_START_MIN) / (MEALS_PER_DAY - 1);
    (0..MEALS_PER_DAY).map(move |i| {
        let minutes = WINDOW_START_MIN + i * step;
        tm.as_ref()
            .and_then(|tm| {
                localtime::epoch_for_civil(
                    tm.tm_year + 1900,
                    tm.tm_mon,
                    tm.tm_mday + day_offset as i32,
                    (minutes / 60) as i32,
                    (minutes % 60) as i32,
                    0,
                )
            })
            .map_or_else(
                || {
                    let day = crate::util::now_secs() as i64 / 86400 * 86400 + day_offset * 86400;
                    (day + minutes * 60) as f64
                },
                |epoch| epoch as f64,
            )
    })
}

/// The earliest meal time strictly after `t`.
fn next_meal(t: f64) -> f64 {
    (-2..=2)
        .flat_map(meals_for_day)
        .find(|&m| m > t)
        .unwrap_or(t)
}

/// The latest meal time at or before `t`. Meal times are generated in
/// ascending order, so the last one passing the filter is the latest.
fn prev_meal(t: f64) -> f64 {
    (-2..=2)
        .flat_map(meals_for_day)
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

impl crate::widget::Widget for Hunger {
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
        _x: i16,
        _y: i16,
        _state: u16,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        Self::click(self);
        Ok(crate::widget::ClickOutcome::default())
    }

    // Clicking anywhere logs a meal.
    fn cursor_at(&self, _x: i16, _y: i16) -> crate::CursorKind {
        crate::CursorKind::Hand
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: f64 = 86400.0;

    /// Today's meal times, going through the same path the widget uses.
    fn meals() -> Vec<f64> {
        meals_for_day(0).collect()
    }

    #[test]
    fn five_meals_span_the_window() {
        let midnight = today_midnight();
        let m = meals();
        assert_eq!(m.len(), 5);
        assert_eq!(m[0], midnight + (7 * 3600) as f64); // 7:00
        assert_eq!(m[4], midnight + (20 * 3600) as f64); // 20:00
        // Evenly spaced 3h15m apart.
        assert_eq!(m[1] - m[0], 3.25 * 3600.0);
    }

    #[test]
    fn full_just_after_a_meal_empty_at_the_next() {
        // 1s after the first meal: bar is essentially full.
        let first = meals_for_day(0).next().unwrap();
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
        let now = crate::util::now_secs();
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
        let meal = meals_for_day(0).nth(1).unwrap(); // 10:15 today
        assert_eq!(prev_meal(meal), meal);
        let after = next_meal(meal);
        assert_eq!(after - meal, 3.25 * 3600.0);
    }

    #[test]
    fn future_eaten_through_breaks_meal_window_without_guarding() {
        // Regression / documentation: next_meal only searches a window of
        // roughly [now - 2 days, now + 2 days] because meals_for_day is
        // anchored to *today*, not to the timestamp passed in. A persisted
        // "eaten through" value from beyond that window (e.g. a future
        // timestamp saved under a skewed clock) finds no meal greater than
        // itself, so next_meal falls back to returning the input unchanged.
        // That collapses meal_window's span to the 1-second floor, which is
        // exactly the degenerate case Hunger::load must avoid by rejecting
        // future timestamps before they ever reach meal_window.
        let now = crate::util::now_secs();
        let implausible_future = now + 30.0 * DAY;
        assert_eq!(next_meal(implausible_future), implausible_future);
        let (target, span) = meal_window(implausible_future);
        assert_eq!(target, implausible_future);
        assert_eq!(span, 1.0);
    }
}
