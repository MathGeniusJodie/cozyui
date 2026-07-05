//! Live budget countdown widget. Ports the standalone `budgit` HTML/JS toy:
//! a balance that bleeds away in real time based on a fixed monthly burn.

use std::error::Error;
use std::fs;
use std::process::Command;
use std::sync::mpsc::Receiver;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::localtime::days_from_civil;
use crate::palette_color;
use crate::text::BitmapFont;
use crate::{Framebuffer, Index, Palette};

const WIDTH: usize = 210;

const DAYS_PER_MONTH: f64 = 365.25 / 12.0;
/// Seconds in an average month (365.25 / 12 days).
const MONTH_SECS: f64 = DAYS_PER_MONTH * 24.0 * 3600.0;

/// Editable budget config (see `budgit.conf`), looked up in
/// `$XDG_CONFIG_HOME/cozyui/` first, then the source checkout.
const CONFIG_FILE: &str = "budgit.conf";

/// Ledger CSV whose entries sum to the starting balance; same lookup.
const LEDGER_FILE: &str = "budgit.csv";

const REFRESH: Duration = Duration::from_secs(1);

/// How long the background SVG counter waits between scans (measured from the
/// end of the previous scan, so a slow scan never overlaps the next).
const SVG_REFRESH: Duration = Duration::from_secs(1);

/// Default folder of finished templates; its SVGs set the "paths per finished
/// SVG" baseline used to estimate how many in-progress SVGs are complete.
/// Overridable in `budgit.conf` with `svg done dir = <path>`.
const DEFAULT_DONE_DIR: &str = "~/Desktop/allfiles/templates/done/";

/// Default folder of in-progress templates (the `done/` subfolder is
/// excluded). Overridable in `budgit.conf` with `svg templates dir = <path>`.
const DEFAULT_TEMPLATES_DIR: &str = "~/Desktop/allfiles/templates/";

const TOP_GAP: usize = 12;
const LABEL_GAP: usize = 4;
const FRACTION_GAP: usize = 2;
const STATS_GAP: usize = 14;
const STAT_ROW_H: usize = 12;

/// One budget period: a set of monthly expenses that takes effect on
/// `start_secs` (Unix seconds) and runs until the next period begins.
struct Period {
    start_secs: f64,
    /// Labelled monthly expenses for this period.
    expenses: Vec<(String, f64)>,
}

impl Period {
    /// Total monthly burn for this period: always the live sum of
    /// `expenses`, so it can never drift from a later mutation the way a
    /// field cached once at parse time could.
    fn burn(&self) -> f64 {
        self.expenses.iter().map(|(_, amount)| amount).sum()
    }
}

/// Parsed budget config: a chronological list of expense periods plus the
/// global `dollars per svg` payout used to credit completed SVG work.
struct Config {
    periods: Vec<Period>,
    dollars_per_svg: f64,
    /// Folder whose finished SVGs set the paths-per-SVG baseline.
    done_dir: String,
    /// Folder of in-progress SVG templates.
    templates_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            periods: Vec::new(),
            dollars_per_svg: 0.0,
            done_dir: DEFAULT_DONE_DIR.to_string(),
            templates_dir: DEFAULT_TEMPLATES_DIR.to_string(),
        }
    }
}

/// Parser state for the section currently being read: no `[date]` header seen
/// yet, inside one being discarded due to a bad date, or actively
/// accumulating expenses into a `Period`. Pushing an expense is only
/// reachable from the `Active` arm, so misattributing an expense to a
/// discarded section or one that doesn't exist yet is a compile-time
/// impossibility rather than a runtime `if skip_period` check a future call
/// site could forget.
enum ParseState {
    None,
    Skipped,
    Active(Period),
}

