use std::error::Error;
use std::time::Duration;

use crate::app_color;
use crate::localtime::{self, local_time};
use crate::palette_color;
use crate::text::{BitmapFont, draw_text_centered, draw_text_centered_tight};
use crate::{Caps, Framebuffer, Index, Palette, Rect, Sprite, TRANSPARENT, draw_filled_circle};

// The background art (`assets/days.png`) is natively `BASE_WIDTH` wide; the
// calendar view needs one extra column to the left of the day grid for week
// numbers, so the card is drawn wider than the art via 9-slice stretching
// (see `BG_*_CAP` below) rather than shipping a second, wider PNG.
const BASE_WIDTH: usize = 116;
// One grid-column's worth of width for the week-number digits (reuses
// `CALENDAR_COL_W`, the same width the day grid's own columns use), plus the
// calendar font's "w" glyph (6px, measured via `BitmapFont::text_width`) for
// the "w" prefix (e.g. "w27"), plus 4px more of breathing room.
pub(crate) const WEEK_COL_W: usize = CALENDAR_COL_W + 6 + 4;
const WIDTH: usize = BASE_WIDTH + WEEK_COL_W;
const HEIGHT: usize = 116;
// 9-slice caps for `background`: kept clear of the header's two rivets
// (native x 15-22 and 90-97) and the right-edge stacked-page ridge (native x
// 105-115), so stretching the card only widens the flat middle, not those
// details. Top/bottom caps are unused in practice since the card never grows
// taller, but must stay within the art's height.
const BG_LEFT_CAP: usize = 24;
const BG_RIGHT_CAP: usize = 26;
const BG_TOP_CAP: usize = 25;
const BG_BOTTOM_CAP: usize = 20;
// Where the plain (non-calendar) card's left edge sits within the widget's
// frame: inset by the week-number column's width so its right edge lines up
// with the calendar view's (which spans the full, wider `WIDTH`) without the
// widget's footprint needing to change size when `calendar_mode` toggles.
const PLAIN_CARD_X: usize = WEEK_COL_W;
const SHADOW_X_OFFSET: usize = 1;
const SHADOW_Y_OFFSET: usize = 4;
const DATE_REFRESH: Duration = Duration::from_secs(60);
const TOP_GAP: usize = 10;
const LABEL_GAP: usize = 26;
const NUMBER_GAP: usize = 6;
const MONTH_GAP: usize = 6;
const CALENDAR_TITLE_Y: isize = 11;
const CALENDAR_WEEKDAY_Y: isize = 34;
const CALENDAR_GRID_Y: isize = 47;
const CALENDAR_COL_W: usize = 13;
const CALENDAR_ROW_H: usize = 12;
const WEEK_NUM_LEFT: usize = 12;
const CALENDAR_LEFT: usize = WEEK_NUM_LEFT + WEEK_COL_W;
// A month can span at most 6 grid rows (e.g. a 31-day month starting on
// Saturday). `calendar_row_week` bounds-checks each row against the month's
// actual length, so iterating this fixed upper bound can't over- or
// under-run the grid.
const MAX_CALENDAR_ROWS: usize = 6;
const TODAY_CIRCLE_X_OFFSET: isize = -1;
const TODAY_CIRCLE_Y_OFFSET: isize = 0;
const TODAY_CIRCLE_RADIUS: isize = 6;
const WEEKDAY_LABELS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

const WEEKDAYS: [&str; 7] = [
    "SUNDAY",
    "MONDAY",
    "TUESDAY",
    "WEDNESDAY",
    "THURSDAY",
    "FRIDAY",
    "SATURDAY",
];
const MONTHS: [&str; 12] = [
    "JANUARY",
    "FEBRUARY",
    "MARCH",
    "APRIL",
    "MAY",
    "JUNE",
    "JULY",
    "AUGUST",
    "SEPTEMBER",
    "OCTOBER",
    "NOVEMBER",
    "DECEMBER",
];

#[derive(Clone, PartialEq, Eq)]
struct DateParts {
    year: String,
    weekday: String,
    day: String,
    month: String,
    year_num: i32,
    month_index: usize,
    day_num: i32,
}

/// Whether the wall clock has been read successfully at least once.
/// `Unknown` only occurs right at startup, if the very first `localtime_r`
/// call fails (afterward `Day::update` always keeps the previous `Known` date
/// rather than overwrite it — see its doc comment). Keeping this as an enum,
/// instead of a sentinel `DateParts` with deliberately bogus numeric fields
/// (year 0, etc.), means `render`/`render_calendar` can't accidentally treat
/// placeholder numbers as a real date: there are no numeric fields to read at
/// all in the `Unknown` case.
#[derive(Clone, PartialEq, Eq)]
enum DateState {
    Known(DateParts),
    Unknown,
}

