//! Toodle's persistence layer: the todo markdown files on disk are the source
//! of truth and may be rewritten at any time by other programs (sync clients,
//! editors). Each section is managed by a [`SectionStore`] that tracks the
//! exact disk state it was last synced with, detects external changes cheaply
//! via file fingerprints, and folds them in with a three-way line merge before
//! any write — so the widget can never clobber an edit it has not seen.

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::mpsc;

pub(super) const LINE_COUNT: usize = 6;
pub(super) const SECTION_COUNT: usize = 4;
/// Cap on how many pages a single category can show; extra overflow pages are
/// simply not navigable.
pub(super) const MAX_PAGES_PER_SECTION: usize = 4;

/// Config file naming the directory that holds every toodle markdown file. The
/// first non-blank, non-comment line is the root path (`~` expands to `$HOME`).
/// Looked up in `$XDG_CONFIG_HOME/cozyui/` first, then the source checkout.
const TOODLE_CONF_FILE: &str = "toodle.conf";
/// Root used when `toodle.conf` is missing or blank.
const DEFAULT_TOODLE_ROOT: &str = "~/Desktop/RemoteVault/✅ Toodle/";
const TODO_FILE_NAMES: [&str; SECTION_COUNT] = [
    "toodle_urgent.md",
    "toodle_frog.md",
    "toodle_normal.md",
    "toodle_snail.md",
];
/// Completed todos are filed under here, one file per day they were finished.
const DONE_DIR_NAME: &str = "toodle_done";
const ARCHIVE_TRANSACTION_NAME: &str = "toodle_archive_transaction.json";

/// Root directory for all toodle markdown files, configurable via `toodle.conf`.
/// Resolved once and cached; falls back to [`DEFAULT_TOODLE_ROOT`] when the
/// config is missing or contains no usable path.
pub(super) fn toodle_root() -> &'static str {
    static ROOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let configured = fs::read_to_string(crate::paths::config_file(TOODLE_CONF_FILE))
            .ok()
            .and_then(|text| {
                text.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| DEFAULT_TOODLE_ROOT.to_owned());
        let expanded = crate::paths::expand_tilde(&configured);
        expanded.trim_end_matches('/').to_owned()
    })
}

pub(super) fn todo_file(section: usize) -> String {
    format!("{}/{}", toodle_root(), TODO_FILE_NAMES[section])
}

pub(super) fn done_dir() -> String {
    format!("{}/{DONE_DIR_NAME}", toodle_root())
}

pub(super) fn archive_transaction_path() -> String {
    format!("{}/{ARCHIVE_TRANSACTION_NAME}", toodle_root())
}

/// Path of the done-todo file for a given date and priority tag, the single
/// source of the `<root>/YYYY-MM-DD_<tag>.md` naming convention (shared with
/// the stats widget).
pub(crate) fn done_file_path(year: i32, month: i32, day: i32, tag: &str) -> String {
    format!("{}/{year:04}-{month:02}-{day:02}_{tag}.md", done_dir())
}

/// Path of the done-todo file for `section` today, named by date and priority.
pub(super) fn daily_done_path(section: usize) -> String {
    let tm = crate::localtime::local_time().unwrap_or_default();
    done_file_path(
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        section_tag(section),
    )
}

/// Priorities in stacking order (bottom of the bar first), matching toodle's
/// section order: urgent, frog, normal, snail. Shared with `stats.rs`, which
/// renders these as a stacked bar graph.
pub(crate) const PRIORITY_TAGS: [&str; SECTION_COUNT] = ["urgent", "frog", "normal", "snail"];

/// Priority tag used in daily done filenames (`YYYY-MM-DD_<tag>.md`).
pub(super) const fn section_tag(section: usize) -> &'static str {
    PRIORITY_TAGS[section % SECTION_COUNT]
}

/// Identity of one on-disk file version. The inode is included so an atomic
/// rename-over (how sync tools and we ourselves write) is always detected even
/// if size and mtime happen to match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Fingerprint {
    ino: u64,
    size: u64,
    mtime_s: i64,
    mtime_ns: i64,
}

