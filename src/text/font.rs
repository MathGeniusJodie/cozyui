use std::error::Error;

use super::wrap;
use crate::{Framebuffer, Index, Rgba, decode_png_with_size};

/// One bitmap atlas covering a contiguous block of `FontSpec::block`
/// codepoints starting at `first_codepoint`. A font's glyphs are split across
/// several atlases so coverage can extend across Unicode without baking one
/// enormous, mostly-empty image; blocks with no ink are simply absent.
pub struct FontAtlas {
    pub(crate) first_codepoint: usize,
    pub(crate) path: &'static str,
}

pub struct FontSpec {
    pub(crate) atlases: &'static [FontAtlas],
    /// Codepoints covered by each atlas; cells are laid out by `code - first`.
    pub(crate) block: usize,
    pub(crate) cell_w: usize,
    pub(crate) cell_h: usize,
    pub(crate) cols: usize,
    pub(crate) x_origin: usize,
    pub(crate) advance: &'static [u8],
}

struct LoadedAtlas {
    first_codepoint: usize,
    width: usize,
    pixels: Vec<bool>,
}

/// A character resolved to a concrete atlas cell and its advance width: `ch`
/// itself when a loaded block covers it, otherwise a fallback font's glyph, or
/// finally the `?` fallback. Resolving once per glyph keeps `is_on` a plain
/// array index in the per-pixel loops. The cell geometry travels with the
/// glyph because a fallback glyph comes from a font with its own cell size.
struct Glyph<'a> {
    atlas: &'a LoadedAtlas,
    cell_x: usize,
    cell_y: usize,
    cell_w: usize,
    cell_h: usize,
    x_origin: usize,
    advance: usize,
}

impl Glyph<'_> {
    fn is_on(&self, x: usize, y: usize) -> bool {
        self.atlas.pixels[(self.cell_y + y) * self.atlas.width + self.cell_x + x]
    }
}

pub struct BitmapFont {
    spec: &'static FontSpec,
    atlases: Vec<LoadedAtlas>,
    /// Consulted for codepoints this font lacks, before giving up on `?`.
    fallback: Option<Box<Self>>,
}

impl BitmapFont {
    pub(crate) fn load(spec: &'static FontSpec) -> Result<Self, Box<dyn Error>> {
        Self::load_inner(spec, None)
    }

    /// Load `spec`, resolving any codepoint it lacks against `fallback` (itself
    /// a full font, so it may chain its own fallback) before the `?` glyph.
    pub(crate) fn load_with_fallback(
        spec: &'static FontSpec,
        fallback: &'static FontSpec,
    ) -> Result<Self, Box<dyn Error>> {
        Self::load_inner(spec, Some(Box::new(Self::load(fallback)?)))
    }

    fn load_inner(
        spec: &'static FontSpec,
        fallback: Option<Box<Self>>,
    ) -> Result<Self, Box<dyn Error>> {
        let block_rows = spec.block.div_ceil(spec.cols);
        let mut atlases = Vec::with_capacity(spec.atlases.len());
        for atlas in spec.atlases {
            let (width, height, pixels) = decode_png_with_size(atlas.path)?;
            if width < spec.cols * spec.cell_w || height < block_rows * spec.cell_h {
                return Err(format!(
                    "font atlas {} is too small for {}x{} cells in {} columns",
                    atlas.path, spec.cell_w, spec.cell_h, spec.cols
                )
                .into());
            }
            atlases.push(LoadedAtlas {
                first_codepoint: atlas.first_codepoint,
                width,
                pixels: pixels.into_iter().map(is_glyph_ink).collect(),
            });
        }
        Ok(Self {
            spec,
            atlases,
            fallback,
        })
    }

