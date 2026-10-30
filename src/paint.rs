use crate::theme::Color;
use ab_glyph::{point, Font, FontArc, Glyph, PxScale, ScaleFont};
use std::path::Path;

pub struct Painter {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

pub struct Fonts {
    pub regular: FontArc,
    pub bold: FontArc,
}

impl Fonts {
    pub fn load() -> Result<Self, String> {
        let regular = load_first(&[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ])?;
        let bold = load_first(&[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        ])
        .unwrap_or_else(|_| regular.clone());
        Ok(Self { regular, bold })
    }
}

fn load_first(paths: &[&str]) -> Result<FontArc, String> {
    for path in paths {
        if Path::new(path).is_file() {
            let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
            return FontArc::try_from_vec(bytes).map_err(|e| format!("cannot load {path}: {e}"));
        }
    }
    Err("no usable system font found".into())
}

impl Painter {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height) as usize],
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let len = (width * height) as usize;
        self.width = width;
        self.height = height;
        if self.pixels.len() != len {
            self.pixels.resize(len, 0);
        }
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn clear(&mut self, color: Color) {
        self.pixels.fill(color.pixel());
    }

    pub fn fill(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color) {
        let color = color.pixel();
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x.saturating_add(w as i32)).max(0) as u32;
        let y1 = (y.saturating_add(h as i32)).max(0) as u32;
        let x1 = x1.min(self.width);
        let y1 = y1.min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for row in y0..y1 {
            let start = (row * self.width + x0) as usize;
            let end = (row * self.width + x1) as usize;
            self.pixels[start..end].fill(color);
        }
    }

    pub fn border(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color) {
        self.fill(x, y, w, 1, color);
        self.fill(x, y.saturating_add(h as i32 - 1), w, 1, color);
        self.fill(x, y, 1, h, color);
        self.fill(x.saturating_add(w as i32 - 1), y, 1, h, color);
    }

    pub fn text_width(&self, font: &FontArc, size: f32, text: &str) -> f32 {
        let scaled = font.as_scaled(PxScale::from(size));
        text.chars()
            .map(|ch| scaled.h_advance(font.glyph_id(ch)))
            .sum()
    }

    pub fn text(
        &mut self,
        font: &FontArc,
        size: f32,
        x: f32,
        baseline: f32,
        text: &str,
        color: Color,
    ) {
        self.text_spaced(font, size, x, baseline, text, color, 0.0);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_spaced(
        &mut self,
        font: &FontArc,
        size: f32,
        x: f32,
        baseline: f32,
        text: &str,
        color: Color,
        spacing: f32,
    ) {
        let scaled = font.as_scaled(PxScale::from(size));
        let mut pen = x;
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            let glyph = Glyph {
                id,
                scale: PxScale::from(size),
                position: point(pen, baseline),
            };
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                let ox = bounds.min.x as i32;
                let oy = bounds.min.y as i32;
                outline.draw(|gx, gy, coverage| {
                    let px = ox + gx as i32;
                    let py = oy + gy as i32;
                    self.blend(px, py, color, coverage);
                });
            }
            pen += scaled.h_advance(id) + spacing;
        }
    }

    pub fn centered_text(
        &mut self,
        font: &FontArc,
        size: f32,
        center_x: f32,
        baseline: f32,
        text: &str,
        color: Color,
    ) {
        let width = self.text_width(font, size, text);
        self.text(font, size, center_x - width / 2.0, baseline, text, color);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn centered_spaced_text(
        &mut self,
        font: &FontArc,
        size: f32,
        center_x: f32,
        baseline: f32,
        text: &str,
        color: Color,
        spacing: f32,
    ) {
        let width = self.text_width(font, size, text)
            + spacing * text.chars().count().saturating_sub(1) as f32;
        self.text_spaced(
            font,
            size,
            center_x - width / 2.0,
            baseline,
            text,
            color,
            spacing,
        );
    }

    fn blend(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as u32 * self.width + x as u32) as usize;
        let dst = self.pixels[index];
        let a = (coverage.clamp(0.0, 1.0) * 255.0).round() as u32;
        let inv = 255 - a;
        let r = ((dst >> 16) & 0xff) * inv + u32::from(color.r) * a;
        let g = ((dst >> 8) & 0xff) * inv + u32::from(color.g) * a;
        let b = (dst & 0xff) * inv + u32::from(color.b) * a;
        self.pixels[index] = ((r / 255) << 16) | ((g / 255) << 8) | (b / 255);
    }
}