impl Config {
    /// Parses the config text. A `[YYYY-MM-DD]` header opens a new period and
    /// the `Label = amount` lines beneath it are that period's monthly
    /// expenses. A period's burn rate applies from its date until the next
    /// period begins, so editing a later period never rewrites past spending.
    /// `#` starts a comment only at the start of the (trimmed) line or when
    /// preceded by whitespace, so a `#` inside a label (e.g. `Repair (unit
    /// #4)`) doesn't truncate the line. Blank lines are ignored.
    fn parse(text: &str) -> Result<Self, Box<dyn Error>> {
        let mut periods: Vec<Period> = Vec::new();
        let mut state = ParseState::None;
        let mut dollars_per_svg = 0.0;
        let mut done_dir = DEFAULT_DONE_DIR.to_string();
        let mut templates_dir = DEFAULT_TEMPLATES_DIR.to_string();

        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(date) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                // Whatever section was active is done, successfully parsed or
                // not, now that the next one is starting.
                if let ParseState::Active(period) = std::mem::replace(&mut state, ParseState::None)
                {
                    periods.push(period);
                }
                // A bad date skips the whole period (header and its expenses)
                // rather than aborting the widget, so a typo in one section
                // can't take down the app or misattribute its expenses to the
                // previous period.
                match parse_date(date.trim()) {
                    Ok((y, m, d)) => {
                        // Resolve the header's civil date to local midnight
                        // (DST-aware) rather than raw UTC epoch arithmetic, so
                        // the period switches over at the date it names in
                        // the user's own timezone. `epoch_for_civil` wants a
                        // 0-based month, unlike `parse_date`'s 1-based one.
                        let start_secs = crate::localtime::epoch_for_civil(y, m - 1, d, 0, 0, 0)
                            .map_or_else(
                                || (days_from_civil(y, m, d) * 86400) as f64,
                                |epoch| epoch as f64,
                            );
                        state = ParseState::Active(Period {
                            start_secs,
                            expenses: Vec::new(),
                        });
                    }
                    Err(err) => {
                        eprintln!("budgit.conf: skipping [{}] section: {err}", date.trim());
                        state = ParseState::Skipped;
                    }
                }
                continue;
            }
            let (label, value) = line
                .split_once('=')
                .ok_or_else(|| format!("budgit.conf: missing '=' in line: {raw}"))?;
            let label = label.trim();
            let value = value.trim();
            // Global settings (not tied to any expense period).
            if label.eq_ignore_ascii_case("svg done dir") {
                done_dir = value.to_string();
                continue;
            }
            if label.eq_ignore_ascii_case("svg templates dir") {
                templates_dir = value.to_string();
                continue;
            }
            if label.eq_ignore_ascii_case("dollars per svg") {
                let value = value.trim_matches('$').trim();
                match value.parse() {
                    Ok(amount) => dollars_per_svg = amount,
                    Err(_) => {
                        eprintln!("budgit.conf: skipping bad amount for {label}: {value}");
                    }
                }
                continue;
            }
            let amount: f64 = match value.parse() {
                Ok(amount) => amount,
                Err(_) => {
                    eprintln!("budgit.conf: skipping bad amount for {label}: {value}");
                    continue;
                }
            };
            match &mut state {
                ParseState::Active(period) => period.expenses.push((label.to_string(), amount)),
                ParseState::Skipped => {}
                ParseState::None => {
                    return Err("budgit.conf: expense before any [date] section".into());
                }
            }
        }

        if let ParseState::Active(period) = state {
            periods.push(period);
        }

        if periods.is_empty() {
            return Err("budgit.conf: no [date] sections found".into());
        }
        periods.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
        Ok(Self {
            periods,
            dollars_per_svg,
            done_dir,
            templates_dir,
        })
    }
}

/// Strips a trailing `#` comment from a line. A `#` only starts a comment at
/// the start of the (trimmed) line, or when it stands as its own token (set
/// off by whitespace on both sides, or trailing at end of line). That keeps a
/// `#` embedded in a title or label (e.g. `Repair (unit #4)`) from being
/// mistaken for a comment marker, since there it's glued to the digit that
/// follows rather than set off by whitespace.
fn strip_comment(raw: &str) -> &str {
    if raw.trim_start().starts_with('#') {
        return "";
    }
    // Walk chars, not bytes: a UTF-8 continuation byte can numerically match
    // a whitespace codepoint (e.g. U+00A0, U+0085) when cast to `char` on its
    // own, which would misfire "preceded/followed by whitespace" for a
    // non-ASCII label adjacent to a literal `#`.
    let mut prev_whitespace = false;
    let mut chars = raw.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        let followed_by_space_or_end = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if ch == '#' && prev_whitespace && followed_by_space_or_end {
            return &raw[..i];
        }
        prev_whitespace = ch.is_whitespace();
    }
    raw
}

/// Sums the ledger CSV in `budgit.csv` into the starting balance. Income is
/// positive, expenses negative. Columns are `title,amount,date`; the header
/// row, blank lines, and `#` comments are skipped, and the date is optional
/// (only the amount is used). A missing file means a zero balance.
fn load_ledger_balance() -> Result<f64, Box<dyn Error>> {
    match fs::read_to_string(crate::paths::config_file(LEDGER_FILE)) {
        Ok(text) => sum_ledger(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0.0),
        Err(err) => Err(err.into()),
    }
}

