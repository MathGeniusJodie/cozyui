use std::error::Error;
use std::fs::File;
use std::io::BufReader;

/// A palette index stored in sprite pixels. `TRANSPARENT` is the only
/// non-palette value; palettes never contain transparent colors.
pub type Index = u8;
pub const TRANSPARENT: Index = 0xFF;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
}

impl Rgb {
    pub(crate) const fn transparent(self) -> Rgba {
        Rgba {
            r: self.r,
            g: self.g,
            b: self.b,
            a: 0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Rgba {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

impl From<Rgb> for Rgba {
    fn from(color: Rgb) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
            a: 255,
        }
    }
}

impl Rgba {
    const fn rgb(self) -> Rgb {
        Rgb {
            r: self.r,
            g: self.g,
            b: self.b,
        }
    }
}

/// What a palette slot resolves to when painted. Checkerboards are valid
/// anywhere a solid color is; their phase is anchored to destination
/// coordinates in fat-pixel units so overlapping dithers mesh.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum Paint {
    Solid(Index),
    Checker(Index, Index),
    Transparent,
}

impl Paint {
    const fn pick(self, cell_x: usize, cell_y: usize) -> Option<Index> {
        match self {
            Self::Solid(index) => Some(index),
            Self::Checker(even, odd) => Some(if (cell_x + cell_y).is_multiple_of(2) {
                even
            } else {
                odd
            }),
            Self::Transparent => None,
        }
    }
}

/// Per-draw index remap. Indices without an explicit entry pass through.
pub struct Swap {
    paints: Vec<Paint>,
    uniform: Option<Paint>,
}

impl Swap {
    pub(crate) const fn identity() -> Self {
        Self {
            paints: Vec::new(),
            uniform: None,
        }
    }

    /// Every opaque pixel becomes `paint` (silhouettes, shadows, tints).
    pub(crate) const fn uniform(paint: Paint) -> Self {
        Self {
            paints: Vec::new(),
            uniform: Some(paint),
        }
    }

    pub(crate) fn from_indices(indices: &[Index]) -> Self {
        Self {
            paints: indices.iter().map(|&index| Paint::Solid(index)).collect(),
            uniform: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set(mut self, index: Index, paint: Paint) -> Self {
        let slot = index as usize;
        if self.paints.len() <= slot {
            let len = self.paints.len() as Index;
            self.paints.extend((len..=index).map(Paint::Solid));
        }
        self.paints[slot] = paint;
        self
    }

    fn paint(&self, index: Index) -> Paint {
        if index == TRANSPARENT {
            return Paint::Transparent;
        }
        if let Some(uniform) = self.uniform {
            return uniform;
        }
        self.paints
            .get(index as usize)
            .copied()
            .unwrap_or(Paint::Solid(index))
    }
}

pub struct Palette {
    colors: Vec<Rgb>,
    remap: Vec<Paint>,
}

impl Palette {
    pub(crate) fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let colors = decode_png(path)?
            .into_iter()
            .map(Rgba::rgb)
            .collect::<Vec<_>>();
        if colors.is_empty() {
            return Err(format!("palette PNG has no colors: {path}").into());
        }
        Ok(Self::from_colors(colors))
    }

    pub(crate) fn from_colors(colors: Vec<Rgb>) -> Self {
        let remap = (0..colors.len())
            .map(|index| Paint::Solid(index as Index))
            .collect();
        Self { colors, remap }
    }

    #[allow(dead_code)]
    pub(crate) const fn len(&self) -> usize {
        self.colors.len()
    }

    pub(crate) fn color(&self, index: Index) -> Rgb {
        self.colors[Self::wrap(index as usize, self.colors.len())]
    }

    /// Like `index % len` but branch-only for in-range indices; `%` is an
    /// integer division and this runs per pixel in the raster loops.
    const fn wrap(index: usize, len: usize) -> usize {
        if index < len { index } else { index % len }
    }

    /// Global theme layer: every slot can become another color or checker.
    /// Applied when sprites and paints are resolved at draw time.
    #[allow(dead_code)]
    pub(crate) fn set_remap(&mut self, index: Index, paint: Paint) {
        let slot = index as usize % self.colors.len();
        self.remap[slot] = paint;
    }

    /// Resolve an index through the global remap at destination cell
    /// coordinates (fat-pixel units, screen-anchored checker phase).
    pub(crate) fn resolve(&self, index: Index, cell_x: usize, cell_y: usize) -> Option<Rgb> {
        if index == TRANSPARENT {
            return None;
        }
        let paint = self.remap[Self::wrap(index as usize, self.colors.len())];
        paint.pick(cell_x, cell_y).map(|index| self.color(index))
    }

    /// Resolve a per-draw paint, then the global remap.
    pub(crate) fn resolve_paint(&self, paint: Paint, cell_x: usize, cell_y: usize) -> Option<Rgb> {
        let index = paint.pick(cell_x, cell_y)?;
        self.resolve(index, cell_x, cell_y)
    }

    pub(crate) fn nearest_index(&self, color: Rgb) -> Index {
        self.colors
            .iter()
            .enumerate()
            .min_by_key(|(_, candidate)| color_distance(**candidate, color))
            .map_or(0, |(index, _)| index as Index)
    }

    pub(crate) fn nearest(&self, color: Rgb) -> Rgb {
        self.color(self.nearest_index(color))
    }

    pub(crate) fn exact_index(&self, color: Rgb) -> Option<Index> {
        self.colors
            .iter()
            .position(|candidate| *candidate == color)
            .map(|index| index as Index)
    }

    pub(crate) fn closest_to_white(&self) -> Rgb {
        self.nearest(Rgb {
            r: 255,
            g: 255,
            b: 255,
        })
    }

    /// index-in-self -> nearest index-in-other; basis for cross-palette
    /// sprite import and palette migration.
    pub(crate) fn mapping_to(&self, other: &Self) -> Vec<Index> {
        self.colors
            .iter()
            .map(|&color| other.nearest_index(color))
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) w: usize,
    pub(crate) h: usize,
}

impl Rect {
    pub(crate) const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    pub(crate) fn contains(self, x: i16, y: i16) -> bool {
        let x = x.max(0) as usize;
        let y = y.max(0) as usize;
        self.contains_point(x, y)
    }

