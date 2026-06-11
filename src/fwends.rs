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
use crate::text_edit::{TextEdit, TextEditOutcome, char_len};
use crate::text_input::{EditKey, KeyInput, edit_key};
use crate::{Framebuffer, Index, Palette, Rect, Rgb, Rgba, Sprite, Swap, TRANSPARENT};
use serde_json::{Value, json};

const SCALE: usize = 1;
const GLYPH_SCALE: usize = 1;
const CONTENT_W: usize = 348;
const W: usize = CONTENT_W + ERASER_W - ERASER_CONTENT_OVERLAP;
const H: usize = 318;
const ERASER_W: usize = 45;
const ERASER_CONTENT_OVERLAP: usize = 30;
const CONTENT_X_OFFSET: usize = 80;
const PAD: usize = 8;
const CHAT_Y: usize = 8;
const CHAT_INPUT_GAP: usize = 6;
const INPUT_X: usize = 24;
const INPUT_BOTTOM_PAD: usize = 122;
const INPUT_TEXT_Y: usize = 24;
const TEXT_PAD: usize = 7;
const INPUT_BOX_Y_OFFSET: isize = -13;
const INPUT_BOX_RIGHT_PAD: usize = 11;
const INPUT_EXTRA_W: usize = 40;
const INPUT_EXTRA_H: usize = 4;
const SELECTED_FWEND_GAP: usize = 4;
const BUBBLE_PAD_X: usize = 14;
const BUBBLE_PAD_TOP: usize = 8;
const BUBBLE_PAD_BOTTOM: usize = 11;
const STICKY_PAD_LEFT: usize = 10;
const STICKY_PAD_RIGHT: usize = 20;
const STICKY_PAD_TOP: usize = 24;
const STICKY_PAD_BOTTOM: usize = 20;
const BUBBLE_GAP: usize = 5;
const BUBBLE_MIN_W: usize = 38;
const BUBBLE_MAX_W: usize = 142;
const STICKY_MIN_W: usize = 141;
const STICKY_MIN_H: usize = 0;
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
const OPENROUTER_FALLBACK_MODEL: &str = "@preset/free";
const USER_NAME: &str = "Jodie";
const REQUEST_TIMEOUT_SECS: &str = "30";
const FOCUS_PENCIL_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/focus_pencil.png");
const PENCIL_SHADOW_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/toodle_pencil_shadow.png"
);
const LAMP_ON_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/lamp_on.png");
const LAMP_OFF_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/lamp_off.png");
const ERASER_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/eraser.png");
const USER_STICKY_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sticky.png");
const SMOL_FWENDS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/smol_fwends.png");
const SMOL_ICON_SIZE: usize = 11;
const SMOL_ICON_GAP: usize = 2;
const SMOL_ICON_Y_OFFSET: usize = 3;
const INPUT_STICKY_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sticky_stack.png");
const PENCIL_TIP_X: usize = 0;
const PENCIL_TIP_Y: usize = 24;
const LAMP_RIGHT_PAD: usize = 140;
const LAMP_Y_OFFSET: usize = 70;
const ERASER_RIGHT_PAD: usize = 0;

const MODELS: [Model; 4] = [
    Model {
        id: "anthropic/claude-haiku-4.5",
        thinking_id: "anthropic/claude-opus-4.5",
        name: "Claude",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/claw.png"),
    },
    Model {
        id: "deepseek/deepseek-v4-flash",
        thinking_id: "deepseek/deepseek-v4-pro",
        name: "Deepseek",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/deep.png"),
    },
    Model {
        id: "qwen/qwen3.6-35b-a3b",
        thinking_id: "qwen/qwen3.7-plus",
        name: "Qwen",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/qwen.png"),
    },
    Model {
        id: "moonshotai/kimi-k2.6",
        thinking_id: "moonshotai/kimi-k2.6",
        name: "Kimi",
        asset_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kimi.png"),
    },
];

pub(crate) struct Fwends {
    avatars: [Sprite; 4],
    bubble: Sprite,
    user_sticky: Sprite,
    input_sticky: Sprite,
    smol_icons: Sprite,
    pencil: Sprite,
    pencil_shadow: Sprite,
    lamp_on_image: Sprite,
    lamp_off_image: Sprite,
    eraser: Sprite,
    font: BitmapFont,
    messages: Vec<Message>,
    input: String,
    input_edit: TextEdit,
    selected_model: usize,
    lamp_on: bool,
    focused: bool,
    height: usize,
    scroll_y: usize,
    model_slot_w: usize,
    model_slot_h: usize,
    pending: Option<Receiver<Result<String, String>>>,
    system_prompt: String,
}

