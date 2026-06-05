use std::error::Error;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app_color;
use crate::bitmap_font::BitmapFont;
use crate::palette_color;
use crate::pixolde_bold_font;
use crate::rozha_one_48_font;
use crate::{Framebuffer, Image, Palette, Rgba};

const WIDTH: usize = 116;
const HEIGHT: usize = 116;
const BACKGROUND_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/days.png");
const SHADOW_X_OFFSET: isize = 1;
const SHADOW_Y_OFFSET: isize = 4;
const PADDING: usize = 0;
const DATE_REFRESH: Duration = Duration::from_secs(60);
const TOP_GAP: usize = 10;
const LABEL_GAP: usize = 26;
const NUMBER_GAP: usize = 6;
const MONTH_GAP: usize = 6;

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
    background: Image,
    label_font: BitmapFont,
    number_font: BitmapFont,
    date: DateParts,
    last_check: Instant,
}

impl Day {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            background: Image::load(BACKGROUND_PATH, palette)?,
            label_font: BitmapFont::load(&pixolde_bold_font::PIXOLDE_BOLD_SPEC)?,
            number_font: BitmapFont::load(&rozha_one_48_font::ROZHA_ONE_48_SPEC)?,
            date: current_date_parts(),
            last_check: Instant::now(),
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

        let black = palette.color(palette_color::BLACK);
        let cream = palette.color(palette_color::CREAM);
        let crimson = palette.color(palette_color::CRIMSON);
        let purple = palette.color(palette_color::PURPLE);
        let rose = palette.color(palette_color::ROSE);
        let year_h = self.tight_height(&self.label_font, &self.date.year);
        let weekday_h = self.tight_height(&self.label_font, &self.date.weekday);
        let day_h = self.tight_height(&self.number_font, &self.date.day);
        let month_h = self.tight_height(&self.label_font, &self.date.month);
        let content_h = year_h + LABEL_GAP + weekday_h + NUMBER_GAP + day_h + MONTH_GAP + month_h;
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
