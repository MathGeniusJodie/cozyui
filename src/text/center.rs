//! Centered-text drawing shared by widgets that lay out a title or stat
//! value within a fixed-width band (a whole canvas, a card, a bar slot...).

use pixel_fonts::BitmapFont;
use pixel_graphics::{Framebuffer, Index};

/// Draws `text` horizontally centered (by `font.text_width`, the nominal
/// advance width) within `[left, left + width)`, at baseline `y`.
pub fn draw_text_centered(
    fb: &mut Framebuffer,
    font: &BitmapFont,
    text: &str,
    left: isize,
    width: usize,
    y: isize,
    color: Index,
) {
    let x = left + (width.saturating_sub(font.text_width(text)) / 2) as isize;
    font.draw_text(fb, text, x, y, color);
}

/// Draws `text` centered on both axes using its actual glyph ink bounds
/// rather than the font's nominal cell/advance metrics, so short or
/// tall-looking glyph runs don't look off-center against the surrounding
/// layout. `y` is the top of the ink (not the top of the much taller cell
/// box); does nothing if `text` has no ink (e.g. all spaces).
pub fn draw_text_centered_tight(
    fb: &mut Framebuffer,
    font: &BitmapFont,
    text: &str,
    left: isize,
    width: usize,
    y: isize,
    color: Index,
) {
    let Some(bounds) = font.text_ink_bounds(text) else {
        return;
    };
    let center = left + (width.saturating_sub(bounds.width()) / 2) as isize;
    let x = (center - bounds.min_x).max(0);
    let draw_y = (y - bounds.min_y as isize).max(0);
    font.draw_text(fb, text, x, draw_y, color);
}
