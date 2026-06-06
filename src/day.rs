use std::error::Error;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app_color;
use crate::bitmap_font::BitmapFont;
use crate::palette_color;
use crate::pixolde_bold_font;
use crate::poco_font;
use crate::rozha_one_48_font;
use crate::{Framebuffer, Image, Palette, Rgba, draw_filled_circle};

const WIDTH: usize = 116;
const HEIGHT: usize = 116;
const BACKGROUND_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/days.png");
const SHADOW_X_OFFSET: isize = 1;
const SHADOW_Y_OFFSET: isize = 4;
const DATE_REFRESH: Duration = Duration::from_secs(60);
const TOP_GAP: usize = 10;
const LABEL_GAP: usize = 26;
const NUMBER_GAP: usize = 6;
const MONTH_GAP: usize = 6;
const CALENDAR_TITLE_Y: usize = 6;
const CALENDAR_WEEKDAY_Y: usize = 29;
const CALENDAR_GRID_Y: usize = 42;
const CALENDAR_COL_W: usize = 13;
const CALENDAR_ROW_H: usize = 12;
const CALENDAR_LEFT: usize = 12;
const TODAY_CIRCLE_X_OFFSET: isize = -1;
const TODAY_CIRCLE_Y_OFFSET: isize = 2;
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
    weekday_index: i32,
}

pub(crate) struct Day {
    background: Image,
    label_font: BitmapFont,
    number_font: BitmapFont,
    calendar_font: BitmapFont,
    date: DateParts,
    last_check: Instant,
    calendar_mode: bool,
}

impl Day {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            background: Image::load(BACKGROUND_PATH, palette)?,
            label_font: BitmapFont::load(&pixolde_bold_font::PIXOLDE_BOLD_SPEC)?,
            number_font: BitmapFont::load(&rozha_one_48_font::ROZHA_ONE_48_SPEC)?,
            calendar_font: BitmapFont::load(&poco_font::POCO_SPEC)?,
            date: current_date_parts(),
            last_check: Instant::now(),
            calendar_mode: false,
        })
    }

    pub(crate) fn width(&self) -> usize {
        WIDTH + SHADOW_X_OFFSET as usize
    }

    pub(crate) fn height(&self) -> usize {
        HEIGHT + SHADOW_Y_OFFSET as usize
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK).transparent()
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.draw_image_shadow(
            &self.background,
            SHADOW_X_OFFSET,
            SHADOW_Y_OFFSET,
            1,
            palette.color(app_color::BACKGROUND_SHADOW),
        );
        fb.draw_image(&self.background, 0, 0, 1);

        if self.calendar_mode {
            self.render_calendar(fb, palette);
        } else {
            self.render_day(fb, palette);
        }
    }

    pub(crate) fn toggle_mode(&mut self) {
        self.calendar_mode = !self.calendar_mode;
    }

    fn render_day(&self, fb: &mut Framebuffer, palette: &Palette) {
        let black = palette.color(palette_color::BLACK);
        let cream = palette.color(palette_color::CREAM);
        let crimson = palette.color(palette_color::CRIMSON);
        let purple = palette.color(palette_color::PURPLE);
        let rose = palette.color(palette_color::ROSE);
        let year_h = self.tight_height(&self.label_font, &self.date.year);
        let weekday_h = self.tight_height(&self.label_font, &self.date.weekday);
        let day_h = self.tight_height(&self.number_font, &self.date.day);
        let mut y = TOP_GAP;

        self.draw_centered_tight(fb, &self.label_font, &self.date.year, y - 1, purple);
        self.draw_centered_tight(fb, &self.label_font, &self.date.year, y + 1, rose);
        self.draw_centered_tight(fb, &self.label_font, &self.date.year, y, cream);
        y += year_h + LABEL_GAP;
        self.draw_centered_tight(fb, &self.label_font, &self.date.weekday, y, black);
        y += weekday_h + NUMBER_GAP;
        self.draw_centered_tight(fb, &self.number_font, &self.date.day, y, crimson);
        y += day_h + MONTH_GAP;
        self.draw_centered_tight(fb, &self.label_font, &self.date.month, y, black);
    }

    fn render_calendar(&self, fb: &mut Framebuffer, palette: &Palette) {
        let black = palette.color(palette_color::BLACK);
        let cream = palette.color(palette_color::CREAM);
        let crimson = palette.color(palette_color::CRIMSON);
        let title = format!(
            "{} {}",
            short_month_name(self.date.month_index),
            self.date.year_num
        );

        self.draw_centered(fb, &self.calendar_font, &title, CALENDAR_TITLE_Y, cream);

        for (weekday, label) in WEEKDAY_LABELS.iter().enumerate() {
            self.draw_calendar_cell(fb, label, weekday, CALENDAR_WEEKDAY_Y, black);
        }

        let first_weekday = first_weekday_of_month(&self.date);
        let days = days_in_month(self.date.year_num, self.date.month_index);
        for day in 1..=days {
            let index = first_weekday + day as usize - 1;
            let col = index % 7;
            let row = index / 7;
            let y = CALENDAR_GRID_Y + row * CALENDAR_ROW_H;
            let color = if day == self.date.day_num {
                cream
            } else {
                black
            };
            if day == self.date.day_num {
                let (center_x, center_y) = self.calendar_cell_center(col, y);
                draw_filled_circle(fb, center_x, center_y, TODAY_CIRCLE_RADIUS, crimson);
            }
            self.draw_calendar_cell(fb, &day.to_string(), col, y, color);
        }
    }

    pub(crate) fn update(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_check) < DATE_REFRESH {
            return false;
        }

        self.last_check = now;
        let date = current_date_parts();
        if date == self.date {
            return false;
        }

        self.date = date;
        true
    }

    fn tight_height(&self, font: &BitmapFont, text: &str) -> usize {
        font.text_ink_bounds(text)
            .map(|bounds| bounds.height())
            .unwrap_or_else(|| font.cell_h())
    }

    fn draw_centered_tight(
        &self,
        fb: &mut Framebuffer,
        font: &BitmapFont,
        text: &str,
        y: usize,
        color: Rgba,
    ) {
        let Some(bounds) = font.text_ink_bounds(text) else {
            return;
        };
        let x = centered_x(bounds.width()).saturating_add_signed(-bounds.min_x);
        let draw_y = y.saturating_sub(bounds.min_y);
        font.draw_text(fb, text, x, draw_y, 1, color);
    }

    fn draw_centered(
        &self,
        fb: &mut Framebuffer,
        font: &BitmapFont,
        text: &str,
        y: usize,
        color: Rgba,
    ) {
        let x = centered_x(font.text_width(text));
        font.draw_text(fb, text, x, y, 1, color);
    }

    fn draw_calendar_cell(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        col: usize,
        y: usize,
        color: Rgba,
    ) {
        let cell_x = CALENDAR_LEFT + col * CALENDAR_COL_W;
        let text_x =
            cell_x + CALENDAR_COL_W.saturating_sub(self.calendar_font.text_width(text)) / 2;
        self.calendar_font.draw_text(fb, text, text_x, y, 1, color);
    }

    fn calendar_cell_center(&self, col: usize, y: usize) -> (isize, isize) {
        (
            (CALENDAR_LEFT + col * CALENDAR_COL_W + CALENDAR_COL_W / 2) as isize
                + TODAY_CIRCLE_X_OFFSET,
            (y + self.calendar_font.cell_h() / 2) as isize + TODAY_CIRCLE_Y_OFFSET,
        )
    }
}