    pub(crate) const fn contains_point(self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub(crate) const fn local(self, x: i16, y: i16) -> (i16, i16) {
        (x - self.x as i16, y - self.y as i16)
    }
}

/// Indexed pixel art: one palette index per pixel, `TRANSPARENT` for holes.
pub struct Sprite {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pixels: Vec<Index>,
}

impl Sprite {
    /// Decode a PNG whose colors are interpreted via `source`, storing
    /// indices in `target`'s space.
    pub(crate) fn load(
        path: &str,
        source: &Palette,
        target: &Palette,
    ) -> Result<Self, Box<dyn Error>> {
        let (width, height, pixels) = decode_png_with_size(path)?;
        if pixels.len() != width * height {
            return Err(format!(
                "PNG pixel count mismatch for {path}: got {}, expected {}",
                pixels.len(),
                width * height
            )
            .into());
        }
        let lut = source.mapping_to(target);
        let pixels = pixels
            .into_iter()
            .map(|color| {
                if color.a == 0 {
                    return TRANSPARENT;
                }
                let source_index = source
                    .exact_index(color.rgb())
                    .unwrap_or_else(|| source.nearest_index(color.rgb()));
                lut[source_index as usize]
            })
            .collect();
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub(crate) fn load_native(path: &str, palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Self::load(path, palette, palette)
    }

    pub(crate) fn at(&self, x: usize, y: usize) -> Index {
        self.pixels[y * self.width + x]
    }

    pub(crate) fn is_opaque(&self, x: usize, y: usize) -> bool {
        self.at(x, y) != TRANSPARENT
    }

    #[allow(dead_code)]
    pub(crate) fn region(&self, src: Rect) -> Self {
        let w = src.w.min(self.width.saturating_sub(src.x));
        let h = src.h.min(self.height.saturating_sub(src.y));
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                pixels.push(self.at(src.x + x, src.y + y));
            }
        }
        Self {
            width: w,
            height: h,
            pixels,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn flip_h(&self) -> Self {
        self.map_coords(|x, y| (self.width - 1 - x, y), self.width, self.height)
    }

    #[allow(dead_code)]
    pub(crate) fn flip_v(&self) -> Self {
        self.map_coords(|x, y| (x, self.height - 1 - y), self.width, self.height)
    }

    /// Rotate 90 degrees clockwise.
    #[allow(dead_code)]
    pub(crate) fn rot90(&self) -> Self {
        self.map_coords(|x, y| (y, self.height - 1 - x), self.height, self.width)
    }

    fn map_coords(
        &self,
        source: impl Fn(usize, usize) -> (usize, usize),
        width: usize,
        height: usize,
    ) -> Self {
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let (sx, sy) = source(x, y);
                pixels.push(self.at(sx, sy));
            }
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Keep pixels only where `mask` is opaque.
    #[allow(dead_code)]
    pub(crate) fn mask(&self, mask: &Self) -> Self {
        let mut pixels = self.pixels.clone();
        for y in 0..self.height {
            for x in 0..self.width {
                let masked = x >= mask.width || y >= mask.height || !mask.is_opaque(x, y);
                if masked {
                    pixels[y * self.width + x] = TRANSPARENT;
                }
            }
        }
        Self {
            width: self.width,
            height: self.height,
            pixels,
        }
    }

    /// Remap every pixel index from one palette's space to another's.
    #[allow(dead_code)]
    pub(crate) fn convert(&self, from: &Palette, to: &Palette) -> Self {
        let lut = from.mapping_to(to);
        let pixels = self
            .pixels
            .iter()
            .map(|&index| {
                if index == TRANSPARENT {
                    TRANSPARENT
                } else {
                    lut[index as usize % lut.len()]
                }
            })
            .collect();
        Self {
            width: self.width,
            height: self.height,
            pixels,
        }
    }
}

pub struct Framebuffer {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pixels: Vec<u8>,
}

impl Framebuffer {
    pub(crate) const BYTES_PER_PIXEL: usize = 4;
    const ALPHA_OFFSET: usize = 3;