pub struct Day {
    background: Sprite,
    label_font: BitmapFont,
    number_font: BitmapFont,
    calendar_font: BitmapFont,
    date: DateState,
    throttle: crate::util::Throttle,
    calendar_mode: bool,
}

impl Day {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            background: crate::assets::days(),
            label_font: BitmapFont::load_with_fallback(
                &pixel_fonts::PIXOLDE_BOLD_SPEC,
                &pixel_fonts::FUSION_PIXEL_12_SPEC,
            )?,
            number_font: BitmapFont::load(&pixel_fonts::ROZHA_ONE_48_SPEC)?,
            calendar_font: BitmapFont::load_with_fallback(
                &pixel_fonts::POCO_SPEC,
                &pixel_fonts::FUSION_PIXEL_8_SPEC,
            )?,
            // No previous date to fall back to at startup, so show a clearly
            // invalid placeholder (see `DateState::Unknown`) rather than a
            // plausible-looking wrong date.
            date: current_date_parts().map_or(DateState::Unknown, DateState::Known),
            throttle: crate::util::Throttle::new(),
            calendar_mode: false,
        })
    }

    /// Left edge and width of whichever card is actually on screen: the
    /// full, 9-sliced `WIDTH` in calendar mode (the week-number column only
    /// exists there), or the plain, unstretched `BASE_WIDTH` card (inset by
    /// `PLAIN_CARD_X`) in single-date mode, so both modes' cards share the
    /// same right edge and the widget's on-screen footprint never has to
    /// change size when `calendar_mode` toggles. `render` and the text-drawing
    /// helpers that center within this card both key off this single source
    /// of truth so they can't drift apart on which card is drawn where.
    const fn card_geometry(&self) -> (usize, usize) {
        if self.calendar_mode {
            (0, WIDTH)
        } else {
            (PLAIN_CARD_X, BASE_WIDTH)
        }
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        let (card_x, card_w) = self.card_geometry();
        let card_x = card_x as isize;
        let caps = Caps::new(BG_LEFT_CAP, BG_RIGHT_CAP, BG_TOP_CAP, BG_BOTTOM_CAP);
        fb.draw_resized_silhouette(
            &self.background,
            Rect::new(
                card_x + SHADOW_X_OFFSET as isize,
                SHADOW_Y_OFFSET as isize,
                card_w,
                HEIGHT,
            ),
            caps,
            palette,
            app_color::BACKGROUND_SHADOW_PAINT,
        );
        fb.draw_resized(
            &self.background,
            Rect::new(card_x, 0, card_w, HEIGHT),
            caps,
            palette,
        );

        match &self.date {
            DateState::Known(date) if self.calendar_mode => self.render_calendar(fb, palette, date),
            DateState::Known(date) => self.render_day(fb, palette, date),
            // No numeric fields exist to feed a calendar grid or single-date
            // view, so both modes fall back to the same "?" placeholder text.
            DateState::Unknown => self.render_day_text(fb, "????", "?", "?", "?"),
        }
    }

    pub(crate) const fn toggle_mode(&mut self) {
        self.calendar_mode = !self.calendar_mode;
    }

    fn render_day(&self, fb: &mut Framebuffer, _palette: &Palette, date: &DateParts) {
        self.render_day_text(fb, &date.year, &date.weekday, &date.day, &date.month);
    }

    /// Draws the single-date view's four text lines directly, used both by
    /// `render_day` (with a real date's fields) and by `render` for the
    /// `DateState::Unknown` placeholder (with literal "?" strings) — so the
    /// placeholder never has to fake numeric `DateParts` fields just to reuse
    /// this drawing code.
    fn render_day_text(
        &self,
        fb: &mut Framebuffer,
        year: &str,
        weekday: &str,
        day: &str,
        month: &str,
    ) {
        let black = palette_color::BLACK;
        let cream = palette_color::CREAM;
        let crimson = palette_color::CRIMSON;
        let purple = palette_color::PURPLE;
        let rose = palette_color::ROSE;
        let (card_x, card_w) = self.card_geometry();
        let card_x = card_x as isize;
        let year_h = self.tight_height(&self.label_font, year);
        let weekday_h = self.tight_height(&self.label_font, weekday);
        let day_h = self.tight_height(&self.number_font, day);
        let mut y = TOP_GAP;

        draw_text_centered_tight(
            fb,
            &self.label_font,
            year,
            card_x,
            card_w,
            y as isize - 1,
            purple,
        );
        draw_text_centered_tight(
            fb,
            &self.label_font,
            year,
            card_x,
            card_w,
            y as isize + 1,
            rose,
        );
        draw_text_centered_tight(fb, &self.label_font, year, card_x, card_w, y as isize, cream);
        y += year_h + LABEL_GAP;
        draw_text_centered_tight(
            fb,
            &self.label_font,
            weekday,
            card_x,
            card_w,
            y as isize,
            black,
        );
        y += weekday_h + NUMBER_GAP;
        draw_text_centered_tight(
            fb,
            &self.number_font,
            day,
            card_x,
            card_w,
            y as isize,
            crimson,
        );
        y += day_h + MONTH_GAP;
        draw_text_centered_tight(
            fb,
            &self.label_font,
            month,
            card_x,
            card_w,
            y as isize,
            black,
        );
    }

    fn render_calendar(&self, fb: &mut Framebuffer, _palette: &Palette, date: &DateParts) {
        let black = palette_color::BLACK;
        let cream = palette_color::CREAM;
        let crimson = palette_color::CRIMSON;
        let title = format!("{} {}", short_month_name(date.month_index), date.year_num);
        let (card_x, card_w) = self.card_geometry();

        draw_text_centered(
            fb,
            &self.calendar_font,
            &title,
            card_x as isize,
            card_w,
            CALENDAR_TITLE_Y,
            cream,
        );

        for (weekday, label) in WEEKDAY_LABELS.iter().enumerate() {
            self.draw_calendar_cell(fb, label, weekday, CALENDAR_WEEKDAY_Y, black);
        }

        let first_day_epoch =
            localtime::days_from_civil(date.year_num, date.month_index as i32 + 1, 1);
        let first_weekday = weekday_of(first_day_epoch);
        let days = localtime::days_in_month(date.year_num, date.month_index as i32 + 1);
        for day in 1..=days {
            let index = first_weekday + day as usize - 1;
            let col = index % 7;
            let row = index / 7;
            let y = CALENDAR_GRID_Y + row as isize * CALENDAR_ROW_H as isize;
            let color = if day == date.day_num { cream } else { black };
            if day == date.day_num {
                let (center_x, center_y) = self.calendar_cell_center(col, y);
                draw_filled_circle(fb, center_x, center_y, TODAY_CIRCLE_RADIUS, crimson);
            }
            self.draw_calendar_cell(fb, &day.to_string(), col, y, color);
        }

        // One ISO week number per grid row, to the left of the week it
        // labels. Grid rows run Sunday-Saturday but ISO weeks run
        // Monday-Sunday, so each row's number is taken from its Monday
        // (column 1) rather than its Sunday, which actually belongs to the
        // previous row's ISO week. A row whose Monday falls past the end of
        // the month (a trailing lone Sunday, when the month's last day is
        // itself a Sunday) has no week of its own to show: its one visible
        // day already belongs to the week shown on the row above, so
        // `calendar_row_week` returns `None` and it's skipped rather than
        // mislabeled with the following month's week.
        for row in 0..MAX_CALENDAR_ROWS {
            if let Some(week) = calendar_row_week(first_day_epoch, days, row) {
                let y = CALENDAR_GRID_Y + row as isize * CALENDAR_ROW_H as isize;
                self.draw_week_number(fb, week, y, crimson);
            }
        }
    }

    pub(crate) fn update(&mut self) -> bool {
        if !self.throttle.ready(DATE_REFRESH) {
            return false;
        }

        // On failure, keep showing the previously displayed date rather than
        // overwrite it with a wrong one.
        let Some(date) = current_date_parts() else {
            return false;
        };
        let date = DateState::Known(date);
        if date == self.date {
            return false;
        }

        self.date = date;
        true
    }

    #[allow(clippy::unused_self)]
    fn tight_height(&self, font: &BitmapFont, text: &str) -> usize {
        font.text_ink_bounds(text)
            .map_or_else(|| font.cell_h(), |bounds| bounds.height())
    }

    fn draw_calendar_cell(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        col: usize,
        y: isize,
        color: Index,
    ) {
        let cell_x = (CALENDAR_LEFT + col * CALENDAR_COL_W) as isize;
        self.draw_cell_text(fb, text, cell_x, CALENDAR_COL_W, y, color);
    }

    fn draw_week_number(&self, fb: &mut Framebuffer, week: i32, y: isize, color: Index) {
        self.draw_cell_text(
            fb,
            &format!("w{week}"),
            WEEK_NUM_LEFT as isize,
            WEEK_COL_W,
            y,
            color,
        );
    }

    fn draw_cell_text(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        cell_x: isize,
        col_w: usize,
        y: isize,
        color: Index,
    ) {
        draw_text_centered(fb, &self.calendar_font, text, cell_x, col_w, y, color);
    }

    fn calendar_cell_center(&self, col: usize, y: isize) -> (isize, isize) {
        (
            (CALENDAR_LEFT + col * CALENDAR_COL_W + CALENDAR_COL_W / 2) as isize
                + TODAY_CIRCLE_X_OFFSET,
            y + self.calendar_font.cell_h() as isize / 2 + TODAY_CIRCLE_Y_OFFSET,
        )
    }
}

