#[derive(Clone, Copy)]
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
}

pub const BACKGROUND: Color = Color::new(0x0f, 0x0f, 0x10);
pub const SURFACE: Color = Color::new(0x16, 0x16, 0x18);
pub const FOREGROUND: Color = Color::new(0xe5, 0xe5, 0xe5);
pub const MUTED: Color = Color::new(0xa8, 0xa8, 0xa8);
pub const BORDER: Color = Color::new(0x2a, 0x2a, 0x2d);
pub const ACCENT: Color = Color::new(0x5a, 0x5a, 0x60);
pub const ERROR: Color = Color::new(0xd7, 0x7a, 0x7a);
