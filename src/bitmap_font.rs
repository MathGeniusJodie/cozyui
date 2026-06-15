use std::error::Error;

use crate::text_wrap;
use crate::{Framebuffer, Index, Rgba, decode_png_with_size};

pub struct FontSpec {
    pub(crate) atlas_path: &'static str,
    pub(crate) cell_w: usize,
    pub(crate) cell_h: usize,
    pub(crate) cols: usize,
    pub(crate) x_origin: usize,
    pub(crate) advance: &'static [u8; 128],
}

pub struct BitmapFont {
    spec: &'static FontSpec,
    width: usize,
    pixels: Vec<bool>,
}

impl BitmapFont {
    pub(crate) fn load(spec: &'static FontSpec) -> Result<Self, Box<dyn Error>> {
        let (width, height, pixels) = decode_png_with_size(spec.atlas_path)?;
        let rows = 128_usize.div_ceil(spec.cols);
        if width < spec.cols * spec.cell_w || height < rows * spec.cell_h {
            return Err(format!(
                "font atlas {} is too small for {}x{} cells in {} columns",
                spec.atlas_path, spec.cell_w, spec.cell_h, spec.cols
            )
            .into());
        }
        let pixels = pixels.into_iter().map(is_glyph_ink).collect();
        Ok(Self {
            spec,
            width,
            pixels,
        })
    }

    pub(crate) const fn cell_h(&self) -> usize {
        self.spec.cell_h
    }

    pub(crate) const fn advance(&self, ch: char) -> usize {
        let code = glyph_code(ch);
        self.spec.advance[code] as usize
    }

    pub(crate) fn text_width(&self, text: &str) -> usize {
        text.chars().map(|ch| self.advance(ch)).sum()
    }

    pub(crate) fn text_ink_bounds(&self, text: &str) -> Option<TextInkBounds> {
        let mut bounds: Option<TextInkBounds> = None;
        let mut cursor_x = 0_isize;
        for ch in text.chars() {
            for gy in 0..self.spec.cell_h {
                for gx in 0..self.spec.cell_w {
                    if !self.is_on(ch, gx, gy) {
                        continue;
                    }

                    let x = cursor_x + gx as isize - self.spec.x_origin as isize;
                    let y = gy;
                    bounds = Some(bounds.map_or(
                        TextInkBounds {
                            min_x: x,
                            min_y: y,
                            max_x: x + 1,
                            max_y: y + 1,
                        },
                        |bounds| TextInkBounds {
                            min_x: bounds.min_x.min(x),
                            min_y: bounds.min_y.min(y),
                            max_x: bounds.max_x.max(x + 1),
                            max_y: bounds.max_y.max(y + 1),
                        },
                    ));
                }
            }
            cursor_x += self.advance(ch) as isize;
        }
        bounds
    }

    pub(crate) fn wrap_lines(&self, text: &str, max_width: usize) -> Vec<String> {
        text_wrap::wrap_lines(text, max_width, |ch| self.advance(ch))
    }

    pub(crate) fn draw_text(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        x: usize,
        y: usize,
        color: Index,
    ) {
        self.draw_text_limited(fb, text, x, y, color, usize::MAX);
    }

    /// Draw text whose origin may sit left of the visible area, keeping only
    /// pixels with x in `clip_x..clip_x + clip_w`: marquees, panning text.
    pub(crate) fn draw_text_clipped(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        x: isize,
        y: usize,
        color: Index,
        clip_x: usize,
        clip_w: usize,
    ) {
        let mut cursor_x = x;
        for ch in text.chars() {
            let advance = self.advance(ch) as isize;
            if cursor_x >= (clip_x + clip_w) as isize {
                break;
            }
            if cursor_x + self.spec.cell_w as isize > clip_x as isize {
                self.draw_glyph_clipped(fb, ch, cursor_x, y, color, clip_x, clip_x + clip_w);
            }
            cursor_x += advance;
        }
    }

    pub(crate) fn draw_text_limited(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        x: usize,
        y: usize,
        color: Index,
        max_chars: usize,
    ) {
        let mut cursor_x = x;
        for ch in text.chars().take(max_chars) {
            self.draw_glyph(fb, ch, cursor_x, y, color);
            cursor_x += self.advance(ch);
        }
    }

    fn is_on(&self, ch: char, x: usize, y: usize) -> bool {
        let code = glyph_code(ch);
        let sx = (code % self.spec.cols) * self.spec.cell_w + x;
        let sy = (code / self.spec.cols) * self.spec.cell_h + y;
        self.pixels[sy * self.width + sx]
    }

    fn draw_glyph(&self, fb: &mut Framebuffer, ch: char, x: usize, y: usize, color: Index) {
        self.draw_glyph_clipped(fb, ch, x as isize, y, color, 0, usize::MAX);
    }

    fn draw_glyph_clipped(
        &self,
        fb: &mut Framebuffer,
        ch: char,
        x: isize,
        y: usize,
        color: Index,
        clip_left: usize,
        clip_right: usize,
    ) {
        for gy in 0..self.spec.cell_h {
            for gx in 0..self.spec.cell_w {
                if !self.is_on(ch, gx, gy) {
                    continue;
                }
                let dest_x = x + (gx as isize - self.spec.x_origin as isize);
                if dest_x < clip_left as isize || dest_x as usize >= clip_right {
                    continue;
                }
                fb.set_pixel(dest_x as usize, y + gy, color);
            }
        }
    }
}

pub struct TextInkBounds {
    pub(crate) min_x: isize,
    pub(crate) min_y: usize,
    pub(crate) max_x: isize,
    pub(crate) max_y: usize,
}

impl TextInkBounds {
    pub(crate) fn width(&self) -> usize {
        (self.max_x - self.min_x).max(0) as usize
    }

    pub(crate) const fn height(&self) -> usize {
        self.max_y.saturating_sub(self.min_y)
    }
}

const fn glyph_code(ch: char) -> usize {
    if ch.is_ascii() {
        ch as usize
    } else {
        '?' as usize
    }
}

const fn is_glyph_ink(color: Rgba) -> bool {
    let luminance = color.r as u16 + color.g as u16 + color.b as u16;
    luminance >= 384
}