/// The file's current fingerprint, or `None` if it does not exist.
fn fingerprint(path: &str) -> io::Result<Option<Fingerprint>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(Fingerprint {
            ino: meta.ino(),
            size: meta.size(),
            mtime_s: meta.mtime(),
            mtime_ns: meta.mtime_nsec(),
        })),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Read the whole file, treating a missing file as empty (files can vanish
/// between a stat and the read when another program rewrites them).
fn read_or_empty(path: &str) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err),
    }
}

#[derive(Clone)]
pub(super) struct TodoItem {
    pub(super) text: String,
    pub(super) checked: bool,
    /// Whether this line is a markdown checkbox (`- [ ]` / `- [x]`). Plain lines
    /// from the file are kept verbatim and rendered without a checkbox to their
    /// left. New lines typed in the widget default to checkboxes.
    pub(super) is_checkbox: bool,
}

/// New lines created in the widget are checkbox todos by default; plain lines
/// only arise from non-checkbox text in the backing file.
impl Default for TodoItem {
    fn default() -> Self {
        Self {
            text: String::new(),
            checked: false,
            is_checkbox: true,
        }
    }
}

impl TodoItem {
    pub(super) fn parse(line: &str) -> Self {
        for (prefix, checked) in [("- [x]", true), ("- [X]", true), ("- [ ]", false)] {
            if let Some(rest) = line.strip_prefix(prefix) {
                return Self {
                    text: rest.strip_prefix(' ').unwrap_or(rest).to_string(),
                    checked,
                    is_checkbox: true,
                };
            }
        }
        Self {
            text: line.to_string(),
            checked: false,
            is_checkbox: false,
        }
    }

    pub(super) fn serialize(&self) -> String {
        if !self.is_checkbox {
            self.text.clone()
        } else if self.checked {
            format!("- [x] {}", self.text)
        } else {
            format!("- [ ] {}", self.text)
        }
    }

    /// Whether a checkbox should be drawn to the left of this line. Plain lines
    /// never show one; a checkbox todo shows one once it has content, is
    /// checked, or is the line currently being edited.
    pub(super) const fn renders_checkbox(&self, focused: bool) -> bool {
        self.is_checkbox && (focused || self.checked || !self.text.is_empty())
    }

    pub(super) const fn is_blank(&self) -> bool {
        !self.checked && self.text.is_empty()
    }
}

#[derive(Clone)]
pub(super) struct TodoList {
    pub(super) items: Vec<TodoItem>,
}

impl TodoList {
    pub(super) fn from_text(text: &str) -> Self {
        let mut list = Self {
            items: text.lines().map(TodoItem::parse).collect(),
        };
        list.trim_trailing_blank_items();
        list
    }

    pub(super) fn page_count(&self) -> usize {
        let pages_with_items = self.items.len().div_ceil(LINE_COUNT).max(1);
        let last_page_start = (pages_with_items - 1) * LINE_COUNT;
        let last_page_full = !self.items.is_empty()
            && (last_page_start..last_page_start + LINE_COUNT)
                .all(|index| self.items.get(index).is_some_and(|item| !item.is_blank()));
        let count = if last_page_full {
            pages_with_items + 1
        } else {
            pages_with_items
        };
        count.min(MAX_PAGES_PER_SECTION)
    }

    pub(super) fn item(&self, page: usize, line: usize) -> &TodoItem {
        static BLANK_ITEM: TodoItem = TodoItem {
            text: String::new(),
            checked: false,
            is_checkbox: true,
        };
        self.items
            .get(page * LINE_COUNT + line)
            .unwrap_or(&BLANK_ITEM)
    }

    pub(super) fn item_mut(&mut self, page: usize, line: usize) -> &mut TodoItem {
        let index = page * LINE_COUNT + line;
        if self.items.len() <= index {
            self.items.resize_with(index + 1, TodoItem::default);
        }
        &mut self.items[index]
    }

