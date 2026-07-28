//! Conversation state and the OpenRouter request pipeline: assembling chat
//! history, spawning/cancelling in-flight requests, and parsing replies. No
//! rendering here — see `mod.rs` for how messages get drawn.

use std::fmt::Write as _;
use std::fs;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

use crate::Sprite;
use crate::openrouter;

const HISTORY_LIMIT: usize = 8;
const SYSTEM_PROMPT_FILE: &str = "fwends_system_prompt.txt";

/// Display name labelling the user's chat messages: `$COZYUI_USER_NAME`, else
/// the login name with its first letter capitalized, else "Fwend".
pub(super) fn user_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("COZYUI_USER_NAME")
            .or_else(|_| std::env::var("USER").map(capitalize_first))
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Fwend".to_string())
    })
}

fn capitalize_first(name: String) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => name,
    }
}

#[derive(Clone, Copy)]
pub(super) struct Model {
    id: &'static str,
    thinking_id: &'static str,
    pub(super) name: &'static str,
    pub(super) icon_index: usize,
    pub(super) avatar: fn() -> Sprite,
}

pub(super) const MODELS: [Model; 4] = [
    Model {
        id: "anthropic/claude-sonnet-5",
        thinking_id: "anthropic/claude-opus-5",
        name: "Claude",
        icon_index: 2,
        avatar: crate::assets::claw,
    },
    Model {
        id: "deepseek/deepseek-v4-flash",
        thinking_id: "deepseek/deepseek-v4-pro",
        name: "Deepseek",
        icon_index: 1,
        avatar: crate::assets::deep,
    },
    Model {
        id: "qwen/qwen3.6-35b-a3b",
        thinking_id: "qwen/qwen3.7-max",
        name: "Qwen",
        icon_index: 0,
        avatar: crate::assets::qwen,
    },
    Model {
        id: "moonshotai/kimi-k3",
        thinking_id: "moonshotai/kimi-k3",
        name: "Kimi",
        icon_index: 3,
        avatar: crate::assets::kimi,
    },
];

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Role {
    User,
    Assistant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageKind {
    Normal,
    Intro,
}

/// A chat message. `kind`/`author` only ever make sense for `Assistant`
/// messages (the intro blurb and the "which fwend said this" tag), so they
/// live in that variant instead of being optional fields on a single struct
/// that a `User` message could also (invalidly) be built with.
#[derive(Clone, PartialEq)]
pub(super) enum Message {
    User {
        text: String,
    },
    Assistant {
        text: String,
        kind: MessageKind,
        author: Option<&'static str>,
    },
}

impl Message {
    pub(super) const fn user(text: String) -> Self {
        Self::User { text }
    }

    #[cfg(test)]
    pub(super) const fn assistant(text: String) -> Self {
        Self::Assistant {
            text,
            kind: MessageKind::Normal,
            author: None,
        }
    }

    /// Builds a resolved reply tagged with which fwend sent it (used once
    /// `drain_reply` has the finished text in hand — see `ChatState`).
    pub(super) const fn assistant_with_author(text: String, author: &'static str) -> Self {
        Self::Assistant {
            text,
            kind: MessageKind::Normal,
            author: Some(author),
        }
    }

    fn intro(text: String) -> Self {
        Self::Assistant {
            text,
            kind: MessageKind::Intro,
            author: None,
        }
    }

    pub(super) const fn role(&self) -> Role {
        match self {
            Self::User { .. } => Role::User,
            Self::Assistant { .. } => Role::Assistant,
        }
    }

    pub(super) fn text(&self) -> &str {
        match self {
            Self::User { text } | Self::Assistant { text, .. } => text,
        }
    }

    pub(super) const fn kind(&self) -> MessageKind {
        match self {
            Self::User { .. } => MessageKind::Normal,
            Self::Assistant { kind, .. } => *kind,
        }
    }

    pub(super) const fn author(&self) -> Option<&'static str> {
        match self {
            Self::User { .. } => None,
            Self::Assistant { author, .. } => *author,
        }
    }
}

pub(super) fn intro_message() -> Message {
    Message::intro("pick a fwend and say hi".to_string())
}

