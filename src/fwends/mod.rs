use std::error::Error;

use crate::palette_color;
use crate::text::{
    BitmapFont, EditKey, KeyInput, LinePlacement, TextEditOutcome, TextField, TextLayout, edit_key,
};
use crate::widget::Widget;
use crate::{
    Caps, CursorKind, Framebuffer, Index, Paint, Palette, PaletteIndex, Rect, Sprite, Swap,
    TRANSPARENT,
};

mod chat;

/// `isize::max` isn't usable in `const fn` (`Ord` isn't a stable const trait
/// yet), so the few const layout getters below that need a clamped-at-zero
/// *signed* subtraction use this instead. Most layout math here is plain
/// `usize` (widths/heights/paddings can't be negative), so this is only
/// needed where a position (not a size) is computed from other positions.
const fn max_isize(a: isize, b: isize) -> isize {
    if a > b { a } else { b }
}

const CONTENT_W: usize = 348;
const W: usize = CONTENT_W + ERASER_W - ERASER_CONTENT_OVERLAP;
const H: usize = 318;
const ERASER_W: usize = 45;
const ERASER_CONTENT_OVERLAP: usize = 30;
const CONTENT_X_OFFSET: usize = 80;
const PAD: usize = 8;
const CHAT_Y: isize = 8;
const CHAT_INPUT_GAP: isize = 6;
const INPUT_X: usize = 24;
const INPUT_BOTTOM_PAD: usize = 122;
const INPUT_TEXT_Y: isize = 24;
const TEXT_PAD: usize = 7;
const INPUT_BOX_Y_OFFSET: isize = -13;
const INPUT_BOX_RIGHT_PAD: usize = 11;
const INPUT_EXTRA_W: usize = 40;
const INPUT_EXTRA_H: usize = 4;
const SELECTED_FWEND_GAP: usize = 4;
const SELECTED_FWEND_Y_OFFSET: isize = 10;
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
const INPUT_MAX_LINES: usize = 5;
const SMOL_ICON_SIZE: usize = 11;
const SMOL_ICON_GAP: usize = 2;
const SMOL_ICON_Y_OFFSET: usize = 3;
const PENCIL_TIP_X: usize = 0;
const PENCIL_TIP_Y: usize = 24;
const LAMP_RIGHT_PAD: usize = 140;
const LAMP_Y_OFFSET: usize = 60;
const ERASER_RIGHT_PAD: usize = 0;

pub struct Fwends {
    avatars: [Sprite; chat::MODELS.len()],
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
    messages: Vec<chat::Message>,
    input: TextField,
    selected_model: usize,
    lamp_on: bool,
    focused: bool,
    height: usize,
    scroll_y: usize,
    model_slot_w: usize,
    model_slot_h: usize,
    /// Whether a reply is in flight (and for whom); the single source of
    /// truth the "..." placeholder bubble is rendered from, instead of that
    /// bubble being a real (and separately tracked) entry in `messages`.
    state: chat::ChatState,
    system_prompt: String,
    // message_layouts() wraps every message through the font engine, so the
    // result is cached here and refreshed via messages_changed() instead of
    // recomputed on every scroll and frame.
    layouts: Vec<MessageLayout>,
    // Per-message layout cache (paired with the `Message` that produced it),
    // indexed the same as `messages`. Lets message_layouts() only recompute
    // wrapping for messages that are new or have changed instead of
    // re-wrapping the whole history on every new message. The pending-reply
    // placeholder bubble is appended separately (see `message_layouts`),
    // since it isn't a real message and so isn't part of this cache.
    layout_cache: Vec<(chat::Message, MessageLayout)>,
}

impl Fwends {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let avatars = chat::MODELS.map(|model| (model.avatar)());
        let model_slot_w = avatars.iter().map(|avatar| avatar.width).max().unwrap_or(1);
        let model_slot_h = avatars
            .iter()
            .map(|avatar| avatar.height)
            .max()
            .unwrap_or(1);
        let selected_model = 0;

