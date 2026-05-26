use std::error::Error;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::bitmap_font::BitmapFont;
use crate::palette_color;
use crate::pixolde_bold_font;
use crate::rozha_one_48_font;
use crate::{Framebuffer, Palette, Rgba};

const WIDTH: usize = 160;
const HEIGHT: usize = 112;
const PADDING: usize = 8;
const DATE_REFRESH: Duration = Duration::from_secs(60);
const LABEL_GAP: usize = 2;
const NUMBER_GAP: usize = 0;
const MONTH_GAP: usize = 3;

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
}

pub(crate) struct Day {
    label_font: BitmapFont,
    number_font: BitmapFont,
    date: DateParts,
    last_check: Instant,
}

impl Day {
    pub(crate) fn load() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            label_font: BitmapFont::load(&pixolde_bold_font::PIXOLDE_BOLD_SPEC)?,
            number_font: BitmapFont::load(&rozha_one_48_font::ROZHA_ONE_48_SPEC)?,
            date: current_date_parts(),
            last_check: Instant::now(),
        })
    }

    pub(crate) fn width(&self) -> usize {
        WIDTH
    }

    pub(crate) fn height(&self) -> usize {
        HEIGHT
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::CREAM)
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));

        let black = palette.color(palette_color::BLACK);
        let crimson = palette.color(palette_color::CRIMSON);
        let year_h = self.tight_height(&self.label_font, &self.date.year);
        let weekday_h = self.tight_height(&self.label_font, &self.date.weekday);
        let day_h = self.tight_height(&self.number_font, &self.date.day);
        let month_h = self.tight_height(&self.label_font, &self.date.month);
        let content_h = year_h + LABEL_GAP + weekday_h + NUMBER_GAP + day_h + MONTH_GAP + month_h;
        let mut y = PADDING.max(HEIGHT.saturating_sub(content_h) / 2);

        self.draw_centered_tight(fb, &self.label_font, &self.date.year, y, black);
        y += year_h + LABEL_GAP;
        self.draw_centered_tight(fb, &self.label_font, &self.date.weekday, y, black);
        y += weekday_h + NUMBER_GAP;
        self.draw_centered_tight(fb, &self.number_font, &self.date.day, y, crimson);
        y += day_h + MONTH_GAP;
        self.draw_centered_tight(fb, &self.label_font, &self.date.month, y, black);
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
}

fn centered_x(text_width: usize) -> usize {
    WIDTH.saturating_sub(text_width) / 2
}

fn current_date_parts() -> DateParts {
    let tm = local_time().unwrap_or_default();
    let month = MONTHS
        .get(tm.tm_mon.clamp(0, 11) as usize)
        .unwrap_or(&"JANUARY")
        .to_string();
    let weekday = WEEKDAYS
        .get(tm.tm_wday.clamp(0, 6) as usize)
        .unwrap_or(&"SUNDAY")
        .to_string();

    DateParts {
        year: (tm.tm_year + 1900).to_string(),
        weekday,
        day: tm.tm_mday.clamp(1, 31).to_string(),
        month,
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