pub(super) fn load_system_prompt() -> String {
    let path = crate::paths::config_file(SYSTEM_PROMPT_FILE);
    fs::read_to_string(&path).unwrap_or_else(|err| {
        eprintln!("fwends: could not read system prompt at {path}, using default: {err}");
        "You are a warm, concise chat companion. Answer directly and never reveal hidden reasoning."
            .to_string()
    })
}

pub(super) fn request_history(
    messages: &[Message],
    current_name: &str,
    user_name: &str,
) -> Vec<Message> {
    let mut recent: Vec<&Message> = messages
        .iter()
        .rev()
        .filter(|message| message.kind() == MessageKind::Normal)
        .take(HISTORY_LIMIT)
        .collect();
    recent.reverse();
    recent
        .into_iter()
        .map(|message| match message {
            Message::User { text } => Message::user(format!("{user_name}: {text}")),
            Message::Assistant {
                text,
                author: Some(author),
                ..
            } if *author != current_name => Message::user(format!("{author}: {text}")),
            _ => message.clone(),
        })
        .collect()
}

pub(super) fn strip_self_prefix(text: &str, name: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(name)
        && let Some(rest) = rest.trim_start().strip_prefix(':')
    {
        return rest.trim_start().to_string();
    }
    trimmed.to_string()
}

pub(super) fn fwend_system_prompt(template: &str, name: &str, user_name: &str) -> String {
    let mut prompt = template.replace("[[FREND_NAME]]", name);
    let _ = write!(
        prompt,
        "\n\nThis is a group chat: messages from {user_name} and from other fwends arrive labeled like \"{user_name}: ...\" or \"Qwen: ...\". Your own earlier replies are unlabeled. Never start your reply with \"{name}:\" — just speak."
    );
    prompt
}

fn send_openrouter_request(
    model: &str,
    system_prompt: &str,
    history: &[Message],
    latest_text: &str,
    thinking: bool,
    pid_slot: &openrouter::PidSlot,
) -> Result<String, String> {
    let response = openrouter::post_cancelable(
        &chat_body(model, system_prompt, history, latest_text, thinking),
        if thinking {
            openrouter::THINKING_TIMEOUT_SECS
        } else {
            openrouter::DEFAULT_TIMEOUT_SECS
        },
        Some(pid_slot),
    )?;
    extract_content(&response).map_err(|err| format!("{err}: {}", compact_error(&response)))
}

fn chat_body(
    model: &str,
    system_prompt: &str,
    history: &[Message],
    latest_text: &str,
    thinking: bool,
) -> Value {
    let mut messages = vec![json!({
        "role": "system",
        "content": system_prompt.trim(),
    })];
    for message in history {
        messages.push(json!({
            "role": match message.role() {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            "content": message.text(),
        }));
    }
    messages.push(json!({
        "role": "user",
        "content": latest_text,
    }));

    // Thinking mode turns reasoning on (but still excludes the reasoning trace
    // from the reply, which the UI never shows); regular mode disables it.
    let reasoning = if thinking {
        json!({"effort": "high", "exclude": true})
    } else {
        json!({"exclude": true})
    };

    // OpenRouter ignores "model" when a "models" routing list is present, so
    // the requested model must lead the list with the preset as fallback.
    json!({
        "model": model,
        "models": [model, openrouter::FALLBACK_MODEL],
        "messages": messages,
        "reasoning": reasoning,
        "include_reasoning": false,
    })
}

fn extract_content(response: &Value) -> Result<String, &'static str> {
    let Some(content) = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
    else {
        return Err("OpenRouter response did not include assistant content");
    };

    let text = openrouter::content_text(content);
    if text.trim().is_empty() {
        Err("OpenRouter assistant content was empty")
    } else {
        Ok(normalize_display_text(&text))
    }
}

fn normalize_display_text(text: &str) -> String {
    let text = crate::emojimap::replace_emoji(text);
    let text = deunicode::deunicode(&text);
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' | '\r' => out.push(' '),
            ch if ch.is_ascii() && !ch.is_control() => out.push(ch),
            '\t' => out.push(' '),
            _ => {}
        }
    }
    out
}

