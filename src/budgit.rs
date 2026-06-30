//! Live budget countdown widget. Ports the standalone `budgit` HTML/JS toy:
//! a balance that bleeds away in real time based on a fixed monthly burn.

use std::error::Error;
use std::fs;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::palette_color;
use crate::pixolde_bold_font;
use crate::pixolde_font;
use crate::rozha_one_48_font;
use crate::text::BitmapFont;
use crate::{Framebuffer, Index, Palette};

const WIDTH: usize = 210;
const HEIGHT: usize = 208;

/// Seconds in an average month (365.25 / 12 days).
const MONTH_SECS: f64 = (365.25 / 12.0) * 24.0 * 3600.0;
const DAYS_PER_MONTH: f64 = 365.25 / 12.0;

/// Path to the editable budget config (see `budgit.conf`).
const CONFIG_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/budgit.conf");

/// Path to the ledger CSV whose entries sum to the starting balance.
const LEDGER_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/budgit.csv");

const REFRESH: Duration = Duration::from_secs(1);

/// How long the background SVG counter waits between scans (measured from the
/// end of the previous scan, so a slow scan never overlaps the next).
const SVG_REFRESH: Duration = Duration::from_secs(1);

/// Folder of finished templates; its SVGs set the "paths per finished SVG"
/// baseline used to estimate how many in-progress SVGs are complete.
const DONE_DIR: &str = "~/Desktop/allfiles/templates/done/";

/// Folder of in-progress templates (the `done/` subfolder is excluded).
const TEMPLATES_DIR: &str = "~/Desktop/allfiles/templates/";

const TOP_GAP: usize = 12;
const LABEL_GAP: usize = 4;
const FRACTION_GAP: usize = 2;
const STATS_GAP: usize = 14;
const STAT_ROW_H: usize = 12;

/// One budget period: a set of monthly expenses that takes effect on
/// `start_secs` (Unix seconds) and runs until the next period begins.
struct Period {
    start_secs: f64,
    /// Total monthly burn for this period (sum of `expenses`).
    burn: f64,
    /// Labelled monthly expenses for this period.
    expenses: Vec<(String, f64)>,
}

/// Parsed budget config: a chronological list of expense periods plus the
/// global `dollars per svg` payout used to credit completed SVG work.
struct Config {
    periods: Vec<Period>,
    dollars_per_svg: f64,
}

impl Config {
    /// Parses the config text. A `[YYYY-MM-DD]` header opens a new period and
    /// the `Label = amount` lines beneath it are that period's monthly
    /// expenses. A period's burn rate applies from its date until the next
    /// period begins, so editing a later period never rewrites past spending.
    /// `#` starts a comment and blank lines are ignored.
    fn parse(text: &str) -> Result<Self, Box<dyn Error>> {
        let mut periods: Vec<Period> = Vec::new();
        let mut dollars_per_svg = 0.0;

        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(date) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let (y, m, d) = parse_date(date.trim())?;
                periods.push(Period {
                    start_secs: (days_from_civil(y, m, d) * 86400) as f64,
                    burn: 0.0,
                    expenses: Vec::new(),
                });
                continue;
            }
            let (label, value) = line
                .split_once('=')
                .ok_or_else(|| format!("budgit.conf: missing '=' in line: {raw}"))?;
            let label = label.trim();
            let value = value.trim();
            // Global setting (not tied to any expense period).
            if label.eq_ignore_ascii_case("dollars per svg") {
                let value = value.trim_matches('$').trim();
                dollars_per_svg = value
                    .parse()
                    .map_err(|_| format!("budgit.conf: bad amount for {label}: {value}"))?;
                continue;
            }
            let amount: f64 = value
                .parse()
                .map_err(|_| format!("budgit.conf: bad amount for {label}: {value}"))?;
            let period = periods
                .last_mut()
                .ok_or("budgit.conf: expense before any [date] section")?;
            period.burn += amount;
            period.expenses.push((label.to_string(), amount));
        }

        if periods.is_empty() {
            return Err("budgit.conf: no [date] sections found".into());
        }
        periods.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
        Ok(Self {
            periods,
            dollars_per_svg,
        })
    }
}