    pub(super) fn delete_item(&mut self, page: usize, line: usize) -> bool {
        let index = page * LINE_COUNT + line;
        if index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        true
    }

    pub(super) fn trim_trailing_blank_items(&mut self) {
        while self.items.last().is_some_and(TodoItem::is_blank) {
            self.items.pop();
        }
    }

    pub(super) fn serialized_text(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }

        let text = self
            .items
            .iter()
            .map(TodoItem::serialize)
            .collect::<Vec<_>>()
            .join("\n");
        format!("{text}\n")
    }

    /// The items as normalized serialized lines, the form all merging works in.
    fn serialized_lines(&self) -> Vec<String> {
        self.items.iter().map(TodoItem::serialize).collect()
    }
}

/// One section's todo list plus everything needed to keep it in lockstep with
/// its backing file: `base` is the exact disk text the list was last synced
/// with (loaded, merged, or written), and `fingerprint` identifies that disk
/// version so external rewrites are detected without reading the file.
pub(super) struct SectionStore {
    list: TodoList,
    base: String,
    fingerprint: Option<Fingerprint>,
    dirty: bool,
    /// Text handed to the save worker and not yet confirmed written; while
    /// set, disk syncing is suspended (the file is about to be replaced by
    /// our own rename, which must not read as an external change).
    saving: Option<String>,
}

impl SectionStore {
    pub(super) fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let fingerprint = fingerprint(path)?;
        let base = if fingerprint.is_some() {
            read_or_empty(path)?
        } else {
            String::new()
        };
        Ok(Self {
            list: TodoList::from_text(&base),
            base,
            fingerprint,
            dirty: false,
            saving: None,
        })
    }

    pub(super) const fn list(&self) -> &TodoList {
        &self.list
    }

    pub(super) const fn list_mut(&mut self) -> &mut TodoList {
        &mut self.list
    }

    /// Flag unsaved in-memory edits; they reach disk via [`Self::save`].
    pub(super) const fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub(super) const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Detect and fold in an external change to the backing file. Unsaved
    /// in-app edits are preserved by a three-way merge and re-flagged dirty so
    /// the merged result is written back. Returns whether the list changed.
    pub(super) fn absorb_external(&mut self, path: &str) -> Result<bool, Box<dyn Error>> {
        if self.saving.is_some() {
            // Our own rename is in flight; the fingerprint is stale by
            // construction. complete_save re-arms syncing with the written
            // version, and any genuinely external rewrite after that still
            // mismatches it and is folded in on the next poll.
            return Ok(false);
        }
        // Stat before reading: if the file changes again mid-read, the stale
        // fingerprint recorded here guarantees the next poll re-syncs.
        let disk_fingerprint = fingerprint(path)?;
        if disk_fingerprint == self.fingerprint {
            return Ok(false);
        }
        let disk_text = read_or_empty(path)?;
        if disk_text == self.base {
            // Content is what we already synced with (e.g. a bare touch).
            self.fingerprint = disk_fingerprint;
            return Ok(false);
        }

        let base_lines = normalized_lines(&self.base);
        let their_lines = normalized_lines(&disk_text);
        let our_lines = self.list.serialized_lines();
        let merged = merge_lines(&base_lines, &our_lines, &their_lines);

        let changed = merged != our_lines;
        self.dirty = merged != their_lines;
        self.list.items = merged.iter().map(|line| TodoItem::parse(line)).collect();
        self.list.trim_trailing_blank_items();
        self.base = disk_text;
        self.fingerprint = disk_fingerprint;
        Ok(changed)
    }

    /// Write the list to its backing file, first absorbing any external change
    /// so the write can never be based on stale disk state. Returns whether an
    /// external change altered the list (the caller may need UI fixups).
    pub(super) fn save(&mut self, path: &str) -> Result<bool, Box<dyn Error>> {
        let externally_changed = self.absorb_external(path)?;
        self.list.trim_trailing_blank_items();
        let text = self.list.serialized_text();
        let written = AtomicWrite::stage(path, text.as_bytes())?.commit()?;
        self.base = text;
        self.fingerprint = Some(written);
        self.dirty = false;
        Ok(externally_changed)
    }

    pub(super) fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// Start an asynchronous save: absorb any external change first (so the
    /// write is never based on stale disk state), then return the serialized
    /// text for the caller to hand to the [`SaveWorker`]. The text is `None`
    /// when a save is already in flight — `dirty` is left set so the debounce
    /// retries after that save completes. Returns whether the absorb altered
    /// the list (the caller may need UI fixups).
    pub(super) fn begin_save(
        &mut self,
        path: &str,
    ) -> Result<(bool, Option<String>), Box<dyn Error>> {
        if self.saving.is_some() {
            return Ok((false, None));
        }
        let externally_changed = self.absorb_external(path)?;
        self.list.trim_trailing_blank_items();
        let text = self.list.serialized_text();
        self.saving = Some(text.clone());
        self.dirty = false;
        Ok((externally_changed, Some(text)))
    }

    /// Fold in the save worker's outcome: on success the written text becomes
    /// the new synced base; on failure the section is re-flagged dirty so the
    /// next debounce retries.
    pub(super) fn complete_save(&mut self, result: Result<Fingerprint, String>) {
        let Some(text) = self.saving.take() else {
            return;
        };
        match result {
            Ok(written) => {
                self.base = text;
                self.fingerprint = Some(written);
            }
            Err(err) => {
                eprintln!("toodle background save failed: {err}");
                self.dirty = true;
            }
        }
    }

    /// Adopt state that was already committed to disk elsewhere (the archive
    /// transaction writes section files itself).
    pub(super) fn adopt(&mut self, list: TodoList, base: String, fingerprint: Fingerprint) {
        self.list = list;
        self.base = base;
        self.fingerprint = Some(fingerprint);
        self.dirty = false;
    }
}

