//! Weekly completion bar graph. One bar per day for the last 7 days; each bar's
//! height is the count of todos completed that day, and the bar is split into
//! colored segments by the priority the todos were filed under (so a day with 3
//! urgent and 3 snail todos draws half crimson, half blue).

use std::error::Error;
use std::time::Duration;

use crate::localtime::{civil_from_days, days_from_civil};
use crate::palette_color;
use crate::text::{BitmapFont, draw_text_centered};
use crate::{Framebuffer, Index, Palette};

const DAYS: usize = 7;
const PRIORITY_COUNT: usize = crate::toodle::PRIORITY_TAGS.len();

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

/// One day+priority done file with its cached line count.
struct DayFile {
    path: String,
    id: Option<crate::util::Fingerprint>,
    count: usize,
}

pub struct Stats {
    font: BitmapFont,
    week: WeekCounts,
    /// Count cache, `files[day][priority]`, keyed by path + stat identity.
    files: [[DayFile; PRIORITY_COUNT]; DAYS],
    throttle: crate::util::Throttle,
    week_counts_read_failing: crate::util::FailureLog,
}

/// Per-priority segment heights for one day's bar: proportional to `count /
/// max_total` scaled to `chart_h` (so the busiest day's bar fills the
/// chart), with every nonzero count floored to `MIN_SEGMENT_H` so a single
/// completed todo stays a visible sliver. The pixels added by that floor are
/// taken back from the day's largest segment — rather than clamping
/// whichever segment happens to be drawn last, which could zero it out —
/// so the stack never exceeds `chart_h`.
fn segment_heights(
    day_counts: &[usize; PRIORITY_COUNT],
    max_total: usize,
    chart_h: usize,
) -> [usize; PRIORITY_COUNT] {
    let mut heights = [0; PRIORITY_COUNT];
    if max_total == 0 {
        return heights;
    }

    let mut floor_sum = 0;
    let mut largest = 0;
    for (priority, &count) in day_counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        heights[priority] = ((count * chart_h) / max_total).max(MIN_SEGMENT_H);
        floor_sum += heights[priority];
        if heights[priority] > heights[largest] {
            largest = priority;
        }
    }

    if let Some(overflow) = floor_sum.checked_sub(chart_h) {
        heights[largest] = heights[largest].saturating_sub(overflow);
    }

    heights
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
            throttle: crate::util::Throttle::new(),
            week_counts_read_failing: crate::util::FailureLog::new(),
        };
        stats.refresh_week_counts()?;
        Ok(stats)
    }

    /// Re-read the done files periodically; returns whether the view changed.
    pub(crate) fn update(&mut self) -> bool {
        if !self.throttle.ready(REFRESH) {
            return false;
        }
        match self.refresh_week_counts() {
            Ok(changed) => {
                self.week_counts_read_failing
                    .record_ok(|| "stats: week counts reads recovered".to_string());
                changed
            }
            Err(err) => {
                self.week_counts_read_failing
                    .record_err(|| format!("stats: failed to read week counts: {err}"));
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

        // Compute the whole pass into locals first, and only commit to
        // `self` once every file has been read successfully. A transient IO
        // error partway through the loop must not leave `self.files` and
        // `self.week.counts` out of sync with each other.
        let mut new_files: [[DayFile; PRIORITY_COUNT]; DAYS] = std::array::from_fn(|_| {
            std::array::from_fn(|_| DayFile {
                path: String::new(),
                id: None,
                count: 0,
            })
        });
        let mut new_counts = self.week.counts;
        let mut changed = false;
        for (col, day_files) in self.files.iter().enumerate() {
            let (y, m, d) = civil_from_days(sunday + col as i64);
            for (priority, file) in day_files.iter().enumerate() {
                let path =
                    crate::toodle::done_file_path(y, m, d, crate::toodle::PRIORITY_TAGS[priority]);
                let id = file_id(&path);
                let (id, count) = if path == file.path && id == file.id {
                    (id, file.count)
                } else {
                    let count = crate::toodle::count_done_lines(&path)?;
                    changed |= count != file.count;
                    (id, count)
                };
                new_counts[col][priority] = count;
                new_files[col][priority] = DayFile { path, id, count };
            }
        }
        self.files = new_files;
        self.week.counts = new_counts;
        Ok(changed)
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let title = "DONE THIS WEEK";
        draw_text_centered(
            fb,
            &self.font,
            title,
            0,
            WIDTH,
            TOP_GAP as isize,
            palette_color::CREAM,
        );

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
            for (priority, &seg_h) in segment_heights(day_counts, max_total, chart_h)
                .iter()
                .enumerate()
            {
                if seg_h == 0 {
                    continue;
                }
                y -= seg_h;
                fb.fill_rect(
                    bar_x as isize,
                    y as isize,
                    bar_w,
                    seg_h,
                    PRIORITY_COLORS[priority],
                );
            }

            // Total count, centered just above the top of the bar.
            if total > 0 {
                let count_text = total.to_string();
                let count_y = y.saturating_sub(self.font.cell_h() + COUNT_GAP);
                draw_text_centered(
                    fb,
                    &self.font,
                    &count_text,
                    bar_x as isize,
                    bar_w,
                    count_y as isize,
                    palette_color::CREAM,
                );
            }

            let label = WEEKDAY_INITIALS[col % 7];
            draw_text_centered(
                fb,
                &self.font,
                label,
                bar_x as isize,
                bar_w,
                (chart_bottom + LABEL_GAP) as isize,
                palette_color::CREAM,
            );
        }
    }
}

