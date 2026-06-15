// senpai: a smart, expensive model briefs a fast, cheap student before the
// student does all the actual work and gives the final answer.
//
// Flow (always, no escalation tool):
//   1. student model (cheap) scans the conversation history (with the
//      read_block tool) and extracts a tiny RELEVANT CONTEXT note for the
//      user's message — omitted entirely when nothing earlier matters.
//   2. senpai (config.senpai_model) reads only the user's message plus that
//      note (no history, no tools), emits a terse free-form briefing.
//      Aggressively capped output; telegraphic style, so truncation degrades
//      gracefully.
//   3. student (config.student_model) responds, with tools (run_python,
//      wolframscript, web_qa) available and the briefing in context.
//
// Cost notes: only the cheap model ever sees the full history; senpai's
// input is a small one-off (message + context note), so it carries no cache
// breakpoints. senpai's output is the expensive part => terse style + low
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
// Run: OPENROUTER_API_KEY=... BRAVE_ANSWERS_KEY=... \
//      cargo run --bin senpai -- "one-shot message"
//      OPENROUTER_API_KEY=... BRAVE_ANSWERS_KEY=... \
//      cargo run --bin senpai  (interactive chat)
#![allow(dead_code)]

use crate::openrouter::{self, FALLBACK_MODEL, content_text};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::Write;
use std::process::{Command, Stdio};

const BRAVE_ANSWERS_API: &str = "https://api.search.brave.com/res/v1/chat/completions";
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

// The fallback-detection check reads as three unrelated conditions on
// `requested`/`served`; clippy's grouping heuristic misfires here.
#[allow(clippy::suspicious_operation_groupings)]
fn curl_post(body: &Value) -> Result<Value, String> {
    let requested = body["model"].as_str().unwrap_or("");

    // OpenRouter ignores "model" when a "models" routing list is present, so
    // the requested model leads the list and the free preset is the fallback.
    let mut body = body.clone();
    if requested != FALLBACK_MODEL {
        body["models"] = json!([requested, FALLBACK_MODEL]);
    }
    let resp = openrouter::post(&body)?;

    // The response names the model that actually served the request; presets
    // resolve to arbitrary concrete models, so only a non-preset mismatch
    // proves the fallback kicked in.
    let served = resp["model"].as_str().unwrap_or("");
    if !requested.starts_with('@') && !served.is_empty() && served != requested {
        println!("[{requested} unavailable; {FALLBACK_MODEL} served {served}]");
    }
    Ok(resp)
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
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &s[..end])
}

// -------------------------------------------------------------------- tools

// Model-emitted code runs inside a bubblewrap sandbox: read-only /usr, /opt
// and /etc, throwaway tmpfs for /tmp and the home directory, no network
// (web access goes through web_qa/fetch_url instead), PID/IPC/etc.
// namespaces unshared. Wolfram is the reason for the home-dir binds: the
// kernel refuses to run without its license/config dirs (fine read-only)
// and a writable ~/.cache/Wolfram. Everything else in the real home stays
// invisible. Fails closed: without bwrap the tools error out rather than
// run unsandboxed.
fn sandboxed(program: &str, args: &[&str]) -> Command {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let mut cmd = Command::new("bwrap");
    cmd.args([
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/opt",
        "/opt",
        "--ro-bind",
        "/etc",
        "/etc",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib",
        "/lib64",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/bin",
        "/sbin",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--tmpfs",
        &home,
    ]);
    for dir in [".WolframEngine", ".Wolfram", ".config/Wolfram"] {
        let path = format!("{home}/{dir}");
        if std::path::Path::new(&path).exists() {
            cmd.args(["--ro-bind", &path, &path]);
        }
    }
    let cache = format!("{home}/.cache/Wolfram");
    if std::path::Path::new(&cache).exists() {
        cmd.args(["--bind", &cache, &cache]);
    }
    cmd.args(["--setenv", "HOME", &home]);
    cmd.args(["--unshare-all", "--die-with-parent", "--", program]);
    cmd.args(args);
    cmd
}

