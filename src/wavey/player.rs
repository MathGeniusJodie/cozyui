//! The mpv backend: station config, the persistent player session (abduco +
//! FIFO command hand-off), mpv IPC (title/metadata polling, transport
//! commands), and OS volume control. No rendering here — see `mod.rs` for the
//! widget state and drawing.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::util::runtime_dir;

pub(super) const STATIONS_FILE: &str = "radio_stations.txt";

const VOLUME_REFRESH: Duration = Duration::from_secs(3);
const TITLE_REFRESH: Duration = Duration::from_secs(1);
const TITLE_READ_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub(super) struct Station {
    pub(super) label: String,
    pub(super) mpv_args: String,
}

pub(super) fn load_stations(path: &str) -> Vec<Station> {
    fs::read_to_string(path)
        .ok()
        .map(|text| {
            text.lines()
                .filter_map(parse_station)
                .collect::<Vec<Station>>()
        })
        .filter(|stations| !stations.is_empty())
        .unwrap_or_else(default_stations)
}

fn parse_station(line: &str) -> Option<Station> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (label, mpv_args) = line.split_once('|')?;
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    Some(Station {
        label: label.chars().take(6).collect(),
        mpv_args: mpv_args.trim().to_string(),
    })
}

fn default_stations() -> Vec<Station> {
    [
        (
            "FM",
            "--no-video --really-quiet https://somafm.com/groovesalad.pls",
        ),
        (
            "POP",
            "--no-video --really-quiet https://somafm.com/poptron.pls",
        ),
        (
            "DNB",
            "--no-video --really-quiet https://somafm.com/deepspaceone.pls",
        ),
        (
            "JAZZ",
            "--no-video --really-quiet https://somafm.com/sonicuniverse.pls",
        ),
        (
            "LPS",
            "--no-video --really-quiet https://somafm.com/lush.pls",
        ),
    ]
    .into_iter()
    .map(|(label, mpv_args)| Station {
        label: label.to_string(),
        mpv_args: mpv_args.to_string(),
    })
    .collect()
}

fn title_from_metadata(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    ["title", "icy-title", "icy_title"]
        .iter()
        .find_map(|key| metadata_value(object, key))
}

fn track_line_from_metadata(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let title = title_from_metadata(value);
    // Untagged web tracks (e.g. SoundCloud) have no artist tag, only the
    // uploading account; use it unless the title already names the artist
    // dash-style, as YouTube channel uploads usually do.
    let artist = ["artist", "album_artist", "albumartist"]
        .iter()
        .find_map(|key| metadata_value(object, key))
        .or_else(|| {
            if title.as_deref().is_some_and(|title| title.contains(" - ")) {
                return None;
            }
            metadata_value(object, "uploader")
        });

    match (artist, title) {
        (Some(artist), Some(title)) => {
            if title.to_lowercase().contains(&artist.to_lowercase()) {
                Some(title)
            } else {
                Some(format!("{artist} - {title}"))
            }
        }
        (Some(artist), None) => Some(artist),
        (None, Some(title)) => Some(title),
        (None, None) => None,
    }
}

fn metadata_value(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    object.iter().find_map(|(candidate, value)| {
        if candidate.eq_ignore_ascii_case(key) {
            json_string(value)
        } else {
            None
        }
    })
}

fn json_string(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .filter(|text| !text.trim().is_empty())
}

#[allow(clippy::needless_pass_by_value)]
fn clean_title(title: String) -> Option<String> {
    let title = deunicode::deunicode(&title);
    let title = title.split_whitespace().collect::<Vec<&str>>().join(" ");
    (!title.is_empty()).then_some(title)
}

/// The currently playing track's display title, fetched from mpv over IPC.
/// Blocking (socket round trips with read timeouts) — call off the UI thread.
fn current_mpv_title() -> Option<String> {
    let chapter_title = mpv_property("chapter-metadata")
        .as_ref()
        .and_then(title_from_metadata);
    let media_title = mpv_property("media-title").as_ref().and_then(json_string);

    if let Some(chapter_title) = chapter_title {
        if let Some(media_title) = media_title.as_deref()
            && !media_title.eq_ignore_ascii_case(&chapter_title)
        {
            return clean_title(format!("{media_title} - {chapter_title}"));
        }
        return clean_title(chapter_title);
    }

    mpv_property("metadata")
        .as_ref()
        .and_then(track_line_from_metadata)
        .or(media_title)
        .and_then(clean_title)
}