    pub(crate) fn new(width: usize, height: usize, fill: impl Into<Rgba>) -> Self {
        let mut fb = Self {
            width,
            height,
            pixels: vec![0; width * height * Self::BYTES_PER_PIXEL],
        };
        fb.clear(fill);
        fb
    }

    const fn color_bytes(color: Rgba) -> [u8; Self::BYTES_PER_PIXEL] {
        [color.b, color.g, color.r, color.a]
    }

    const fn pixel_offset(&self, x: usize, y: usize) -> usize {
        (y * self.width + x) * Self::BYTES_PER_PIXEL
    }

    pub(crate) fn row_bytes(&self, y: usize, x: usize, width: usize) -> &[u8] {
        let start = self.pixel_offset(x, y);
        let end = start + width * Self::BYTES_PER_PIXEL;
        &self.pixels[start..end]
    }

    fn row_bytes_mut(&mut self, y: usize, x: usize, width: usize) -> &mut [u8] {
        let start = self.pixel_offset(x, y);
        let end = start + width * Self::BYTES_PER_PIXEL;
        &mut self.pixels[start..end]
    }

    pub(crate) fn ximage_bytes(&self) -> &[u8] {
        &self.pixels
    }

    pub(crate) fn clear(&mut self, color: impl Into<Rgba>) {
        let color = Self::color_bytes(color.into());
        fill_pattern(&mut self.pixels, &color);
    }

    /// Fill the whole framebuffer with `sprite` scaled up, repeating edge
    /// pixels past the sprite's extent. Transparent pixels keep the
    /// framebuffer's existing content (e.g. a widget's see-through corners).
    pub(crate) fn clear_scaled(&mut self, sprite: &Sprite, scale: usize, palette: &Palette) {
        // Resolve each source row once into a byte row, write it, and copy it
        // to the remaining rows of the scaled band; rows with transparency
        // fall back to per-pixel writes since they can't be blanket-copied.
        let mut row = vec![0u8; self.width * Self::BYTES_PER_PIXEL];
        let sx_map: Vec<usize> = (0..self.width)
            .map(|x| (x / scale).min(sprite.width - 1))
            .collect();
        for band in 0..self.height.div_ceil(scale) {
            let sy = band.min(sprite.height - 1);
            let mut opaque_row = true;
            for (x, &sx) in sx_map.iter().enumerate().take(self.width) {
                match palette.resolve(sprite.at(sx, sy), sx, sy) {
                    Some(color) => {
                        let offset = x * Self::BYTES_PER_PIXEL;
                        row[offset..offset + Self::BYTES_PER_PIXEL]
                            .copy_from_slice(&Self::color_bytes(color.into()));
                    }
                    None => opaque_row = false,
                }
            }
            let y0 = band * scale;
            let band_end = (y0 + scale).min(self.height);
            if opaque_row {
                self.row_bytes_mut(y0, 0, self.width).copy_from_slice(&row);
                let first_row = self.pixel_offset(0, y0);
                let row_len = self.width * Self::BYTES_PER_PIXEL;
                for y in y0 + 1..band_end {
                    let dest = self.pixel_offset(0, y);
                    self.pixels
                        .copy_within(first_row..first_row + row_len, dest);
                }
            } else {
                for y in y0..band_end {
                    for (x, &sx) in sx_map.iter().enumerate().take(self.width) {
                        if let Some(color) = palette.resolve(sprite.at(sx, sy), sx, sy) {
                            self.set_pixel(x, y, color);
                        }
                    }
                }
            }
        }
    }

