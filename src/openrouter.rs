// Shared OpenRouter transport: a single secure curl POST used by both the
// fwends chat widget and the senpai pipeline. The API key is streamed to curl
// through a `--config -` file on stdin and the request body lives in a 0600
// temp file, so neither the key nor the body ever appears in curl's argv (where
// it would be visible to other processes via the process list).
//
// Self-contained on purpose (std + serde_json only): the standalone `senpai`
// binary pulls this module in directly, so it must not depend on any other
// cozyui module.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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
    let raw = post_raw(body.to_string().as_bytes())?;
    Ok(serde_json::from_slice(&raw).unwrap_or_else(
        |_| json!({"error": {"message": String::from_utf8_lossy(&raw).to_string()}}),
    ))
}

fn post_raw(body: &[u8]) -> Result<Vec<u8>, String> {
    let api_key =
        env::var("OPENROUTER_API_KEY").map_err(|_| "OPENROUTER_API_KEY is not set".to_string())?;
    let body_file = CurlBodyFile::new(body)?;
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", REQUEST_TIMEOUT_SECS, "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("curl failed: {err}"))?;
    let config = format!(
        "url = \"{}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\n",
        URL,
        curl_config_escape(&api_key),
        curl_config_escape(&body_file.path_string())
    );
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "curl stdin was not available".to_string())?;
    stdin
        .write_all(config.as_bytes())
        .map_err(|err| format!("curl config write failed: {err}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|err| format!("curl failed: {err}"))?;

    if !output.status.success() {
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
        let path = unique_temp_path();
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

fn unique_temp_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut path = env::temp_dir();
    path.push(format!(
        "cozyui-openrouter-{}-{nanos}.json",
        std::process::id()
    ));
    path
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