/// A file's lines normalized through parse/serialize so formatting variants
/// (`- [X]` vs `- [x]`) compare equal across base, ours, and theirs.
fn normalized_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| TodoItem::parse(line).serialize())
        .collect()
}

/// Three-way merge of line-based todo files: the external edit (`theirs` vs
/// `base`) is applied on top of unsaved in-app edits (`ours`). Lines are
/// matched by content as a multiset — an external deletion only removes a line
/// we have not modified, external additions are appended, and a line edited on
/// both sides survives as both versions. Nothing is ever silently lost.
fn merge_lines(base: &[String], ours: &[String], theirs: &[String]) -> Vec<String> {
    if ours == base {
        return theirs.to_vec();
    }
    if theirs == base {
        return ours.to_vec();
    }

    // Per distinct line: how many copies theirs has gained (+) or lost (-)
    // relative to base.
    let mut delta: HashMap<&str, i64> = HashMap::new();
    for line in theirs {
        *delta.entry(line).or_insert(0) += 1;
    }
    for line in base {
        *delta.entry(line).or_insert(0) -= 1;
    }

    let mut merged = Vec::with_capacity(ours.len());
    for line in ours {
        if let Some(count) = delta.get_mut(line.as_str())
            && *count < 0
        {
            // Deleted externally and unmodified by us: let the deletion win.
            *count += 1;
            continue;
        }
        merged.push(line.clone());
    }
    for line in theirs {
        if let Some(count) = delta.get_mut(line.as_str())
            && *count > 0
        {
            *count -= 1;
            merged.push(line.clone());
        }
    }
    merged
}

/// Today's per-priority archived-todo counts (the gold-star tally), kept fresh
/// by re-counting whenever the daily done file changes on disk — including the
/// path itself changing at midnight.
pub(super) struct DoneCounts {
    sections: [DoneFile; SECTION_COUNT],
}

struct DoneFile {
    path: String,
    fingerprint: Option<Fingerprint>,
    count: usize,
}

