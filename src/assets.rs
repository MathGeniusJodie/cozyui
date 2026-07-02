//! Art baked into the binary at build time (see build.rs): palette-indexed
//! sprites, the na16 palette, and the terminal glyph ink mask. No PNG files
//! or decoding needed at runtime.

use crate::{Palette, Rgb, Sprite};

include!(concat!(env!("OUT_DIR"), "/baked_assets.rs"));

pub(crate) fn palette() -> Palette {
    Palette::from_colors(
        PALETTE_BYTES
            .chunks_exact(3)
            .map(|c| Rgb {
                r: c[0],
                g: c[1],
                b: c[2],
            })
            .collect(),
    )
}
