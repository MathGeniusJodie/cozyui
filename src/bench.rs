//! Timing harness for the raster hot paths. Not run by default:
//!
//!     cargo test --release bench_ -- --ignored --nocapture
//!
//! Each bench prints the mean time per iteration; compare before/after when
//! touching Framebuffer, sprite drawing, or widget layout code.

use std::time::Instant;

use crate::{Framebuffer, Palette};

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
    crate::assets::palette()
}

#[test]
#[ignore]
fn bench_fill_from_sprite() {
    let palette = palette();
    let desk = crate::assets::desk();
    let mut fb = Framebuffer::new(desk.width, desk.height, 0);
    time("fill_from_sprite", 200, || {
        fb.fill_from_sprite(&desk, &palette);
    });
}

#[test]
#[ignore]
fn bench_draw_sprite() {
    let palette = palette();
    let desk = crate::assets::desk();
    let mut fb = Framebuffer::new(desk.width, desk.height, 0);
    time("draw_sprite", 200, || {
        fb.draw_sprite(&desk, 0, 0, &palette);
    });
}

#[test]
#[ignore]
fn bench_fill_rect() {
    let mut fb = Framebuffer::new(800, 600, 0);
    time("fill_rect 800x600", 500, || {
        fb.fill_rect(0, 0, 800, 600, 3);
    });
}

#[test]
#[ignore]
fn bench_desk_background() {
    let palette = palette();
    let desk = crate::assets::desk();
    let mut fb = Framebuffer::new(1100, 800, 0);
    let full = crate::Rect::new(0, 0, fb.width, fb.height);
    time("draw_stretched_desk_region (full window)", 100, || {
        crate::draw_stretched_desk_region(&mut fb, &desk, &palette, full);
    });
}