fn compact_error(response: &Value) -> String {
    // Prefer the API's own error message; otherwise fall back to a truncated
    // dump of the response. Truncate by chars: a byte split could panic.
    if let Some(message) = response
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return message.chars().take(120).collect();
    }
    let text: String = response
        .to_string()
        .replace('\n', " ")
        .chars()
        .take(120)
        .collect();
    if text.is_empty() {
        "empty OpenRouter response".to_string()
    } else {
        text
    }
}

/// An in-flight (or just-finished) chat request, plus what's needed to
/// cancel it cleanly. See the `cancel`/`cancel_and_wait` docs for why
/// cancellation needs both a signal and (sometimes) a bounded join.
pub(super) struct PendingReply {
    rx: Receiver<Result<String, String>>,
    author: &'static str,
    // Pid of the in-flight curl request, so erase_chat_history/shutdown can
    // cancel it instead of leaving it running for up to the request timeout
    // after the reply is no longer wanted.
    pid_slot: openrouter::PidSlot,
    // Joined on cancellation so the request thread's `CurlBodyFile` (whose
    // `Drop` deletes the 0600 temp file holding the request body) actually
    // runs before we move on — a non-main thread's destructors don't run on
    // process exit, so without this join, quitting while a reply is pending
    // would leak both the temp file and the curl child past app shutdown.
    handle: thread::JoinHandle<()>,
}

impl PendingReply {
    /// Kicks off the request on a background thread and returns immediately;
    /// poll for the result with `poll`.
    pub(super) fn spawn(
        model: Model,
        thinking: bool,
        system_prompt: String,
        history: Vec<Message>,
        latest_text: String,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let pid_slot: openrouter::PidSlot = Arc::new(Mutex::new(None));
        let thread_pid_slot = Arc::clone(&pid_slot);
        let handle = thread::spawn(move || {
            let pid_slot = thread_pid_slot;
            // Lamp on: thinking mode — same single-model request, but using
            // the fwend's beefier thinking model with reasoning turned on.
            // Lamp off: the fast model with reasoning off.
            let model_id = if thinking {
                model.thinking_id
            } else {
                model.id
            };
            let result = send_openrouter_request(
                model_id,
                &system_prompt,
                &history,
                &latest_text,
                thinking,
                &pid_slot,
            );
            let _ = tx.send(result);
        });
        Self {
            rx,
            author: model.name,
            pid_slot,
            handle,
        }
    }

    pub(super) const fn author(&self) -> &'static str {
        self.author
    }

    /// `None` while still in flight. `Some(Err(_))` covers both an API/
    /// network error and the reply thread dying without sending (e.g. a
    /// panic) — callers shouldn't have to distinguish "no answer" from "bad
    /// answer".
    pub(super) fn poll(&self) -> Option<Result<String, String>> {
        match self.rx.try_recv() {
            Ok(reply) => Some(reply),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("reply thread died".to_string())),
        }
    }

    /// Best-effort signal: SIGTERMs the in-flight curl, if its pid has been
    /// recorded yet. Safe to call even if curl already exited between the
    /// pid check and the kill: `post_raw` holds the child unreaped (a
    /// zombie) until it clears the slot, so the pid can never be recycled
    /// while this lock sees it as `Some`. Holding the guard across the kill
    /// (instead of copying the pid out and dropping the lock first)
    /// serializes us against that clear: either we see the pid and the
    /// child is still a zombie (a SIGTERM to it is a harmless no-op if it's
    /// already dead), or the slot is already `None` and we signal nothing.
    fn signal_cancel(&self) {
        let guard = self.pid_slot.lock().unwrap();
        if let Some(pid) = *guard {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    }

    /// Cancels the in-flight curl (if any) without waiting for its request
    /// thread to unwind: called from `erase_chat_history`, which runs on the
    /// UI thread and must not freeze the whole overlay for up to the request
    /// timeout if the pid hasn't been recorded yet. Dropping `self.handle`
    /// here without joining it still lets the thread (and its `CurlBodyFile`
    /// cleanup) run to completion detached, since the process keeps running
    /// afterward — std detaches a `JoinHandle`'s thread on drop.
    pub(super) fn cancel(self) {
        self.signal_cancel();
    }

    /// Cancels and waits, but only up to `timeout`: called from `shutdown`,
    /// where joining unboundedly would risk hanging process exit for up to
    /// the full request timeout if a SIGTERM lands in the brief window
    /// before the request thread has recorded curl's pid (so `signal_cancel`
    /// has nothing to kill yet). Bounding the wait keeps shutdown prompt even
    /// then, at the cost of possibly not seeing the temp-file cleanup finish
    /// in that rare case.
    pub(super) fn cancel_and_wait(self, timeout: std::time::Duration) {
        self.signal_cancel();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let handle = self.handle;
        thread::spawn(move || {
            let _ = handle.join();
            let _ = done_tx.send(());
        });
        let _ = done_rx.recv_timeout(timeout);
    }
}