/// Sums the ledger CSV in `budgit.csv` into the starting balance. Income is
/// positive, expenses negative. Columns are `title,amount,date`; the header
/// row, blank lines, and `#` comments are skipped, and the date is optional
/// (only the amount is used). A missing file means a zero balance.
fn load_ledger_balance() -> Result<f64, Box<dyn Error>> {
    match fs::read_to_string(LEDGER_PATH) {
        Ok(text) => sum_ledger(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0.0),
        Err(err) => Err(err.into()),
    }
}

/// Sums the amount column of ledger CSV text. See [`load_ledger_balance`].
fn sum_ledger(text: &str) -> Result<f64, Box<dyn Error>> {
    let mut balance = 0.0;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(',');
        let title = fields.next().unwrap_or("").trim();
        let amount = fields.next().unwrap_or("").trim();
        // Skip the header row.
        if amount.eq_ignore_ascii_case("amount") {
            continue;
        }
        balance += amount
            .parse::<f64>()
            .map_err(|_| format!("budgit.csv: bad amount for {title:?}: {amount}"))?;
    }
    Ok(balance)
}

/// Parses a `YYYY-MM-DD` date into its components.
fn parse_date(value: &str) -> Result<(i32, i32, i32), Box<dyn Error>> {
    let mut parts = value.split('-');
    let mut next = || -> Result<i32, Box<dyn Error>> {
        parts
            .next()
            .and_then(|p| p.trim().parse().ok())
            .ok_or_else(|| format!("budgit.conf: bad date: {value}").into())
    };
    Ok((next()?, next()?, next()?))
}

#[derive(Clone, PartialEq)]
struct BudgetView {
    dollars: String,
    fraction: String,
    color: Index,
    monthly: String,
    daily: String,
    days_left: String,
    svgs_completed: String,
}

pub struct Budgit {
    label_font: BitmapFont,
    balance_font: BitmapFont,
    stat_font: BitmapFont,
    start_balance: f64,
    /// Expense periods in chronological order; burn is piecewise over time.
    periods: Vec<Period>,
    /// Dollars credited per completed SVG (from `budgit.conf`).
    dollars_per_svg: f64,
    /// Latest completed-SVG estimate received from the background counter.
    svgs_completed: f64,
    /// Receives fresh estimates from the off-thread ripgrep scanner.
    svg_rx: Receiver<f64>,
    view: BudgetView,
    last_check: Instant,
}

impl Budgit {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let label_font = BitmapFont::load_with_fallback(
            &pixolde_bold_font::PIXOLDE_BOLD_SPEC,
            &crate::fusion_pixel_12_font::FUSION_PIXEL_12_SPEC,
        )?;
        let balance_font = BitmapFont::load(&rozha_one_48_font::ROZHA_ONE_48_SPEC)?;
        let stat_font = BitmapFont::load_with_fallback(
            &pixolde_font::PIXOLDE_SPEC,
            &crate::fusion_pixel_8_font::FUSION_PIXEL_8_SPEC,
        )?;

        let config = Config::parse(&fs::read_to_string(CONFIG_PATH)?)?;
        let start_balance = load_ledger_balance()?;
        // The estimate starts at zero and is filled in by the background
        // counter once its first off-thread scan completes.
        let svgs_completed = 0.0;
        let svg_rx = spawn_svg_counter();
        let view = compute_view(
            start_balance,
            &config.periods,
            now_secs(),
            svgs_completed,
            config.dollars_per_svg,
        );

        Ok(Self {
            label_font,
            balance_font,
            stat_font,
            start_balance,
            periods: config.periods,
            dollars_per_svg: config.dollars_per_svg,
            svgs_completed,
            svg_rx,
            view,
            last_check: Instant::now(),
        })
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