fn mpv_property(property: &str) -> Option<serde_json::Value> {
    let mut stream = UnixStream::connect(mpv_ipc_path()).ok()?;
    let _ = stream.set_read_timeout(Some(TITLE_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(TITLE_READ_TIMEOUT));

    let message = serde_json::json!({
        "command": ["get_property", property],
        "request_id": 1
    })
    .to_string();
    stream.write_all(message.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;

    let mut reader = BufReader::new(stream);
    for _ in 0..64 {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let Ok(response) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if response
            .get("request_id")
            .and_then(serde_json::Value::as_i64)
            != Some(1)
        {
            continue;
        }
        if response.get("error").and_then(serde_json::Value::as_str) != Some("success") {
            return None;
        }
        return response.get("data").cloned();
    }

    None
}

/// Sends a transport command (play/pause, next, ...) to the running mpv over
/// its IPC socket. A no-op (logged) if mpv isn't reachable.
pub(super) fn send_command(command: &[&str]) {
    let mut stream = match UnixStream::connect(mpv_ipc_path()) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("wavey: mpv IPC connect failed: {err}");
            return;
        }
    };
    let _ = stream.set_read_timeout(Some(TITLE_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(TITLE_READ_TIMEOUT));
    let mut message = serde_json::json!({ "command": command }).to_string();
    message.push('\n');
    if let Err(err) = stream.write_all(message.as_bytes()) {
        eprintln!("wavey: mpv IPC write failed: {err}");
    }
}

/// One poller result: the display title and the stream URL it belongs to.
pub(super) struct TitleUpdate {
    pub(super) title: String,
    pub(super) url: Option<String>,
}

/// Poll mpv for the playing title (and its URL, for title-click copies) off
/// the UI thread; a wedged mpv then can't stall rendering. The thread exits
/// once the receiver is dropped.
pub(super) fn spawn_title_poller() -> mpsc::Receiver<TitleUpdate> {
    crate::util::spawn_poller(TITLE_REFRESH, || {
        let title = current_mpv_title().unwrap_or_default();
        let url = (!title.is_empty())
            .then(|| mpv_property("path").as_ref().and_then(json_string))
            .flatten();
        Some(TitleUpdate { title, url })
    })
}

/// Poll the system volume off the UI thread: `read_system_volume` shells out
/// to `wpctl`/`pactl` synchronously, which must never block a frame. The
/// first reading is sent immediately (it seeds the placeholder set in `load`);
/// the thread exits once the receiver is dropped.
pub(super) fn spawn_volume_poller() -> mpsc::Receiver<u8> {
    crate::util::spawn_poller(VOLUME_REFRESH, read_system_volume)
}

/// One-shot, off-thread probe for an mpv left running in the "wavey" abduco
/// session by a previous cozyui instance (the IPC round trip blocks, so it
/// must not run in `load`). The station index is stamped onto mpv via
/// --script-opts at launch, so the running player itself records which
/// station it is; querying any property doubles as the liveness check. A
/// message means "mpv is alive", carrying the stamped station if readable.
pub(super) fn spawn_resume_probe() -> mpsc::Receiver<Option<usize>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Some(opts) = mpv_property("script-opts") {
            let _ = tx.send(station_from_script_opts(&opts));
        }
    });
    rx
}

pub(super) fn mpv_ipc_path() -> PathBuf {
    // Fixed (non-pid) name on purpose: a restarted cozyui reconnects to an
    // mpv left running in the persistent player session. kill_player anchors
    // its pkill pattern on this exact path, so it can only ever match the
    // wavey mpv, not unrelated mpv processes.
    runtime_dir().join("cozyui-mpv-wavey.sock")
}

/// SIGKILL the wavey mpv (matched by its IPC socket argument)
/// so the session loop frees up immediately instead of waiting for a
/// graceful quit. The pattern anchors on the full socket path, so it can
/// only match the wavey mpv, never an unrelated mpv.
///
/// Non-blocking: `stop_player` runs inside `click` on the UI thread, so the
/// pkill child is spawned and reaped on a background thread instead of
/// waited on synchronously. Callers that need the old mpv gone before doing
/// more work (see `start_player`) should use `kill_player_blocking` instead,
/// from a thread that isn't the UI thread.
pub(super) fn kill_player() {
    let _ = crate::util::spawn_and_reap(&mut kill_player_command());
}

/// Same as `kill_player`, but waits for `pkill` to finish before returning.
/// Only call this off the UI thread: `start_player` uses it to guarantee the
/// old mpv is gone before the new one is queued onto the same socket path.
fn kill_player_blocking() {
    let _ = kill_player_command().status();
}

