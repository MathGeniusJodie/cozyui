// Shared OpenRouter transport: a single secure curl POST used by the fwends
// chat widget. The API key is streamed to curl through a `--config -` file on
// stdin and the request body lives in a 0600 temp file, so neither the key nor
// the body ever appears in curl's argv (where it would be visible to other
// processes via the process list).
//
// Self-contained on purpose (std + serde_json only).
#![allow(dead_code)]

use serde_json::{Value, json};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Shared slot a caller can use to record the pid of the in-flight curl
/// child so it can be killed (e.g. `libc::kill(pid, libc::SIGTERM)`) to
/// cancel a request early instead of waiting out `REQUEST_TIMEOUT_SECS`.
pub type PidSlot = Arc<Mutex<Option<u32>>>;

const URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const REQUEST_TIMEOUT_SECS: &str = "30";

/// Free preset `OpenRouter` routes to when the requested model errors or runs
/// out of credits. Callers append it to a `models` routing list so the chat
/// degrades instead of dying.
pub const FALLBACK_MODEL: &str = "@preset/free";

/// POST a JSON body to `OpenRouter` and parse the response. Transport failures
/// (missing key, curl spawn/exec error, non-zero exit) surface as `Err`; a
/// successful call whose body isn't valid JSON is wrapped in an error-shaped
/// `Value` so the usual `["error"]["message"]` extraction still works.
pub fn post(body: &Value) -> Result<Value, String> {
    post_cancelable(body, None)
}

/// Same as [`post`], but if `pid_slot` is given, the spawned curl child's pid
/// is recorded there for the duration of the request so a caller on another
/// thread can cancel it early (e.g. `libc::kill(pid, libc::SIGTERM)`) instead
/// of waiting out `REQUEST_TIMEOUT_SECS`.
pub fn post_cancelable(body: &Value, pid_slot: Option<&PidSlot>) -> Result<Value, String> {
    let raw = post_raw(body.to_string().as_bytes(), pid_slot)?;
    Ok(serde_json::from_slice(&raw).unwrap_or_else(
        |_| json!({"error": {"message": String::from_utf8_lossy(&raw).to_string()}}),
    ))
}

fn post_raw(body: &[u8], pid_slot: Option<&PidSlot>) -> Result<Vec<u8>, String> {
    let api_key =
        env::var("OPENROUTER_API_KEY").map_err(|_| "OPENROUTER_API_KEY is not set".to_string())?;
    let body_file = CurlBodyFile::new(body)?;
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "--max-time",
            REQUEST_TIMEOUT_SECS,
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("curl failed: {err}"))?;
    if let Some(slot) = pid_slot {
        *slot.lock().unwrap() = Some(child.id());
    }
    let config = format!(
        "url = \"{}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\n",
        URL,
        curl_config_escape(&api_key),
        curl_config_escape(&body_file.path_string())
    );
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "curl stdin was not available".to_string())
        .and_then(|mut stdin| {
            stdin
                .write_all(config.as_bytes())
                .map_err(|err| format!("curl config write failed: {err}"))
        });
    if let Err(err) = write_result {
        // Without this, an early write failure (e.g. curl exiting before it
        // reads the config) would drop the Child unwaited, leaking a zombie,
        // and leave a dead pid in the slot for a canceller to kill.
        let _ = child.kill();
        let _ = child.wait();
        if let Some(slot) = pid_slot {
            *slot.lock().unwrap() = None;
        }
        return Err(err);
    }
    let output = child.wait_with_output();
    // Clear the slot before inspecting the result: from here the pid is dead
    // (or the wait failed), so a canceller must never signal it again.
    if let Some(slot) = pid_slot {
        *slot.lock().unwrap() = None;
    }
    let output = output.map_err(|err| format!("curl failed: {err}"))?;

    if !output.status.success() {
        // With --fail-with-body, curl still writes the error response body to
        // stdout (it just also exits non-zero), so try to surface the API's
        // own error message before falling back to stderr/exit status.
        if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&output.stdout)
            && let Some(message) = map.get("error").and_then(|error| error.get("message"))
            && let Some(message) = message.as_str()
        {
            return Err(message.to_string());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("OpenRouter request failed with status {}", output.status)
        } else {
            stderr.trim().to_string()
        });
    }
    Ok(output.stdout)
}

fn curl_config_escape(text: &str) -> String {
    // Control chars (esp. newlines) could smuggle extra config lines past the
    // quoting, so drop them entirely.
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .chars()
        .filter(|ch| !ch.is_control())
        .collect()
}

struct CurlBodyFile {
    path: PathBuf,
}

impl CurlBodyFile {
    fn new(contents: &[u8]) -> Result<Self, String> {
        let mut base = env::temp_dir();
        base.push(format!("cozyui-openrouter-{}.json", std::process::id()));
        let path = PathBuf::from(crate::util::unique_temp_path(&base.to_string_lossy()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| format!("request body temp file failed: {err}"))?;
        file.write_all(contents)
            .map_err(|err| format!("request body temp write failed: {err}"))?;
        file.sync_all()
            .map_err(|err| format!("request body temp sync failed: {err}"))?;
        Ok(Self { path })
    }

    fn path_string(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for CurlBodyFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Message content as plain text: either a JSON string or, when server-side
/// tools ran, an array of parts whose `text`/`content` fields are joined.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}
