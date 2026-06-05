use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::bitmap_font::BitmapFont;
use crate::comicoro_font;
use crate::palette_color;
use crate::text_input::{EditKey, KeyInput, edit_key};
use crate::{Framebuffer, Image, Palette, Rect, Rgba};
use serde_json::{Value, json};

const SCALE: usize = 1;
const GLYPH_SCALE: usize = 1;
const W: usize = 268;
const H: usize = 318;
const PAD: usize = 8;
const MODEL_Y: usize = 8;
const MODEL_GAP: usize = 8;
const SPEAKER_GAP: usize = 6;
const CHAT_Y: usize = 48;
const CHAT_H: usize = 142;
const INPUT_X: usize = 24;
const INPUT_Y: usize = 196;
const INPUT_TEXT_Y: usize = 24;
const TEXT_PAD: usize = 7;
const BUBBLE_PAD_X: usize = 14;
const BUBBLE_PAD_TOP: usize = 8;
const BUBBLE_PAD_BOTTOM: usize = 11;
const STICKY_PAD_LEFT: usize = 10;
const STICKY_PAD_RIGHT: usize = 20;
const STICKY_PAD_TOP: usize = 20;
const STICKY_PAD_BOTTOM: usize = 20;
const BUBBLE_GAP: usize = 5;
const BUBBLE_MIN_W: usize = 38;
const BUBBLE_MAX_W: usize = 142;
const BUBBLE_LEFT_CAP: usize = 21;
const BUBBLE_RIGHT_CAP: usize = 21;
const BUBBLE_TOP_CAP: usize = 17;
const BUBBLE_BOTTOM_CAP: usize = 17;
const STICKY_LEFT_CAP: usize = 10;
const STICKY_RIGHT_CAP: usize = 20;
const STICKY_TOP_CAP: usize = 20;
const STICKY_BOTTOM_CAP: usize = 20;
const SCROLL_STEP: usize = 24;
const LINE_H: usize = 16;
const MAX_INPUT_CHARS: usize = 96;
const SYSTEM_PROMPT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fwends_system_prompt.txt");
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const REQUEST_TIMEOUT_SECS: &str = "30";
const FOCUS_PENCIL_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/focus_pencil.png");
const USER_STICKY_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sticky.png");
const INPUT_STICKY_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sticky_stack.png");
const PENCIL_TIP_X: usize = 0;
const PENCIL_TIP_Y: usize = 24;

const MODELS: [Model; 4] = [
    Model {
        id: "anthropic/claude-haiku-4.5",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/claw.png"),
    },
    Model {
        id: "deepseek/deepseek-v4-flash",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/deep.png"),
    },
    Model {
        id: "qwen/qwen3.6-35b-a3b",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/qwen.png"),
    },
    Model {
        id: "moonshotai/kimi-k2.6",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kimi.png"),
    },
];

pub(crate) struct Fwends {
    avatars: [Image; 4],
    bubble: Image,
    user_sticky: Image,
    input_sticky: Image,
    pencil: Image,
    font: BitmapFont,
    messages: Vec<Message>,
    input: String,
    selected_model: usize,
    focused: bool,
    scroll_y: usize,
    model_slot_w: usize,
    model_slot_h: usize,
    pending: Option<Receiver<Result<String, String>>>,
    system_prompt: String,
}

impl Fwends {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let avatars = [
            Image::load(MODELS[0].asset_path, palette)?,
            Image::load(MODELS[1].asset_path, palette)?,
            Image::load(MODELS[2].asset_path, palette)?,
            Image::load(MODELS[3].asset_path, palette)?,
        ];
        let model_slot_w = avatars.iter().map(|avatar| avatar.width).max().unwrap_or(1);
        let model_slot_h = avatars
            .iter()
            .map(|avatar| avatar.height)
            .max()
            .unwrap_or(1);
        let selected_model = 0;