/// Run a program in the sandbox and return its combined stdout+stderr,
/// truncated to `cap` bytes. `fail_msg` prefixes a spawn/exec failure.
fn run_sandboxed(program: &str, args: &[&str], fail_msg: &str, cap: usize) -> String {
    let out = sandboxed(program, args).output().map_or_else(
        |e| format!("{fail_msg}: {e}"),
        |o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        },
    );
    truncate(&out, cap)
}

fn run_python(code: &str) -> String {
    run_sandboxed("python3", &["-c", code], "exec error", 4000)
}

fn run_wolframscript(code: &str) -> String {
    run_sandboxed(
        "wolframscript",
        &["-code", code],
        "wolframscript unavailable",
        2000,
    )
}

// Web Q&A: one Brave Answers call, capped and configured for a tiny
// search-backed answer. Failures surface as an honest NOT FOUND.
const BRAVE_ANSWER_MODEL: &str = "brave";

fn web_qa(question: &str) -> String {
    brave_answer_qa(question)
        .unwrap_or_else(|err| format!("A: NOT FOUND\nEPISTEMIC: web call failed ({err})"))
}

fn brave_answer_qa(question: &str) -> Result<String, String> {
    let body = json!({
        "model": BRAVE_ANSWER_MODEL,
        "stream": false,
        "max_completion_tokens": 140,
        "web_search_options": {
            "country": "CA",
            "language": "en",
            "safesearch": "moderate",
            "enable_entities": false,
            "enable_citations": true,
            "enable_research": false
        },
        // Brave Answers allows exactly one message, and derives its search
        // query from it — so the question leads and the instructions trail
        // in parentheses (instructions-first polluted the search query and
        // produced NOT FOUND on easy questions).
        "messages": [
            {"role": "user", "content": format!(
                "{}\n\n\
                 (Answer from current Brave web results only. Format exactly:\n\
                 A: <answer, <=2 lines>\n\
                 EPISTEMIC: <confident|likely|uncertain> - <1-line source basis>\n\
                 If the results don't contain the answer, output exactly:\n\
                 A: NOT FOUND\nEPISTEMIC: searched, no reliable source\n\
                 No preamble.)",
                brave_query(question)
            )}
        ]
    });
    let resp = curl_post_brave_answers(&body)?;
    if resp["choices"][0].is_null() {
        return Err(api_error_message(&resp));
    }
    let ch = &resp["choices"][0];
    let mut text = content_text(&ch["message"]["content"]);
    if text.trim().is_empty() {
        return Err(format!(
            "empty Brave Answers response (finish_reason: {})",
            ch["finish_reason"].as_str().unwrap_or("none")
        ));
    }
    if ch["finish_reason"] == "length" {
        text.push_str("\n(answer truncated)");
    }
    Ok(truncate(&text, 1200))
}

fn curl_post_brave_answers(body: &Value) -> Result<Value, String> {
    let key =
        std::env::var("BRAVE_ANSWERS_KEY").map_err(|_| "BRAVE_ANSWERS_KEY not set".to_string())?;
    let out = Command::new("curl")
        .args([
            "-sS",
            "--compressed",
            BRAVE_ANSWERS_API,
            "-H",
            "Accept: application/json",
            "-H",
            "Accept-Encoding: gzip",
            "-H",
            "Content-Type: application/json",
        ])
        .arg("-H")
        .arg(format!("x-subscription-token: {key}"))
        .args(["--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin
                .take()
                .unwrap()
                .write_all(body.to_string().as_bytes())?;
            c.wait_with_output()
        })
        .map_err(|e| format!("Brave Answers curl failed: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Brave Answers curl failed: {stderr}"));
    }
    Ok(parse_json_body(&out.stdout))
}

/// Parse a response body as JSON; on failure wrap the raw text in an
/// error-shaped Value so `api_error_message` can surface it.
fn parse_json_body(stdout: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(
        |_| json!({"error": {"message": String::from_utf8_lossy(stdout).to_string()}}),
    )
}