    pub(crate) fn update(&mut self) -> bool {
        let now = Instant::now();
        // Drain any estimates produced by the background counter, keeping the
        // most recent. This never blocks the render thread.
        while let Ok(completed) = self.svg_rx.try_recv() {
            self.svgs_completed = completed;
        }
        if now.duration_since(self.last_check) < REFRESH {
            return false;
        }
        self.last_check = now;
        let view = compute_view(
            self.start_balance,
            &self.periods,
            now_secs(),
            self.svgs_completed,
            self.dollars_per_svg,
        );
        if view == self.view {
            return false;
        }
        self.view = view;
        true
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let muted = palette_color::CREAM;
        let mut y = TOP_GAP;

        // "MONEY LEFT" label.
        self.draw_centered(fb, &self.label_font, "MONEY LEFT", y, muted);
        y += self.label_font.cell_h() + LABEL_GAP;

        // Big balance (dollars) in the calendar's big font, positioned by its
        // ink bounds so the tall cell box doesn't blow out the layout.
        let balance_h = self
            .balance_font
            .text_ink_bounds(&self.view.dollars)
            .map_or_else(|| self.balance_font.cell_h(), |b| b.height());
        self.draw_centered_tight(
            fb,
            &self.balance_font,
            &self.view.dollars,
            y,
            self.view.color,
        );
        y += balance_h + FRACTION_GAP;

        // Fractional cents underneath, bold and in the balance color.
        self.draw_centered(
            fb,
            &self.label_font,
            &self.view.fraction,
            y,
            self.view.color,
        );
        y += self.label_font.cell_h() + STATS_GAP;

        // Stat rows.
        let rows = [
            ("Monthly", self.view.monthly.as_str()),
            ("Daily", self.view.daily.as_str()),
            ("Days left", self.view.days_left.as_str()),
            ("~ svgs completed", self.view.svgs_completed.as_str()),
        ];
        for (label, value) in rows {
            self.draw_row(fb, label, value, y);
            y += STAT_ROW_H;
        }
    }

    #[allow(clippy::unused_self)]
    fn draw_centered(
        &self,
        fb: &mut Framebuffer,
        font: &BitmapFont,
        text: &str,
        y: usize,
        color: Index,
    ) {
        let x = WIDTH.saturating_sub(font.text_width(text)) / 2;
        font.draw_text(fb, text, x, y, color);
    }

    /// Centers using the glyph ink bounds and draws so that `y` is the top of
    /// the ink, not the (much taller) cell box.
    #[allow(clippy::unused_self)]
    fn draw_centered_tight(
        &self,
        fb: &mut Framebuffer,
        font: &BitmapFont,
        text: &str,
        y: usize,
        color: Index,
    ) {
        let Some(bounds) = font.text_ink_bounds(text) else {
            return;
        };
        let x = (WIDTH.saturating_sub(bounds.width()) / 2).saturating_add_signed(-bounds.min_x);
        let draw_y = y.saturating_sub(bounds.min_y);
        font.draw_text(fb, text, x, draw_y, color);
    }

    fn draw_row(&self, fb: &mut Framebuffer, label: &str, value: &str, y: usize) {
        const PAD: usize = 14;
        self.label_font
            .draw_text(fb, label, PAD, y, palette_color::CREAM);
        let x = WIDTH.saturating_sub(PAD + self.stat_font.text_width(value));
        self.stat_font
            .draw_text(fb, value, x, y, palette_color::CREAM);
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn compute_view(
    start_balance: f64,
    periods: &[Period],
    now: f64,
    svgs_completed: f64,
    dollars_per_svg: f64,
) -> BudgetView {
    // Spend is integrated period by period so a later period's burn never
    // applies to time before it began. `burn` is the rate active right now,
    // used for the monthly/daily display and the days-left projection.
    let mut spent = 0.0;
    let mut burn = 0.0;
    for (i, period) in periods.iter().enumerate() {
        let end = periods
            .get(i + 1)
            .map_or(now, |next| next.start_secs.min(now));
        let active = (end - period.start_secs).max(0.0);
        spent += active / MONTH_SECS * period.burn;
        if period.start_secs <= now {
            burn = period.burn;
        }
    }
    // Completed SVG work is credited to the balance at the configured rate.
    let earned = dollars_per_svg * svgs_completed;
    let remaining = start_balance + earned - spent;
    let per_sec = burn / MONTH_SECS;
    let per_day = burn / DAYS_PER_MONTH;

    let color = if remaining > 0.0 {
        palette_color::LIME
    } else if remaining > -100.0 {
        palette_color::ORANGE
    } else {
        palette_color::CRIMSON
    };

    let (dollars, fraction) = split_money(remaining);

    let days_left = if burn > 0.0 && remaining > 0.0 {
        let days = (remaining / per_sec) / 86400.0;
        format!("{days:.1}d")
    } else if remaining <= 0.0 {
        "0d".to_string()
    } else {
        "inf".to_string()
    };

    BudgetView {
        dollars,
        fraction,
        color,
        monthly: format!("{}/mo", fmt_money(burn, 2)),
        daily: format!("{}/day", fmt_money(per_day, 2)),
        days_left,
        svgs_completed: format!("{svgs_completed:.1}"),
    }
}

/// Spawns a background thread that estimates completed SVGs off the render
/// thread. It computes the finished-SVG path average once, then loops scanning
/// the in-progress folder, sending each estimate and sleeping `SVG_REFRESH`
/// *after* the scan finishes (so a slow scan never overlaps the next). The
/// thread exits when the receiver is dropped.
fn spawn_svg_counter() -> Receiver<f64> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let avg = avg_paths_per_svg(DONE_DIR);
        loop {
            let completed = compute_svgs_completed(TEMPLATES_DIR, avg);
            if tx.send(completed).is_err() {
                break;
            }
            thread::sleep(SVG_REFRESH);
        }
    });
    rx
}