        let mut fwends = Self {
            avatars,
            bubble: crate::assets::bubble(),
            user_sticky: crate::assets::sticky(),
            input_sticky: crate::assets::sticky_stack(),
            smol_icons: crate::assets::smol_fwends(),
            pencil: crate::assets::focus_pencil(),
            pencil_shadow: crate::assets::toodle_pencil_shadow(),
            lamp_on_image: crate::assets::lamp_on(),
            lamp_off_image: crate::assets::lamp_off(),
            eraser: crate::assets::eraser(),
            font: BitmapFont::load_with_fallback(
                &pixel_fonts::COMICORO_SPEC,
                &pixel_fonts::FUSION_PIXEL_10_SPEC,
            )?,
            messages: vec![chat::intro_message()],
            input: TextField::new(MAX_INPUT_CHARS, INPUT_MAX_LINES),
            selected_model,
            lamp_on: false,
            focused: false,
            height: H,
            scroll_y: 0,
            model_slot_w,
            model_slot_h,
            state: chat::ChatState::Idle,
            system_prompt: chat::load_system_prompt(),
            layouts: Vec::new(),
            layout_cache: Vec::new(),
        };
        fwends.messages_changed();
        Ok(fwends)
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn min_height(&self) -> usize {
        H
    }

    pub(crate) fn set_height(&mut self, height: usize) -> bool {
        let height = height.max(self.min_height());
        if self.height == height {
            return false;
        }

        // A user reading scrollback keeps their place across a resize.
        let was_at_bottom = self.scroll_y >= self.max_scroll();
        self.height = height;
        if was_at_bottom {
            self.scroll_to_bottom();
        } else {
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
        true
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        self.draw_lamp(fb, palette);
        self.draw_messages(fb, palette);
        self.draw_input(fb, palette);
        self.draw_selected_fwend(fb, palette);
        self.draw_eraser(fb, palette);
    }

    pub(crate) fn click(&mut self, x: isize, y: isize) {
        self.focused = false;
        self.input.end_drag();
        if x < 0 || y < 0 {
            return;
        }

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
            self.select_next_model();
            return;
        }

        if self.input_contains(x, y) {
            self.focused = true;
            let cursor = self.input.index_at(&self.input_layout(&self.font), x, y);
            self.input.begin_drag(cursor);
        }
    }

    pub(crate) const fn text_dragging(&self) -> bool {
        self.input.is_dragging()
    }

