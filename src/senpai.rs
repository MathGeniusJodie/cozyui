// senpai: a smart, expensive model briefs a fast, cheap student before the
// student does all the actual work and gives the final answer.
//
// Flow (always, no escalation tool):
//   1. senpai (config.senpai_model) reads the user's message (a task,
//      a question, or casual chat), emits a terse free-form briefing.
//      Aggressively capped output; telegraphic style, so truncation degrades
//      gracefully.
//   2. student (config.student_model) responds, with tools (run_python,
//      wolfram, web_qa) available and the briefing in context.
//
// Cost notes: senpai's system prompt carries cache_control (cached input is
// ~1/50th the price of output tokens, so the fixed prefix is deliberately
// rich). senpai's output is the expensive part => terse style + low
// max_tokens.
//
// History across turns is curated by us (the chat API is stateless): only
// raw user/assistant exchanges are kept — senpai briefings and tool-call
// traffic never leave the turn they happened in. Old messages are compacted
// into archive blocks: full text kept locally, a one-line summary (written by
// the cheap student model) shown in context. Senpai alone can pull a block's
// full text back via the read_block tool.
//
// This file is both a module of cozyui (fwends' thinking mode calls
// `respond`) and the root-adjacent guts of the standalone `senpai` binary
// (src/senpai_main.rs calls `cli_main`). Each crate uses a different half,
// so dead-code lints are suppressed file-wide.
//
// Run: OPENROUTER_API_KEY=... cargo run --bin senpai -- "one-shot message"
//      OPENROUTER_API_KEY=... cargo run --bin senpai  (interactive chat)
#![allow(dead_code)]

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

const API: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_SENPAI: &str = "anthropic/claude-opus-4.5";
// Note: OpenRouter rejects presets whose fallback list has more than 3
// models ("'models' array must have 3 items or fewer").
const DEFAULT_STUDENT: &str = "@preset/fast";

/// Which models play which role, plus an optional persona appended to the
/// student's system prompt (so a host app can keep its own voice while
/// borrowing the senpai/student machinery).
pub struct SenpaiConfig {
    pub senpai_model: String,
    pub student_model: String,
    pub persona: Option<String>,
}

impl Default for SenpaiConfig {
    fn default() -> Self {
        Self {
            senpai_model: DEFAULT_SENPAI.to_string(),
            student_model: DEFAULT_STUDENT.to_string(),
            persona: None,
        }
    }
}

/// One-shot turn for embedding: `history` is prior raw chat messages as
/// OpenAI-style {"role", "content"} values (no compaction is applied — the
/// caller curates its own history). Returns the student's reply.
pub fn respond(
    config: &SenpaiConfig,
    history: &[Value],
    user_message: &str,
) -> Result<String, String> {
    let chat = Chat {
        archive: Vec::new(),
        recent: history.to_vec(),
    };
    run_turn(config, &chat, user_message)
}

// ---------------------------------------------------------------- transport

fn curl_post(body: &Value) -> Result<Value, String> {
    let key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| "OPENROUTER_API_KEY not set".to_string())?;
    let out = Command::new("curl")
        .args(["-s", API, "-H", "Content-Type: application/json"])
        .arg("-H")
        .arg(format!("Authorization: Bearer {key}"))
        .args(["--data-binary", "@-"]) // body via stdin: avoids argv quoting issues
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin.take().unwrap().write_all(body.to_string().as_bytes())?;
            c.wait_with_output()
        })
        .map_err(|e| format!("curl failed: {e}"))?;
    Ok(serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        json!({"error": {"message": String::from_utf8_lossy(&out.stdout).to_string()}})
    }))
}

fn choice(resp: &Value) -> &Value {
    if resp["choices"][0].is_null() {
        // Error responses have no choices; don't fail silently.
        eprintln!(
            "[api error: {}]",
            resp["error"]["message"].as_str().unwrap_or("unknown")
        );
    }
    &resp["choices"][0]
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…[truncated]", &s[..n]) }
}

// -------------------------------------------------------------------- tools