    /// Write a single pixel (bounds-checked). Cheaper than a 1x1 `fill_rect` in
    /// per-pixel loops.
    pub(crate) fn set_pixel(&mut self, x: usize, y: usize, color: impl Into<Rgba>) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = self.pixel_offset(x, y);
        self.pixels[offset..offset + Self::BYTES_PER_PIXEL]
            .copy_from_slice(&Self::color_bytes(color.into()));
    }

    pub(crate) fn fill_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        color: impl Into<Rgba>,
    ) {
        if x >= self.width || y >= self.height {
            return;
        }

        let width = w.min(self.width - x);
        let height = h.min(self.height - y);
        let color = Self::color_bytes(color.into());
        fill_pattern(self.row_bytes_mut(y, x, width), &color);
        let row_len = width * Self::BYTES_PER_PIXEL;
        let first_row = self.pixel_offset(x, y);
        for py in y + 1..y + height {
            let dest = self.pixel_offset(x, py);
            self.pixels
                .copy_within(first_row..first_row + row_len, dest);
        }
    }

    pub(crate) fn draw_sprite(
        &mut self,
        sprite: &Sprite,
        dest_x: isize,
        dest_y: isize,
        scale: usize,
        palette: &Palette,
    ) {
        self.draw_sprite_full(
            sprite,
            Rect::new(0, 0, sprite.width, sprite.height),
            dest_x,
            dest_y,
            scale,
            None,
            palette,
            None,
        );
    }

    pub(crate) fn draw_sprite_swapped(
        &mut self,
        sprite: &Sprite,
        dest_x: isize,
        dest_y: isize,
        scale: usize,
        palette: &Palette,
        swap: &Swap,
    ) {
        self.draw_sprite_full(
            sprite,
            Rect::new(0, 0, sprite.width, sprite.height),
            dest_x,
            dest_y,
            scale,
            None,
            palette,
            Some(swap),
        );
    }

    /// Every opaque pixel painted as `paint`: shadows, silhouettes.
    pub(crate) fn draw_sprite_silhouette(
        &mut self,
        sprite: &Sprite,
        dest_x: isize,
        dest_y: isize,
        scale: usize,
        palette: &Palette,
        paint: Paint,
    ) {
        self.draw_sprite_swapped(
            sprite,
            dest_x,
            dest_y,
            scale,
            palette,
            &Swap::uniform(paint),
        );
    }

