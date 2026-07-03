//! Weekly completion bar graph. One bar per day for the last 7 days; each bar's
//! height is the count of todos completed that day, and the bar is split into
//! colored segments by the priority the todos were filed under (so a day with 3
//! urgent and 3 snail todos draws half crimson, half blue).

use std::error::Error;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::palette_color;
use crate::text::BitmapFont;
use crate::{Framebuffer, Index, Palette};

const DAYS: usize = 7;
const PRIORITY_COUNT: usize = 4;

/// Priorities in stacking order (bottom of the bar first), matching toodle's
/// section order: urgent, frog, normal, snail.
const PRIORITY_TAGS: [&str; PRIORITY_COUNT] = ["urgent", "frog", "normal", "snail"];
const PRIORITY_COLORS: [Index; PRIORITY_COUNT] = [
    palette_color::CRIMSON,
    palette_color::GREEN,
    palette_color::ORANGE,
    palette_color::BLUE,
];

/// Single-letter weekday labels, indexed by `tm_wday` (0 = Sunday).
const WEEKDAY_INITIALS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];

const WIDTH: usize = 210;
const HEIGHT: usize = 132;

const TOP_GAP: usize = 10;
const SIDE_PAD: usize = 14;
const LABEL_GAP: usize = 4;
const BAR_GAP: usize = 6;
/// Gap between the top of a bar and the total-count number above it.
const COUNT_GAP: usize = 2;
/// Drawn so a single completed todo is still a visible sliver.
const MIN_SEGMENT_H: usize = 2;

const REFRESH: Duration = Duration::from_secs(2);

/// Per-day, per-priority completed-todo counts for the current week. Column `i`
/// is weekday `i` (0 = Sunday), so the chart always runs Sunday through Saturday.
#[derive(Clone, PartialEq, Eq)]
struct WeekCounts {
    counts: [[usize; PRIORITY_COUNT]; DAYS],
}

/// Identity of one done file's on-disk version, so the periodic refresh only
/// re-reads a file whose stat changed (same scheme as toodle's `DoneCounts`).
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileId {
    ino: u64,
    size: u64,
    mtime_s: i64,
    mtime_ns: i64,
}

/// One day+priority done file with its cached line count.
struct DayFile {
    path: String,
    id: Option<FileId>,
    count: usize,
}

pub struct Stats {
    font: BitmapFont,
    week: WeekCounts,
    /// Count cache, `files[day][priority]`, keyed by path + stat identity.
    files: [[DayFile; PRIORITY_COUNT]; DAYS],
    last_check: Instant,
    logged_error: bool,
}