fn centered_x(text_width: usize) -> usize {
    WIDTH.saturating_sub(text_width) / 2
}

fn current_date_parts() -> DateParts {
    let tm = local_time().unwrap_or_default();
    let year = tm.tm_year + 1900;
    let month_index = tm.tm_mon.clamp(0, 11) as usize;
    let month = MONTHS.get(month_index).unwrap_or(&"JANUARY").to_string();
    let weekday = WEEKDAYS
        .get(tm.tm_wday.clamp(0, 6) as usize)
        .unwrap_or(&"SUNDAY")
        .to_string();

    DateParts {
        year: year.to_string(),
        weekday,
        day: tm.tm_mday.clamp(1, 31).to_string(),
        month,
        year_num: year,
        month_index,
        day_num: tm.tm_mday.clamp(1, 31),
        weekday_index: tm.tm_wday.clamp(0, 6),
    }
}

fn first_weekday_of_month(date: &DateParts) -> usize {
    let weekday = date.weekday_index;
    let offset = (date.day_num - 1).rem_euclid(7);
    (weekday - offset).rem_euclid(7) as usize
}

fn days_in_month(year: i32, month_index: usize) -> i32 {
    match month_index {
        0 | 2 | 4 | 6 | 7 | 9 | 11 => 31,
        3 | 5 | 8 | 10 => 30,
        1 if is_leap_year(year) => 29,
        1 => 28,
        _ => 31,
    }
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn short_month_name(month_index: usize) -> &'static str {
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
        _ => "JAN",
    }
}

fn local_time() -> Option<Tm> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as TimeT;
    let mut out = Tm::default();
    let result = unsafe { localtime_r(&seconds, &mut out) };
    (!result.is_null()).then_some(out)
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