/// Sums the amount column of ledger CSV text. See [`load_ledger_balance`].
/// A malformed line is skipped (with a warning naming the line number) rather
/// than aborting the whole widget.
fn sum_ledger(text: &str) -> Result<f64, Box<dyn Error>> {
    let mut balance = 0.0;
    for (i, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        // Parse from the right: if a date column is present, the last field
        // is the date and the second-to-last is the amount; otherwise (no
        // date) the last field is the amount. Everything before that is the
        // title, which may itself contain commas.
        let mut fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 2 {
            eprintln!(
                "budgit.csv: skipping malformed line {}: too few columns",
                i + 1
            );
            continue;
        }
        if fields.len() >= 3 {
            fields.pop(); // date (unused)
        }
        let amount = fields.pop().unwrap_or("").trim();
        let title = fields.join(",");
        let title = title.trim();
        // Skip the header row.
        if amount.eq_ignore_ascii_case("amount") {
            continue;
        }
        match amount.parse::<f64>() {
            Ok(value) => balance += value,
            Err(_) => {
                eprintln!(
                    "budgit.csv: skipping malformed line {}: bad amount for {title:?}: {amount}",
                    i + 1
                );
            }
        }
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
    let (y, m, d) = (next()?, next()?, next()?);
    if !(1..=12).contains(&m) || !(1..=days_in_month(y, m)).contains(&d) {
        return Err(format!("budgit.conf: bad date: {value}").into());
    }
    Ok((y, m, d))
}

/// Number of days in `month` (1-based) of `year`, with the Gregorian
/// leap-year rule.
fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        _ => 28,
    }
}

/// Whether `year` is a Gregorian leap year.
fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Vertical layout for the widget's sections, shared by [`Budgit::load`]
/// (which only needs the total `height`) and [`Budgit::render`] (which needs
/// each section's starting `y`) so the two can't drift apart.
struct Layout {
    label_y: usize,
    balance_y: usize,
    fraction_y: usize,
    stats_y: usize,
    height: usize,
}

/// Computes the layout given the balance line's ink height (which depends on
/// the digits actually being drawn; see the two call sites).
fn compute_layout(label_font: &BitmapFont, stat_font: &BitmapFont, balance_h: usize) -> Layout {
    let label_y = TOP_GAP;
    let balance_y = label_y + label_font.cell_h() + LABEL_GAP;
    let fraction_y = balance_y + balance_h + FRACTION_GAP;
    let stats_y = fraction_y + label_font.cell_h() + STATS_GAP;
    // The last stat row contributes its full text height rather than the row
    // advance, so the widget ends exactly at the bottom of the last row.
    let height = stats_y + 3 * STAT_ROW_H + label_font.cell_h().max(stat_font.cell_h());
    Layout {
        label_y,
        balance_y,
        fraction_y,
        stats_y,
        height,
    }
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
    view: crate::util::Refresh<BudgetView>,
    /// Widget height, computed from the fonts so it ends exactly at the last
    /// stat row (mirrors the layout math in `render`).
    height: usize,
}

impl Budgit {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let label_font = BitmapFont::load_with_fallback(
            &pixel_fonts::PIXOLDE_BOLD_SPEC,
            &pixel_fonts::FUSION_PIXEL_12_SPEC,
        )?;
        let balance_font = BitmapFont::load(&pixel_fonts::ROZHA_ONE_48_SPEC)?;
        let stat_font = BitmapFont::load_with_fallback(
            &pixel_fonts::PIXOLDE_SPEC,
            &pixel_fonts::FUSION_PIXEL_8_SPEC,
        )?;