/// Builds today's date parts from the system clock, or `None` if the
/// underlying `localtime_r` call fails. Callers should keep showing whatever
/// date they last had rather than fall back to a default (which would render
/// as the bogus "January 1, 1900").
fn current_date_parts() -> Option<DateParts> {
    let tm = local_time()?;
    let year = tm.tm_year + 1900;
    let month_index = tm.tm_mon.clamp(0, 11) as usize;
    let month = MONTHS.get(month_index).unwrap_or(&"JANUARY").to_string();
    let weekday = WEEKDAYS
        .get(tm.tm_wday.clamp(0, 6) as usize)
        .unwrap_or(&"SUNDAY")
        .to_string();

    Some(DateParts {
        year: year.to_string(),
        weekday,
        day: tm.tm_mday.clamp(1, 31).to_string(),
        month,
        year_num: year,
        month_index,
        day_num: tm.tm_mday.clamp(1, 31),
    })
}

/// Weekday index (0 = Sunday) of the given epoch day (days since
/// 1970-01-01, as returned by `days_from_civil`), derived from the shared
/// civil-date math in [`crate::localtime`] rather than a separate calendar
/// implementation. `days_from_civil` counts from 1970-01-01, a Thursday
/// (index 4), so weekday = (epoch_day + 4) mod 7.
const fn weekday_of(epoch_day: i64) -> usize {
    (epoch_day + 4).rem_euclid(7) as usize
}