/// Set while metadata reads are failing, so the error is logged once per
/// failure episode (with a recovery note) instead of once per file per
/// refresh tick. A free function (not a `Stats` method), so this needs a
/// `static` rather than a struct field like the other widgets' equivalents.
static METADATA_READ_FAILING: crate::util::FailureLog = crate::util::FailureLog::new();

/// The file's current stat identity, or `None` if it does not exist or its
/// metadata could not be read.
fn file_id(path: &str) -> Option<crate::util::Fingerprint> {
    match crate::util::fingerprint(path) {
        Ok(id @ Some(_)) => {
            METADATA_READ_FAILING.record_ok(|| "stats: metadata reads recovered".to_string());
            id
        }
        Ok(None) => None,
        Err(err) => {
            METADATA_READ_FAILING.record_err(|| {
                format!("stats: failed to read metadata for {path}: {err} (suppressing repeats)")
            });
            None
        }
    }
}

impl crate::widget::Widget for Stats {
    fn width(&self) -> usize {
        WIDTH
    }

    fn height(&self) -> usize {
        HEIGHT
    }

    // Not interactive; clicks land nowhere and the cursor stays an arrow.
    fn hit_test(&self, _x: isize, _y: isize) -> Option<crate::CursorKind> {
        None
    }

    fn fill_color(&self, _palette: &Palette) -> Index {
        palette_color::BLACK
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
    fn segment_heights_never_exceeds_chart_h() {
        // The busiest day (max_total == this day's total): one dominant
        // priority plus several minor ones whose MIN_SEGMENT_H padding would,
        // unclamped, push the stack past chart_h and zero out a segment.
        let heights = segment_heights(&[80, 1, 1, 1], 83, 85);
        assert_eq!(heights.iter().sum::<usize>(), 85);
        assert!(heights.iter().all(|&h| h >= MIN_SEGMENT_H));
    }

    #[test]
    fn segment_heights_floors_a_visible_sliver_for_every_nonzero_count() {
        let heights = segment_heights(&[1, 1, 1, 1], 1000, 80);
        assert_eq!(heights, [MIN_SEGMENT_H; PRIORITY_COUNT]);
    }

    #[test]
    fn segment_heights_is_all_zero_when_nothing_completed() {
        assert_eq!(segment_heights(&[0, 0, 0, 0], 0, 80), [0; PRIORITY_COUNT]);
    }
}