impl Fwends {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let avatars = [
            Sprite::load_native(MODELS[0].asset_path, palette)?,
            Sprite::load_native(MODELS[1].asset_path, palette)?,
            Sprite::load_native(MODELS[2].asset_path, palette)?,
            Sprite::load_native(MODELS[3].asset_path, palette)?,
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
            bubble: Sprite::load_native(
                concat!(env!("CARGO_MANIFEST_DIR"), "/assets/bubble.png"),
                palette,
            )?,
            user_sticky: Sprite::load_native(USER_STICKY_PATH, palette)?,
            input_sticky: Sprite::load_native(INPUT_STICKY_PATH, palette)?,
            smol_icons: Sprite::load_native(SMOL_FWENDS_PATH, palette)?,
            pencil: Sprite::load_native(FOCUS_PENCIL_PATH, palette)?,
            pencil_shadow: Sprite::load_native(PENCIL_SHADOW_PATH, palette)?,
            lamp_on_image: Sprite::load_native(LAMP_ON_PATH, palette)?,
            lamp_off_image: Sprite::load_native(LAMP_OFF_PATH, palette)?,
            eraser: Sprite::load_native(ERASER_PATH, palette)?,
            font: BitmapFont::load(&comicoro_font::COMICORO_SPEC)?,
            messages: vec![intro_message()],
            input: String::new(),
            input_edit: TextEdit::default(),
            selected_model,
            lamp_on: false,
            focused: false,
            height: H * SCALE,
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
        self.height
    }

    pub(crate) fn min_height(&self) -> usize {
        H * SCALE
    }

