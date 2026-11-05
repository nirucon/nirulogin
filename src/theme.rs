//! Colors and the optional `/etc/nirulogin/theme.toml`.
//!
//! The file is a flat list of `key = "value"` lines (section headers such as
//! `[palette]` are accepted and ignored). Unknown keys and malformed values are
//! ignored, so a missing or partial file always falls back to the built-in
//! NIRUX palette below.
//!
//! ```toml
//! background = "#0f0f10"
//! surface    = "#161618"
//! foreground = "#e5e5e5"
//! muted      = "#a8a8a8"
//! border     = "#2a2a2d"
//! accent     = "#5a5a60"
//! error      = "#d77a7a"
//! wallpaper  = "/usr/share/nirulogin/wallpaper.png"  # PNG, greeter-readable
//! dim        = "0.55"                                 # 0 = black, 1 = unchanged
//! ```

use std::path::{Path, PathBuf};

pub const THEME_FILE: &str = "/etc/nirulogin/theme.toml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn pixel(self) -> u32 {
        (u32::from(self.r) << 16) | (u32::from(self.g) << 8) | u32::from(self.b)
    }

    /// Linear blend: `t` = 0 gives `self`, 1 gives `other`.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let ch = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Color::new(
            ch(self.r, other.r),
            ch(self.g, other.g),
            ch(self.b, other.b),
        )
    }

    pub fn parse(value: &str) -> Option<Self> {
        // "#rrggbb" or bare "rrggbb" (the form NIRUX appearance.toml uses).
        let value = value.trim();
        let hex = value.strip_prefix('#').unwrap_or(value);
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self::new(byte(0)?, byte(2)?, byte(4)?))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub foreground: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub error: Color,
    pub wallpaper: Option<PathBuf>,
    pub dim: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::new(0x0f, 0x0f, 0x10),
            surface: Color::new(0x16, 0x16, 0x18),
            foreground: Color::new(0xe5, 0xe5, 0xe5),
            muted: Color::new(0xa8, 0xa8, 0xa8),
            border: Color::new(0x2a, 0x2a, 0x2d),
            accent: Color::new(0x5a, 0x5a, 0x60),
            error: Color::new(0xd7, 0x7a, 0x7a),
            wallpaper: None,
            dim: 0.45,
        }
    }
}

