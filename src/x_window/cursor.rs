//! Hardware cursor loading and switching: builds the ARGB `cursor_*` sprites
//! into RENDER cursors and swaps the window's active one.

use std::error::Error;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::render::{self, ConnectionExt as RenderConnectionExt, PictType};
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, CreateGCAux, Cursor, ImageFormat, ImageOrder,
};

use super::XWindow;
use crate::{CURSOR_KIND_COUNT, CursorKind, Palette, Sprite, TRANSPARENT, assets};

impl XWindow {
    /// Build the four ARGB hardware cursors from the baked `cursor_*` sprites
    /// via the RENDER extension and show the pointer one. Leaves the default
    /// X cursor in place when the server can't do RENDER cursors (>= 0.5).
    pub(crate) fn load_cursors(&mut self, palette: &Palette) -> Result<(), Box<dyn Error>> {
        if self
            .conn
            .extension_information(render::X11_EXTENSION_NAME)?
            .is_none()
        {
            return Ok(());
        }
        let version = self.conn.render_query_version(0, 8)?.reply()?;
        if version.major_version == 0 && version.minor_version < 5 {
            return Ok(());
        }
        let formats = self.conn.render_query_pict_formats()?.reply()?;
        let Some(argb32) = formats.formats.iter().find(|f| {
            f.depth == 32
                && f.type_ == PictType::DIRECT
                && f.direct.alpha_mask == 0xFF
                && f.direct.alpha_shift == 24
                && f.direct.red_shift == 16
                && f.direct.green_shift == 8
                && f.direct.blue_shift == 0
        }) else {
            return Ok(());
        };

        // Hotspots: arrow tip, I-beam center, fingertip, circle center.
        let sprites: [(Sprite, i16, i16); CURSOR_KIND_COUNT] = [
            (assets::cursor_pointer(), 4, 0),
            (assets::cursor_text(), 12, 12),
            (assets::cursor_hand(), 11, 0),
            (assets::cursor_disabled(), 12, 12),
        ];
        let mut cursors = [0; CURSOR_KIND_COUNT];
        for (cursor, (sprite, hot_x, hot_y)) in cursors.iter_mut().zip(&sprites) {
            *cursor = self.create_cursor(sprite, palette, argb32.id, *hot_x, *hot_y)?;
        }
        self.cursors = Some(cursors);
        self.set_cursor(CursorKind::Pointer)
    }

    fn create_cursor(
        &self,
        sprite: &Sprite,
        palette: &Palette,
        format: render::Pictformat,
        hot_x: i16,
        hot_y: i16,
    ) -> Result<Cursor, Box<dyn Error>> {
        let mut data = Vec::with_capacity(sprite.width * sprite.height * 4);
        let msb_first = self.conn.setup().image_byte_order == ImageOrder::MSB_FIRST;
        for y in 0..sprite.height {
            for x in 0..sprite.width {
                let index = sprite.at(x, y);
                // RENDER wants premultiplied alpha; with only fully opaque or
                // fully transparent pixels the colors pass through unchanged.
                let pixel = if index == TRANSPARENT {
                    [0, 0, 0, 0]
                } else {
                    let c = palette.color(index);
                    if msb_first {
                        [0xFF, c.r, c.g, c.b]
                    } else {
                        [c.b, c.g, c.r, 0xFF]
                    }
                };
                data.extend_from_slice(&pixel);
            }
        }

        let pixmap = self.conn.generate_id()?;
        self.conn.create_pixmap(
            32,
            pixmap,
            self.window,
            sprite.width as u16,
            sprite.height as u16,
        )?;
        let gc = self.conn.generate_id()?;
        self.conn.create_gc(gc, pixmap, &CreateGCAux::new())?;
        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            sprite.width as u16,
            sprite.height as u16,
            0,
            0,
            0,
            32,
            &data,
        )?;
        let picture = self.conn.generate_id()?;
        self.conn.render_create_picture(
            picture,
            pixmap,
            format,
            &render::CreatePictureAux::new(),
        )?;
        let cursor = self.conn.generate_id()?;
        self.conn
            .render_create_cursor(cursor, picture, hot_x as u16, hot_y as u16)?;
        self.conn.render_free_picture(picture)?;
        self.conn.free_gc(gc)?;
        self.conn.free_pixmap(pixmap)?;
        self.conn.flush()?;
        Ok(cursor)
    }

    /// Switch the window's cursor; no-ops when unchanged or unavailable.
    pub(crate) fn set_cursor(&mut self, kind: CursorKind) -> Result<(), Box<dyn Error>> {
        let Some(cursors) = &self.cursors else {
            return Ok(());
        };
        if self.current_cursor == Some(kind) {
            return Ok(());
        }
        self.conn.change_window_attributes(
            self.window,
            &ChangeWindowAttributesAux::new().cursor(cursors[kind as usize]),
        )?;
        self.conn.flush()?;
        self.current_cursor = Some(kind);
        Ok(())
    }
}