    pub(crate) fn set_height(&mut self, height: usize) -> bool {
        let height = height.max(self.min_height());
        if self.height == height {
            return false;
        }

        self.height = height;
        self.scroll_to_bottom();
        true
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK).transparent()
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));

        self.draw_lamp(fb, palette);
        self.draw_messages(fb, palette);
        self.draw_input(fb, palette);
        self.draw_selected_fwend(fb, palette);
        self.draw_eraser(fb, palette);
    }

    pub(crate) fn click(&mut self, x: i16, y: i16) {
        self.focused = false;
        self.input_edit.end_drag();
        if x < 0 || y < 0 {
            return;
        }

        let x = x as usize / SCALE;
        let y = y as usize / SCALE;
        if self.eraser_contains(x, y) {
            self.erase_chat_history();
            return;
        }

        if self.lamp_contains(x, y) {
            self.lamp_on = !self.lamp_on;
            return;
        }

        let fwend_rect = self.selected_fwend_rect();
        if fwend_rect.contains_point(x, y) {
            self.selected_model = (self.selected_model + 1) % MODELS.len();
            return;
        }

        let input_x = self.input_sticky_x();
        if x >= input_x
            && x < input_x + self.input_sticky_w()
            && y >= self.input_y()
            && y < self.input_y() + self.input_sticky_h()
        {
            self.focused = true;
            let cursor = self.input_index_at(x, y);
            self.input_edit.begin_drag(cursor, &self.input);
        }
    }

    pub(crate) fn drag_text(&mut self, x: i16, y: i16) -> bool {
        if !self.input_edit.is_dragging() {
            return false;
        }
        let x = x.max(0) as usize / SCALE;
        let y = y.max(0) as usize / SCALE;
        let cursor = self.input_index_at(x, y);
        self.input_edit.drag_to(cursor, &self.input)
    }

    pub(crate) fn end_text_drag(&mut self) {
        self.input_edit.end_drag();
    }

    pub(crate) fn text_dragging(&self) -> bool {
        self.input_edit.is_dragging()
    }

    pub(crate) fn handle_key_press(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        if self.focused {
            let outcome =
                self.input_edit
                    .handle_key(input, &mut self.input, clipboard_text, |candidate| {
                        char_len(candidate) <= MAX_INPUT_CHARS
                    });
            if let TextEditOutcome::Handled { changed: _, copy } = outcome {
                return Ok(copy);
            }
        }

        match edit_key(input) {
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
        Ok(None)
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
            message.text = match message.author {
                Some(name) => strip_self_prefix(&text, name),
                None => text,
            };
            message.kind = MessageKind::Normal;
            self.scroll_to_bottom();
            return true;
        }
        self.messages.push(Message::assistant(text));
        self.scroll_to_bottom();
        true
    }

    pub(crate) fn scroll_up(&mut self, x: i16, y: i16) {
        if !self.chat_contains(x, y) {
            return;
        }
        self.scroll_y = self.scroll_y.saturating_sub(SCROLL_STEP);
    }

    pub(crate) fn scroll_down(&mut self, x: i16, y: i16) {
        if !self.chat_contains(x, y) {
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
        self.input_edit.set_cursor(0, &self.input);
        self.messages
            .retain(|message| message.kind != MessageKind::Intro);
        self.messages.push(Message::user(text.clone()));
        let selected_model = self.selected_model;
        let thinking = self.lamp_on;
        let model = MODELS[selected_model.min(MODELS.len() - 1)];
        self.messages.push(Message::pending(model.name));
        self.scroll_to_bottom();

        let model_id = model.id(thinking).to_string();
        let system_prompt = fwend_system_prompt(&self.system_prompt, model.name, thinking);
        let history = request_history(&self.messages, model.name);
        let text = format!("{USER_NAME}: {text}");
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);

        thread::spawn(move || {
            let result =
                send_openrouter_request(&model_id, &system_prompt, &history, &text, thinking);
            let _ = tx.send(result);
        });
    }

    fn erase_chat_history(&mut self) {
        self.pending = None;
        self.messages = vec![intro_message()];
        self.scroll_to_bottom();
    }

    fn draw_messages(&self, fb: &mut Framebuffer, palette: &Palette) {
        let layouts = self.message_layouts();
        let viewport_top = CHAT_Y;
        let viewport_bottom = CHAT_Y + self.chat_h();
        let content_height = layouts
            .last()
            .map(|layout| layout.y + layout.h)
            .unwrap_or(0);
        let bottom_offset = self.chat_h().saturating_sub(content_height);

        for layout in layouts {
            let y = CHAT_Y as isize + bottom_offset as isize + layout.y as isize
                - self.scroll_y as isize;
            if y >= viewport_bottom as isize || y + layout.h as isize <= viewport_top as isize {
                continue;
            }

            self.draw_bubble(fb, palette, layout.x, y, layout.w, layout.h, layout.style);
            if let Some(src) = layout.author.and_then(smol_icon_rect) {
                let clip = Rect::new(0, viewport_top * SCALE, self.width(), self.chat_h() * SCALE);
                let icon_x = (layout.x + layout.w + SMOL_ICON_GAP) * SCALE;
                let icon_y =
                    (y + layout.h as isize - (SMOL_ICON_SIZE + SMOL_ICON_Y_OFFSET) as isize)
                        * SCALE as isize;
                fb.draw_sprite_full(
                    &self.smol_icons,
                    src,
                    icon_x as isize,
                    icon_y,
                    SCALE,
                    Some(clip),
                    palette,
                    None,
                );
            }
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

    #[allow(clippy::too_many_arguments)]
    fn draw_bubble(
        &self,
        fb: &mut Framebuffer,
        palette: &Palette,
        x: usize,
        y: isize,
        w: usize,
        h: usize,
        style: MessageStyle,
    ) {
        let clip_top = CHAT_Y;
        let clip_bottom = CHAT_Y + self.chat_h();
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
                let Some(color) = palette.resolve(image.at(sx, sy), px, py as usize) else {
                    continue;
                };
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
            let w = (text_w + style.pad_left + style.pad_right).clamp(style.min_w, BUBBLE_MAX_W);
            let h = (lines.len() * LINE_H + style.pad_top + style.pad_bottom)
                .max(style.top_cap + style.bottom_cap + 1)
                .max(style.min_h);
            let x = if style.align_right {
                CONTENT_W - PAD - w
            } else {
                self.assistant_bubble_x()
            };
            layouts.push(MessageLayout {
                style,
                lines,
                author: message.author,
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
        self.input_sticky_x()
    }

    fn content_height(&self) -> usize {
        self.message_layouts()
            .last()
            .map(|layout| layout.y + layout.h)
            .unwrap_or(0)
    }

    fn max_scroll(&self) -> usize {
        self.content_height().saturating_sub(self.chat_h())
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_y = self.max_scroll();
    }

    fn draw_lamp(&self, fb: &mut Framebuffer, palette: &Palette) {
        let image = self.lamp_image();
        let (x, y) = self.lamp_position();
        fb.draw_sprite(image, x as isize, y as isize, SCALE, palette);
    }

    fn draw_eraser(&self, fb: &mut Framebuffer, palette: &Palette) {
        let (x, y) = self.eraser_position();
        fb.draw_sprite(&self.eraser, x as isize, y as isize, SCALE, palette);
    }

    fn eraser_contains(&self, x: usize, y: usize) -> bool {
        let (eraser_x, eraser_y) = self.eraser_position();
        if x < eraser_x
            || y < eraser_y
            || x >= eraser_x + self.eraser.width
            || y >= eraser_y + self.eraser.height
        {
            return false;
        }

        self.eraser.is_opaque(x - eraser_x, y - eraser_y)
    }

    fn eraser_position(&self) -> (usize, usize) {
        (
            W.saturating_sub(self.eraser.width + ERASER_RIGHT_PAD),
            (self.height / SCALE).saturating_sub(self.eraser.height),
        )
    }

    fn lamp_contains(&self, x: usize, y: usize) -> bool {
        let image = self.lamp_image();
        let (lamp_x, lamp_y) = self.lamp_position();
        if x < lamp_x
            || y < lamp_y
            || x >= lamp_x + image.width
            || y >= lamp_y + image.height
            || y >= self.input_y()
        {
            return false;
        }

        image.is_opaque(x - lamp_x, y - lamp_y)
    }

    fn lamp_image(&self) -> &Sprite {
        if self.lamp_on {
            &self.lamp_on_image
        } else {
            &self.lamp_off_image
        }
    }

    fn lamp_position(&self) -> (usize, usize) {
        let image = self.lamp_image();
        (
            CONTENT_W.saturating_sub(image.width + LAMP_RIGHT_PAD),
            (self.height / SCALE).saturating_sub(image.height + LAMP_Y_OFFSET),
        )
    }

    fn draw_input(&self, fb: &mut Framebuffer, palette: &Palette) {
        draw_resized_image(
            fb,
            palette,
            &self.input_sticky,
            self.input_sticky_x(),
            self.input_y(),
            self.input_sticky_w(),
            self.input_sticky_h(),
            STICKY_LEFT_CAP,
            STICKY_RIGHT_CAP,
            STICKY_TOP_CAP,
            STICKY_BOTTOM_CAP,
        );

        let label = if self.pending.is_some() {
            "wait a sec..."
        } else {
            &self.input
        };
        let text_y = (self.input_y() + INPUT_TEXT_Y).saturating_add_signed(INPUT_BOX_Y_OFFSET);
        let text_x = self.input_text_x();
        let max_width = self.input_text_width();
        let lines = self.font.wrap_lines(label, max_width);
        if self.pending.is_none() {
            draw_selection(
                fb,
                &self.font,
                &self.input,
                self.input_edit.selection_range(),
                text_x * SCALE,
                text_y * SCALE,
                max_width,
                LINE_H * SCALE,
                GLYPH_SCALE,
                palette.color(palette_color::LAVENDER),
            );
        }
        for (index, line) in lines.into_iter().take(5).enumerate() {
            self.font.draw_text(
                fb,
                &line,
                text_x * SCALE,
                (text_y + index * LINE_H) * SCALE,
                GLYPH_SCALE,
                palette.color(palette_color::BLACK),
            );
        }
        self.draw_focused_pencil(fb, palette);
    }

    fn draw_selected_fwend(&self, fb: &mut Framebuffer, palette: &Palette) {
        let avatar = &self.avatars[self.selected_model.min(self.avatars.len() - 1)];
        let rect = self.selected_fwend_rect();
        let avatar_x = rect.x + rect.w.saturating_sub(avatar.width) / 2;
        let avatar_y = rect.y + rect.h.saturating_sub(avatar.height);
        let (lamp_x, lamp_y) = self.lamp_position();
        draw_lamp_masked_ellipse(
            fb,
            (avatar_x + avatar.width / 2) as isize,
            (avatar_y + avatar.height / 2) as isize,
            avatar.width + 10,
            avatar.height.max(1) + 8,
            palette,
            self.lamp_image(),
            lamp_x,
            lamp_y,
        );
        fb.draw_sprite(
            avatar,
            (avatar_x * SCALE) as isize,
            (avatar_y * SCALE) as isize,
            SCALE,
            palette,
        );
    }

    fn selected_fwend_rect(&self) -> FwendRect {
        FwendRect {
            x: CONTENT_X_OFFSET + INPUT_X,
            y: self.input_y(),
            w: self.model_slot_w,
            h: self.model_slot_h,
        }
    }

    fn draw_focused_pencil(&self, fb: &mut Framebuffer, palette: &Palette) {
        if !self.focused {
            return;
        }

        let (cursor_x, cursor_y) = self.input_cursor_position();
        let dest_x = cursor_x.saturating_sub(PENCIL_TIP_X);
        let dest_y = cursor_y.saturating_sub(PENCIL_TIP_Y);
        draw_yellow_pencil_shadow(
            fb,
            &self.pencil_shadow,
            (dest_x * SCALE) as isize,
            (dest_y * SCALE) as isize,
            palette,
        );
        fb.draw_sprite(
            &self.pencil,
            (dest_x * SCALE) as isize,
            (dest_y * SCALE) as isize,
            SCALE,
            palette,
        );
    }

    fn input_cursor_position(&self) -> (usize, usize) {
        let text_x = self.input_text_x();
        let max_width = self.input_text_width();
        let lines = self.font.wrap_lines(&self.input, max_width);
        let (line_index, line_start, line) =
            line_for_char_index(&lines, self.input_edit.cursor()).unwrap_or((0, 0, ""));
        let text_y = (self.input_y() + INPUT_TEXT_Y).saturating_add_signed(INPUT_BOX_Y_OFFSET);
        (
            text_x
                + self
                    .font
                    .text_width(prefix_chars(line, self.input_edit.cursor() - line_start))
                    .min(max_width),
            text_y + line_index * LINE_H,
        )
    }

    fn input_index_at(&self, x: usize, y: usize) -> usize {
        let text_x = self.input_text_x();
        let max_width = self.input_text_width();
        let text_y = (self.input_y() + INPUT_TEXT_Y).saturating_add_signed(INPUT_BOX_Y_OFFSET);
        let line_index = y.saturating_sub(text_y) / LINE_H;
        let lines = self.font.wrap_lines(&self.input, max_width);
        text_index_at(&self.font, &lines, line_index, x.saturating_sub(text_x))
    }

    fn input_text_x(&self) -> usize {
        self.input_sticky_x() + TEXT_PAD
    }

    fn input_text_width(&self) -> usize {
        let right_edge =
            self.input_sticky_x() + self.input_sticky_w() - TEXT_PAD - INPUT_BOX_RIGHT_PAD;
        right_edge.saturating_sub(self.input_text_x())
    }

    fn input_sticky_x(&self) -> usize {
        CONTENT_X_OFFSET + INPUT_X + self.model_slot_w + SELECTED_FWEND_GAP
    }

    fn input_sticky_w(&self) -> usize {
        self.input_sticky.width + INPUT_EXTRA_W
    }

    fn input_sticky_h(&self) -> usize {
        self.input_sticky.height + INPUT_EXTRA_H
    }

    fn input_y(&self) -> usize {
        (self.height / SCALE).saturating_sub(INPUT_BOTTOM_PAD + INPUT_EXTRA_H)
    }

    fn chat_h(&self) -> usize {
        self.input_y().saturating_sub(CHAT_Y + CHAT_INPUT_GAP)
    }

    fn chat_contains(&self, x: i16, y: i16) -> bool {
        if x < 0 || y < 0 {
            return false;
        }
        let x = x as usize / SCALE;
        let y = y as usize / SCALE;
        (PAD..CONTENT_W - PAD).contains(&x) && (CHAT_Y..CHAT_Y + self.chat_h()).contains(&y)
    }
}

#[derive(Clone, Copy)]
struct FwendRect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl FwendRect {
    fn contains_point(self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

fn draw_selection(
    fb: &mut Framebuffer,
    font: &BitmapFont,
    text: &str,
    selection: Option<(usize, usize)>,
    x: usize,
    y: usize,
    max_width: usize,
    line_h: usize,
    scale: usize,
    color: Rgb,
) {
    let Some((selection_start, selection_end)) = selection else {
        return;
    };
    let lines = font.wrap_lines(text, max_width);
    let mut line_start = 0;
    for (line_index, line) in lines.iter().enumerate().take(5) {
        let line_len = line.chars().count();
        let line_end = line_start + line_len;
        let start = selection_start.max(line_start);
        let end = selection_end.min(line_end);
        if start < end {
            let prefix = prefix_chars(line, start - line_start);
            let selected = prefix_chars(line, end - line_start);
            let sel_x = x + font.text_width(prefix) * scale;
            let sel_w = font
                .text_width(selected)
                .saturating_sub(font.text_width(prefix))
                * scale;
            fb.fill_rect(
                sel_x,
                y + line_index * line_h,
                sel_w.max(1),
                font.cell_h() * scale,
                color,
            );
        }
        line_start = line_end;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_lamp_masked_ellipse(
    fb: &mut Framebuffer,
    center_x: isize,
    center_y: isize,
    diameter_w: usize,
    diameter_h: usize,
    palette: &Palette,
    lamp: &Sprite,
    lamp_x: usize,
    lamp_y: usize,
) {
    let radius_x = diameter_w.div_ceil(2).max(1) as isize;
    let radius_y = diameter_h.div_ceil(2).max(1) as isize;

    let rx2 = radius_x * radius_x;
    let ry2 = radius_y * radius_y;
    let threshold = rx2 * ry2;
    for y in center_y - radius_y..=center_y + radius_y {
        for x in center_x - radius_x..=center_x + radius_x {
            let dx = x - center_x;
            let dy = y - center_y;
            if dx * dx * ry2 + dy * dy * rx2 > threshold {
                continue;
            }
            if x < 0 || y < 0 {
                continue;
            }
            let x = x as usize;
            let y = y as usize;
            if !lamp_masks_pixel(lamp, lamp_x, lamp_y, x, y) {
                continue;
            }
            let Some(color) = lamp_shadow_color(lamp, lamp_x, lamp_y, x, y, palette) else {
                continue;
            };
            fb.set_pixel(x, y, color);
        }
    }
}

fn lamp_masks_pixel(lamp: &Sprite, lamp_x: usize, lamp_y: usize, x: usize, y: usize) -> bool {
    let Some(local_x) = x.checked_sub(lamp_x) else {
        return false;
    };
    let Some(local_y) = y.checked_sub(lamp_y) else {
        return false;
    };
    local_x < lamp.width && local_y < lamp.height && lamp.is_opaque(local_x, local_y)
}

fn lamp_shadow_color(
    lamp: &Sprite,
    lamp_x: usize,
    lamp_y: usize,
    x: usize,
    y: usize,
    palette: &Palette,
) -> Option<Rgb> {
    let local_x = x.checked_sub(lamp_x)?;
    let local_y = y.checked_sub(lamp_y)?;
    if local_x >= lamp.width || local_y >= lamp.height {
        return None;
    }

    let mapped = match lamp.at(local_x, local_y) {
        TRANSPARENT => return None,
        palette_color::ROSE => palette_color::CRIMSON,
        palette_color::PEACH => palette_color::CRIMSON,
        palette_color::PLUM => palette_color::BLACK,
        palette_color::CRIMSON => palette_color::PLUM,
        other => other,
    };
    palette.resolve(mapped, x, y)
}

#[allow(clippy::too_many_arguments)]
fn draw_resized_image(
    fb: &mut Framebuffer,
    palette: &Palette,
    image: &Sprite,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    left_cap: usize,
    right_cap: usize,
    top_cap: usize,
    bottom_cap: usize,
) {
    for dy in 0..h {
        let sy = stretch_source_coord(dy, h, image.height, top_cap, bottom_cap);
        for dx in 0..w {
            let sx = stretch_source_coord(dx, w, image.width, left_cap, right_cap);
            let Some(color) = palette.resolve(image.at(sx, sy), x + dx, y + dy) else {
                continue;
            };
            fb.fill_rect((x + dx) * SCALE, (y + dy) * SCALE, SCALE, SCALE, color);
        }
    }
}

fn draw_yellow_pencil_shadow(
    fb: &mut Framebuffer,
    image: &Sprite,
    dest_x: isize,
    dest_y: isize,
    palette: &Palette,
) {
    fb.draw_sprite_swapped(
        image,
        dest_x,
        dest_y,
        SCALE,
        palette,
        &Swap::from_indices(&YELLOW_PAGE_REMAP),
    );
}

const PALETTE_COLOR_COUNT: usize = 16;

const IDENTITY_PAGE_REMAP: [Index; PALETTE_COLOR_COUNT] = [
    palette_color::LAVENDER,
    palette_color::GUNMETAL,
    palette_color::PLUM,
    palette_color::BROWN,
    palette_color::PEACH,
    palette_color::CREAM,
    palette_color::LIME,
    palette_color::GREEN,
    palette_color::ORANGE,
    palette_color::CRIMSON,
    palette_color::ROSE,
    palette_color::PURPLE,
    palette_color::CYAN,
    palette_color::BLUE,
    palette_color::PINE,
    palette_color::BLACK,
];

const YELLOW_PAGE_REMAP: [Index; PALETTE_COLOR_COUNT] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::LIME as usize] = palette_color::PEACH;
    remap[palette_color::ROSE as usize] = palette_color::PEACH;
    remap
};

fn line_for_char_index(lines: &[String], index: usize) -> Option<(usize, usize, &str)> {
    let mut line_start = 0;
    for (line_index, line) in lines.iter().enumerate() {
        let line_len = line.chars().count();
        let line_end = line_start + line_len;
        if index <= line_end {
            return Some((line_index, line_start, line));
        }
        line_start = line_end;
    }
    lines
        .last()
        .map(|line| (lines.len().saturating_sub(1), line_start, line.as_str()))
}

fn text_index_at(font: &BitmapFont, lines: &[String], line_index: usize, x: usize) -> usize {
    let mut line_start = 0;
    for (index, line) in lines.iter().enumerate() {
        let line_len = line.chars().count();
        if index == line_index.min(lines.len().saturating_sub(1)) {
            return line_start + char_index_at_x(font, line, x);
        }
        line_start += line_len;
    }
    line_start
}

fn char_index_at_x(font: &BitmapFont, text: &str, x: usize) -> usize {
    let mut cursor_x = 0;
    for (index, ch) in text.chars().enumerate() {
        let width = font.advance(ch);
        if x < cursor_x + width / 2 {
            return index;
        }
        cursor_x += width;
    }
    text.chars().count()
}

fn prefix_chars(text: &str, len: usize) -> &str {
    let byte = text
        .char_indices()
        .nth(len)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[..byte]
}

#[derive(Clone, Copy)]
struct Model {
    id: &'static str,
    thinking_id: &'static str,
    name: &'static str,
    asset_path: &'static str,
}

impl Model {
    fn id(self, thinking: bool) -> &'static str {
        if thinking { self.thinking_id } else { self.id }
    }
}

#[derive(Clone)]
struct Message {
    role: Role,
    text: String,
    kind: MessageKind,
    author: Option<&'static str>,
}

struct MessageLayout {
    style: MessageStyle,
    lines: Vec<String>,
    author: Option<&'static str>,
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
            kind: MessageKind::Normal,
            author: None,
        }
    }

    fn assistant(text: String) -> Self {
        Self {
            role: Role::Assistant,
            text,
            kind: MessageKind::Normal,
            author: None,
        }
    }

    fn intro(text: String) -> Self {
        Self {
            kind: MessageKind::Intro,
            ..Self::assistant(text)
        }
    }

    fn pending(author: &'static str) -> Self {
        Self {
            kind: MessageKind::Pending,
            author: Some(author),
            ..Self::assistant("...".to_string())
        }
    }

    fn style(&self) -> MessageStyle {
        match self.role {
            Role::User => USER_MESSAGE_STYLE,
            Role::Assistant => ASSISTANT_MESSAGE_STYLE,
        }
    }
}

fn intro_message() -> Message {
    Message::intro("pick a fwend and say hi".to_string())
}

fn smol_icon_rect(name: &str) -> Option<Rect> {
    let index = match name {
        "Qwen" => 0,
        "Deepseek" => 1,
        "Claude" => 2,
        "Kimi" => 3,
        _ => return None,
    };
    Some(Rect::new(
        (index % 2) * SMOL_ICON_SIZE,
        (index / 2) * SMOL_ICON_SIZE,
        SMOL_ICON_SIZE,
        SMOL_ICON_SIZE,
    ))
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
    min_w: usize,
    min_h: usize,
    align_right: bool,
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
    min_w: BUBBLE_MIN_W,
    min_h: 0,
    align_right: false,
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
    min_w: STICKY_MIN_W,
    min_h: STICKY_MIN_H,
    align_right: true,
};

fn request_history(messages: &[Message], current_name: &str) -> Vec<Message> {
    messages
        .iter()
        .filter(|message| message.kind == MessageKind::Normal)
        .rev()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| match (message.role, message.author) {
            (Role::User, _) => Message::user(format!("{USER_NAME}: {}", message.text)),
            (Role::Assistant, Some(author)) if author != current_name => {
                Message::user(format!("{author}: {}", message.text))
            }
            (Role::Assistant, _) => message,
        })
        .collect()
}

fn strip_self_prefix(text: &str, name: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(name)
        && let Some(rest) = rest.trim_start().strip_prefix(':')
    {
        return rest.trim_start().to_string();
    }
    trimmed.to_string()
}

fn fwend_system_prompt(template: &str, name: &str, thinking: bool) -> String {
    let mut prompt = template.replace("[[FREND_NAME]]", name);
    prompt.push_str(&format!(
        "\n\nThis is a group chat: messages from {USER_NAME} and from other fwends arrive labeled like \"{USER_NAME}: ...\" or \"Qwen: ...\". Your own earlier replies are unlabeled. Never start your reply with \"{name}:\" — just speak."
    ));
    if thinking {
        format!(
            "{prompt}\n\nThe lamp is on: think carefully through hard requests before answering, but keep hidden reasoning out of the visible response."
        )
    } else {
        prompt
    }
}

fn send_openrouter_request(
    model: &str,
    system_prompt: &str,
    history: &[Message],
    latest_text: &str,
    thinking: bool,
) -> Result<String, String> {
    let api_key =
        env::var("OPENROUTER_API_KEY").map_err(|_| "OPENROUTER_API_KEY is not set".to_string())?;
    let body = chat_body(model, system_prompt, history, latest_text, thinking);
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

fn chat_body(
    model: &str,
    system_prompt: &str,
    history: &[Message],
    latest_text: &str,
    thinking: bool,
) -> String {
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

    let reasoning = if thinking {
        json!({
            "effort": "low",
            "exclude": true,
        })
    } else {
        json!({
            "exclude": true,
        })
    };

    let mut body = json!({
        "model": model,
        "models": [OPENROUTER_FALLBACK_MODEL],
        "messages": messages,
        "reasoning": reasoning,
        "include_reasoning": false,
    });
    if thinking {
        body["tools"] = json!([
            {
                "type": "openrouter:web_fetch",
            }
        ]);
    }
    body.to_string()
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
    #[ignore]
    fn bench_message_layouts() {
        use std::time::Instant;

        let palette = crate::Palette::load(concat!(env!("CARGO_MANIFEST_DIR"), "/na16-1x.png"))
            .unwrap();
        let mut fwends = Fwends::load(&palette).unwrap();
        for i in 0..60 {
            fwends.messages.push(Message::user(format!(
                "message number {i} with enough words to need wrapping across lines"
            )));
            fwends.messages.push(Message::assistant(format!(
                "reply number {i}, also long enough that the wrapper has to break it up"
            )));
        }

        let iterations = 200;
        let start = Instant::now();
        for _ in 0..iterations {
            // What one draw_messages + content_height + max_scroll costs.
            std::hint::black_box(fwends.message_layouts());
            std::hint::black_box(fwends.content_height());
            std::hint::black_box(fwends.max_scroll());
        }
        println!(
            "fwends render-layout work: {:?} per frame ({iterations} iterations)",
            start.elapsed() / iterations
        );
    }

    #[test]
    fn extracts_assistant_message_content() {
        let json = r#"{"content":"wrong","choices":[{"message":{"role":"assistant","content":"hello\nthere"}}]}"#;

        assert_eq!(extract_content(json).as_deref(), Ok("hello there"));
    }

    #[test]
    fn chat_body_preserves_unicode_and_escapes_json() {
        let body = chat_body("model", "system", &[], "hi \"there\" 🩷", false);
        let parsed: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(parsed["model"], "model");
        assert_eq!(parsed["models"][0], "@preset/free");
        assert_eq!(parsed["messages"][1]["content"], "hi \"there\" 🩷");
    }

    #[test]
    fn chat_body_enables_low_effort_reasoning_and_web_fetch_when_thinking() {
        let body = chat_body("model", "system", &[], "hi", true);
        let parsed: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(parsed["reasoning"]["effort"], "low");
        assert_eq!(parsed["reasoning"]["exclude"], true);
        assert_eq!(parsed["tools"][0]["type"], "openrouter:web_fetch");
    }

    #[test]
    fn chat_body_omits_web_fetch_when_not_thinking() {
        let body = chat_body("model", "system", &[], "hi", false);
        let parsed: Value = serde_json::from_str(&body).unwrap();

        assert!(parsed.get("tools").is_none());
    }

    #[test]
    fn fwend_system_prompt_replaces_name_placeholder() {
        let prompt = fwend_system_prompt("you are [[FREND_NAME]]!", "Qwen", false);

        assert!(prompt.starts_with("you are Qwen!"));
        assert!(prompt.contains("Never start your reply with \"Qwen:\""));
    }

    #[test]
    fn request_history_tags_user_and_other_models() {
        let mut claude_reply = Message::assistant("hi jodie".to_string());
        claude_reply.author = Some("Claude");
        let mut qwen_reply = Message::assistant("hello!".to_string());
        qwen_reply.author = Some("Qwen");
        let messages = vec![
            intro_message(),
            Message::user("hey".to_string()),
            claude_reply,
            qwen_reply,
        ];

        let history = request_history(&messages, "Qwen");

        assert_eq!(history.len(), 3);
        assert!(matches!(history[0].role, Role::User));
        assert_eq!(history[0].text, "Jodie: hey");
        assert!(matches!(history[1].role, Role::User));
        assert_eq!(history[1].text, "Claude: hi jodie");
        assert!(matches!(history[2].role, Role::Assistant));
        assert_eq!(history[2].text, "hello!");
    }

    #[test]
    fn strips_reflexive_name_prefix_from_reply() {
        assert_eq!(strip_self_prefix("Qwen: hi there", "Qwen"), "hi there");
        assert_eq!(strip_self_prefix("  Qwen : hi", "Qwen"), "hi");
        assert_eq!(strip_self_prefix("Qwen is great", "Qwen"), "Qwen is great");
        assert_eq!(strip_self_prefix("hi there", "Qwen"), "hi there");
    }
}