    pub(crate) fn draw_sprite_region(
        &mut self,
        sprite: &Sprite,
        src: Rect,
        dest_x: isize,
        dest_y: isize,
        scale: usize,
        palette: &Palette,
    ) {
        self.draw_sprite_full(sprite, src, dest_x, dest_y, scale, None, palette, None);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_sprite_full(
        &mut self,
        sprite: &Sprite,
        src: Rect,
        dest_x: isize,
        dest_y: isize,
        scale: usize,
        clip: Option<Rect>,
        palette: &Palette,
        swap: Option<&Swap>,
    ) {
        let width = src.w.min(sprite.width.saturating_sub(src.x));
        let height = src.h.min(sprite.height.saturating_sub(src.y));

        // Unscaled, unclipped draws write rows directly instead of going through
        // fill_rect per pixel — the common case for full-size sprite blits. A
        // per-draw swap is fine here; it only changes how each index resolves,
        // not the row-at-a-time write pattern.
        if scale == 1 && clip.is_none() && dest_x >= 0 && dest_y >= 0 {
            let dest_x = dest_x as usize;
            let dest_y = dest_y as usize;
            if dest_x >= self.width || dest_y >= self.height {
                return;
            }
            let copy_w = width.min(self.width - dest_x);
            let copy_h = height.min(self.height - dest_y);
            for y in 0..copy_h {
                let row = self.row_bytes_mut(dest_y + y, dest_x, copy_w);
                for x in 0..copy_w {
                    let index = sprite.at(src.x + x, src.y + y);
                    let color = swap.map_or_else(
                        || palette.resolve(index, dest_x + x, dest_y + y),
                        |swap| palette.resolve_paint(swap.paint(index), dest_x + x, dest_y + y),
                    );
                    let Some(color) = color else {
                        continue;
                    };
                    let offset = x * Self::BYTES_PER_PIXEL;
                    row[offset..offset + Self::BYTES_PER_PIXEL]
                        .copy_from_slice(&Self::color_bytes(color.into()));
                }
            }
            return;
        }

        for y in 0..height {
            for x in 0..width {
                let index = sprite.at(src.x + x, src.y + y);
                let dx = dest_x + (x * scale) as isize;
                let dy = dest_y + (y * scale) as isize;
                if dx < 0 || dy < 0 {
                    continue;
                }

                let dx = dx as usize;
                let dy = dy as usize;
                if clip.is_some_and(|clip| !clip.contains_point(dx, dy)) {
                    continue;
                }

                let cell_x = dx / scale.max(1);
                let cell_y = dy / scale.max(1);
                let color = swap.map_or_else(
                    || palette.resolve(index, cell_x, cell_y),
                    |swap| palette.resolve_paint(swap.paint(index), cell_x, cell_y),
                );
                let Some(color) = color else {
                    continue;
                };
                self.fill_rect(dx, dy, scale, scale, color);
            }
        }
    }

    pub(crate) fn blit_from(&mut self, src: &Self, dest_x: usize, dest_y: usize) {
        if dest_x >= self.width || dest_y >= self.height {
            return;
        }

        let copy_width = src.width.min(self.width - dest_x);
        let copy_height = src.height.min(self.height - dest_y);
        for y in 0..copy_height {
            let src_row = src.row_bytes(y, 0, copy_width);
            let dst_row = self.row_bytes_mut(dest_y + y, dest_x, copy_width);
            for (src_pixel, dst_pixel) in src_row
                .chunks_exact(Self::BYTES_PER_PIXEL)
                .zip(dst_row.chunks_exact_mut(Self::BYTES_PER_PIXEL))
            {
                if src_pixel[Self::ALPHA_OFFSET] != 0 {
                    dst_pixel.copy_from_slice(src_pixel);
                }
            }
        }
    }
}

/// Fill `bytes` with a repeating 4-byte pattern by doubling copies: O(log n)
/// `copy_within` calls instead of one slice write per pixel.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn fill_pattern(bytes: &mut [u8], pattern: &[u8; Framebuffer::BYTES_PER_PIXEL]) {
    if bytes.is_empty() {
        return;
    }
    let len = bytes.len().min(pattern.len());
    bytes[..len].copy_from_slice(&pattern[..len]);
    let mut filled = len;
    while filled < bytes.len() {
        let copy = filled.min(bytes.len() - filled);
        bytes.copy_within(..copy, filled);
        filled += copy;
    }
}

fn decode_png(path: &str) -> Result<Vec<Rgba>, Box<dyn Error>> {
    Ok(decode_png_with_size(path)?.2)
}

pub fn decode_png_with_size(path: &str) -> Result<(usize, usize, Vec<Rgba>), Box<dyn Error>> {
    let file = File::open(path)?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut data = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data)?;
    let bytes = &data[..info.buffer_size()];

    let mut pixels = Vec::with_capacity((info.width * info.height) as usize);
    match info.color_type {
        png::ColorType::Rgb => {
            for chunk in bytes.chunks_exact(3) {
                pixels.push(Rgba {
                    r: chunk[0],
                    g: chunk[1],
                    b: chunk[2],
                    a: 255,
                });
            }
        }
        png::ColorType::Rgba => {
            for chunk in bytes.chunks_exact(4) {
                pixels.push(Rgba {
                    r: chunk[0],
                    g: chunk[1],
                    b: chunk[2],
                    a: chunk[3],
                });
            }
        }
        png::ColorType::Indexed => {
            let palette = reader
                .info()
                .palette
                .as_ref()
                .ok_or("indexed PNG has no palette")?;
            let trns = reader.info().trns.as_deref().unwrap_or(&[]);
            for &idx in bytes {
                let base = idx as usize * 3;
                if base + 2 >= palette.len() {
                    return Err(
                        format!("indexed PNG palette index {idx} out of bounds in {path}").into(),
                    );
                }
                let a = trns.get(idx as usize).copied().unwrap_or(255);
                pixels.push(Rgba {
                    r: palette[base],
                    g: palette[base + 1],
                    b: palette[base + 2],
                    a,
                });
            }
        }
        other => return Err(format!("unsupported PNG color type: {other:?}").into()),
    }

    Ok((info.width as usize, info.height as usize, pixels))
}

const fn color_distance(a: Rgb, b: Rgb) -> u32 {
    let dr = a.r as i32 - b.r as i32;
    let dg = a.g as i32 - b.g as i32;
    let db = a.b as i32 - b.b as i32;
    (dr * dr + dg * dg + db * db) as u32
}