fn run_python(code: &str) -> String {
    let out = Command::new("python3")
        .args(["-c", code])
        .output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_else(|e| format!("exec error: {e}"));
    truncate(&out, 4000)
}

fn run_wolfram(code: &str) -> String {
    let out = Command::new("wolframscript")
        .args(["-code", code])
        .output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_else(|e| format!("wolframscript unavailable: {e}"));
    truncate(&out, 2000)
}

// Web Q&A: one-shot web-search-enabled sub-call using OpenRouter's
// server-side tools (the old web plugin and ":online" suffix are deprecated).
// Those tools currently 500 on @preset/ models, so this sub-call pins a
// direct model id. Short answer + epistemic status, or honest miss.
const WEB_QA_MODEL: &str = "deepseek/deepseek-v4-flash";

fn web_qa(question: &str) -> String {
    let body = json!({
        "model": WEB_QA_MODEL,
        "tools": [
            {"type": "openrouter:web_search"},
            {"type": "openrouter:web_fetch"}
        ],
        // No max_tokens: server-side tool rounds and (minimal) reasoning
        // spend from the same budget, and a clipped budget surfaced as empty
        // answers. The system prompt bounds the answer length instead.
        "reasoning": {"effort": "minimal", "exclude": true},
        "messages": [
            {"role": "system", "content":
                "Answer from web search results only. Format:\n\
                 A: <answer, <=2 lines>\n\
                 EPISTEMIC: <confident|likely|uncertain> — <1-line basis, e.g. '3 sources agree' or 'single blog post'>\n\
                 If the results don't contain the answer, output exactly:\n\
                 A: NOT FOUND\nEPISTEMIC: searched, no reliable source\n\
                 No preamble. Never answer from memory."},
            {"role": "user", "content": question}
        ]
    });
    let resp = match curl_post(&body) {
        Ok(resp) => resp,
        Err(err) => return format!("A: NOT FOUND\nEPISTEMIC: web call failed ({err})"),
    };
    let ch = choice(&resp);
    let mut text = content_text(&ch["message"]["content"]);
    if text.trim().is_empty() {
        return format!(
            "A: NOT FOUND\nEPISTEMIC: web call failed (finish_reason: {})",
            ch["finish_reason"].as_str().unwrap_or("none")
        );
    }
    // A clipped answer is still an answer; mark it rather than discard it.
    if ch["finish_reason"] == "length" {
        text.push_str("\n(answer truncated)");
    }
    text
}

// Raw page fetch: curl + crude HTML-to-text, no LLM tokens spent. For when
// the URL is already known; web_qa is for finding answers.
fn fetch_url(url: &str) -> String {
    let out = Command::new("curl")
        .args(["-sL", "--max-time", "15", url])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_else(|e| format!("fetch error: {e}"));
    truncate(&html_to_text(&out), 6000)
}