        Ok(Self {
            avatars,
            bubble: Image::load(
                concat!(env!("CARGO_MANIFEST_DIR"), "/assets/bubble.png"),
                palette,
            )?,
            user_sticky: Image::load(USER_STICKY_PATH, palette)?,
            input_sticky: Image::load(INPUT_STICKY_PATH, palette)?,
            pencil: Image::load(FOCUS_PENCIL_PATH, palette)?,
            font: BitmapFont::load(&comicoro_font::COMICORO_SPEC)?,
            messages: vec![Message::intro(
                "pick a fwend and say hi".to_string(),
                selected_model,
            )],
            input: String::new(),
            selected_model,
            focused: false,
            scroll_y: 0,
            model_slot_w,
            model_slot_h,
            pending: None,
            system_prompt: fs::read_to_string(SYSTEM_PROMPT_PATH).unwrap_or_else(|_| {
                "You are a warm, concise chat companion. Answer directly and never reveal hidden reasoning.".to_string()
            }),
        })
    }

    pub(crate) fn width(&self) -> usize {
        W * SCALE
    }

    pub(crate) fn height(&self) -> usize {
        H * SCALE
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK).transparent()
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));

        for (index, avatar) in self.avatars.iter().enumerate() {
            self.draw_model_button(fb, palette, index, avatar);
        }

        self.draw_messages(fb, palette);
        self.draw_input(fb, palette);
    }

    pub(crate) fn click(&mut self, x: i16, y: i16) {
        self.focused = false;
        if x < 0 || y < 0 {
            return;
        }

        let x = x as usize / SCALE;
        let y = y as usize / SCALE;
        for index in 0..MODELS.len() {
            let slot_x = self.model_slot_x(index);
            if x >= slot_x
                && x < slot_x + self.model_slot_w
                && y >= MODEL_Y
                && y < MODEL_Y + self.model_slot_h
            {
                self.selected_model = index;
                return;
            }
        }

        if x >= INPUT_X
            && x < INPUT_X + self.input_sticky.width
            && y >= INPUT_Y
            && y < INPUT_Y + self.input_sticky.height
        {
            self.focused = true;
        }
    }

    pub(crate) fn handle_key_press(&mut self, input: &KeyInput) -> Result<(), Box<dyn Error>> {
        match edit_key(input) {
            EditKey::Insert(ch) if self.focused => {
                if self.input.chars().count() < MAX_INPUT_CHARS {
                    self.input.push(ch);
                }
            }
            EditKey::Backspace if self.focused => {
                self.input.pop();
            }
            EditKey::Enter if self.focused => self.send(),
            EditKey::Escape => self.focused = false,
            EditKey::Tab => self.selected_model = (self.selected_model + 1) % MODELS.len(),
            EditKey::Left => {
                self.selected_model = self
                    .selected_model
                    .checked_sub(1)
                    .unwrap_or(MODELS.len() - 1);
            }
            EditKey::Right => self.selected_model = (self.selected_model + 1) % MODELS.len(),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn drain_reply(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };

        let Ok(reply) = rx.try_recv() else {
            return false;
        };

        self.pending = None;
        let text = match reply {
            Ok(text) => text,
            Err(err) => format!("oops: {err}"),
        };
        if let Some(message) = self.messages.last_mut()
            && message.kind == MessageKind::Pending
        {
            message.text = text;
            message.kind = MessageKind::Normal;
            self.scroll_to_bottom();
            return true;
        }
        self.messages
            .push(Message::assistant(text, self.selected_model));
        self.scroll_to_bottom();
        true
    }

    pub(crate) fn scroll_up(&mut self, x: i16, y: i16) {
        if !chat_contains(x, y) {
            return;
        }
        self.scroll_y = self.scroll_y.saturating_sub(SCROLL_STEP);
    }

    pub(crate) fn scroll_down(&mut self, x: i16, y: i16) {
        if !chat_contains(x, y) {
            return;
        }
        self.scroll_y = (self.scroll_y + SCROLL_STEP).min(self.max_scroll());
    }

    fn send(&mut self) {
        if self.pending.is_some() {
            return;
        }

        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }

        self.input.clear();
        self.messages.push(Message::user(text.clone()));
        let selected_model = self.selected_model;
        self.messages.push(Message::pending(selected_model));
        self.scroll_to_bottom();

        let model = MODELS[selected_model].id.to_string();
        let system_prompt = self.system_prompt.clone();
        let history = request_history(&self.messages);
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);

        thread::spawn(move || {
            let result = send_openrouter_request(&model, &system_prompt, &history, &text);
            let _ = tx.send(result);
        });
    }

    fn draw_model_button(
        &self,
        fb: &mut Framebuffer,
        palette: &Palette,
        index: usize,
        avatar: &Image,
    ) {
        let x = self.model_slot_x(index);
        let selected = index == self.selected_model;
        if selected {
            fill_scaled_rect(
                fb,
                x.saturating_sub(1),
                MODEL_Y.saturating_sub(1),
                self.model_slot_w + 2,
                self.model_slot_h + 2,
                palette.color(palette_color::CYAN),
            );
        }

        let avatar_x = x + self.model_slot_w.saturating_sub(avatar.width) / 2;
        let avatar_y = MODEL_Y + self.model_slot_h.saturating_sub(avatar.height);
        fb.draw_image(
            avatar,
            (avatar_x * SCALE) as isize,
            (avatar_y * SCALE) as isize,
            SCALE,
        );
    }

    fn model_slot_x(&self, index: usize) -> usize {
        PAD + index * (self.model_slot_w + MODEL_GAP)
    }

    fn draw_messages(&self, fb: &mut Framebuffer, palette: &Palette) {
        let layouts = self.message_layouts();
        let viewport_top = CHAT_Y;
        let viewport_bottom = CHAT_Y + CHAT_H;

        for layout in layouts {
            let y = CHAT_Y as isize + layout.y as isize - self.scroll_y as isize;
            if y >= viewport_bottom as isize || y + layout.h as isize <= viewport_top as isize {
                continue;
            }

            if layout.style.show_avatar {
                self.draw_speaker_avatar(fb, y, layout.h, layout.model_index);
            }
            self.draw_bubble(fb, layout.x, y, layout.w, layout.h, layout.style);
            let text_x = layout.x + layout.style.pad_left;
            let mut text_y = y + layout.style.pad_top as isize;
            for line in layout.lines {
                if text_y >= viewport_top as isize
                    && text_y + self.font.cell_h() as isize <= viewport_bottom as isize
                {
                    self.font.draw_text(
                        fb,
                        &line,
                        text_x * SCALE,
                        text_y as usize * SCALE,
                        GLYPH_SCALE,
                        palette.color(palette_color::BLACK),
                    );
                }
                text_y += LINE_H as isize;
            }
        }
    }

    fn draw_bubble(
        &self,
        fb: &mut Framebuffer,
        x: usize,
        y: isize,
        w: usize,
        h: usize,
        style: MessageStyle,
    ) {
        let clip_top = CHAT_Y;
        let clip_bottom = CHAT_Y + CHAT_H;
        let image = match style.skin {
            MessageSkin::Bubble => &self.bubble,
            MessageSkin::Sticky => &self.user_sticky,
        };

        for dy in 0..h {
            let py = y + dy as isize;
            if py < clip_top as isize || py >= clip_bottom as isize {
                continue;
            }
            for dx in 0..w {
                let px = x + dx;
                let sx = stretch_source_coord(dx, w, image.width, style.left_cap, style.right_cap);
                let sy = stretch_source_coord(dy, h, image.height, style.top_cap, style.bottom_cap);
                let color = image.at(sx, sy);
                if color.a == 0 {
                    continue;
                }
                fb.fill_rect(px * SCALE, py as usize * SCALE, SCALE, SCALE, color);
            }
        }
    }

    fn message_layouts(&self) -> Vec<MessageLayout> {
        let mut layouts = Vec::new();
        let mut y = 0;
        for message in &self.messages {
            let style = message.style();
            let max_text_w = BUBBLE_MAX_W - style.pad_left - style.pad_right;
            let lines = self.font.wrap_lines(&message.text, max_text_w);
            let text_w = lines
                .iter()
                .map(|line| self.font.text_width(line))
                .max()
                .unwrap_or(0);
            let w = (text_w + style.pad_left + style.pad_right).clamp(BUBBLE_MIN_W, BUBBLE_MAX_W);
            let h = (lines.len() * LINE_H + style.pad_top + style.pad_bottom)
                .max(style.top_cap + style.bottom_cap + 1);
            let x = if style.align_right {
                W - PAD - w
            } else {
                self.assistant_bubble_x()
            };
            layouts.push(MessageLayout {
                style,
                model_index: message.model_index,
                lines,
                x,
                y,
                w,
                h,
            });
            y += h + BUBBLE_GAP;
        }
        layouts
    }

    fn assistant_bubble_x(&self) -> usize {
        PAD + self.model_slot_w + SPEAKER_GAP
    }

    fn draw_speaker_avatar(
        &self,
        fb: &mut Framebuffer,
        bubble_y: isize,
        bubble_h: usize,
        model_index: usize,
    ) {
        let avatar = &self.avatars[model_index.min(self.avatars.len() - 1)];
        let avatar_x = PAD + self.model_slot_w.saturating_sub(avatar.width) / 2;
        let avatar_y = bubble_y + bubble_h as isize - avatar.height as isize;
        let clip_top = CHAT_Y as isize;
        let clip_bottom = (CHAT_Y + CHAT_H) as isize;

        fb.draw_image_region_mapped(
            avatar,
            Rect::new(0, 0, avatar.width, avatar.height),
            (avatar_x * SCALE) as isize,
            avatar_y * SCALE as isize,
            SCALE,
            Some(Rect::new(
                0,
                clip_top as usize,
                W * SCALE,
                (clip_bottom - clip_top) as usize,
            )),
            Some,
        );
    }

    fn content_height(&self) -> usize {
        self.message_layouts()
            .last()
            .map(|layout| layout.y + layout.h)
            .unwrap_or(0)
    }

    fn max_scroll(&self) -> usize {
        self.content_height().saturating_sub(CHAT_H)
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_y = self.max_scroll();
    }

    fn draw_input(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.draw_image(
            &self.input_sticky,
            (INPUT_X * SCALE) as isize,
            (INPUT_Y * SCALE) as isize,
            SCALE,
        );

        let label = if self.pending.is_some() {
            "wait a sec..."
        } else {
            &self.input
        };
        let max_width = self.input_sticky.width - TEXT_PAD * 2;
        let lines = self.font.wrap_lines(label, max_width);
        for (index, line) in lines.into_iter().take(5).enumerate() {
            self.font.draw_text(
                fb,
                &line,
                (INPUT_X + TEXT_PAD) * SCALE,
                (INPUT_Y + INPUT_TEXT_Y + index * LINE_H) * SCALE,
                GLYPH_SCALE,
                palette.color(palette_color::BLACK),
            );
        }
        self.draw_focused_pencil(fb);
    }

    fn draw_focused_pencil(&self, fb: &mut Framebuffer) {
        if !self.focused {
            return;
        }

        let (cursor_x, cursor_y) = self.input_cursor_position();
        let dest_x = cursor_x.saturating_sub(PENCIL_TIP_X);
        let dest_y = cursor_y.saturating_sub(PENCIL_TIP_Y);
        fb.draw_image(
            &self.pencil,
            (dest_x * SCALE) as isize,
            (dest_y * SCALE) as isize,
            SCALE,
        );
    }

    fn input_cursor_position(&self) -> (usize, usize) {
        let max_width = self.input_sticky.width - TEXT_PAD * 2;
        let lines = self.font.wrap_lines(&self.input, max_width);
        let line_index = lines.len().saturating_sub(1).min(4);
        (
            INPUT_X + TEXT_PAD + self.font.text_width(&lines[line_index]).min(max_width),
            INPUT_Y + INPUT_TEXT_Y + line_index * LINE_H,
        )
    }
}

