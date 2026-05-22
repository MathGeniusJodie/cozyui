use std::error::Error;
use std::fs::File;
use std::io::BufReader;

#[derive(Clone, Copy)]
pub(crate) struct Rgba {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) w: usize,
    pub(crate) h: usize,
}

impl Rect {
    pub(crate) fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    pub(crate) fn contains(self, x: i16, y: i16) -> bool {
        let x = x.max(0) as usize;
        let y = y.max(0) as usize;
        self.contains_point(x, y)
    }

    pub(crate) fn contains_point(self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub(crate) fn local(self, x: i16, y: i16) -> (i16, i16) {
        (x - self.x as i16, y - self.y as i16)
    }
}

pub(crate) struct Palette {
    colors: Vec<Rgba>,
}

impl Palette {
    pub(crate) fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let pixels = decode_png(path)?;
        let colors = pixels
            .into_iter()
            .map(|mut color| {
                color.a = 255;
                color
            })
            .collect::<Vec<_>>();

        if colors.is_empty() {
            return Err(format!("palette PNG has no colors: {path}").into());
        }

        Ok(Self { colors })
    }

    pub(crate) fn color(&self, index: usize) -> Rgba {
        self.colors[index % self.colors.len()]
    }

    pub(crate) fn nearest(&self, color: Rgba) -> Rgba {
        self.colors
            .iter()
            .copied()
            .min_by_key(|candidate| color_distance(*candidate, color))
            .unwrap_or(self.colors[0])
    }

    pub(crate) fn closest_to_white(&self) -> Rgba {
        self.nearest(Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        })
    }

    pub(crate) fn darkest(&self) -> Rgba {
        self.colors
            .iter()
            .copied()
            .min_by_key(|color| color.r as u16 + color.g as u16 + color.b as u16)
            .unwrap_or(self.colors[0])
    }
}

pub(crate) struct Image {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pixels: Vec<Rgba>,
}

impl Image {
    pub(crate) fn load(path: &str, palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let (width, height, pixels) = decode_png_with_size(path)?;
        if pixels.len() != width * height {
            return Err(format!(
                "PNG pixel count mismatch for {path}: got {}, expected {}",
                pixels.len(),
                width * height
            )
            .into());
        }
        let pixels = pixels
            .into_iter()
            .map(|color| {
                if color.a == 0 {
                    let mut transparent = palette.darkest();
                    transparent.a = 0;
                    transparent
                } else {
                    palette.nearest(color)
                }
            })
            .collect();
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub(crate) fn at(&self, x: usize, y: usize) -> Rgba {
        self.pixels[y * self.width + x]
    }
}

pub(crate) struct Framebuffer {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pixels: Vec<u8>,
}

impl Framebuffer {
    pub(crate) const BYTES_PER_PIXEL: usize = 4;

    pub(crate) fn new(width: usize, height: usize, fill: Rgba) -> Self {
        Self::new_filled(width, height, fill)
    }

    fn color_bytes(color: Rgba) -> [u8; Self::BYTES_PER_PIXEL] {
        [color.b, color.g, color.r, 0]
    }

    fn pixel_offset(&self, x: usize, y: usize) -> usize {
        (y * self.width + x) * Self::BYTES_PER_PIXEL
    }

    fn set_pixel(&mut self, x: usize, y: usize, color: Rgba) {
        let offset = self.pixel_offset(x, y);
        self.pixels[offset..offset + Self::BYTES_PER_PIXEL]
            .copy_from_slice(&Self::color_bytes(color));
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

    fn filled_bytes(width: usize, height: usize, fill: Rgba) -> Vec<u8> {
        let mut pixels = vec![0; width * height * Self::BYTES_PER_PIXEL];
        let color = Self::color_bytes(fill);
        for pixel in pixels.chunks_exact_mut(Self::BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&color);
        }
        pixels
    }