impl DoneCounts {
    pub(super) fn load() -> Result<Self, Box<dyn Error>> {
        let mut counts = Self {
            sections: std::array::from_fn(|_| DoneFile {
                path: String::new(),
                fingerprint: None,
                count: 0,
            }),
        };
        counts.refresh()?;
        Ok(counts)
    }

    /// Re-count any section whose daily done file (or its date-based path)
    /// changed. Returns whether any count changed.
    pub(super) fn refresh(&mut self) -> Result<bool, Box<dyn Error>> {
        let mut changed = false;
        for (section, state) in self.sections.iter_mut().enumerate() {
            let path = daily_done_path(section);
            let fingerprint = fingerprint(&path)?;
            if path == state.path && fingerprint == state.fingerprint {
                continue;
            }
            let count = read_or_empty(&path)?
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    !trimmed.is_empty() && !trimmed.starts_with('#')
                })
                .count();
            changed |= count != state.count;
            *state = DoneFile {
                path,
                fingerprint,
                count,
            };
        }
        Ok(changed)
    }

    pub(super) fn count(&self, section: usize) -> usize {
        self.sections[section].count
    }
}

pub(super) struct AtomicWrite {
    path: String,
    temp_path: String,
}

impl AtomicWrite {
    pub(super) fn stage(path: &str, contents: impl AsRef<[u8]>) -> Result<Self, Box<dyn Error>> {
        let temp_path = crate::util::unique_temp_path(path);
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;
            file.write_all(contents.as_ref())?;
            file.sync_all()?;
        }
        Ok(Self {
            path: path.to_string(),
            temp_path,
        })
    }

    /// Rename the temp file into place and return the resulting file's
    /// fingerprint. The fingerprint is taken from the temp file *before* the
    /// rename (which preserves inode and mtime): statting the final path
    /// afterwards could race with an external writer and adopt a foreign
    /// version's fingerprint as our own, masking that change forever.
    pub(super) fn commit(self) -> Result<Fingerprint, Box<dyn Error>> {
        let written = fingerprint(&self.temp_path)?.ok_or("staged temp file vanished")?;
        fs::rename(&self.temp_path, &self.path)?;
        // The rename itself is not durable until the directory is fsynced;
        // without this, a crash right after "saved" can silently revert the
        // file to its previous version on some filesystems.
        sync_parent_dir(&self.path)?;
        Ok(written)
    }
}

/// fsync the directory containing `path`, making a just-committed rename in
/// it durable.
fn sync_parent_dir(path: &str) -> io::Result<()> {
    let parent = Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Background writer running section saves (write, fsync, rename, directory
/// fsync) off the UI thread; done inline from the input path, the fsyncs
/// caused visible frame hitches on slow disks. Jobs and results both carry
/// the section index; the single worker thread keeps saves ordered.
pub(super) struct SaveWorker {
    jobs: mpsc::Sender<SaveJob>,
    results: mpsc::Receiver<(usize, Result<Fingerprint, String>)>,
}

struct SaveJob {
    section: usize,
    path: String,
    text: String,
}

impl SaveWorker {
    pub(super) fn spawn() -> Self {
        let (jobs, job_rx) = mpsc::channel::<SaveJob>();
        let (result_tx, results) = mpsc::channel();
        std::thread::spawn(move || {
            // Exits when the Toodle (and with it the job sender) is dropped.
            while let Ok(job) = job_rx.recv() {
                let result = AtomicWrite::stage(&job.path, job.text.as_bytes())
                    .and_then(AtomicWrite::commit)
                    .map_err(|err| err.to_string());
                if result_tx.send((job.section, result)).is_err() {
                    return;
                }
            }
        });
        Self { jobs, results }
    }

    pub(super) fn submit(&self, section: usize, path: String, text: String) {
        let _ = self.jobs.send(SaveJob {
            section,
            path,
            text,
        });
    }

    pub(super) fn try_result(&self) -> Option<(usize, Result<Fingerprint, String>)> {
        self.results.try_recv().ok()
    }

    /// Blocking receive, for the flush paths that must not proceed while a
    /// save is still in flight.
    pub(super) fn wait_result(&self) -> Option<(usize, Result<Fingerprint, String>)> {
        self.results.recv().ok()
    }
}

impl Drop for AtomicWrite {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temp_path);
    }
}

