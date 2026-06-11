//! Timing harness for the raster hot paths. Not run by default:
//!
//!     cargo test --release bench_ -- --ignored --nocapture
//!
//! Each bench prints the mean time per iteration; compare before/after when
//! touching Framebuffer, sprite drawing, or widget layout code.
#![cfg(test)]

use std::time::Instant;

use crate::{Framebuffer, Palette, Sprite};

const DESK_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/desk.png");
const PALETTE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/na16-1x.png");

fn time<F: FnMut()>(label: &str, iterations: u32, mut work: F) {
    // Warm up caches and lazy state.
    work();
    let start = Instant::now();
    for _ in 0..iterations {
        work();
    }
    let per_iter = start.elapsed() / iterations;
    println!("{label}: {per_iter:?} per iteration ({iterations} iterations)");
}

fn palette() -> Palette {
    Palette::load(PALETTE_PATH).unwrap()
}

#[test]
#[ignore]
fn bench_clear_scaled() {
    let palette = palette();
    let desk = Sprite::load_native(DESK_PATH, &palette).unwrap();
    let mut fb = Framebuffer::new(desk.width, desk.height, palette.color(0));
    time("clear_scaled (1x)", 200, || {
        fb.clear_scaled(&desk, 1, &palette);
    });

    let mut fb3 = Framebuffer::new(desk.width * 3, desk.height * 3, palette.color(0));
    time("clear_scaled (3x)", 50, || {
        fb3.clear_scaled(&desk, 3, &palette);
    });
}

#[test]
#[ignore]
fn bench_draw_sprite() {
    let palette = palette();
    let desk = Sprite::load_native(DESK_PATH, &palette).unwrap();
    let mut fb = Framebuffer::new(desk.width, desk.height, palette.color(0));
    time("draw_sprite (1x)", 200, || {
        fb.draw_sprite(&desk, 0, 0, 1, &palette);
    });
}

#[test]
#[ignore]
fn bench_fill_rect() {
    let palette = palette();
    let mut fb = Framebuffer::new(800, 600, palette.color(0));
    time("fill_rect 800x600", 500, || {
        fb.fill_rect(0, 0, 800, 600, palette.color(3));
    });
}

#[test]
#[ignore]
fn bench_desk_background() {
    let palette = palette();
    let desk = Sprite::load_native(DESK_PATH, &palette).unwrap();
    let mut fb = Framebuffer::new(1100, 800, palette.color(0));
    let full = crate::Rect::new(0, 0, fb.width, fb.height);
    time("draw_stretched_desk_region (full window)", 100, || {
        crate::draw_stretched_desk_region(&mut fb, &desk, &palette, full);
    });
}