    fn new_filled(width: usize, height: usize, fill: Rgba) -> Self {
        Self {
            width,
            height,
            pixels: Self::filled_bytes(width, height, fill),
        }
    }

    pub(crate) fn clear(&mut self, color: Rgba) {
        let color = Self::color_bytes(color);
        for pixel in self.pixels.chunks_exact_mut(Self::BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&color);
        }
    }

    pub(crate) fn clear_scaled(&mut self, image: &Image, scale: usize) {
        for y in 0..self.height {
            for x in 0..self.width {
                let sx = (x / scale).min(image.width - 1);
                let sy = (y / scale).min(image.height - 1);
                self.set_pixel(x, y, image.at(sx, sy));
            }
        }
    }

    pub(crate) fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Rgba) {
        if x >= self.width || y >= self.height {
            return;
        }

        let width = w.min(self.width - x);
        let height = h.min(self.height - y);
        let color = Self::color_bytes(color);
        for py in y..y + height {
            for pixel in self
                .row_bytes_mut(py, x, width)
                .chunks_exact_mut(Self::BYTES_PER_PIXEL)
            {
                pixel.copy_from_slice(&color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_scaled_region(
        &mut self,
        image: &Image,
        src_x: usize,
        src_y: usize,
        dest_x: usize,
        dest_y: usize,
        width: usize,
        height: usize,
        scale: usize,
    ) {
        self.draw_image_region(
            image,
            Rect::new(src_x, src_y, width, height),
            dest_x as isize,
            dest_y as isize,
            scale,
        );
    }

    pub(crate) fn draw_image(&mut self, image: &Image, dest_x: isize, dest_y: isize, scale: usize) {
        self.draw_image_region(
            image,
            Rect::new(0, 0, image.width, image.height),
            dest_x,
            dest_y,
            scale,
        );
    }

    pub(crate) fn draw_image_region(
        &mut self,
        image: &Image,
        src: Rect,
        dest_x: isize,
        dest_y: isize,
        scale: usize,
    ) {
        self.draw_image_region_mapped(image, src, dest_x, dest_y, scale, None, Some)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_image_region_mapped<F>(
        &mut self,
        image: &Image,
        src: Rect,
        dest_x: isize,
        dest_y: isize,
        scale: usize,
        clip: Option<Rect>,
        mut map_color: F,
    ) where
        F: FnMut(Rgba) -> Option<Rgba>,
    {
        let width = src.w.min(image.width.saturating_sub(src.x));
        let height = src.h.min(image.height.saturating_sub(src.y));
        for y in 0..height {
            for x in 0..width {
                let Some(color) = map_color(image.at(src.x + x, src.y + y)) else {
                    continue;
                };
                if color.a == 0 {
                    continue;
                }

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
                self.fill_rect(dx, dy, scale, scale, color);
            }
        }
    }

    pub(crate) fn blit_from(&mut self, src: &Framebuffer, dest_x: usize, dest_y: usize) {
        if dest_x >= self.width || dest_y >= self.height {
            return;
        }

        let copy_width = src.width.min(self.width - dest_x);
        let copy_height = src.height.min(self.height - dest_y);
        for y in 0..copy_height {
            self.row_bytes_mut(dest_y + y, dest_x, copy_width)
                .copy_from_slice(src.row_bytes(y, 0, copy_width));
        }
    }
}

fn decode_png(path: &str) -> Result<Vec<Rgba>, Box<dyn Error>> {
    Ok(decode_png_with_size(path)?.2)
}

pub(crate) fn decode_png_with_size(
    path: &str,
) -> Result<(usize, usize, Vec<Rgba>), Box<dyn Error>> {
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

fn color_distance(a: Rgba, b: Rgba) -> u32 {
    let dr = a.r as i32 - b.r as i32;
    let dg = a.g as i32 - b.g as i32;
    let db = a.b as i32 - b.b as i32;
    (dr * dr + dg * dg + db * db) as u32
}
