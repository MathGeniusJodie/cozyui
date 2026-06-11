// senpai: a smart, expensive model briefs a fast, cheap student before the
// student does all the actual work and gives the final answer.
//
// Flow (always, no escalation tool):
//   1. senpai (anthropic/claude-opus-4.5) reads the task, emits a terse
//      free-form briefing. Aggressively capped output; telegraphic style, so
//      truncation degrades gracefully.
//   2. student (@preset/fast) executes with tools (run_python, wolfram,
//      web_qa) with the briefing in context, and answers the user.
//
// Cost notes: senpai's system prompt carries cache_control (cached input is
// ~1/50th the price of output tokens, so the fixed prefix is deliberately
// rich). senpai's output is the expensive part => terse style + low
// max_tokens.
//
// Run: OPENROUTER_API_KEY=... cargo run -- "your task"

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

const API: &str = "https://openrouter.ai/api/v1/chat/completions";
const SENPAI: &str = "anthropic/claude-opus-4.5";
const STUDENT: &str = "@preset/fast";

// ---------------------------------------------------------------- transport

fn curl_post(body: &Value) -> Value {
    let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY not set");
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
        .expect("curl failed");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        json!({"error": String::from_utf8_lossy(&out.stdout).to_string()})
    })
}

fn choice(resp: &Value) -> &Value {
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

// Web Q&A: one-shot web-search-enabled sub-call ("<model>:online" engages
// OpenRouter's web plugin). Short answer + epistemic status, or honest miss.
fn web_qa(question: &str) -> String {
    let body = json!({
        "model": format!("{STUDENT}:online"),
        "max_tokens": 200,
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
    let resp = curl_post(&body);
    choice(&resp)["message"]["content"]
        .as_str()
        .unwrap_or("A: NOT FOUND\nEPISTEMIC: web call failed")
        .to_string()
}

// ------------------------------------------------------------------- senpai
// Always runs first, once. Rich fixed prefix (cached), terse output.

const SENPAI_SYSTEM: &str = "\
You are senpai: a terse senior advisor briefing a not very smart or knowledgable junior agent before it
attempts a task. The junior agent doesn't know better, you have to give them the best chance of succeeding
with the minimum ammount of advice. (hard cap of 100 tokens).
Telegraphic style permitted: imperative fragments, no full sentences required.

The junior has tools: run_python (python3 -c), wolfram
(wolframscript, symbolic/exact math), web_qa (one factual question -> short
web-sourced answer with epistemic status, or NOT FOUND).

Bad Example:
  task: integral of x^2 sin x, plus current marathon WR
  you: 'use the wolfram and web_qa tool'
  reason: you could be more idiot-proof.

Good Example:
  task: integral of x^2 sin x, plus current marathon WR
  you: wolfram Integrate[x^2 Sin[x],x], web_qa marathon WR, WR likely stands for world record

Good Example:
    task: How do I divide 4 oranges among 4 children if I have only one knife?
    you: the knife is a red herring, 4 oranges, 4 children, give each child one orange.

Bad (never do):
    you: 'Looking at this task, the first thing to consider...'
    reason: lots of wasted tokens.";

fn senpai_briefing(task: &str) -> String {
    let messages = vec![
        // cache_control: fixed prefix cached by Anthropic via OpenRouter
        json!({"role": "system", "content": [{
            "type": "text", "text": SENPAI_SYSTEM,
            "cache_control": {"type": "ephemeral"}
        }]}),
        json!({"role": "user", "content": format!("task: {task}")}),
    ];

    let resp = curl_post(&json!({
        "model": SENPAI, "max_tokens": 150, "messages": messages,
    }));
    let text = choice(&resp)["message"]["content"].as_str().unwrap_or("");
    if text.trim().is_empty() {
        return "(senpai unavailable; proceed with your own judgement)".into();
    }
    text.trim().to_string()
}

// ------------------------------------------------------------------ student

const STUDENT_SYSTEM: &str = "\
You are a fast agent with tools: run_python, wolfram, web_qa.
A smart and wise senpai has reviewed the task; their briefing is included
with the task. Follow it unless evidence contradicts it. Work step by step,
then answer the user directly and concisely.";

const STUDENT_TOOLS_JSON: &str = r#"[
 {"type":"function","function":{"name":"run_python",
  "description":"Run python3 -c <code>. stdout+stderr returned.",
  "parameters":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}}},
 {"type":"function","function":{"name":"wolfram",
  "description":"Run wolframscript -code <code> for symbolic/exact math.",
  "parameters":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}}},
 {"type":"function","function":{"name":"web_qa",
  "description":"Ask a web-search-backed sub-agent one factual question. Returns short answer + epistemic status, or NOT FOUND.",
  "parameters":{"type":"object","properties":{"question":{"type":"string"}},"required":["question"]}}}
]"#;

fn run_task(task: &str) {
    // 1. senpai briefs.
    let briefing = senpai_briefing(task);
    eprintln!("--- senpai ---\n{briefing}\n--------------");

    // 2. student executes.
    let tools: Value = serde_json::from_str(STUDENT_TOOLS_JSON).unwrap();
    let mut messages = vec![
        json!({"role": "system", "content": STUDENT_SYSTEM}),
        json!({"role": "user", "content":
            format!("TASK: {task}\n\nSENPAI BRIEFING:\n{briefing}")}),
    ];

    for _step in 0..20 {
        let resp = curl_post(&json!({
            "model": STUDENT, "max_tokens": 1500,
            "messages": messages, "tools": tools,
        }));
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
                    _ => "unknown tool".into(),
                };
                eprintln!("[{name}] -> {}", truncate(&result, 200));
                messages.push(json!({
                    "role": "tool", "tool_call_id": tc["id"], "content": result
                }));
            }
            continue;
        }

        // 3. student gives the final answer.
        println!("{}", msg["content"].as_str().unwrap_or("(no content)"));
        return;
    }
    eprintln!("step limit reached");
}

fn main() {
    let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let task = if task.is_empty() {
        "What is the integral of x^2 * sin(x), verified numerically, and \
         what's the current world record for the marathon?".to_string()
    } else {
        task
    };
    run_task(&task);
}