fn brave_query(question: &str) -> String {
    compact_text(
        &question
            .split_whitespace()
            .take(50)
            .collect::<Vec<_>>()
            .join(" "),
        400,
    )
}

fn compact_text(text: &str, max_bytes: usize) -> String {
    let text = html_to_text(text);
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", text[..end].trim_end())
}

fn api_error_message(resp: &Value) -> String {
    resp["error"]["message"]
        .as_str()
        .or_else(|| resp["error"]["detail"].as_str())
        .or_else(|| resp["error"].as_str())
        .unwrap_or("unknown API error")
        .to_string()
}

// Raw page fetch: curl + crude HTML-to-text, no LLM tokens spent. For when
// the URL is already known; web_qa is for finding answers.
fn fetch_url(url: &str) -> String {
    let out = Command::new("curl")
        .args(["-sL", "--max-time", "15", url])
        .output()
        .map_or_else(
            |e| format!("fetch error: {e}"),
            |o| String::from_utf8_lossy(&o.stdout).to_string(),
        );
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
    let mut chars = html.char_indices();
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
        Some(text_message(
            "user",
            &format!(
                "(older conversation, summarized into blocks)\n{}",
                lines.join("\n")
            ),
            true,
        ))
    }

    fn read_block(&self, id: usize) -> String {
        self.archive.get(id).map_or_else(
            || format!("no such block: {id}"),
            |block| block_transcript(&block.messages),
        )
    }
}