    pub(crate) fn handle_key_press(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Option<String> {
        let key = edit_key(input);

        // Tab always switches models, mirroring a click on the fwend avatar,
        // regardless of focus. Plain Left/Right do the same *only* when the
        // input isn't in a state to consume them for cursor movement: unfocused,
        // or read-only while awaiting a reply. Otherwise they fall through to
        // the input's own handling below so the cursor moves / selection
        // collapses like any normal text field. Shift+Left/Right always fall
        // through so text selection in the input still works; `edit_key`
        // doesn't distinguish shift on its own.
        let input_editable = self.focused && !self.state.is_awaiting();
        match key {
            EditKey::Tab => {
                self.select_next_model();
                return None;
            }
            EditKey::Right if !input.shift() && !input_editable => {
                self.select_next_model();
                return None;
            }
            EditKey::Left if !input.shift() && !input_editable => {
                self.select_prev_model();
                return None;
            }
            _ => {}
        }

        if input_editable {
            let layout = self.input_layout(&self.font);
            let outcome = self.input.handle_key(input, clipboard_text, &layout);
            if let TextEditOutcome::Handled { changed: _, copy } = outcome {
                return copy;
            }
        }

        match key {
            EditKey::Enter if self.focused => self.send(),
            EditKey::Escape => self.focused = false,
            _ => {}
        }
        None
    }

    const fn select_next_model(&mut self) {
        self.selected_model = (self.selected_model + 1) % chat::MODELS.len();
    }

    fn select_prev_model(&mut self) {
        self.selected_model = self
            .selected_model
            .checked_sub(1)
            .unwrap_or(chat::MODELS.len() - 1);
    }

    pub(crate) fn drain_reply(&mut self) -> bool {
        let chat::ChatState::Awaiting(pending) = &self.state else {
            return false;
        };
        let Some(reply) = pending.poll() else {
            return false;
        };

        // Already matched `Awaiting` above; `reply` was drained from the one
        // and only poll of this pending request, so it's safe to move it out.
        let chat::ChatState::Awaiting(pending) =
            std::mem::replace(&mut self.state, chat::ChatState::Idle)
        else {
            unreachable!("just matched Awaiting above")
        };
        let author = pending.author();
        let text = match reply {
            Ok(text) => chat::strip_self_prefix(&text, author),
            Err(err) => format!("oops: {err}"),
        };
        self.messages
            .push(chat::Message::assistant_with_author(text, author));
        self.messages_changed();
        true
    }

    pub(crate) fn scroll_up(&mut self, x: isize, y: isize) -> bool {
        if !self.chat_contains(x, y) {
            return false;
        }
        let new_scroll_y = self.scroll_y.saturating_sub(SCROLL_STEP);
        let changed = new_scroll_y != self.scroll_y;
        self.scroll_y = new_scroll_y;
        changed
    }

    pub(crate) fn scroll_down(&mut self, x: isize, y: isize) -> bool {
        if !self.chat_contains(x, y) {
            return false;
        }
        let new_scroll_y = (self.scroll_y + SCROLL_STEP).min(self.max_scroll());
        let changed = new_scroll_y != self.scroll_y;
        self.scroll_y = new_scroll_y;
        changed
    }

    fn send(&mut self) {
        if self.state.is_awaiting() {
            return;
        }

        let text = self.input.text().trim().to_string();
        if text.is_empty() {
            return;
        }

        self.input.clear();
        self.messages
            .retain(|message| message.kind() != chat::MessageKind::Intro);
        let thinking = self.lamp_on;
        let model = chat::MODELS[self.selected_model];

        // Build the history before pushing the new message: both request
        // paths send the latest user message separately, so it must not also
        // appear at the end of the history.
        let system_prompt =
            chat::fwend_system_prompt(&self.system_prompt, model.name, chat::user_name());
        let history = chat::request_history(&self.messages, model.name, chat::user_name());

        let prefixed_text = format!("{}: {text}", chat::user_name());
        self.messages.push(chat::Message::user(text));

        self.state = chat::ChatState::Awaiting(chat::PendingReply::spawn(
            model,
            thinking,
            system_prompt,
            history,
            prefixed_text,
        ));
        self.messages_changed();
        // Sending your own message always jumps to it.
        self.scroll_to_bottom();
    }

    fn erase_chat_history(&mut self) {
        if let chat::ChatState::Awaiting(pending) =
            std::mem::replace(&mut self.state, chat::ChatState::Idle)
        {
            pending.cancel();
        }
        self.messages = vec![chat::intro_message()];
        self.messages_changed();
    }

    /// Cancels any in-flight request so the app can exit without leaking the
    /// curl child or its temp request-body file. See
    /// `PendingReply::cancel_and_wait` and `PendingReply::handle` for why
    /// this must (boundedly) join, not just signal.
    pub(crate) fn shutdown(&mut self) {
        if let chat::ChatState::Awaiting(pending) =
            std::mem::replace(&mut self.state, chat::ChatState::Idle)
        {
            pending.cancel_and_wait(std::time::Duration::from_secs(2));
        }
    }

    /// Refresh the cached layouts. Follows new messages only when the view was
    /// already at the bottom; a user reading scrollback keeps their place
    /// (clamped to the new content height).
    fn messages_changed(&mut self) {
        let was_at_bottom = self.scroll_y >= self.max_scroll();
        self.layouts = self.message_layouts();
        if was_at_bottom {
            self.scroll_to_bottom();
        } else {
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
    }

    fn draw_messages(&self, fb: &mut Framebuffer, palette: &Palette) {
        let viewport_top = CHAT_Y;
        let viewport_bottom = CHAT_Y + self.chat_h() as isize;
        let bottom_offset = self.chat_h().saturating_sub(self.content_height());

        for layout in &self.layouts {
            let y = CHAT_Y + bottom_offset as isize + layout.y as isize - self.scroll_y as isize;
            if y >= viewport_bottom || y + layout.h as isize <= viewport_top {
                continue;
            }

            self.draw_bubble(fb, palette, layout.x, y, layout.w, layout.h, layout.style);
            if let Some(src) = layout.author.and_then(smol_icon_rect) {
                let clip = Rect::new(0, viewport_top, self.width(), self.chat_h());
                let icon_x = layout.x + (layout.w + SMOL_ICON_GAP) as isize;
                let icon_y = y + layout.h as isize - (SMOL_ICON_SIZE + SMOL_ICON_Y_OFFSET) as isize;
                fb.draw_sprite_full(
                    &self.smol_icons,
                    src,
                    icon_x,
                    icon_y,
                    Some(clip),
                    palette,
                    None,
                );
            }
            let text_x = layout.x + layout.style.pad_left as isize;
            let mut text_y = y + layout.style.pad_top as isize;
            for line in &layout.lines {
                if text_y >= viewport_top && text_y + self.font.cell_h() as isize <= viewport_bottom
                {
                    self.font
                        .draw_text(fb, line, text_x, text_y, palette_color::BLACK);
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
        x: isize,
        y: isize,
        w: usize,
        h: usize,
        style: MessageStyle,
    ) {
        let clip_top = CHAT_Y;
        let clip_bottom = CHAT_Y + self.chat_h() as isize;
        let image = match style.skin {
            MessageSkin::Bubble => &self.bubble,
            MessageSkin::Sticky => &self.user_sticky,
        };

        for dy in 0..h {
            let py = y + dy as isize;
            if py < clip_top || py >= clip_bottom {
                continue;
            }
            let sy = pixel_graphics::stretch_source_coord(
                dy,
                h,
                image.height,
                style.top_cap,
                style.bottom_cap,
            );
            for dx in 0..w {
                let sx = pixel_graphics::stretch_source_coord(
                    dx,
                    w,
                    image.width,
                    style.left_cap,
                    style.right_cap,
                );
                let px = x + dx as isize;
                let Some(color) = palette.resolve_index(image.at(sx, sy), px, py) else {
                    continue;
                };
                fb.set_pixel(px, py, color);
            }
        }
    }

    /// Wraps `message` through the font engine and lays it out at `y = 0`;
    /// the caller fills in the real cumulative `y`.
    fn compute_message_layout(&self, message: &chat::Message) -> MessageLayout {
        let style = message_style(message.role());
        let max_text_w = BUBBLE_MAX_W - style.pad_left - style.pad_right;
        let layout = TextLayout::new(
            &self.font,
            0,
            0,
            max_text_w,
            LinePlacement::Uniform { line_h: LINE_H },
        );
        let lines = layout.wrap(message.text());
        let text_w = lines
            .iter()
            .map(|line| self.font.text_width(line))
            .max()
            .unwrap_or(0);
        let w = (text_w + style.pad_left + style.pad_right).clamp(style.min_w, BUBBLE_MAX_W);
        let h = (lines.len() * LINE_H + style.pad_top + style.pad_bottom)
            .max(style.top_cap + style.bottom_cap + 1);
        let x = if style.align_right {
            (CONTENT_W - PAD - w) as isize
        } else {
            self.assistant_bubble_x()
        };
        MessageLayout {
            style,
            lines,
            author: message.author(),
            x,
            y: 0,
            w,
            h,
        }
    }

    /// Rebuilds `self.layout_cache` so it has one entry per message, reusing
    /// cached wrapping for any message whose content hasn't changed since it
    /// was last computed. Only new/changed messages pay the wrapping cost;
    /// unchanged ones are just cloned out of the cache. Cumulative `y` is
    /// always recomputed since earlier heights may have changed.
    ///
    /// If a reply is currently pending, one more layout for the "..."
    /// placeholder bubble is appended after the cached ones. It isn't a real
    /// message, so it isn't cached — it's just one extra (cheap) wrap.
    fn message_layouts(&mut self) -> Vec<MessageLayout> {
        self.layout_cache.truncate(self.messages.len());
        for (i, message) in self.messages.iter().enumerate() {
            let stale = self
                .layout_cache
                .get(i)
                .is_none_or(|(cached_message, _)| cached_message != message);
            if stale {
                let layout = self.compute_message_layout(message);
                if i < self.layout_cache.len() {
                    self.layout_cache[i] = (message.clone(), layout);
                } else {
                    self.layout_cache.push((message.clone(), layout));
                }
            }
        }

        let mut layouts = Vec::with_capacity(self.layout_cache.len() + 1);
        let mut y = 0;
        for (_, cached) in &self.layout_cache {
            let mut layout = cached.clone();
            layout.y = y;
            y += layout.h + BUBBLE_GAP;
            layouts.push(layout);
        }

        if let chat::ChatState::Awaiting(pending) = &self.state {
            let placeholder =
                chat::Message::assistant_with_author("...".to_string(), pending.author());
            let mut layout = self.compute_message_layout(&placeholder);
            layout.y = y;
            layouts.push(layout);
        }

        layouts
    }

    const fn assistant_bubble_x(&self) -> isize {
        self.input_sticky_x()
    }

    fn content_height(&self) -> usize {
        self.layouts.last().map_or(0, |layout| layout.y + layout.h)
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
        fb.draw_sprite(image, x, y, palette);
    }

    fn draw_eraser(&self, fb: &mut Framebuffer, palette: &Palette) {
        let (x, y) = self.eraser_position();
        fb.draw_sprite(&self.eraser, x, y, palette);
    }

    fn eraser_contains(&self, x: isize, y: isize) -> bool {
        let (eraser_x, eraser_y) = self.eraser_position();
        if x < eraser_x
            || y < eraser_y
            || x >= eraser_x + self.eraser.width as isize
            || y >= eraser_y + self.eraser.height as isize
        {
            return false;
        }

        self.eraser
            .is_opaque((x - eraser_x) as usize, (y - eraser_y) as usize)
    }

    const fn eraser_position(&self) -> (isize, isize) {
        (
            W.saturating_sub(self.eraser.width + ERASER_RIGHT_PAD) as isize,
            self.height.saturating_sub(self.eraser.height) as isize,
        )
    }

    fn lamp_contains(&self, x: isize, y: isize) -> bool {
        let image = self.lamp_image();
        let (lamp_x, lamp_y) = self.lamp_position();
        if x < lamp_x
            || y < lamp_y
            || x >= lamp_x + image.width as isize
            || y >= lamp_y + image.height as isize
            || y >= self.input_y()
        {
            return false;
        }

        image.is_opaque((x - lamp_x) as usize, (y - lamp_y) as usize)
    }

    const fn lamp_image(&self) -> &Sprite {
        if self.lamp_on {
            &self.lamp_on_image
        } else {
            &self.lamp_off_image
        }
    }

    const fn lamp_position(&self) -> (isize, isize) {
        let image = self.lamp_image();
        (
            CONTENT_W.saturating_sub(image.width + LAMP_RIGHT_PAD) as isize,
            self.height.saturating_sub(image.height + LAMP_Y_OFFSET) as isize,
        )
    }

    fn draw_input(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.draw_resized(
            &self.input_sticky,
            Rect::new(
                self.input_sticky_x(),
                self.input_y(),
                self.input_sticky_w(),
                self.input_sticky_h(),
            ),
            Caps::new(
                STICKY_LEFT_CAP,
                STICKY_RIGHT_CAP,
                STICKY_TOP_CAP,
                STICKY_BOTTOM_CAP,
            ),
            palette,
        );

        let layout = self.input_layout(&self.font);
        if self.state.is_awaiting() {
            layout.draw(fb, "wait a sec...", palette_color::BLACK);
        } else {
            self.input
                .draw(fb, &layout, palette_color::BLACK, palette_color::LAVENDER);
        }
        self.draw_focused_pencil(fb, palette);
    }

    fn draw_selected_fwend(&self, fb: &mut Framebuffer, palette: &Palette) {
        let avatar = &self.avatars[self.selected_model];
        let rect = self.selected_fwend_rect();
        let avatar_x = rect.x + (rect.w.saturating_sub(avatar.width) / 2) as isize;
        let avatar_y = rect.y + rect.h.saturating_sub(avatar.height) as isize;
        let (lamp_x, lamp_y) = self.lamp_position();
        draw_lamp_masked_ellipse(
            fb,
            avatar_x + (avatar.width / 2) as isize,
            avatar_y + (avatar.height / 2) as isize,
            (avatar.width + 10) as isize,
            (avatar.height.max(1) + 8) as isize,
            palette,
            self.lamp_image(),
            lamp_x,
            lamp_y,
        );
        fb.draw_sprite(avatar, avatar_x, avatar_y, palette);
    }

    const fn selected_fwend_rect(&self) -> FwendRect {
        FwendRect {
            x: (CONTENT_X_OFFSET + INPUT_X) as isize,
            y: self.input_y() + SELECTED_FWEND_Y_OFFSET,
            w: self.model_slot_w,
            h: self.model_slot_h,
        }
    }

    fn draw_focused_pencil(&self, fb: &mut Framebuffer, palette: &Palette) {
        if !self.focused || self.state.is_awaiting() {
            return;
        }

        let (cursor_x, cursor_y) = self.input.cursor_position(&self.input_layout(&self.font));
        let dest_x = (cursor_x - PENCIL_TIP_X as isize).max(0);
        let dest_y = (cursor_y - PENCIL_TIP_Y as isize).max(0);
        draw_yellow_pencil_shadow(fb, &self.pencil_shadow, dest_x, dest_y, palette);
        fb.draw_sprite(&self.pencil, dest_x, dest_y, palette);
    }

    /// The text layout for the single-line-stacked input sticky.
    const fn input_layout<'a>(&self, font: &'a BitmapFont) -> TextLayout<'a> {
        TextLayout::new(
            font,
            self.input_text_x(),
            self.input_text_y(),
            self.input_text_width(),
            LinePlacement::Uniform { line_h: LINE_H },
        )
    }

    const fn input_text_x(&self) -> isize {
        self.input_sticky_x() + TEXT_PAD as isize
    }

    const fn input_text_y(&self) -> isize {
        max_isize(self.input_y() + INPUT_TEXT_Y + INPUT_BOX_Y_OFFSET, 0)
    }

    const fn input_text_width(&self) -> usize {
        let right_edge = self.input_sticky_x() + self.input_sticky_w() as isize
            - TEXT_PAD as isize
            - INPUT_BOX_RIGHT_PAD as isize;
        max_isize(right_edge - self.input_text_x(), 0) as usize
    }

    const fn input_sticky_x(&self) -> isize {
        (CONTENT_X_OFFSET + INPUT_X + self.model_slot_w + SELECTED_FWEND_GAP) as isize
    }

    const fn input_sticky_w(&self) -> usize {
        self.input_sticky.width + INPUT_EXTRA_W
    }

    const fn input_sticky_h(&self) -> usize {
        self.input_sticky.height + INPUT_EXTRA_H
    }

    const fn input_y(&self) -> isize {
        self.height.saturating_sub(INPUT_BOTTOM_PAD + INPUT_EXTRA_H) as isize
    }

    /// Whether `(x, y)` lands on the editable input sticky. Excludes the
    /// awaiting-reply state, when the sticky shows "wait a sec..." instead of
    /// the input text and clicks would select against invisible text.
    fn input_contains(&self, x: isize, y: isize) -> bool {
        let input_x = self.input_sticky_x();
        !self.state.is_awaiting()
            && x >= input_x
            && x < input_x + self.input_sticky_w() as isize
            && y >= self.input_y()
            && y < self.input_y() + self.input_sticky_h() as isize
    }

    const fn chat_h(&self) -> usize {
        max_isize(self.input_y() - (CHAT_Y + CHAT_INPUT_GAP), 0) as usize
    }

    fn chat_contains(&self, x: isize, y: isize) -> bool {
        ((PAD as isize)..(CONTENT_W as isize - PAD as isize)).contains(&x)
            && (CHAT_Y..CHAT_Y + self.chat_h() as isize).contains(&y)
    }
}

impl crate::widget::Widget for Fwends {
    fn width(&self) -> usize {
        W
    }

    fn height(&self) -> usize {
        self.height
    }

    fn layout_height(&self) -> usize {
        self.min_height()
    }

    fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        Self::render(self, fb, palette);
    }

    fn click(
        &mut self,
        x: isize,
        y: isize,
        _shift: bool,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        Self::click(self, x, y);
        Ok(crate::widget::ClickOutcome {
            text_drag: self.text_dragging(),
            ..Default::default()
        })
    }

    fn blur(&mut self) {
        self.focused = false;
        self.input.end_drag();
    }

    fn scroll(&mut self, x: isize, y: isize, direction: crate::widget::ScrollDirection) -> bool {
        match direction {
            crate::widget::ScrollDirection::Up => self.scroll_up(x, y),
            crate::widget::ScrollDirection::Down => self.scroll_down(x, y),
        }
    }

    /// Mirrors the hit-testing in `click`, without side effects.
    fn hit_test(&self, x: isize, y: isize) -> Option<CursorKind> {
        if x < 0 || y < 0 {
            return None;
        }
        if self.eraser_contains(x, y)
            || self.lamp_contains(x, y)
            || self.selected_fwend_rect().contains_point(x, y)
        {
            return Some(CursorKind::Hand);
        }
        if self.input_contains(x, y) {
            return Some(CursorKind::Text);
        }
        None
    }

    fn handle_key_press(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Ok(Self::handle_key_press(self, input, clipboard_text))
    }

    fn wants_clipboard(&self, input: &KeyInput) -> bool {
        input.is_plain_paste_shortcut()
    }

    fn drag_text(&mut self, x: isize, y: isize) -> bool {
        if !self.input.is_dragging() {
            return false;
        }
        let x = x.max(0);
        let y = y.max(0);
        let cursor = self.input.index_at(&self.input_layout(&self.font), x, y);
        self.input.drag_to(cursor)
    }

    fn end_text_drag(&mut self) {
        self.input.end_drag();
    }
}

#[derive(Clone, Copy)]
struct FwendRect {
    x: isize,
    y: isize,
    w: usize,
    h: usize,
}

impl FwendRect {
    const fn contains_point(self, x: isize, y: isize) -> bool {
        x >= self.x && x < self.x + self.w as isize && y >= self.y && y < self.y + self.h as isize
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_lamp_masked_ellipse(
    fb: &mut Framebuffer,
    center_x: isize,
    center_y: isize,
    diameter_w: isize,
    diameter_h: isize,
    palette: &Palette,
    lamp: &Sprite,
    lamp_x: isize,
    lamp_y: isize,
) {
    let radius_x = ((diameter_w + 1) / 2).max(1);
    let radius_y = ((diameter_h + 1) / 2).max(1);

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
            let Some(color) = lamp_shadow_color(lamp, lamp_x, lamp_y, x, y, palette) else {
                continue;
            };
            fb.set_pixel(x, y, color);
        }
    }
}

fn lamp_shadow_color(
    lamp: &Sprite,
    lamp_x: isize,
    lamp_y: isize,
    x: isize,
    y: isize,
    palette: &Palette,
) -> Option<Index> {
    let local_x = x - lamp_x;
    let local_y = y - lamp_y;
    if local_x < 0
        || local_y < 0
        || local_x >= lamp.width as isize
        || local_y >= lamp.height as isize
    {
        return None;
    }

    let mapped = match lamp.at(local_x as usize, local_y as usize) {
        TRANSPARENT => return None,
        palette_color::ROSE => palette_color::CRIMSON,
        #[allow(clippy::match_same_arms)]
        palette_color::PEACH => palette_color::CRIMSON,
        palette_color::PLUM => palette_color::BLACK,
        palette_color::CRIMSON => palette_color::PLUM,
        other => other,
    };
    palette.resolve_index(mapped, x, y)
}

/// The shadow art's LIME (lifted-line) and ROSE (page-tint) pixels both
/// become PEACH on the yellow input sticky; everything else passes through.
fn draw_yellow_pencil_shadow(
    fb: &mut Framebuffer,
    image: &Sprite,
    dest_x: isize,
    dest_y: isize,
    palette: &Palette,
) {
    let peach = Paint::Solid(PaletteIndex::new(palette_color::PEACH));
    let swap = Swap::identity()
        .set(palette_color::LIME, peach)
        .set(palette_color::ROSE, peach);
    fb.draw_sprite_swapped(image, dest_x, dest_y, palette, &swap);
}

#[derive(Clone)]
struct MessageLayout {
    style: MessageStyle,
    lines: Vec<String>,
    author: Option<&'static str>,
    x: isize,
    y: usize,
    w: usize,
    h: usize,
}

fn smol_icon_rect(name: &str) -> Option<Rect> {
    let index = chat::MODELS
        .iter()
        .find(|model| model.name == name)?
        .icon_index;
    Some(Rect::new(
        (index % 2) as isize * SMOL_ICON_SIZE as isize,
        (index / 2) as isize * SMOL_ICON_SIZE as isize,
        SMOL_ICON_SIZE,
        SMOL_ICON_SIZE,
    ))
}

const fn message_style(role: chat::Role) -> MessageStyle {
    match role {
        chat::Role::User => USER_MESSAGE_STYLE,
        chat::Role::Assistant => ASSISTANT_MESSAGE_STYLE,
    }
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
    align_right: true,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn bench_message_layouts() {
        use std::time::Instant;

        let palette = crate::assets::palette();
        let mut fwends = Fwends::load(&palette).unwrap();
        for i in 0..60 {
            fwends.messages.push(chat::Message::user(format!(
                "message number {i} with enough words to need wrapping across lines"
            )));
            fwends.messages.push(chat::Message::assistant(format!(
                "reply number {i}, also long enough that the wrapper has to break it up"
            )));
        }

        let iterations = 200;
        let start = Instant::now();
        for _ in 0..iterations {
            // What one messages_changed (full layout rebuild) costs.
            std::hint::black_box(fwends.message_layouts());
            std::hint::black_box(fwends.max_scroll());
        }
        println!(
            "fwends layout rebuild: {:?} ({iterations} iterations)",
            start.elapsed() / iterations
        );
    }
}