    /// Resolve `ch` to its atlas cell and advance: this font first, then the
    /// fallback chain, finally the `?` glyph. `?` always lives in the first
    /// block, so resolution never fails.
    fn glyph(&self, ch: char) -> Glyph<'_> {
        self.resolve(ch as usize)
            .or_else(|| self.locate('?' as usize))
            .expect("'?' fallback glyph must be present in the first atlas block")
    }

    /// This font's glyph for `code` if it has one, otherwise the fallback
    /// chain's. Does not apply the `?` substitution so callers can distinguish
    /// "no font covers this" from a deliberate fallback.
    fn resolve(&self, code: usize) -> Option<Glyph<'_>> {
        self.locate(code)
            .or_else(|| self.fallback.as_deref().and_then(|f| f.resolve(code)))
    }

    fn locate(&self, code: usize) -> Option<Glyph<'_>> {
        self.atlases.iter().find_map(|atlas| {
            let local = code
                .checked_sub(atlas.first_codepoint)
                .filter(|&local| local < self.spec.block)?;
            Some(Glyph {
                atlas,
                cell_x: (local % self.spec.cols) * self.spec.cell_w,
                cell_y: (local / self.spec.cols) * self.spec.cell_h,
                cell_w: self.spec.cell_w,
                cell_h: self.spec.cell_h,
                x_origin: self.spec.x_origin,
                advance: self.spec.advance[code] as usize,
            })
        })
    }

    pub(crate) const fn cell_h(&self) -> usize {
        self.spec.cell_h
    }

    pub(crate) fn advance(&self, ch: char) -> usize {
        self.glyph(ch).advance
    }

    pub(crate) fn text_width(&self, text: &str) -> usize {
        text.chars().map(|ch| self.advance(ch)).sum()
    }

    pub(crate) fn text_ink_bounds(&self, text: &str) -> Option<TextInkBounds> {
        let mut bounds: Option<TextInkBounds> = None;
        let mut cursor_x = 0_isize;
        for ch in text.chars() {
            let glyph = self.glyph(ch);
            for gy in 0..glyph.cell_h {
                for gx in 0..glyph.cell_w {
                    if !glyph.is_on(gx, gy) {
                        continue;
                    }

                    let x = cursor_x + gx as isize - glyph.x_origin as isize;
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
            cursor_x += glyph.advance as isize;
        }
        bounds
    }

    pub(crate) fn wrap_lines(&self, text: &str, max_width: usize) -> Vec<String> {
        wrap::wrap_lines(text, max_width, |ch| self.advance(ch))
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
            if cursor_x >= (clip_x + clip_w) as isize {
                break;
            }
            let glyph = self.glyph(ch);
            if cursor_x + glyph.cell_w as isize > clip_x as isize {
                Self::draw_glyph(fb, &glyph, cursor_x, y, color, clip_x, clip_x + clip_w);
            }
            cursor_x += glyph.advance as isize;
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
            let glyph = self.glyph(ch);
            Self::draw_glyph(fb, &glyph, cursor_x as isize, y, color, 0, usize::MAX);
            cursor_x += glyph.advance;
        }
    }

    fn draw_glyph(
        fb: &mut Framebuffer,
        glyph: &Glyph<'_>,
        x: isize,
        y: usize,
        color: Index,
        clip_left: usize,
        clip_right: usize,
    ) {
        for gy in 0..glyph.cell_h {
            for gx in 0..glyph.cell_w {
                if !glyph.is_on(gx, gy) {
                    continue;
                }
                let dest_x = x + (gx as isize - glyph.x_origin as isize);
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

const fn is_glyph_ink(color: Rgba) -> bool {
    let luminance = color.r as u16 + color.g as u16 + color.b as u16;
    luminance >= 384
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peanut_money_font::PEANUT_MONEY_SPEC;

    fn font() -> BitmapFont {
        BitmapFont::load(&PEANUT_MONEY_SPEC).unwrap()
    }

    fn glyph_ink(font: &BitmapFont, ch: char) -> Vec<(usize, usize)> {
        let glyph = font.glyph(ch);
        let mut ink = Vec::new();
        for y in 0..glyph.cell_h {
            for x in 0..glyph.cell_w {
                if glyph.is_on(x, y) {
                    ink.push((x, y));
                }
            }
        }
        ink
    }

    #[test]
    fn renders_non_ascii_glyph_distinct_from_fallback() {
        let font = font();
        // Some glyph beyond ASCII must render its own ink, distinct from the
        // '?' fallback, proving the extra atlas blocks are loaded and indexed.
        let fallback = glyph_ink(&font, '?');
        let found = (0x00A0..0x0180u32).filter_map(char::from_u32).any(|ch| {
            let ink = glyph_ink(&font, ch);
            !ink.is_empty() && ink != fallback && font.advance(ch) > 0
        });
        assert!(found, "no non-ascii glyph rendered distinct ink");
    }

    #[test]
    fn missing_glyph_resolves_through_fallback_font() {
        // peanut_money lacks CJK, so a fallback font must supply U+4E00's ink
        // and advance instead of the bare '?' glyph the chain ends on.
        use crate::fusion_pixel_10_font::FUSION_PIXEL_10_SPEC;
        let font =
            BitmapFont::load_with_fallback(&PEANUT_MONEY_SPEC, &FUSION_PIXEL_10_SPEC).unwrap();
        let cjk = '\u{4E00}';
        let ink = glyph_ink(&font, cjk);
        assert!(!ink.is_empty(), "fallback glyph rendered no ink");
        assert_ne!(
            ink,
            glyph_ink(&font, '?'),
            "fell back to '?' instead of font"
        );
        assert!(font.advance(cjk) > 0, "fallback glyph reported no advance");
    }

    #[test]
    fn uncovered_codepoint_falls_back_to_question_mark() {
        let font = font();
        // A codepoint past every atlas block falls back to '?' for both ink
        // and advance instead of panicking on the advance table bounds.
        let uncovered = '\u{4E00}';
        assert_eq!(glyph_ink(&font, uncovered), glyph_ink(&font, '?'));
        assert_eq!(font.advance(uncovered), font.advance('?'));
    }
}
