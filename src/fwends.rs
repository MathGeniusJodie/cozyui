use std::env;
use std::error::Error;
use std::fs;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::bitmap_font::BitmapFont;
use crate::comicoro_font;
use crate::palette_color;
use crate::text_input::{EditKey, edit_key};
use crate::{Framebuffer, Image, Palette, Rgba};

const SCALE: usize = 1;
const GLYPH_SCALE: usize = 1;
const W: usize = 190;
const H: usize = 236;
const PAD: usize = 8;
const MODEL_Y: usize = 8;
const MODEL_SLOT_W: usize = 42;
const MODEL_SLOT_H: usize = 34;
const CHAT_Y: usize = 48;
const CHAT_H: usize = 142;
const INPUT_Y: usize = 196;
const INPUT_H: usize = 31;
const TEXT_PAD: usize = 7;
const BUBBLE_PAD_X: usize = 14;
const BUBBLE_PAD_TOP: usize = 8;
const BUBBLE_PAD_BOTTOM: usize = 11;
const BUBBLE_GAP: usize = 5;
const BUBBLE_MIN_W: usize = 38;
const BUBBLE_MAX_W: usize = 142;
const BUBBLE_LEFT_CAP: usize = 21;
const BUBBLE_RIGHT_CAP: usize = 21;
const BUBBLE_TOP_CAP: usize = 17;
const BUBBLE_BOTTOM_CAP: usize = 17;
const SCROLL_STEP: usize = 24;
const LINE_H: usize = 16;
const MAX_INPUT_CHARS: usize = 96;
const SYSTEM_PROMPT_PATH: &str = "fwends_system_prompt.txt";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const FOCUS_PENCIL_PATH: &str = "assets/focus_pencil.png";
const PENCIL_TIP_X: usize = 0;
const PENCIL_TIP_Y: usize = 24;

const MODELS: [Model; 4] = [
    Model {
        name: "Claude",
        id: "anthropic/claude-haiku-4.5",
        asset_path: "assets/claw.png",
    },
    Model {
        name: "DeepSeek",
        id: "deepseek/deepseek-v4-flash",
        asset_path: "assets/deep.png",
    },
    Model {
        name: "Qwen",
        id: "qwen/qwen3.6-35b-a3b",
        asset_path: "assets/qwen.png",
    },
    Model {
        name: "Kimi",
        id: "moonshotai/kimi-k2.6",
        asset_path: "assets/kimi.png",
    },
];

pub(crate) struct Fwends {
    avatars: [Image; 4],
    bubble: Image,
    pencil: Image,
    font: BitmapFont,
    messages: Vec<Message>,
    input: String,
    selected_model: usize,
    focused: bool,
    scroll_y: usize,
    pending: Option<Receiver<Result<String, String>>>,
    system_prompt: String,
}