        let config = match fs::read_to_string(crate::paths::config_file(CONFIG_FILE)) {
            Ok(text) => match Config::parse(&text) {
                Ok(config) => config,
                Err(err) => {
                    eprintln!("budgit.conf: skipping malformed config: {err}");
                    Config::default()
                }
            },
            // A fresh install has no budgit.conf yet, and any other read
            // error (permissions, a remounted config dir, ...) is just as
            // unworkable — in both cases use defaults rather than failing
            // App::load and taking down the whole desktop.
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("budgit.conf: skipping unreadable config: {err}");
                }
                Config::default()
            }
        };
        let start_balance = load_ledger_balance()?;
        // The estimate starts at zero and is filled in by the background
        // counter once its first off-thread scan completes.
        let svgs_completed = 0.0;
        let svg_rx = spawn_svg_counter(config.done_dir.clone(), config.templates_dir.clone());
        let view = compute_view(
            start_balance,
            &config.periods,
            crate::util::now_secs(),
            svgs_completed,
            config.dollars_per_svg,
        );

        // The balance line's height comes from digit ink bounds (a
        // representative worst case, since the real balance isn't known yet)
        // so the reserved height matches what `render` actually draws; see
        // `compute_layout`.
        let balance_h = balance_font
            .text_ink_bounds("-$0123456789,")
            .map_or_else(|| balance_font.cell_h(), |b| b.height());
        let height = compute_layout(&label_font, &stat_font, balance_h).height;

        Ok(Self {
            label_font,
            balance_font,
            stat_font,
            start_balance,
            periods: config.periods,
            dollars_per_svg: config.dollars_per_svg,
            svgs_completed,
            svg_rx,
            view: crate::util::Refresh::new(view),
            height,
        })
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn width(&self) -> usize {
        WIDTH
    }

    pub(crate) const fn height(&self) -> usize {
        self.height
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn fill_color(&self, _palette: &Palette) -> Index {
        palette_color::BLACK
    }

    pub(crate) fn update(&mut self) -> bool {
        // Drain any estimates produced by the background counter, keeping the
        // most recent. This never blocks the render thread.
        while let Ok(completed) = self.svg_rx.try_recv() {
            self.svgs_completed = completed;
        }
        let (start_balance, svgs_completed, dollars_per_svg) = (
            self.start_balance,
            self.svgs_completed,
            self.dollars_per_svg,
        );
        let periods = &self.periods;
        self.view.refresh(REFRESH, || {
            compute_view(
                start_balance,
                periods,
                crate::util::now_secs(),
                svgs_completed,
                dollars_per_svg,
            )
        })
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, _palette: &Palette) {
        let view = self.view.get();
        let muted = palette_color::CREAM;

        // Big balance (dollars) is positioned by its ink bounds so the tall
        // cell box doesn't blow out the layout; the layout math (shared with
        // `load`'s height computation) is derived from that ink height.
        let balance_h = self
            .balance_font
            .text_ink_bounds(&view.dollars)
            .map_or_else(|| self.balance_font.cell_h(), |b| b.height());
        let layout = compute_layout(&self.label_font, &self.stat_font, balance_h);

        // "MONEY LEFT" label.
        self.draw_centered(fb, &self.label_font, "MONEY LEFT", layout.label_y, muted);

        // Big balance (dollars) in the calendar's big font.
        self.draw_centered_tight(
            fb,
            &self.balance_font,
            &view.dollars,
            layout.balance_y,
            view.color,
        );

        // Fractional cents underneath, bold and in the balance color.
        self.draw_centered(
            fb,
            &self.label_font,
            &view.fraction,
            layout.fraction_y,
            view.color,
        );

        // Stat rows.
        let rows = [
            ("Monthly", view.monthly.as_str()),
            ("Daily", view.daily.as_str()),
            ("Days left", view.days_left.as_str()),
            ("~ svgs completed", view.svgs_completed.as_str()),
        ];
        let mut y = layout.stats_y;
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
        spent += active / MONTH_SECS * period.burn();
        if period.start_secs <= now {
            burn = period.burn();
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
/// thread. It computes the finished-SVG path average once, then loops sending
/// an estimate and sleeping `SVG_REFRESH` *after* the scan finishes (so a slow
/// scan never overlaps the next). The ripgrep scan only reruns when the
/// folder's stamp changes; unchanged ticks resend the cached estimate, which
/// also keeps the receiver-dropped exit path exercised. The thread exits when
/// the receiver is dropped.
fn spawn_svg_counter(done_dir: String, templates_dir: String) -> Receiver<f64> {
    let mut avg = None;
    let mut stamp = None;
    let mut completed = 0.0;
    crate::util::spawn_poller(SVG_REFRESH, move || {
        let avg = *avg.get_or_insert_with(|| avg_paths_per_svg(&done_dir));
        let current = svg_dir_stamp(&templates_dir);
        if stamp != Some(current) {
            stamp = Some(current);
            completed = compute_svgs_completed(&templates_dir, avg);
        }
        Some(completed)
    })
}

/// Change stamp for the templates folder: the `.svg` count and newest mtime at
/// depth 1 (matching the non-recursive scan there). `None` when the folder
/// can't be read.
fn svg_dir_stamp(dir: &str) -> Option<(u64, SystemTime)> {
    let entries = fs::read_dir(crate::paths::expand_tilde(dir)).ok()?;
    let mut count = 0;
    let mut newest = UNIX_EPOCH;
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "svg") {
            count += 1;
            if let Ok(mtime) = entry.metadata().and_then(|meta| meta.modified()) {
                newest = newest.max(mtime);
            }
        }
    }
    Some((count, newest))
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
    // Round once at display precision, then split: rounding the fraction on
    // its own could carry it to a whole unit ($123.99995 -> "$123.10000").
    let scaled = (value * 10000.0).round() as i64;
    let sign = if scaled < 0 { "-" } else { "" };
    let scaled = scaled.abs();
    (
        format!("{sign}${}", group_digits(scaled / 10000)),
        format!(".{:04}", scaled % 10000),
    )
}

fn fmt_money(value: f64, decimals: usize) -> String {
    let scale = 10f64.powi(decimals as i32) as i64;
    let scaled = (value * scale as f64).round() as i64;
    let sign = if scaled < 0 { "-" } else { "" };
    let scaled = scaled.abs();
    let whole = scaled / scale;
    if decimals == 0 {
        return format!("{sign}${}", group_digits(whole));
    }
    format!(
        "{sign}${}.{:0width$}",
        group_digits(whole),
        scaled % scale,
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

impl crate::widget::Widget for Budgit {
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

    fn period(start_secs: f64, burn: f64) -> Period {
        Period {
            start_secs,
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
        assert!((cfg.periods[0].burn() - 730.5).abs() < 1e-9);
        assert_eq!(cfg.periods[0].expenses.len(), 2);
        assert_eq!(cfg.periods[0].expenses[0], ("Rent".to_string(), 700.0));
        assert!((cfg.periods[1].burn() - 750.0).abs() < 1e-9);
    }

    #[test]
    fn parse_sorts_periods_chronologically() {
        let text = "[2026-05-01]\nA = 5\n[2026-01-01]\nB = 1\n";
        let cfg = Config::parse(text).unwrap();
        assert!(cfg.periods[0].start_secs < cfg.periods[1].start_secs);
        assert!((cfg.periods[0].burn() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_expense_before_section() {
        assert!(Config::parse("Rent = 700\n").is_err());
    }

    #[test]
    fn parse_rejects_empty_and_bad_amount() {
        assert!(Config::parse("# nothing here\n").is_err());
        // A bad amount is skipped (with a warning) rather than aborting the
        // whole config, mirroring the bad-date handling for [date] sections.
        let config = Config::parse("[2026-01-01]\nRent = lots\n").unwrap();
        assert!(config.periods[0].expenses.is_empty());
        assert!(Config::parse("[2026-01-01]\nRent\n").is_err());
    }

    #[test]
    fn parse_allows_hash_inside_label() {
        let text = "[2026-01-01]\nRepair (unit #4) = 50\n";
        let cfg = Config::parse(text).unwrap();
        assert_eq!(
            cfg.periods[0].expenses[0],
            ("Repair (unit #4)".to_string(), 50.0)
        );
    }

    #[test]
    fn strip_comment_is_not_fooled_by_a_continuation_byte() {
        // 'נ' (U+05E0) encodes as bytes [0xD7, 0xA0]; 0xA0 alone numerically
        // matches U+00A0 (NBSP), a whitespace codepoint. Casting that byte to
        // `char` in isolation (rather than decoding the whole UTF-8 sequence)
        // would misread it as trailing whitespace and wrongly treat the `#`
        // right after it as a comment marker.
        assert_eq!(strip_comment("נ# = 50"), "נ# = 50");
    }

    #[test]
    fn parse_keeps_default_on_bad_dollars_per_svg() {
        let text = "dollars per svg = oops\n[2026-01-01]\nRent = 700\n";
        let cfg = Config::parse(text).unwrap();
        assert_eq!(cfg.dollars_per_svg, 0.0);
    }

    #[test]
    fn parse_date_rejects_invalid_day_for_month() {
        assert!(parse_date("2026-02-30").is_err());
        assert!(parse_date("2026-04-31").is_err());
        assert!(parse_date("2026-02-29").is_err()); // not a leap year
        assert!(parse_date("2024-02-29").is_ok()); // leap year
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
    fn ledger_allows_hash_inside_title() {
        let csv = "Repair (unit #4),-50,2026-01-15\n";
        assert!((sum_ledger(csv).unwrap() - (-50.0)).abs() < 1e-9);
    }

    #[test]
    fn ledger_empty_is_zero_and_bad_amount_is_skipped() {
        assert_eq!(sum_ledger("title,amount,date\n").unwrap(), 0.0);
        assert_eq!(sum_ledger("Rent,oops\n").unwrap(), 0.0);
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