#[derive(Clone, Copy)]
struct Model {
    id: &'static str,
    asset_path: &'static str,
}

#[derive(Clone)]
struct Message {
    role: Role,
    text: String,
    model_index: usize,
    kind: MessageKind,
}

struct MessageLayout {
    style: MessageStyle,
    model_index: usize,
    lines: Vec<String>,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Message {
    fn user(text: String) -> Self {
        Self {
            role: Role::User,
            text,
            model_index: 0,
            kind: MessageKind::Normal,
        }
    }

    fn assistant(text: String, model_index: usize) -> Self {
        Self {
            role: Role::Assistant,
            text,
            model_index,
            kind: MessageKind::Normal,
        }
    }

    fn intro(text: String, model_index: usize) -> Self {
        Self {
            kind: MessageKind::Intro,
            ..Self::assistant(text, model_index)
        }
    }

    fn pending(model_index: usize) -> Self {
        Self {
            kind: MessageKind::Pending,
            ..Self::assistant("...".to_string(), model_index)
        }
    }

    fn style(&self) -> MessageStyle {
        match self.role {
            Role::User => USER_MESSAGE_STYLE,
            Role::Assistant => ASSISTANT_MESSAGE_STYLE,
        }
    }
}

#[derive(Clone, Copy)]
enum Role {
    User,
    Assistant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    Normal,
    Intro,
    Pending,
}

#[derive(Clone, Copy)]
struct MessageStyle {
    skin: MessageSkin,
    pad_left: usize,
    pad_right: usize,
    pad_top: usize,
    pad_bottom: usize,
    left_cap: usize,
    right_cap: usize,
    top_cap: usize,
    bottom_cap: usize,
    align_right: bool,
    show_avatar: bool,
}

#[derive(Clone, Copy)]
enum MessageSkin {
    Bubble,
    Sticky,
}

const ASSISTANT_MESSAGE_STYLE: MessageStyle = MessageStyle {
    skin: MessageSkin::Bubble,
    pad_left: BUBBLE_PAD_X,
    pad_right: BUBBLE_PAD_X,
    pad_top: BUBBLE_PAD_TOP,
    pad_bottom: BUBBLE_PAD_BOTTOM,
    left_cap: BUBBLE_LEFT_CAP,
    right_cap: BUBBLE_RIGHT_CAP,
    top_cap: BUBBLE_TOP_CAP,
    bottom_cap: BUBBLE_BOTTOM_CAP,
    align_right: false,
    show_avatar: true,
};

const USER_MESSAGE_STYLE: MessageStyle = MessageStyle {
    skin: MessageSkin::Sticky,
    pad_left: STICKY_PAD_LEFT,
    pad_right: STICKY_PAD_RIGHT,
    pad_top: STICKY_PAD_TOP,
    pad_bottom: STICKY_PAD_BOTTOM,
    left_cap: STICKY_LEFT_CAP,
    right_cap: STICKY_RIGHT_CAP,
    top_cap: STICKY_TOP_CAP,
    bottom_cap: STICKY_BOTTOM_CAP,
    align_right: true,
    show_avatar: false,
};

fn request_history(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|message| message.kind == MessageKind::Normal)
        .rev()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn send_openrouter_request(
    model: &str,
    system_prompt: &str,
    history: &[Message],
    latest_text: &str,
) -> Result<String, String> {
    let api_key =
        env::var("OPENROUTER_API_KEY").map_err(|_| "OPENROUTER_API_KEY is not set".to_string())?;
    let body = chat_body(model, system_prompt, history, latest_text);
    let body_file = CurlBodyFile::new(body.as_bytes())?;
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", REQUEST_TIMEOUT_SECS, "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("curl failed: {err}"))?;
    let config = format!(
        "url = \"{}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\n",
        OPENROUTER_URL,
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

    let response = String::from_utf8_lossy(&output.stdout);
    extract_content(&response).map_err(|err| format!("{err}: {}", compact_error(&response)))
}

fn curl_config_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
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
        "cozyui-openrouter-{}-{}-{:?}.json",
        std::process::id(),
        nanos,
        thread::current().id()
    ));
    path
}