/// Counts `<path` occurrences across the `.svg` files in `dir`, returning
/// `(total_paths, file_count)`. `recurse` controls descent into
/// subdirectories. Shells out to ripgrep; any failure yields `(0, 0)`.
fn count_svg_paths(dir: &str, recurse: bool) -> (u64, u64) {
    let dir = crate::paths::expand_tilde(dir);
    let mut cmd = Command::new("rg");
    cmd.arg("--count-matches")
        .arg("--no-messages")
        .arg("--glob")
        .arg("*.svg");
    if !recurse {
        cmd.arg("--max-depth").arg("1");
    }
    cmd.arg("<path").arg(&dir);
    let Ok(output) = cmd.output() else {
        return (0, 0);
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut total = 0u64;
    let mut files = 0u64;
    for line in text.lines() {
        // ripgrep prints `path:count`; the count is after the final colon.
        if let Some(count) = line
            .rsplit(':')
            .next()
            .and_then(|n| n.trim().parse::<u64>().ok())
        {
            total += count;
            files += 1;
        }
    }
    (total, files)
}

/// Average `<path>` count per finished SVG in `done_dir` (0.0 if none).
fn avg_paths_per_svg(done_dir: &str) -> f64 {
    let (total, files) = count_svg_paths(done_dir, true);
    if files == 0 {
        0.0
    } else {
        total as f64 / files as f64
    }
}

/// Estimated number of completed in-progress SVGs: the total `<path>` count of
/// the (non-recursive) templates folder divided by the per-SVG average.
fn compute_svgs_completed(templates_dir: &str, avg_paths_per_svg: f64) -> f64 {
    if avg_paths_per_svg <= 0.0 {
        return 0.0;
    }
    let (total, _) = count_svg_paths(templates_dir, false);
    total as f64 / avg_paths_per_svg
}

/// Splits a money value into a big "-$1,234" dollar string and a ".5678"
/// fractional tail for the smaller line.
fn split_money(value: f64) -> (String, String) {
    let neg = value < 0.0;
    let abs = value.abs();
    let dollars = abs.trunc() as i64;
    let frac = ((abs.fract()) * 10000.0).round() as i64;
    let sign = if neg { "-" } else { "" };
    (
        format!("{sign}${}", group_digits(dollars)),
        format!(".{frac:04}"),
    )
}

fn fmt_money(value: f64, decimals: usize) -> String {
    let neg = value < 0.0;
    let abs = value.abs();
    let whole = abs.trunc() as i64;
    let sign = if neg { "-" } else { "" };
    if decimals == 0 {
        return format!("{sign}${}", group_digits(whole));
    }
    let scale = 10f64.powi(decimals as i32);
    let cents = (abs.fract() * scale).round() as i64;
    format!(
        "{sign}${}.{cents:0width$}",
        group_digits(whole),
        width = decimals
    )
}

fn group_digits(value: i64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn period(start_secs: f64, burn: f64) -> Period {
        Period {
            start_secs,
            burn,
            expenses: vec![("x".to_string(), burn)],
        }
    }

    #[test]
    fn parse_splits_dated_sections() {
        let text = "\
            # comment\n\
            [2026-01-01]\n\
            Rent = 700\n\
            Phone = 30.5\n\
            \n\
            [2026-03-01]\n\
            Rent = 750\n";
        let cfg = Config::parse(text).unwrap();
        assert_eq!(cfg.periods.len(), 2);
        assert!((cfg.periods[0].burn - 730.5).abs() < 1e-9);
        assert_eq!(cfg.periods[0].expenses.len(), 2);
        assert_eq!(cfg.periods[0].expenses[0], ("Rent".to_string(), 700.0));
        assert!((cfg.periods[1].burn - 750.0).abs() < 1e-9);
    }

    #[test]
    fn parse_sorts_periods_chronologically() {
        let text = "[2026-05-01]\nA = 5\n[2026-01-01]\nB = 1\n";
        let cfg = Config::parse(text).unwrap();
        assert!(cfg.periods[0].start_secs < cfg.periods[1].start_secs);
        assert!((cfg.periods[0].burn - 1.0).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_expense_before_section() {
        assert!(Config::parse("Rent = 700\n").is_err());
    }

    #[test]
    fn parse_rejects_empty_and_bad_amount() {
        assert!(Config::parse("# nothing here\n").is_err());
        assert!(Config::parse("[2026-01-01]\nRent = lots\n").is_err());
        assert!(Config::parse("[2026-01-01]\nRent\n").is_err());
    }

    #[test]
    fn ledger_sums_signed_amounts_and_skips_header_and_comments() {
        let csv = "\
            # a comment\n\
            title,amount,date\n\
            Paycheck,2000,2026-01-15\n\
            Rent,-700,\n\
            Coffee,-4.5\n";
        assert!((sum_ledger(csv).unwrap() - 1295.5).abs() < 1e-9);
    }

    #[test]
    fn ledger_empty_is_zero_and_bad_amount_errors() {
        assert_eq!(sum_ledger("title,amount,date\n").unwrap(), 0.0);
        assert!(sum_ledger("Rent,oops\n").is_err());
    }

    #[test]
    fn compute_view_integrates_burn_piecewise() {
        // 1 month at $300/mo then 1 month at $600/mo = $900 spent.
        let periods = [period(0.0, 300.0), period(MONTH_SECS, 600.0)];
        let now = 2.0 * MONTH_SECS;
        let view = compute_view(1000.0, &periods, now, 0.0, 0.0);
        assert_eq!(view.dollars, "$100");
        assert_eq!(view.fraction, ".0000");
        assert_eq!(view.color, palette_color::LIME);
        // Active rate is the latest period's burn.
        assert_eq!(view.monthly, "$600.00/mo");
        assert_eq!(view.days_left, "5.1d");
    }

    #[test]
    fn compute_view_ignores_future_periods() {
        let with_future = [
            period(0.0, 300.0),
            period(MONTH_SECS, 600.0),
            period(10.0 * MONTH_SECS, 9999.0),
        ];
        let now = 2.0 * MONTH_SECS;
        let a = compute_view(1000.0, &with_future, now, 0.0, 0.0);
        let b = compute_view(1000.0, &with_future[..2], now, 0.0, 0.0);
        assert_eq!(a.dollars, b.dollars);
        assert_eq!(a.monthly, b.monthly);
    }

    #[test]
    fn compute_view_no_spend_before_start() {
        let periods = [period(MONTH_SECS, 600.0)];
        let view = compute_view(500.0, &periods, 0.0, 0.0, 0.0);
        assert_eq!(view.dollars, "$500");
    }

    #[test]
    fn compute_view_goes_crimson_when_overdrawn() {
        let periods = [period(0.0, 600.0)];
        let view = compute_view(0.0, &periods, MONTH_SECS, 0.0, 0.0);
        assert_eq!(view.color, palette_color::CRIMSON);
        assert_eq!(view.days_left, "0d");
    }
}