fn kill_player_command() -> Command {
    let pattern = format!("mpv .*{}", mpv_ipc_path().display());
    let mut command = Command::new("pkill");
    command
        .args(["-9", "-f", &pattern])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn station_from_script_opts(opts: &serde_json::Value) -> Option<usize> {
    opts.get("cozyui-wavey-station")?.as_str()?.parse().ok()
}

fn player_fifo_path() -> PathBuf {
    // Fixed name, same reasoning as `mpv_ipc_path`: a restarted cozyui must
    // find the same FIFO to talk to the persistent player session.
    runtime_dir().join("cozyui-mpv-wavey.cmd")
}

/// Session setup plus command hand-off, on a background thread: the `mkfifo`/
/// `abduco` `.status()` calls block until those children exit, which must not
/// stall the UI thread (`play_station` runs inside `click`). The ordering
/// matters — the previous mpv must be dead before the FIFO and its reader
/// loop exist (both would otherwise be racing over the same socket path), and
/// the FIFO must exist before the command write, or the shell redirection
/// would create a plain file — so all three steps share one thread.
/// `play_station` also fires a non-blocking `stop_player` first for prompt UI
/// state reset, but the authoritative, ordered kill happens here.
pub(super) fn start_player(command_line: String) {
    thread::spawn(move || {
        kill_player_blocking();
        ensure_player_session();
        queue_player_command(&command_line);
    });
}

/// One persistent "wavey" abduco session running a shell loop that reads mpv
/// command lines from the FIFO and runs them. The loop outlives each mpv, so
/// the session name never has to be recycled and `abduco -a wavey` works from
/// any terminal whenever something is (or was) playing.
fn ensure_player_session() {
    let fifo = player_fifo_path();
    let _ = Command::new("mkfifo")
        .arg(&fifo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Fails fast with "session exists" noise when already running; that is
    // the expected steady state, so the output is discarded. -f reclaims the
    // name from a dead session (e.g. a force-killed mpv from an older cozyui)
    // that would otherwise block creation forever.
    let runner =
        r#"while :; do cmd=$(cat "$COZYUI_MPV_FIFO") || exit; [ -n "$cmd" ] && eval "$cmd"; done"#;
    let _ = Command::new("abduco")
        .args(["-f", "-n", "wavey", "sh", "-c", runner])
        .env("COZYUI_MPV_FIFO", &fifo)
        .env("COZYUI_MPV_IPC", mpv_ipc_path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Writing to the FIFO blocks until the session loop is back at `cat` (i.e.
/// the previous mpv has exited), so the write happens in a throwaway child
/// that gets reaped off the UI thread.
fn queue_player_command(command: &str) {
    let _ = crate::util::spawn_and_reap(
        Command::new("sh")
            .args([
                "-c",
                r#"printf '%s\n' "$COZYUI_MPV_CMD" > "$COZYUI_MPV_FIFO""#,
            ])
            .env("COZYUI_MPV_CMD", command)
            .env("COZYUI_MPV_FIFO", player_fifo_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

pub(super) fn read_system_volume() -> Option<u8> {
    read_wpctl_volume().or_else(read_pactl_volume)
}

fn read_wpctl_volume() -> Option<u8> {
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let value = text
        .split_whitespace()
        .find_map(|word| word.parse::<f32>().ok())?;
    Some((value * 100.0).round().clamp(0.0, 100.0) as u8)
}

fn read_pactl_volume() -> Option<u8> {
    let output = Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().find_map(|word| {
        // Parse as f64 first: pactl prints fractional percentages (e.g.
        // "37.00%"), which `u8::parse` rejects outright, and values can
        // exceed 100 (software-boosted volume), which would previously
        // overflow `u8::parse` too.
        let value: f64 = word.strip_suffix('%')?.parse().ok()?;
        Some(value.round().clamp(0.0, 100.0) as u8)
    })
}

pub(super) fn set_system_volume(volume: u8) {
    // Off-thread so the UI never blocks, and waited on so (a) no zombies and
    // (b) a wpctl that exists but fails at runtime still falls back to pactl.
    thread::spawn(move || {
        let wpctl = Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{volume}%")])
            .status();
        if wpctl.is_ok_and(|status| status.success()) {
            return;
        }

        let _ = Command::new("pactl")
            .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{volume}%")])
            .status();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_line_falls_back_to_uploader_when_untagged() {
        let soundcloud = serde_json::json!({
            "title": "Double Spire (Unbroken Edit)",
            "uploader": "UnbrokenOne",
        });
        let tagged = serde_json::json!({
            "title": "Ask Me",
            "artist": "Duck Sauce",
            "uploader": "ducksaucenyc",
        });
        let youtube_upload = serde_json::json!({
            "title": "Soichi Terada - Double Spire",
            "uploader": "TheDailyDose",
        });

        assert_eq!(
            track_line_from_metadata(&soundcloud).as_deref(),
            Some("UnbrokenOne - Double Spire (Unbroken Edit)")
        );
        assert_eq!(
            track_line_from_metadata(&tagged).as_deref(),
            Some("Duck Sauce - Ask Me")
        );
        assert_eq!(
            track_line_from_metadata(&youtube_upload).as_deref(),
            Some("Soichi Terada - Double Spire")
        );
    }

    #[test]
    fn station_from_script_opts_reads_stamped_index() {
        let opts = serde_json::json!({"cozyui-wavey-station": "2", "other": "x"});

        assert_eq!(station_from_script_opts(&opts), Some(2));
        assert_eq!(station_from_script_opts(&serde_json::json!({})), None);
        assert_eq!(
            station_from_script_opts(&serde_json::json!({"cozyui-wavey-station": "nope"})),
            None
        );
    }
}