fn chat_body(model: &str, system_prompt: &str, history: &[Message], latest_text: &str) -> String {
    let mut messages = vec![json!({
        "role": "system",
        "content": system_prompt.trim(),
    })];
    for message in history {
        messages.push(json!({
            "role": match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            "content": message.text,
        }));
    }
    if history.last().map(|message| message.text.as_str()) != Some(latest_text) {
        messages.push(json!({
            "role": "user",
            "content": latest_text,
        }));
    }

    json!({
        "model": model,
        "messages": messages,
        "reasoning": { "exclude": true },
        "include_reasoning": false,
    })
    .to_string()
}

fn extract_content(response: &str) -> Result<String, &'static str> {
    let value: Value = serde_json::from_str(response).map_err(|_| "invalid OpenRouter JSON")?;
    let Some(content) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
    else {
        return Err("OpenRouter response did not include assistant content");
    };

    let text = content_text(content);
    if text.trim().is_empty() {
        Err("OpenRouter assistant content was empty")
    } else {
        Ok(normalize_display_text(&text))
    }
}

fn content_text(content: &Value) -> String {
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

fn compact_error(response: &str) -> String {
    let mut text = response.replace('\n', " ");
    text.truncate(120);
    if text.is_empty() {
        "empty OpenRouter response".to_string()
    } else {
        text
    }
}

fn fill_scaled_rect(fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize, color: Rgba) {
    fb.fill_rect(x * SCALE, y * SCALE, w * SCALE, h * SCALE, color);
}

fn chat_contains(x: i16, y: i16) -> bool {
    if x < 0 || y < 0 {
        return false;
    }
    let x = x as usize / SCALE;
    let y = y as usize / SCALE;
    (PAD..W - PAD).contains(&x) && (CHAT_Y..CHAT_Y + CHAT_H).contains(&y)
}

fn stretch_source_coord(
    dest: usize,
    dest_len: usize,
    src_len: usize,
    start_cap: usize,
    end_cap: usize,
) -> usize {
    if dest_len <= start_cap + end_cap {
        let src_middle = src_len.saturating_sub(start_cap + end_cap).max(1);
        let dest_middle = dest_len.saturating_sub(start_cap + end_cap).max(1);
        if dest < start_cap.min(dest_len) {
            return dest.min(src_len - 1);
        }
        if dest >= dest_len.saturating_sub(end_cap) {
            return src_len.saturating_sub(dest_len - dest).min(src_len - 1);
        }
        return start_cap + (dest - start_cap) * src_middle / dest_middle;
    }

    if dest < start_cap {
        return dest;
    }
    if dest >= dest_len - end_cap {
        return src_len - (dest_len - dest);
    }

    let src_middle = src_len - start_cap - end_cap;
    let dest_middle = dest_len - start_cap - end_cap;
    start_cap + (dest - start_cap) * src_middle / dest_middle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_assistant_message_content() {
        let json = r#"{"content":"wrong","choices":[{"message":{"role":"assistant","content":"hello\nthere"}}]}"#;

        assert_eq!(extract_content(json).as_deref(), Ok("hello there"));
    }

    #[test]
    fn chat_body_preserves_unicode_and_escapes_json() {
        let body = chat_body("model", "system", &[], "hi \"there\" 🩷");
        let parsed: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(parsed["messages"][1]["content"], "hi \"there\" 🩷");
    }
}