/// The ISO week number to show for calendar grid row `row` (0-based, per
/// `render_calendar`'s Sun-Sat layout), or `None` if that row is a trailing
/// lone Sunday whose week was already labeled on the row above (its own
/// Monday-Saturday span falls past the end of the month, i.e. belongs to a
/// week that has no other day displayed anywhere in this month's grid).
fn calendar_row_week(first_day_epoch: i64, days: i32, row: usize) -> Option<i32> {
    let first_weekday = weekday_of(first_day_epoch);
    let monday_day = row as i64 * 7 + 2 - first_weekday as i64;
    if monday_day > i64::from(days) {
        return None;
    }
    Some(localtime::iso_week_number(first_day_epoch + monday_day - 1))
}

const fn short_month_name(month_index: usize) -> &'static str {
    match month_index {
        0 => "JAN",
        1 => "FEB",
        2 => "MAR",
        3 => "APR",
        4 => "MAY",
        5 => "JUN",
        6 => "JUL",
        7 => "AUG",
        8 => "SEP",
        9 => "OCT",
        10 => "NOV",
        11 => "DEC",
        #[allow(clippy::match_same_arms)]
        _ => "JAN",
    }
}

impl crate::widget::Widget for Day {
    fn width(&self) -> usize {
        WIDTH + SHADOW_X_OFFSET
    }

    fn height(&self) -> usize {
        HEIGHT + SHADOW_Y_OFFSET
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
        _x: isize,
        _y: isize,
        _state: u16,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        self.toggle_mode();
        Ok(crate::widget::ClickOutcome::default())
    }

    // Clicking anywhere toggles the mode.
    fn cursor_at(&self, _x: isize, _y: isize) -> crate::CursorKind {
        crate::CursorKind::Hand
    }
}

#[cfg(test)]
mod tests {
    use super::calendar_row_week;
    use crate::localtime;

    #[test]
    fn trailing_lone_sunday_row_has_no_week_label() {
        // May 2026 has 31 days and starts on a Friday (first_weekday = 5),
        // so the grid's last row (row 5) holds only day 31 -- a Sunday --
        // with no Monday-Saturday days of its own in this month. That
        // Sunday's real ISO week (22) is already shown on the row above for
        // days 25-30; row 5 must not relabel it with the following month's
        // week (23).
        let first_day_epoch = localtime::days_from_civil(2026, 5, 1);
        assert_eq!(calendar_row_week(first_day_epoch, 31, 4), Some(22));
        assert_eq!(calendar_row_week(first_day_epoch, 31, 5), None);
    }
}