impl Fwends {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            avatars: [
                Image::load(MODELS[0].asset_path, palette)?,
                Image::load(MODELS[1].asset_path, palette)?,
                Image::load(MODELS[2].asset_path, palette)?,
                Image::load(MODELS[3].asset_path, palette)?,
            ],
            bubble: Image::load("assets/bubble.png", palette)?,
            pencil: Image::load(FOCUS_PENCIL_PATH, palette)?,
            font: BitmapFont::load(&comicoro_font::COMICORO_SPEC)?,
            messages: vec![Message::assistant("pick a fwend and say hi".to_string())],
            input: String::new(),
            selected_model: 0,
            focused: false,
            scroll_y: 0,
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
        palette.color(palette_color::BLACK)
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));
        fill_scaled_rect(fb, 0, 0, W, H, palette.color(palette_color::PINE));
        fill_scaled_rect(
            fb,
            2,
            2,
            W - 4,
            H - 4,
            palette.color(palette_color::LAVENDER),
        );
        fill_scaled_rect(fb, 4, 4, W - 8, H - 8, palette.color(palette_color::CREAM));
        fill_scaled_rect(
            fb,
            6,
            CHAT_Y - 5,
            W - 12,
            CHAT_H + 8,
            palette.color(palette_color::PEACH),
        );

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
            let slot_x = PAD + index * MODEL_SLOT_W;
            if x >= slot_x
                && x < slot_x + MODEL_SLOT_W
                && y >= MODEL_Y
                && y < MODEL_Y + MODEL_SLOT_H
            {
                self.selected_model = index;
                return;
            }
        }

        if x >= PAD && x < W - PAD && y >= INPUT_Y && y < INPUT_Y + INPUT_H {
            self.focused = true;
        }
    }

    pub(crate) fn handle_key_press(
        &mut self,
        keycode: u8,
        state: u16,
    ) -> Result<(), Box<dyn Error>> {
        match edit_key(keycode, state) {
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
            && message.text == "..."
            && !message.from_user
        {
            message.text = text;
            self.scroll_to_bottom();
            return true;
        }
        self.messages.push(Message::assistant(text));
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
        self.messages.push(Message::assistant("...".to_string()));
        self.scroll_to_bottom();

        let model = MODELS[self.selected_model].id.to_string();
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
        let x = PAD + index * MODEL_SLOT_W;
        let selected = index == self.selected_model;
        let fill = if selected {
            palette.color(palette_color::CYAN)
        } else {
            palette.color(palette_color::CREAM)
        };
        fill_scaled_rect(
            fb,
            x,
            MODEL_Y,
            MODEL_SLOT_W - 4,
            MODEL_SLOT_H,
            palette.color(palette_color::PINE),
        );
        fill_scaled_rect(
            fb,
            x + 1,
            MODEL_Y + 1,
            MODEL_SLOT_W - 6,
            MODEL_SLOT_H - 2,
            fill,
        );

        let avatar_x = x + (MODEL_SLOT_W - 4).saturating_sub(avatar.width) / 2;
        let avatar_y = MODEL_Y + 3;
        fb.draw_scaled_region(
            avatar,
            0,
            0,
            avatar_x * SCALE,
            avatar_y * SCALE,
            avatar.width,
            avatar.height,
            SCALE,
        );

        self.font.draw_text(
            fb,
            MODELS[index].name,
            (x + 4) * SCALE,
            (MODEL_Y + MODEL_SLOT_H - 9) * SCALE,
            GLYPH_SCALE,
            palette.color(palette_color::BLACK),
        );
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

            self.draw_bubble(fb, layout.x, y, layout.w, layout.h, layout.from_user);
            let text_x = layout.x + BUBBLE_PAD_X;
            let mut text_y = y + BUBBLE_PAD_TOP as isize;
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
        from_user: bool,
    ) {
        let clip_top = CHAT_Y;
        let clip_bottom = CHAT_Y + CHAT_H;

        for dy in 0..h {
            let py = y + dy as isize;
            if py < clip_top as isize || py >= clip_bottom as isize {
                continue;
            }
            for dx in 0..w {
                let px = x + dx;
                let mut sx = stretch_source_coord(
                    dx,
                    w,
                    self.bubble.width,
                    BUBBLE_LEFT_CAP,
                    BUBBLE_RIGHT_CAP,
                );
                if from_user {
                    sx = self.bubble.width - 1 - sx;
                }
                let sy = stretch_source_coord(
                    dy,
                    h,
                    self.bubble.height,
                    BUBBLE_TOP_CAP,
                    BUBBLE_BOTTOM_CAP,
                );
                let color = self.bubble.at(sx, sy);
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
            let max_text_w = BUBBLE_MAX_W - BUBBLE_PAD_X * 2;
            let lines = self.font.wrap_lines(&message.text, max_text_w);
            let text_w = lines
                .iter()
                .map(|line| self.font.text_width(line))
                .max()
                .unwrap_or(0);
            let w = (text_w + BUBBLE_PAD_X * 2).clamp(BUBBLE_MIN_W, BUBBLE_MAX_W);
            let h = (lines.len() * LINE_H + BUBBLE_PAD_TOP + BUBBLE_PAD_BOTTOM)
                .max(BUBBLE_TOP_CAP + BUBBLE_BOTTOM_CAP + 1);
            let x = if message.from_user { W - PAD - w } else { PAD };
            layouts.push(MessageLayout {
                from_user: message.from_user,
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
        let border = if self.focused {
            palette.color(palette_color::BLUE)
        } else {
            palette.color(palette_color::PINE)
        };
        fill_scaled_rect(fb, PAD, INPUT_Y, W - PAD * 2, INPUT_H, border);
        fill_scaled_rect(
            fb,
            PAD + 2,
            INPUT_Y + 2,
            W - PAD * 2 - 4,
            INPUT_H - 4,
            palette.color(palette_color::CREAM),
        );

        let label = if self.pending.is_some() {
            "wait a sec..."
        } else if self.input.is_empty() {
            "type here"
        } else {
            &self.input
        };
        let lines = self.font.wrap_lines(label, W - PAD * 2 - TEXT_PAD * 2);
        for (index, line) in lines.into_iter().take(2).enumerate() {
            self.font.draw_text(
                fb,
                &line,
                (PAD + TEXT_PAD) * SCALE,
                (INPUT_Y + 7 + index * LINE_H) * SCALE,
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
        fb.draw_scaled_region(
            &self.pencil,
            0,
            0,
            dest_x * SCALE,
            dest_y * SCALE,
            self.pencil.width,
            self.pencil.height,
            SCALE,
        );
    }

    fn input_cursor_position(&self) -> (usize, usize) {
        let max_width = W - PAD * 2 - TEXT_PAD * 2;
        let lines = self.font.wrap_lines(&self.input, max_width);
        let line_index = lines.len().saturating_sub(1).min(1);
        (
            PAD + TEXT_PAD + self.font.text_width(&lines[line_index]).min(max_width),
            INPUT_Y + 7 + line_index * LINE_H,
        )
    }
}

#[derive(Clone, Copy)]
struct Model {
    name: &'static str,
    id: &'static str,
    asset_path: &'static str,
}

#[derive(Clone)]
struct Message {
    role: Role,
    text: String,
    from_user: bool,
}

struct MessageLayout {
    from_user: bool,
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
            from_user: true,
        }
    }

    fn assistant(text: String) -> Self {
        Self {
            role: Role::Assistant,
            text,
            from_user: false,
        }
    }
}

#[derive(Clone, Copy)]
enum Role {
    User,
    Assistant,
}

fn request_history(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|message| message.text != "..." && message.text != "pick a fwend and say hi")
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
    let output = Command::new("curl")
        .args([
            "-sS",
            OPENROUTER_URL,
            "-H",
            &format!("Authorization: Bearer {api_key}"),
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
        ])
        .output()
        .map_err(|err| format!("curl failed: {err}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let response = String::from_utf8_lossy(&output.stdout);
    extract_content(&response).ok_or_else(|| compact_error(&response))
}

fn chat_body(model: &str, system_prompt: &str, history: &[Message], latest_text: &str) -> String {
    let mut messages = vec![format!(
        "{{\"role\":\"system\",\"content\":\"{}\"}}",
        json_escape(system_prompt.trim())
    )];
    for message in history {
        messages.push(format!(
            "{{\"role\":\"{}\",\"content\":\"{}\"}}",
            match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            json_escape(&message.text)
        ));
    }
    if history.last().map(|message| message.text.as_str()) != Some(latest_text) {
        messages.push(format!(
            "{{\"role\":\"user\",\"content\":\"{}\"}}",
            json_escape(latest_text)
        ));
    }

    format!(
        "{{\"model\":\"{}\",\"messages\":[{}],\"reasoning\":{{\"exclude\":true}},\"include_reasoning\":false}}",
        json_escape(model),
        messages.join(",")
    )
}

fn json_escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push(' '),
            ch if ch.is_ascii() => out.push(ch),
            _ => out.push('?'),
        }
    }
    out
}

fn extract_content(json: &str) -> Option<String> {
    let key = "\"content\"";
    let mut offset = 0;
    while let Some(found) = json[offset..].find(key) {
        let start = offset + found + key.len();
        let after_colon = json[start..].find(':')? + start + 1;
        let value_start = json[after_colon..].find('"')? + after_colon + 1;
        if let Some((value, end)) = parse_json_string(&json[value_start..]) {
            if !value.trim().is_empty() {
                return Some(value);
            }
            offset = value_start + end;
        } else {
            offset = value_start;
        }
    }
    None
}

fn parse_json_string(text: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut escaped = false;
    let mut chars = text.char_indices();
    while let Some((index, ch)) = chars.next() {
        if escaped {
            match ch {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    for _ in 0..4 {
                        let _ = chars.next();
                    }
                    out.push('?');
                }
                other => out.push(other),
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some((out, index + 1));
        } else {
            out.push(ch);
        }
    }
    None
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
    x >= PAD && x < W - PAD && y >= CHAT_Y && y < CHAT_Y + CHAT_H
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