pub(super) fn write_archive_transaction_marker(
    marker_path: &str,
    staged_writes: &[&AtomicWrite],
) -> Result<(), Box<dyn Error>> {
    let writes = staged_writes
        .iter()
        .map(|write| {
            serde_json::json!({
                "path": write.path.as_str(),
                "temp_path": write.temp_path.as_str(),
            })
        })
        .collect::<Vec<_>>();
    let marker = serde_json::to_vec(&writes)?;
    let transaction_marker = AtomicWrite::stage(marker_path, marker)?;
    transaction_marker.commit()?;
    Ok(())
}

pub(super) fn recover_archive_transaction(marker_path: &str) -> Result<(), Box<dyn Error>> {
    if !Path::new(marker_path).exists() {
        return Ok(());
    }

    let marker = fs::read_to_string(marker_path)?;
    let writes = serde_json::from_str::<serde_json::Value>(&marker)?;
    let Some(records) = writes.as_array() else {
        return Err("archive transaction marker is not a JSON array".into());
    };

    for record in records.iter().map(ArchiveWriteRecord::from_json) {
        let record = record?;
        // Probe by renaming rather than checking existence first: a missing
        // temp file (NotFound) just means that write was already committed,
        // and an exists-then-rename pair could race a concurrent instance
        // running the same recovery and abort it halfway.
        match fs::rename(&record.temp_path, &record.path) {
            Ok(()) => sync_parent_dir(&record.path)?,
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }

    fs::remove_file(marker_path)?;
    Ok(())
}

struct ArchiveWriteRecord {
    path: String,
    temp_path: String,
}

impl ArchiveWriteRecord {
    fn from_json(value: &serde_json::Value) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            path: value
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or("archive transaction marker is missing path")?
                .to_string(),
            temp_path: value
                .get("temp_path")
                .and_then(serde_json::Value::as_str)
                .ok_or("archive transaction marker is missing temp_path")?
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn todo_item_round_trips_checked_items() {
        let item = TodoItem::parse("- [x] ship the tiny desktop");

        assert!(item.checked);
        assert!(item.is_checkbox);
        assert_eq!(item.text, "ship the tiny desktop");
        assert_eq!(item.serialize(), "- [x] ship the tiny desktop");
    }

    #[test]
    fn todo_item_round_trips_unchecked_checkbox() {
        let item = TodoItem::parse("- [ ] water the plants");

        assert!(!item.checked);
        assert!(item.is_checkbox);
        assert_eq!(item.text, "water the plants");
        assert_eq!(item.serialize(), "- [ ] water the plants");
    }

    #[test]
    fn todo_item_keeps_plain_lines_verbatim() {
        let item = TodoItem::parse("## groceries");

        assert!(!item.is_checkbox);
        assert!(!item.checked);
        assert!(!item.renders_checkbox(false));
        assert!(!item.renders_checkbox(true));
        assert_eq!(item.serialize(), "## groceries");
    }

    #[test]
    fn todo_list_serializes_checkboxes_and_plain_lines() {
        let mut list = TodoList {
            items: vec![TodoItem::default(); LINE_COUNT],
        };
        list.items[0] = TodoItem::parse("- [x] done");
        list.items[1] = TodoItem::parse("a heading");
        list.trim_trailing_blank_items();

        assert_eq!(list.serialized_text().lines().count(), 2);
        assert_eq!(list.serialized_text(), "- [x] done\na heading\n");
    }

    #[test]
    fn todo_list_adds_blank_page_after_full_page() {
        let mut list = TodoList { items: Vec::new() };
        for line in 0..LINE_COUNT {
            list.item_mut(0, line).text = format!("todo {line}");
        }

        assert_eq!(list.page_count(), 2);

        list.items.pop();
        assert_eq!(list.page_count(), 1);
    }

    #[test]
    fn todo_list_does_not_add_page_after_sparse_page() {
        let mut list = TodoList {
            items: vec![TodoItem::default(); LINE_COUNT],
        };
        list.items[0].text = "first".to_string();
        list.items[LINE_COUNT - 1].text = "last".to_string();

        assert_eq!(list.page_count(), 1);
    }

    #[test]
    fn todo_list_uses_one_file_for_overflow_pages() {
        let mut list = TodoList { items: Vec::new() };
        list.item_mut(1, 0).text = "overflow".to_string();

        assert_eq!(list.serialized_text().lines().count(), LINE_COUNT + 1);
        assert!(list.serialized_text().ends_with("overflow\n"));
    }

    #[test]
    fn todo_list_delete_item_shifts_later_items_up() {
        let mut list = TodoList { items: Vec::new() };
        list.item_mut(0, 0).text = "first".to_string();
        list.item_mut(0, 1).text = String::new();
        list.item_mut(0, 2).text = "third".to_string();

        assert!(list.delete_item(0, 1));

        assert_eq!(list.item(0, 0).text, "first");
        assert_eq!(list.item(0, 1).text, "third");
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn merge_takes_theirs_when_ours_unchanged() {
        let base = lines(&["- [ ] a", "- [ ] b"]);
        let theirs = lines(&["- [ ] a", "- [x] b", "- [ ] c"]);

        assert_eq!(merge_lines(&base, &base.clone(), &theirs), theirs);
    }

    #[test]
    fn merge_keeps_ours_when_theirs_unchanged() {
        let base = lines(&["- [ ] a"]);
        let ours = lines(&["- [ ] a", "- [ ] typed"]);

        assert_eq!(merge_lines(&base, &ours, &base.clone()), ours);
    }

    #[test]
    fn merge_combines_disjoint_edits() {
        let base = lines(&["- [ ] a", "- [ ] b"]);
        let ours = lines(&["- [ ] a", "- [ ] b", "- [ ] typed"]);
        let theirs = lines(&["- [ ] b", "- [ ] synced"]);

        // External change deleted `a` and added `synced`; both apply on top of
        // our new `typed` line.
        assert_eq!(
            merge_lines(&base, &ours, &theirs),
            lines(&["- [ ] b", "- [ ] typed", "- [ ] synced"])
        );
    }

    #[test]
    fn merge_keeps_both_versions_of_a_line_edited_on_both_sides() {
        let base = lines(&["- [ ] call mom"]);
        let ours = lines(&["- [ ] call mom tonight"]);
        let theirs = lines(&["- [x] call mom"]);

        assert_eq!(
            merge_lines(&base, &ours, &theirs),
            lines(&["- [ ] call mom tonight", "- [x] call mom"])
        );
    }

    #[test]
    fn merge_does_not_delete_a_line_we_modified() {
        let base = lines(&["- [ ] draft", "- [ ] keep"]);
        let ours = lines(&["- [ ] draft v2", "- [ ] keep"]);
        let theirs = lines(&["- [ ] keep"]);

        // Theirs deleted `draft`, but we edited it; our version survives.
        assert_eq!(
            merge_lines(&base, &ours, &theirs),
            lines(&["- [ ] draft v2", "- [ ] keep"])
        );
    }

    #[test]
    fn merge_respects_duplicate_line_counts() {
        let base = lines(&["- [ ] x", "- [ ] x"]);
        let ours = lines(&["- [ ] x", "- [ ] x", "- [ ] mine"]);
        let theirs = lines(&["- [ ] x"]);

        // Theirs removed one of the two duplicates: exactly one goes.
        assert_eq!(
            merge_lines(&base, &ours, &theirs),
            lines(&["- [ ] x", "- [ ] mine"])
        );
    }

    #[test]
    fn store_absorbs_external_rewrite_when_clean() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("todos.md");
        let path = path.to_str().unwrap();

        fs::write(path, "- [ ] old\n").unwrap();
        let mut store = SectionStore::load(path).unwrap();

        fs::write(path, "- [ ] new\n").unwrap();
        assert!(store.absorb_external(path).unwrap());
        assert_eq!(store.list().items[0].text, "new");
        assert!(!store.is_dirty());

        // Nothing further changed: no-op.
        assert!(!store.absorb_external(path).unwrap());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_ignores_its_own_writes() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("todos.md");
        let path = path.to_str().unwrap();

        let mut store = SectionStore::load(path).unwrap();
        store.list_mut().item_mut(0, 0).text = "mine".to_string();
        assert!(!store.save(path).unwrap());

        assert!(!store.absorb_external(path).unwrap());
        assert!(!store.is_dirty());
        assert_eq!(fs::read_to_string(path).unwrap(), "- [ ] mine\n");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_merges_external_change_into_dirty_edits() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("todos.md");
        let path = path.to_str().unwrap();

        fs::write(path, "- [ ] shared\n").unwrap();
        let mut store = SectionStore::load(path).unwrap();
        store.list_mut().item_mut(0, 1).text = "typed".to_string();
        store.mark_dirty();

        fs::write(path, "- [ ] shared\n- [ ] synced\n").unwrap();
        assert!(store.absorb_external(path).unwrap());
        // Merged result differs from disk, so it stays dirty until saved.
        assert!(store.is_dirty());

        assert!(!store.save(path).unwrap());
        let saved = fs::read_to_string(path).unwrap();
        assert!(saved.contains("typed"));
        assert!(saved.contains("synced"));
        assert!(saved.contains("shared"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_save_folds_in_unseen_external_change() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("todos.md");
        let path = path.to_str().unwrap();

        fs::write(path, "- [ ] a\n").unwrap();
        let mut store = SectionStore::load(path).unwrap();
        store.list_mut().item_mut(0, 0).checked = true;
        store.mark_dirty();

        // An external writer appends before our save lands.
        fs::write(path, "- [ ] a\n- [ ] external\n").unwrap();
        assert!(store.save(path).unwrap());

        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "- [x] a\n- [ ] external\n"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archive_transaction_recovery_finishes_interrupted_commit() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();

        let marker_path = dir.join("transaction.json");
        let done_path = dir.join("done.txt");
        let page_path = dir.join("page.txt");
        fs::write(&done_path, "old\n").unwrap();
        fs::write(&page_path, "[x] done\nnext\n\n\n\n\n").unwrap();

        let staged_writes = [
            AtomicWrite::stage(done_path.to_str().unwrap(), b"old\ndone\n").unwrap(),
            AtomicWrite::stage(page_path.to_str().unwrap(), b"next\n\n\n\n\n\n").unwrap(),
        ];
        let staged_refs: Vec<&AtomicWrite> = staged_writes.iter().collect();
        write_archive_transaction_marker(marker_path.to_str().unwrap(), &staged_refs).unwrap();

        let marker = fs::read_to_string(&marker_path).unwrap();
        let writes = serde_json::from_str::<serde_json::Value>(&marker).unwrap();
        let writes = writes.as_array().unwrap();
        let done_temp_path = writes[0].get("temp_path").unwrap().as_str().unwrap();
        fs::rename(done_temp_path, &done_path).unwrap();

        recover_archive_transaction(marker_path.to_str().unwrap()).unwrap();

        assert_eq!(fs::read_to_string(&done_path).unwrap(), "old\ndone\n");
        assert_eq!(fs::read_to_string(&page_path).unwrap(), "next\n\n\n\n\n\n");
        assert!(!marker_path.exists());

        fs::remove_dir_all(dir).unwrap();
    }

    fn unique_temp_dir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cozyui-toodle-test-{}-{nanos}", std::process::id()))
    }
}
