//! Bakes all PNG art into palette-indexed binaries at build time, so the
//! shipped binary needs no asset files on disk and no PNG decoding for its
//! own art (pixel-fonts still decodes its font atlases at runtime).
//!
//! Outputs in OUT_DIR:
//! - `palette.bin`: RGB triples of the na16 palette
//! - `<name>.bin`: row-major palette indices per sprite
//! - `glyph_atlas.bin`: 0/1 ink mask for the terminal glyph atlas
//! - `baked_assets.rs`: accessors, included by `src/assets.rs`

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use pixel_graphics::{Palette, Rgba, Sprite};

const PALETTE_PNG: &str = "na16-1x.png";

/// Sprites living outside `assets/`.
const ROOT_SPRITES: &[&str] = &[
    "desk.png",
    "wavey.png",
    "puter_g_lc.png",
    "puter_o_lc.png",
    "puter_hc.png",
];

const GLYPH_ATLAS_PNG: &str = "glyphs/0000-007F.png";
// Must match GLYPH_W/GLYPH_H in src/puter.rs.
const GLYPH_W: usize = 6;
const GLYPH_H: usize = 12;

fn main() {
    println!("cargo::rerun-if-changed=assets");
    println!("cargo::rerun-if-changed={PALETTE_PNG}");
    println!("cargo::rerun-if-changed={GLYPH_ATLAS_PNG}");
    for path in ROOT_SPRITES {
        println!("cargo::rerun-if-changed={path}");
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let palette = Palette::load(PALETTE_PNG).unwrap();
    let mut out = String::new();

    let palette_bytes: Vec<u8> = (0..palette.len())
        .flat_map(|i| {
            let c = palette.color(i as u8);
            [c.r, c.g, c.b]
        })
        .collect();
    std::fs::write(out_dir.join("palette.bin"), &palette_bytes).unwrap();
    out.push_str(
        "pub(crate) static PALETTE_BYTES: &[u8] = \
         include_bytes!(concat!(env!(\"OUT_DIR\"), \"/palette.bin\"));\n",
    );

    let mut sprite_paths: Vec<PathBuf> = ROOT_SPRITES.iter().map(PathBuf::from).collect();
    for entry in std::fs::read_dir("assets").unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "png") {
            sprite_paths.push(path);
        }
    }
    for path in sprite_paths {
        bake_sprite(&path, &palette, &out_dir, &mut out);
    }

    bake_glyph_atlas(&out_dir, &mut out);

    std::fs::write(out_dir.join("baked_assets.rs"), out).unwrap();
}

fn bake_sprite(path: &Path, palette: &Palette, out_dir: &Path, out: &mut String) {
    let sprite = Sprite::load_native(path.to_str().unwrap(), palette).unwrap();
    let name = path
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .replace('-', "_");

    let sprite = &sprite;
    let pixels: Vec<u8> = (0..sprite.height)
        .flat_map(|y| (0..sprite.width).map(move |x| sprite.at(x, y)))
        .collect();
    std::fs::write(out_dir.join(format!("{name}.bin")), &pixels).unwrap();

    write!(
        out,
        "#[allow(dead_code)]\n\
         pub(crate) fn {name}() -> Sprite {{ Sprite::from_indices({w}, {h}, \
         include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{name}.bin\")).to_vec()) }}\n",
        w = sprite.width,
        h = sprite.height,
    )
    .unwrap();
}

/// Bake the terminal glyph atlas to a 0/1 ink mask (the runtime side never
/// needs the colors, only whether each pixel is ink). Mirrors the size checks
/// and `is_glyph_ink` threshold previously done at runtime in puter.rs.
fn bake_glyph_atlas(out_dir: &Path, out: &mut String) {
    let (width, height, pixels) = pixel_graphics::decode_png_with_size(GLYPH_ATLAS_PNG).unwrap();
    assert!(
        width >= GLYPH_W,
        "glyph atlas {GLYPH_ATLAS_PNG} is too narrow for terminal glyphs"
    );
    let rows = 128_usize.div_ceil(width / GLYPH_W);
    assert!(
        height >= rows * GLYPH_H,
        "glyph atlas {GLYPH_ATLAS_PNG} is too small for 128 terminal glyphs"
    );

    let mask: Vec<u8> = pixels.into_iter().map(|c| is_glyph_ink(c) as u8).collect();
    std::fs::write(out_dir.join("glyph_atlas.bin"), &mask).unwrap();
    write!(
        out,
        "pub(crate) const GLYPH_ATLAS_WIDTH: usize = {width};\n\
         pub(crate) static GLYPH_ATLAS_MASK: &[u8] = \
         include_bytes!(concat!(env!(\"OUT_DIR\"), \"/glyph_atlas.bin\"));\n",
    )
    .unwrap();
}

const fn is_glyph_ink(color: Rgba) -> bool {
    let luminance = color.r as u16 + color.g as u16 + color.b as u16;
    luminance >= 384
}