impl Theme {
    pub fn load() -> Self {
        // NIRULOGIN_THEME points at another file for previews and tests.
        let path = std::env::var_os("NIRULOGIN_THEME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(THEME_FILE));
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut theme = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            let value = match value.strip_prefix('"') {
                Some(rest) => match rest.find('"') {
                    Some(end) => &rest[..end],
                    None => continue,
                },
                None => value.split('#').next().unwrap_or("").trim(),
            };
            let slot = match key {
                "background" => &mut theme.background,
                "surface" => &mut theme.surface,
                "foreground" => &mut theme.foreground,
                "muted" => &mut theme.muted,
                "border" => &mut theme.border,
                "accent" => &mut theme.accent,
                "error" => &mut theme.error,
                "wallpaper" => {
                    if value.starts_with('/') {
                        theme.wallpaper = Some(PathBuf::from(value));
                    }
                    continue;
                }
                "dim" => {
                    if let Ok(dim) = value.parse::<f32>() {
                        if dim.is_finite() {
                            theme.dim = dim.clamp(0.0, 1.0);
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            if let Some(color) = Color::parse(value) {
                *slot = color;
            }
        }
        theme
    }
}

/// A decoded wallpaper in 0x00RRGGBB pixels.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

/// Wallpapers larger than this (per side) are refused rather than decoded.
const MAX_SIDE: u32 = 16384;

pub fn load_png(path: &Path) -> Option<Image> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).ok()?;
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let stride = frame.line_size;
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for row in buf[..frame.buffer_size()]
        .chunks(stride)
        .take(height as usize)
    {
        for px in row.chunks(channels).take(width as usize) {
            let (r, g, b) = match channels {
                1 | 2 => (px[0], px[0], px[0]),
                _ => (px[0], px[1], px[2]),
            };
            pixels.push((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
        }
    }
    (pixels.len() == (width * height) as usize).then_some(Image {
        width,
        height,
        pixels,
    })
}

/// Scale `image` to cover `w`×`h` (center crop, bilinear) and darken it.
pub fn cover(image: &Image, w: u32, h: u32, dim: f32) -> Vec<u32> {
    let mut out = vec![0u32; (w as usize) * (h as usize)];
    if w == 0 || h == 0 {
        return out;
    }
    let scale = (w as f32 / image.width as f32).max(h as f32 / image.height as f32);
    let off_x = (image.width as f32 * scale - w as f32) / 2.0;
    let off_y = (image.height as f32 * scale - h as f32) / 2.0;
    let dim = (dim.clamp(0.0, 1.0) * 256.0) as u32;
    let max_x = image.width - 1;
    let max_y = image.height - 1;
    for y in 0..h {
        let sy = ((y as f32 + 0.5 + off_y) / scale - 0.5).max(0.0);
        let y0 = (sy as u32).min(max_y);
        let y1 = (y0 + 1).min(max_y);
        let fy = ((sy - y0 as f32) * 256.0) as u32;
        for x in 0..w {
            let sx = ((x as f32 + 0.5 + off_x) / scale - 0.5).max(0.0);
            let x0 = (sx as u32).min(max_x);
            let x1 = (x0 + 1).min(max_x);
            let fx = ((sx - x0 as f32) * 256.0) as u32;
            let at = |x: u32, y: u32| image.pixels[(y * image.width + x) as usize];
            let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
            let mut pixel = 0u32;
            for shift in [16u32, 8, 0] {
                let ch = |p: u32| (p >> shift) & 0xff;
                let top = ch(a) * (256 - fx) + ch(b) * fx;
                let bottom = ch(c) * (256 - fx) + ch(d) * fx;
                let v = (top * (256 - fy) + bottom * fy) >> 16;
                pixel |= ((v * dim) >> 8).min(255) << shift;
            }
            out[(y * w + x) as usize] = pixel;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_theme_is_the_builtin_palette() {
        assert_eq!(Theme::parse(""), Theme::default());
        assert_eq!(Theme::default().background.pixel(), 0x0f0f10);
    }

    #[test]
    fn theme_file_overrides_colors_and_wallpaper() {
        let theme = Theme::parse(
            "# NIRUX theme niru-nord\n[palette]\nbackground = \"#2e3440\"\naccent=\"#88C0D0\" # frost\n\
             error = \"red\"\nwallpaper = \"/usr/share/nirulogin/wallpaper.png\"\ndim = 0.6\nunknown = 1\n",
        );
        assert_eq!(theme.background, Color::new(0x2e, 0x34, 0x40));
        assert_eq!(theme.accent, Color::new(0x88, 0xc0, 0xd0));
        assert_eq!(theme.error, Theme::default().error, "invalid value ignored");
        assert_eq!(
            Theme::parse("surface = \"3b4252\"").surface,
            Color::new(0x3b, 0x42, 0x52)
        );
        assert_eq!(
            theme.wallpaper.as_deref(),
            Some(Path::new("/usr/share/nirulogin/wallpaper.png"))
        );
        assert!((theme.dim - 0.6).abs() < 1e-6);
        assert_eq!(Theme::parse("wallpaper = \"relative.png\"").wallpaper, None);
    }

    #[test]
    fn cover_scales_crops_and_darkens() {
        let flat = Image {
            width: 2,
            height: 1,
            pixels: vec![0x808080, 0x808080],
        };
        let out = cover(&flat, 4, 4, 1.0);
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|&p| p == 0x808080));
        assert!(cover(&flat, 4, 4, 0.5).iter().all(|&p| p == 0x404040));
        // A 2:1 image covering a square keeps its center: left half white,
        // right half black.
        let split = Image {
            width: 4,
            height: 2,
            pixels: vec![0xffffff, 0xffffff, 0, 0, 0xffffff, 0xffffff, 0, 0],
        };
        let out = cover(&split, 2, 2, 1.0);
        assert!(out[0] > 0x808080 && out[1] < 0x808080);
    }
}