/// Whether a reply is currently in flight, and for whom. The single source
/// of truth for the "..." placeholder bubble: previously that bubble was
/// also stored as an ordinary (if oddly-tagged) entry in `messages`, kept in
/// sync by hand with this state and prone to disagreeing with it.
pub(super) enum ChatState {
    Idle,
    Awaiting(PendingReply),
}

impl ChatState {
    pub(super) const fn is_awaiting(&self) -> bool {
        matches!(self, Self::Awaiting(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_assistant_message_content() {
        let json = r#"{"content":"wrong","choices":[{"message":{"role":"assistant","content":"hello\nthere"}}]}"#;
        let response: Value = serde_json::from_str(json).unwrap();

        assert_eq!(extract_content(&response).as_deref(), Ok("hello there"));
    }

    #[test]
    fn chat_body_preserves_unicode_and_escapes_json() {
        let parsed = chat_body("model", "system", &[], "hi \"there\" 🩷", false);

        assert_eq!(parsed["model"], "model");
        assert_eq!(parsed["models"][0], "model");
        assert_eq!(parsed["models"][1], "@preset/free");
        assert_eq!(parsed["messages"][1]["content"], "hi \"there\" 🩷");
    }

    #[test]
    fn chat_body_excludes_reasoning_and_omits_tools() {
        let parsed = chat_body("model", "system", &[], "hi", false);

        assert_eq!(parsed["reasoning"]["exclude"], true);
        assert!(parsed["reasoning"].get("effort").is_none());
        assert!(parsed.get("tools").is_none());
    }

    #[test]
    fn chat_body_thinking_enables_reasoning_but_excludes_trace() {
        let parsed = chat_body("model", "system", &[], "hi", true);

        assert_eq!(parsed["reasoning"]["effort"], "high");
        assert_eq!(parsed["reasoning"]["exclude"], true);
    }

    #[test]
    fn fwend_system_prompt_replaces_name_placeholder() {
        let prompt = fwend_system_prompt("you are [[FREND_NAME]]!", "Qwen", "Jodie");

        assert!(prompt.starts_with("you are Qwen!"));
        assert!(prompt.contains("Never start your reply with \"Qwen:\""));
    }

    #[test]
    fn request_history_tags_user_and_other_models() {
        let claude_reply = Message::assistant_with_author("hi jodie".to_string(), "Claude");
        let qwen_reply = Message::assistant_with_author("hello!".to_string(), "Qwen");
        let messages = vec![
            intro_message(),
            Message::user("hey".to_string()),
            claude_reply,
            qwen_reply,
        ];

        let history = request_history(&messages, "Qwen", "Jodie");

        assert_eq!(history.len(), 3);
        assert!(matches!(history[0].role(), Role::User));
        assert_eq!(history[0].text(), "Jodie: hey");
        assert!(matches!(history[1].role(), Role::User));
        assert_eq!(history[1].text(), "Claude: hi jodie");
        assert!(matches!(history[2].role(), Role::Assistant));
        assert_eq!(history[2].text(), "hello!");
    }

    #[test]
    fn strips_reflexive_name_prefix_from_reply() {
        assert_eq!(strip_self_prefix("Qwen: hi there", "Qwen"), "hi there");
        assert_eq!(strip_self_prefix("  Qwen : hi", "Qwen"), "hi");
        assert_eq!(strip_self_prefix("Qwen is great", "Qwen"), "Qwen is great");
        assert_eq!(strip_self_prefix("hi there", "Qwen"), "hi there");
    }
}