/// Crude tag stripper: drops script/style bodies, removes tags, decodes a few
/// common entities, collapses blank runs. Good enough for reading articles.
fn html_to_text(html: &str) -> String {
    fn starts_ci(bytes: &[u8], pat: &str) -> bool {
        bytes.len() >= pat.len() && bytes[..pat.len()].eq_ignore_ascii_case(pat.as_bytes())
    }

    let mut text = String::with_capacity(html.len() / 2);
    let bytes = html.as_bytes();
    let mut chars = html.char_indices().peekable();
    let mut skip_until: Option<&str> = None;
    while let Some((i, c)) = chars.next() {
        if let Some(end) = skip_until {
            if starts_ci(&bytes[i..], end) {
                skip_until = None;
            }
            continue;
        }
        if c == '<' {
            if starts_ci(&bytes[i..], "<script") {
                skip_until = Some("</script>");
            } else if starts_ci(&bytes[i..], "<style") {
                skip_until = Some("</style>");
            }
            // Skip to the closing '>'.
            for (_, c2) in chars.by_ref() {
                if c2 == '>' {
                    break;
                }
            }
            text.push(' ');
        } else {
            text.push(c);
        }
    }
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Message content as plain text: either a JSON string or, when server-side
/// tools ran, an array of parts whose text fields are concatenated.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

// ------------------------------------------------------------ chat archive
// Older conversation is compacted into blocks: full text kept locally, a
// super-short summary (made by the cheap student model) shown in context.
// Senpai can pull a block's full text by id via the read_block tool — but
// only senpai, and only when it thinks the block matters.

const RECENT_KEEP: usize = 6; // raw messages always shown verbatim
const COMPACT_TRIGGER: usize = 10; // overflow beyond RECENT_KEEP that triggers compaction
const MIN_BLOCK: usize = 2; // block size clamps: keep blocks meaningful even
const MAX_BLOCK: usize = 24; // when the boundary model answers nonsense

struct Block {
    summary: String,
    messages: Vec<Value>,
}

#[derive(Default)]
struct Chat {
    archive: Vec<Block>,
    recent: Vec<Value>,
}

impl Chat {
    /// Fold the oldest messages into summarized blocks once enough overflow
    /// has built up beyond the verbatim tail. Blocks follow topic boundaries
    /// (found by the cheap model), so each summary covers one coherent
    /// subject instead of an arbitrary slice.
    fn compact(&mut self, student_model: &str) {
        while self.recent.len() >= RECENT_KEEP + COMPACT_TRIGGER {
            let region = self.recent.len() - RECENT_KEEP;
            let cut = topic_boundary(student_model, &self.recent[..region])
                .clamp(MIN_BLOCK, region.min(MAX_BLOCK));
            let messages: Vec<Value> = self.recent.drain(..cut).collect();
            let summary = summarize_block(student_model, &messages);
            eprintln!(
                "[archived block {} ({} messages): {summary}]",
                self.archive.len(),
                messages.len()
            );
            self.archive.push(Block { summary, messages });
        }
    }

    /// One user-role message listing the archive summaries, or None. Carries
    /// a cache breakpoint: its bytes only change at compaction, so between
    /// compactions everything up to here is a stable cached prefix.
    fn summaries_message(&self) -> Option<Value> {
        if self.archive.is_empty() {
            return None;
        }
        let lines: Vec<String> = self
            .archive
            .iter()
            .enumerate()
            .map(|(id, block)| format!("[block {id}] {}", block.summary))
            .collect();
        Some(cached_text_message(
            "user",
            &format!(
                "(older conversation, summarized into blocks)\n{}",
                lines.join("\n")
            ),
        ))
    }

    fn read_block(&self, id: usize) -> String {
        match self.archive.get(id) {
            Some(block) => block_transcript(&block.messages),
            None => format!("no such block: {id}"),
        }
    }
}

/// A text message carrying an Anthropic prompt-cache breakpoint: everything
/// up to and including it becomes a reusable cached prefix (cache reads are
/// ~1/10th input price). Ignored harmlessly by providers without caching.
fn cached_text_message(role: &str, text: &str) -> Value {
    json!({"role": role, "content": [{
        "type": "text", "text": text,
        "cache_control": {"type": "ephemeral"}
    }]})
}

fn block_transcript(messages: &[Value]) -> String {
    messages
        .iter()
        .map(|message| {
            format!(
                "{}: {}",
                message["role"].as_str().unwrap_or("?"),
                message["content"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const BOUNDARY_SYSTEM: &str = "\
You will see a numbered chat transcript. The earliest messages form one
topical block. Reply with ONLY the number of the first message that starts a
clearly NEW topic. If the whole excerpt stays on one topic, reply with the
count of messages. Nothing but the number.";

/// Where the first topic ends in `messages`: index of the first message that
/// starts a new topic, i.e. the size of the leading block. Falls back to a
/// fixed cut if the model's answer doesn't parse.
fn topic_boundary(student_model: &str, messages: &[Value]) -> usize {
    let numbered: String = messages
        .iter()
        .enumerate()
        .map(|(i, message)| {
            format!(
                "{i} {}: {}\n",
                message["role"].as_str().unwrap_or("?"),
                message["content"].as_str().unwrap_or("")
            )
        })
        .collect();
    let resp = curl_post(&json!({
        // Budget covers minimal reasoning too; the answer is a bare number.
        "model": student_model, "max_tokens": 600,
        "reasoning": {"effort": "minimal", "exclude": true},
        "messages": [
            {"role": "system", "content": BOUNDARY_SYSTEM},
            {"role": "user", "content": numbered},
        ],
    }));
    resp.ok()
        .and_then(|resp| {
            choice(&resp)["message"]["content"].as_str().and_then(|text| {
                let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
        })
        .unwrap_or(8)
}

const SUMMARIZER_SYSTEM: &str = "\
Compress this chat excerpt into one tiny summary line (<=25 words).
Start with the topic in a few words. Then append any standalone facts that
do NOT follow from the topic (names, pets, preferences, decisions, dates) —
the summary is the only way anyone can know to look for them later.
Telegraphic, no preamble.
Example: discussion about baking bread; user's cat is called Bingus.";

fn summarize_block(student_model: &str, messages: &[Value]) -> String {
    let resp = curl_post(&json!({
        "model": student_model, "max_tokens": 80,
        "reasoning": {"enabled": false, "exclude": true},
        "messages": [
            {"role": "system", "content": SUMMARIZER_SYSTEM},
            {"role": "user", "content": block_transcript(messages)},
        ],
    }));
    let text = resp
        .as_ref()
        .map(|resp| {
            choice(resp)["message"]["content"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    if text.is_empty() {
        "(summary unavailable)".to_string()
    } else {
        text
    }
}

// ------------------------------------------------------------------- senpai
// Always runs first, once. Rich fixed prefix (cached), terse output.

const SENPAI_SYSTEM: &str = "\
You are senpai: a terse senior advisor briefing a not very smart or knowledgable junior agent before it
responds to a user message. The message may be a task, a question, or just casual chat. The junior agent
doesn't know better, you have to give them the best chance of responding well with the minimum ammount
of advice. (hard cap of 100 tokens).
Telegraphic style permitted: imperative fragments, no full sentences required.

The junior has tools: run_python (python3 -c), wolfram
(wolframscript, symbolic/exact math), web_qa (one factual question -> short
web-sourced answer with epistemic status, or NOT FOUND), fetch_url (raw page
text), and read_block (same as yours) — you can tell it which block to read.
For casual conversation tools are usually wrong: say so, and point out anything
the junior might miss (tone, subtext, what the user actually wants to hear).

You see the conversation so far; the final user message is the one being
responded to now. In long conversations the oldest messages arrive as
one-line summaries labeled [block N]; only the latest messages are verbatim.
You have one tool, read_block(id), returning a block's full text. Use it only
when that block plausibly matters for THIS reply (e.g. the user references
something old); reading costs money, most replies need no blocks.

Bad Example:
  message: integral of x^2 sin x, plus current marathon WR
  you: 'use the wolfram and web_qa tool'
  reason: you could be more idiot-proof.

Good Example:
  message: integral of x^2 sin x, plus current marathon WR
  you: wolfram Integrate[x^2 Sin[x],x], web_qa marathon WR, WR likely stands for world record

Good Example:
    message: How do I divide 4 oranges among 4 children if I have only one knife?
    you: the knife is a red herring, 4 oranges, 4 children, give each child one orange.

Good Example:
    message: ugh, my deploy broke at 2am again lol
    you: venting, not a request. no tools. commiserate first, maybe one light question; don't lecture.

Bad (never do):
    you: 'Looking at this message, the first thing to consider...'
    reason: lots of wasted tokens.";

const SENPAI_TOOLS_JSON: &str = r#"[
 {"type":"function","function":{"name":"read_block",
  "description":"Retrieve the full text of a summarized conversation block by its id.",
  "parameters":{"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]}}}
]"#;

// Prompt layout is append-only between compactions so Anthropic's prefix
// cache pays off every turn: [system (bp)] [summaries (bp)] [recent, plain]
// [current message (bp)]. The current message is sent raw — next turn it sits
// in `recent` byte-identical, extending the cached prefix instead of breaking
// it. Only compaction (summaries change, recent head drained) takes a miss.
fn senpai_briefing(config: &SenpaiConfig, chat: &Chat, user_message: &str) -> String {
    let tools: Value = serde_json::from_str(SENPAI_TOOLS_JSON).unwrap();
    let mut messages = vec![cached_text_message("system", SENPAI_SYSTEM)];
    if let Some(summaries) = chat.summaries_message() {
        messages.push(summaries);
    }
    messages.extend(chat.recent.iter().cloned());
    messages.push(cached_text_message("user", user_message));

    // Small tool loop: senpai may pull a few archive blocks before briefing.
    for _ in 0..4 {
        let resp = match curl_post(&json!({
            "model": config.senpai_model, "max_tokens": 150,
            "messages": messages, "tools": tools,
            "reasoning": {"enabled": false, "exclude": true},
        })) {
            Ok(resp) => resp,
            Err(err) => {
                eprintln!("[senpai call failed: {err}]");
                break;
            }
        };
        let ch = choice(&resp);
        let msg = ch["message"].clone();

        if ch["finish_reason"] == "tool_calls" {
            messages.push(msg.clone());
            for tc in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
                let args: Value = serde_json::from_str(
                    tc["function"]["arguments"].as_str().unwrap_or("{}"),
                ).unwrap_or(json!({}));
                let id = args["id"].as_u64().unwrap_or(u64::MAX) as usize;
                eprintln!("[senpai read block {id}]");
                messages.push(json!({
                    "role": "tool", "tool_call_id": tc["id"],
                    "content": chat.read_block(id)
                }));
            }
            continue;
        }

        let text = content_text(&msg["content"]);
        let text = text.trim();
        if !text.is_empty() {
            return text.to_string();
        }
        break;
    }
    "(senpai unavailable; proceed with your own judgement)".into()
}

// ------------------------------------------------------------------ student

const STUDENT_SYSTEM: &str = "\
You are a helpful assistant with tools: run_python, wolfram, web_qa,
fetch_url, read_block.
The user's message may be a task, a question, or casual conversation; tools
are only for when the answer actually needs them. Older conversation arrives
as one-line summaries labeled [block N]; read_block(id) retrieves a block's
full text when the user refers to something only summarized. Use fetch_url
when you already know the page you need; web_qa when you need an answer
found. A smart and wise senpai has read the message; their private briefing
is attached to it. Follow it unless evidence contradicts it; never mention
the briefing or senpai to the user. Work step by step when the message calls
for it, then reply to the user directly, concisely, and in a tone that
matches theirs.";

const STUDENT_TOOLS_JSON: &str = r#"[
 {"type":"function","function":{"name":"run_python",
  "description":"Run python3 -c <code>. stdout+stderr returned.",
  "parameters":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}}},
 {"type":"function","function":{"name":"wolfram",
  "description":"Run wolframscript -code <code> for symbolic/exact math.",
  "parameters":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}}},
 {"type":"function","function":{"name":"web_qa",
  "description":"Ask a web-search-backed sub-agent one factual question. Returns short answer + epistemic status, or NOT FOUND.",
  "parameters":{"type":"object","properties":{"question":{"type":"string"}},"required":["question"]}}},
 {"type":"function","function":{"name":"fetch_url",
  "description":"Fetch a URL and return its readable text (truncated). Use when the exact page is already known.",
  "parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},
 {"type":"function","function":{"name":"read_block",
  "description":"Retrieve the full text of a summarized conversation block by its [block N] id.",
  "parameters":{"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]}}}
]"#;

fn student_system(config: &SenpaiConfig) -> String {
    match &config.persona {
        Some(persona) => format!("{STUDENT_SYSTEM}\n\nPERSONA (speak as this):\n{persona}"),
        None => STUDENT_SYSTEM.to_string(),
    }
}

// One conversation turn. `chat` holds the cleaned transcript of earlier
// turns: raw user/assistant messages only, with the oldest folded into
// summarized archive blocks. The chat API is stateless, so curation is ours —
// briefings and tool traffic exist only in this turn's working transcript and
// are never carried forward. Returns the student's reply; the caller decides
// what to commit to history.
fn run_turn(config: &SenpaiConfig, chat: &Chat, user_message: &str) -> Result<String, String> {
    // 1. senpai briefs (summaries + recent messages; may pull blocks).
    let briefing = senpai_briefing(config, chat, user_message);
    eprintln!("--- senpai ---\n{briefing}\n--------------");

    // 2. student responds (same compacted view, no retrieval tool). Same
    // cache-friendly layout; the briefing-wrapped final message is the only
    // per-turn divergence.
    let tools: Value = serde_json::from_str(STUDENT_TOOLS_JSON).unwrap();
    let mut messages = vec![cached_text_message("system", &student_system(config))];
    if let Some(summaries) = chat.summaries_message() {
        messages.push(summaries);
    }
    messages.extend(chat.recent.iter().cloned());
    messages.push(json!({"role": "user", "content":
        format!("USER MESSAGE: {user_message}\n\nSENPAI BRIEFING (private):\n{briefing}")}));

    for _step in 0..20 {
        let resp = curl_post(&json!({
            "model": config.student_model, "max_tokens": 1500,
            "reasoning": {"enabled": false, "exclude": true},
            "messages": messages, "tools": tools,
        }))?;
        let ch = choice(&resp);
        let msg = ch["message"].clone();

        if ch["finish_reason"] == "tool_calls" {
            messages.push(msg.clone());
            for tc in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
                let name = tc["function"]["name"].as_str().unwrap_or("");
                let args: Value = serde_json::from_str(
                    tc["function"]["arguments"].as_str().unwrap_or("{}"),
                ).unwrap_or(json!({}));

                let result = match name {
                    "run_python" => run_python(args["code"].as_str().unwrap_or("")),
                    "wolfram" => run_wolfram(args["code"].as_str().unwrap_or("")),
                    "web_qa" => web_qa(args["question"].as_str().unwrap_or("")),
                    "fetch_url" => fetch_url(args["url"].as_str().unwrap_or("")),
                    "read_block" => {
                        chat.read_block(args["id"].as_u64().unwrap_or(u64::MAX) as usize)
                    }
                    _ => "unknown tool".into(),
                };
                eprintln!("[{name}] -> {}", truncate(&result, 200));
                messages.push(json!({
                    "role": "tool", "tool_call_id": tc["id"], "content": result
                }));
            }
            continue;
        }

        // 3. student replies to the user; only the raw exchange is kept.
        let text = content_text(&msg["content"]);
        if text.trim().is_empty() {
            let reason = ch["finish_reason"].as_str().unwrap_or("none").to_string();
            return Err(format!("empty reply (finish_reason: {reason})"));
        }
        return Ok(text);
    }
    Err("step limit reached".to_string())
}

// ---------------------------------------------------------------------- cli

fn cli_turn(config: &SenpaiConfig, chat: &mut Chat, user_message: &str) {
    let reply = match run_turn(config, chat, user_message) {
        Ok(reply) => reply,
        Err(err) => format!("({err})"),
    };
    println!("{reply}");
    chat.recent.push(json!({"role": "user", "content": user_message}));
    chat.recent.push(json!({"role": "assistant", "content": reply}));
    chat.compact(&config.student_model);
}

pub fn cli_main() {
    let config = SenpaiConfig::default();
    let message = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let mut chat = Chat::default();
    if !message.is_empty() {
        cli_turn(&config, &mut chat, &message);
        return;
    }

    // No argument: interactive chat. History carries across turns, cleaned
    // and compacted.
    let stdin = std::io::stdin();
    loop {
        eprint!("you: ");
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            return; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        cli_turn(&config, &mut chat, line);
    }
}