impl Stats {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let font = BitmapFont::load_with_fallback(
            &pixel_fonts::PIXOLDE_SPEC,
            &pixel_fonts::FUSION_PIXEL_8_SPEC,
        )?;
        let mut stats = Self {
            font,
            week: WeekCounts {
                counts: [[0; PRIORITY_COUNT]; DAYS],
            },
            files: std::array::from_fn(|_| {
                std::array::from_fn(|_| DayFile {
                    path: String::new(),
                    id: None,
                    count: 0,
                })
            }),
            last_check: Instant::now(),
            logged_error: false,
        };
        stats.refresh_week_counts()?;
        Ok(stats)
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn width(&self) -> usize {
        WIDTH
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn height(&self) -> usize {
        HEIGHT
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn fill_color(&self, _palette: &Palette) -> Index {
        palette_color::BLACK
    }

    /// Re-read the done files periodically; returns whether the view changed.
    pub(crate) fn update(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_check) < REFRESH {
            return false;
        }
        self.last_check = now;
        match self.refresh_week_counts() {
            Ok(changed) => {
                self.logged_error = false;
                changed
            }
            Err(err) => {
                if !self.logged_error {
                    eprintln!("stats: failed to read week counts: {err}");
                    self.logged_error = true;
                }
                false
            }
        }
    }

    /// Re-count any done file whose path (midnight/week rollover) or stat
    /// identity changed; unchanged files keep their cached count without being
    /// re-read. Returns whether any count changed.
    fn refresh_week_counts(&mut self) -> Result<bool, Box<dyn Error>> {
        let tm = crate::localtime::local_time().unwrap_or_default();
        let today = days_from_civil(tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday);
        let today_wday = i64::from(tm.tm_wday.rem_euclid(7));
        // Back up to this week's Sunday, then walk forward to Saturday.
        let sunday = today - today_wday;

        let mut changed = false;
        for (col, day_files) in self.files.iter_mut().enumerate() {
            let (y, m, d) = civil_from_days(sunday + col as i64);
            for (priority, file) in day_files.iter_mut().enumerate() {
                let path = crate::toodle::done_file_path(y, m, d, PRIORITY_TAGS[priority]);
                let id = file_id(&path)?;
                if path == file.path && id == file.id {
                    continue;
                }
                let count = done_line_count(&path)?;
                changed |= count != file.count;
                *file = DayFile { path, id, count };
                self.week.counts[col][priority] = count;
            }
        }
        Ok(changed)
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let title = "DONE THIS WEEK";
        let title_x = WIDTH.saturating_sub(self.font.text_width(title)) / 2;
        self.font
            .draw_text(fb, title, title_x, TOP_GAP, palette_color::CREAM);

        // Reserve a row above the bars for the count numbers so the tallest
        // bar's total isn't clipped by the title.
        let chart_top = TOP_GAP + 2 * self.font.cell_h() + LABEL_GAP + COUNT_GAP;
        let label_h = self.font.cell_h();
        let chart_bottom = HEIGHT.saturating_sub(label_h + LABEL_GAP);
        let chart_h = chart_bottom.saturating_sub(chart_top);

        let span = WIDTH.saturating_sub(2 * SIDE_PAD);
        let slot_w = (span + BAR_GAP) / DAYS;
        let bar_w = slot_w.saturating_sub(BAR_GAP);

        let max_total = self
            .week
            .counts
            .iter()
            .map(|day| day.iter().sum::<usize>())
            .max()
            .unwrap_or(0);

        for (col, day_counts) in self.week.counts.iter().enumerate() {
            let bar_x = SIDE_PAD + col * slot_w;
            let total: usize = day_counts.iter().sum();

            // Stack priority segments from the bottom up, scaling the day's
            // total against the busiest day so the tallest bar fills the chart.
            let mut y = chart_bottom;
            if max_total > 0 {
                for (priority, &count) in day_counts.iter().enumerate() {
                    if count == 0 {
                        continue;
                    }
                    let seg_h = ((count * chart_h) / max_total)
                        .max(MIN_SEGMENT_H)
                        .min(y.saturating_sub(chart_top));
                    if seg_h == 0 {
                        continue;
                    }
                    y -= seg_h;
                    fb.fill_rect(bar_x, y, bar_w, seg_h, PRIORITY_COLORS[priority]);
                }
            }

            // Total count, centered just above the top of the bar.
            if total > 0 {
                let count_text = total.to_string();
                let count_x = bar_x + bar_w.saturating_sub(self.font.text_width(&count_text)) / 2;
                let count_y = y.saturating_sub(self.font.cell_h() + COUNT_GAP);
                self.font
                    .draw_text(fb, &count_text, count_x, count_y, palette_color::CREAM);
            }

            let label = WEEKDAY_INITIALS[col % 7];
            let label_x = bar_x + bar_w.saturating_sub(self.font.text_width(label)) / 2;
            self.font.draw_text(
                fb,
                label,
                label_x,
                chart_bottom + LABEL_GAP,
                palette_color::CREAM,
            );
        }
    }
}

/// The file's current stat identity, or `None` if it does not exist.
fn file_id(path: &str) -> Result<Option<FileId>, Box<dyn Error>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(FileId {
            ino: meta.ino(),
            size: meta.size(),
            mtime_s: meta.mtime(),
            mtime_ns: meta.mtime_nsec(),
        })),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn done_line_count(path: &str) -> Result<usize, Box<dyn Error>> {
    if !Path::new(path).exists() {
        return Ok(0);
    }
    Ok(fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
const fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) as i64 / 400;
    let yoe = (y as i64) - era * 400;
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as i64;
    let doy = (153 * mp + 2) / 5 + (d as i64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`: returns (year, month, day).
const fn civil_from_days(z: i64) -> (i32, i32, i32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m as i32, d as i32)
}

impl crate::widget::Widget for Stats {
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
    fn civil_round_trips() {
        let days = days_from_civil(2026, 6, 28);
        assert_eq!(civil_from_days(days), (2026, 6, 28));
        // Day before is the 27th.
        assert_eq!(civil_from_days(days - 1), (2026, 6, 27));
    }

    #[test]
    fn done_line_count_ignores_blank_lines() {
        let dir = std::env::temp_dir().join(format!("cozyui-stats-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("count.txt");
        fs::write(&path, "a\n\nb\n  \nc\n").unwrap();
        assert_eq!(done_line_count(path.to_str().unwrap()).unwrap(), 3);
        assert_eq!(done_line_count("/no/such/stats/file").unwrap(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
}