/// A text message, optionally carrying an Anthropic prompt-cache breakpoint:
/// everything up to and including it becomes a reusable cached prefix (cache
/// reads are ~1/10th input price, but writes cost +25% — so `cached` should
/// be false for one-off calls whose prefix will never be reused). Ignored
/// harmlessly by providers without caching.
fn text_message(role: &str, text: &str, cached: bool) -> Value {
    if !cached {
        return json!({"role": role, "content": text});
    }
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
    let mut numbered = String::new();
    for (i, message) in messages.iter().enumerate() {
        let _ = writeln!(
            numbered,
            "{i} {}: {}",
            message["role"].as_str().unwrap_or("?"),
            message["content"].as_str().unwrap_or("")
        );
    }
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
            choice(&resp)["message"]["content"]
                .as_str()
                .and_then(|text| {
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
Advise a very weak junior agent before it answers the user.
Junior has tools: run_python, wolframscript, web_qa (use for ALL factual/current claims)
Pareto optimal of compact and idiot-proof. imperative fragments. 50 word hard cap.

Examples:
  user: good morning!
  you: just answer
  
  user: integral of x^2 sin x, plus current marathon WR
  you: wolframscript Integrate[x^2 Sin[x],x], web_qa 'marathon world record'

  user: How do I divide 4 oranges among 4 children if I have only one knife?
  you: knife is a red herring, one orange per child.
  
  user: ugh, my deploy broke at 2am again lol
  you: venting, not a request. commiserate first, maybe one light question.
  (tone/subtext)

A RELEVANT CONTEXT note extracted from the earlier conversation may be
attached to the message; weave it into your advice where it matters.";

// Senpai never sees the conversation: just the user message and, when the
// context pass found something, a tiny note. One small uncached call.
fn senpai_briefing(config: &SenpaiConfig, user_message: &str, context: Option<&str>) -> String {
    let mut user = user_message.to_string();
    if let Some(context) = context {
        let _ = write!(
            user,
            "\n\nRELEVANT CONTEXT (from earlier conversation):\n{context}"
        );
    }
    let body = json!({
        "model": config.senpai_model, "max_tokens": 150,
        "messages": [
            text_message("system", SENPAI_SYSTEM, false),
            text_message("user", &user, false),
        ],
        "reasoning": {"enabled": false, "exclude": true},
    });
    match curl_post(&body) {
        Ok(resp) => {
            let text = content_text(&choice(&resp)["message"]["content"]);
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
        Err(err) => eprintln!("[senpai call failed: {err}]"),
    }
    "(senpai unavailable; proceed with your own judgement)".into()
}

// ----------------------------------------------------------- context pass

const CONTEXT_SYSTEM: &str = "\
You scan a conversation and extract only what matters for answering the
final user message. Output a tiny note (<=40 words, telegraphic fragments):
facts, names, decisions, or earlier results the reply must respect.
The oldest messages arrive as one-line summaries labeled [block N]; you have
one tool, read_block(id), returning a block's full text. Use it only when
that block plausibly matters for THIS message; most messages need no blocks.
If nothing earlier is relevant, output exactly NONE.";

const CONTEXT_TOOLS_JSON: &str = r#"[
 {"type":"function","function":{"name":"read_block",
  "description":"Retrieve the full text of a summarized conversation block by its id.",
  "parameters":{"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]}}}
]"#;

// Prompt layout is append-only between compactions so the provider's prefix
// cache pays off every turn: [system (bp)] [summaries (bp)] [recent, plain]
// [current message (bp)]. The current message is sent raw — next turn it sits
// in `recent` byte-identical, extending the cached prefix instead of breaking
// it. Only compaction (summaries change, recent head drained) takes a miss.
fn relevant_context(config: &SenpaiConfig, chat: &Chat, user_message: &str) -> Option<String> {
    let has_archive = !chat.archive.is_empty();
    if !has_archive && chat.recent.is_empty() {
        return None;
    }

    let mut messages = vec![text_message("system", CONTEXT_SYSTEM, true)];
    if let Some(summaries) = chat.summaries_message() {
        messages.push(summaries);
    }
    messages.extend(chat.recent.iter().cloned());
    messages.push(text_message("user", user_message, true));

    // Small tool loop: the extractor may pull a few archive blocks first.
    // No archive blocks => no read_block: skip the tool schema.
    for _ in 0..4 {
        let mut body = json!({
            "model": config.student_model, "max_tokens": 100,
            "messages": messages,
            "reasoning": {"enabled": false, "exclude": true},
        });
        if has_archive {
            body["tools"] = serde_json::from_str(CONTEXT_TOOLS_JSON).unwrap();
        }
        let resp = match curl_post(&body) {
            Ok(resp) => resp,
            Err(err) => {
                eprintln!("[context pass failed: {err}]");
                return None;
            }
        };
        let ch = choice(&resp);
        let msg = ch["message"].clone();

        if ch["finish_reason"] == "tool_calls" {
            messages.push(msg.clone());
            for tc in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
                let args: Value =
                    serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}"))
                        .unwrap_or_else(|_| json!({}));
                let id = args["id"].as_u64().unwrap_or(u64::MAX) as usize;
                eprintln!("[context pass read block {id}]");
                messages.push(json!({
                    "role": "tool", "tool_call_id": tc["id"],
                    "content": chat.read_block(id)
                }));
            }
            continue;
        }

        let text = content_text(&msg["content"]);
        let text = text.trim();
        if text.is_empty() || text == "NONE" {
            return None;
        }
        return Some(text.to_string());
    }
    None
}

// ------------------------------------------------------------------ student

const STUDENT_SYSTEM: &str = "\
Tools are only for when the answer actually needs them.
Older conversation arrives as one-line summaries labeled [block N];
read_block(id) retrieves a block's full text when the user refers to something
only summarized.
A smart and wise senpai has read the message; their private briefing
is attached to it. Follow it unless evidence from tools contradicts it; never mention
the briefing or senpai to the user. Work step by step when the message calls
for it, then reply to the user directly, concisely, and in a tone that
matches theirs.";

const STUDENT_TOOLS_JSON: &str = r#"[
 {"type":"function","function":{"name":"run_python",
  "description":"Run python3 -c <code>. stdout+stderr returned.",
  "parameters":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}}},
 {"type":"function","function":{"name":"wolframscript",
  "description":"Run wolframscript -code <code> for symbolic/exact math. ALWAYS use for math, unless python is the better tool, but don't do math by hand ever.",
  "parameters":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}}},
 {"type":"function","function":{"name":"web_qa",
  "description":"Ask a web-search-backed sub-agent one factual question. Returns short answer + epistemic status, or NOT FOUND. ALWAYS use this for factual/current claims",
  "parameters":{"type":"object","properties":{"question":{"type":"string"}},"required":["question"]}}},
 {"type":"function","function":{"name":"fetch_url",
  "description":"Fetch a URL and return its readable text (truncated). Use when the exact page is already known.",
  "parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},
 {"type":"function","function":{"name":"read_block",
  "description":"Retrieve the full text of a summarized conversation block by its [block N] id.",
  "parameters":{"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]}}}
]"#;

fn student_system(config: &SenpaiConfig) -> String {
    // Day granularity only: this string carries a cache breakpoint, so a
    // finer-grained timestamp would break the cached prefix on every call.
    let mut system = format!("{STUDENT_SYSTEM}\nToday's date is {}.", today());
    if let Some(persona) = &config.persona {
        let _ = write!(system, "\n\nPERSONA (speak as this):\n{persona}");
    }
    system
}

fn today() -> String {
    Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

// One conversation turn. `chat` holds the cleaned transcript of earlier
// turns: raw user/assistant messages only, with the oldest folded into
// summarized archive blocks. The chat API is stateless, so curation is ours —
// briefings and tool traffic exist only in this turn's working transcript and
// are never carried forward. Returns the student's reply; the caller decides
// what to commit to history.
fn run_turn(config: &SenpaiConfig, chat: &Chat, user_message: &str) -> Result<String, String> {
    // 1. the cheap model distills the history into a note for senpai
    //    (it alone sees summaries + recent messages; may pull blocks).
    let context = relevant_context(config, chat, user_message);
    if let Some(context) = &context {
        eprintln!("--- context ---\n{context}\n---------------");
    }

    // 2. senpai briefs from the message + note alone.
    let briefing = senpai_briefing(config, user_message, context.as_deref());
    eprintln!("--- senpai ---\n{briefing}\n--------------");

    // 3. student responds with its own compacted view of the history. Same
    // cache-friendly layout; the briefing-wrapped final message is the only
    // per-turn divergence.
    let tools: Value = serde_json::from_str(STUDENT_TOOLS_JSON).unwrap();
    let cached = !chat.archive.is_empty() || !chat.recent.is_empty();
    let mut messages = vec![text_message("system", &student_system(config), cached)];
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
                let args: Value =
                    serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}"))
                        .unwrap_or_else(|_| json!({}));

                let result = match name {
                    "run_python" => run_python(args["code"].as_str().unwrap_or("")),
                    "wolframscript" => run_wolframscript(args["code"].as_str().unwrap_or("")),
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

        // 4. student replies to the user; only the raw exchange is kept.
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
    chat.recent
        .push(json!({"role": "user", "content": user_message}));
    chat.recent
        .push(json!({"role": "assistant", "content": reply}));
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

#[cfg(test)]
mod tests {
    use super::*;

    // Touches the real bwrap/python/wolframscript installs; run manually:
    // cargo test sandbox -- --ignored
    #[test]
    #[ignore]
    fn sandbox_runs_tools_and_blocks_escapes() {
        assert_eq!(
            run_python("print(2**100)").trim(),
            "1267650600228229401496703205376"
        );
        assert_eq!(run_wolframscript("2^10").trim(), "1024");
        // Writes to home land in tmpfs; the file must not exist afterwards.
        let home = std::env::var("HOME").unwrap();
        run_python(&format!(
            "open('{home}/sandbox_escape_test','w').write('x')"
        ));
        assert!(!std::path::Path::new(&format!("{home}/sandbox_escape_test")).exists());
    }
}
