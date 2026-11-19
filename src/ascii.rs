//! The `ascii` style ("NIRUX ascii"): the NIRUX banner from the NIRUX
//! screensaver and lock screen, typed out slowly when the greeter starts,
//! with "A MINIMAL X11 DESKTOP" under it and a block cursor that blinks after
//! the line once the typing is done. Every monitor shows the banner, centred
//! and sized for it; the form monitor has it above the clock and the login
//! form.
//!
//! It is drawn like an old terminal: the character just typed glows in the
//! text colour for a moment and settles into the theme's accent colour (the
//! text colour when the accent is too dim on the background), and on a large
//! banner a dark scanline runs through every third pixel row of the
//! characters. Only the glyphs are scanlined, never the rest of the screen.
//!
//! The animation never touches input: the form works from the first frame
//! and the typing never waits for it. Glyphs are rasterized once per size
//! and stamped from a cache; only what changes is drawn and presented (a
//! character while typing, the cursor's cell when it blinks).

use crate::display::Rect;
use crate::theme::Color;
use ab_glyph::{point, Font, FontArc, Glyph, PxScale, ScaleFont};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The banner: seven rows, all the same width. Kept identical to the NIRUX
/// screensaver's and lock screen's `ascii` look.
pub const BANNER: [&str; 7] = [
    "##    ##  ####  #######   ##    ##  ##    ##",
    "###   ##   ##   ##    ##  ##    ##   ##  ## ",
    "####  ##   ##   ##    ##  ##    ##    ####  ",
    "## ## ##   ##   #######   ##    ##     ##   ",
    "##  ####   ##   ##  ##    ##    ##    ####  ",
    "##   ###   ##   ##   ##   ##    ##   ##  ## ",
    "##    ##  ####  ##    ##   ######   ##    ##",
];

/// The line under the banner.
pub const TAGLINE: &str = "A MINIMAL X11 DESKTOP";

/// Typing pace, the same as the lock screen: the banner one `#` at a time
/// (spaces are skipped, the cursor jumps them), then a breath, then the line
/// at a slower, typed rhythm.
const TYPE_BANNER: Duration = Duration::from_millis(28);
const TYPE_LINE: Duration = Duration::from_millis(70);
const BREATH: Duration = Duration::from_millis(500);
/// The cursor's half period once the text is done: on, then off.
const BLINK: Duration = Duration::from_millis(530);
/// How often the loop wakes while typing.
const FRAME: Duration = Duration::from_millis(16);
/// Scanlines only on a banner at least this tall per row; on a small one
/// they would eat the characters.
const SCANLINE_MIN_ROW: i32 = 24;

fn columns() -> i32 {
    BANNER[0].chars().count() as i32
}

/// Every `#` of the banner as (row, column), in typing order.
pub fn order() -> Vec<(usize, usize)> {
    BANNER
        .iter()
        .enumerate()
        .flat_map(|(r, row)| {
            row.chars()
                .enumerate()
                .filter(|(_, ch)| *ch == '#')
                .map(move |(c, _)| (r, c))
        })
        .collect()
}

/// How much is typed at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reveal {
    /// `#` characters of the banner, in [`order`].
    pub hashes: usize,
    /// Characters of the line.
    pub line: usize,
    /// Everything is typed.
    pub done: bool,
    /// The cursor shows (always while typing, every other half period after).
    pub cursor: bool,
}

/// The typing `elapsed` after the greeter started.
pub fn reveal(elapsed: Duration) -> Reveal {
    let total = order().len();
    let banner_time = TYPE_BANNER * total as u32;
    let line_total = TAGLINE.chars().count();
    if elapsed < banner_time {
        let hashes = (elapsed.as_millis() / TYPE_BANNER.as_millis()) as usize;
        return Reveal {
            hashes,
            line: 0,
            done: false,
            cursor: true,
        };
    }
    let line_start = banner_time + BREATH;
    let line_time = TYPE_LINE * line_total as u32;
    if elapsed < line_start + line_time {
        let line = elapsed
            .checked_sub(line_start)
            .map_or(0, |d| (d.as_millis() / TYPE_LINE.as_millis()) as usize + 1)
            .min(line_total);
        return Reveal {
            hashes: total,
            line,
            done: false,
            cursor: true,
        };
    }
    let since = elapsed - (line_start + line_time);
    Reveal {
        hashes: total,
        line: line_total,
        done: true,
        cursor: (since.as_millis() / BLINK.as_millis()) % 2 == 0,
    }
}

/// When the typing ends.
pub fn typing_time() -> Duration {
    TYPE_BANNER * order().len() as u32 + BREATH + TYPE_LINE * TAGLINE.chars().count() as u32
}

/// How long until [`reveal`] changes after `elapsed`.
pub fn next_change(elapsed: Duration) -> Duration {
    let end = typing_time();
    if elapsed < end {
        return FRAME;
    }
    let into = (elapsed - end).as_millis() % BLINK.as_millis();
    BLINK - Duration::from_millis(into as u64)
}

/// Where the cursor stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    /// In the banner, on (row, column).
    Banner(usize, usize),
    /// On the line, before character N.
    Line(usize),
}

/// The cursor's cell for a reveal: right after the last typed `#`, at the
/// start of the line during the breath, after the line's last character
/// while it is typed, and one space after it when done.
pub fn cursor(r: Reveal) -> Cursor {
    let order = order();
    if r.hashes < order.len() {
        return match r.hashes.checked_sub(1).map(|i| order[i]) {
            None => Cursor::Banner(0, 0),
            Some((row, col)) => Cursor::Banner(row, col + 1),
        };
    }
    Cursor::Line(if r.done { r.line + 1 } else { r.line })
}

/// The banner's font size for a monitor `width` x `height`: `share` of the
/// width, never so tall that the banner takes more than `max_height` of the
/// height. `advance` and `line` are the font's advance width and line height
/// per pixel of size.
pub fn banner_size(
    width: u32,
    height: u32,
    share: f32,
    max_height: f32,
    advance: f32,
    line: f32,
) -> f32 {
    let by_width = width as f32 * share / (columns() as f32 * advance.max(0.1));
    let by_height = height as f32 * max_height / (BANNER.len() as f32 * line.max(0.1));
    by_width.min(by_height).clamp(6.0, 96.0)
}

/// Relative luminance (WCAG).
fn luminance(colour: Color) -> f32 {
    let channel = |v: u8| {
        let c = f32::from(v) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(colour.r) + 0.7152 * channel(colour.g) + 0.0722 * channel(colour.b)
}

/// WCAG contrast between two colours, 1 to 21.
fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The banner colour: the theme's accent when it reads well on the
/// background (contrast 4.5 or more), otherwise the text colour.
pub fn banner_colour(accent: Color, foreground: Color, background: Color) -> Color {
    if contrast(accent, background) >= 4.5 {
        accent
    } else {
        foreground
    }
}

/// A monospace font's proportions, per pixel of size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub advance: f32,
    pub line: f32,
    pub ascent: f32,
}

/// One character cell at a size, in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub adv: i32,
    pub row: i32,
    pub ascent: i32,
}

impl Metrics {
    pub fn of(font: &FontArc) -> Self {
        let s = font.as_scaled(PxScale::from(100.0));
        Self {
            advance: s.h_advance(font.glyph_id('#')) / 100.0,
            line: (s.ascent() - s.descent() + s.line_gap()) / 100.0,
            ascent: s.ascent() / 100.0,
        }
    }

    pub fn cell(&self, size: f32) -> Cell {
        Cell {
            adv: (size * self.advance).round().max(1.0) as i32,
            row: (size * self.line).ceil().max(1.0) as i32,
            ascent: (size * self.ascent).round() as i32,
        }
    }
}

/// Where the banner and the line go, in root (window) coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub left: i32,
    pub top: i32,
    pub size: f32,
    pub tag_left: i32,
    pub tag_top: i32,
    pub tag_size: f32,
}

impl Spot {
    /// The banner at `top`, `size`, the line `gap` below it at `tag_size`,
    /// both centred horizontally on `centre`.
    pub fn centred(m: &Metrics, centre: i32, top: i32, size: f32, tag_size: f32, gap: i32) -> Self {
        let big = m.cell(size);
        let tag = m.cell(tag_size);
        let tag_w = tag.adv * TAGLINE.chars().count() as i32;
        Self {
            left: centre - big.adv * columns() / 2,
            top,
            size,
            tag_left: centre - tag_w / 2,
            tag_top: top + big.row * BANNER.len() as i32 + gap,
            tag_size,
        }
    }

    /// The banner and line block's height.
    pub fn height(&self, m: &Metrics) -> i32 {
        self.tag_top - self.top + m.cell(self.tag_size).row
    }
}

/// A glyph's coverage, offset from its cell's top left.
struct Mask {
    dx: i32,
    dy: i32,
    w: usize,
    h: usize,
    alpha: Vec<u8>,
}

/// The glyphs the banner and line need, rasterized once at one size.
struct Glyphs {
    cell: Cell,
    masks: HashMap<char, Mask>,
}

impl Glyphs {
    fn new(font: &FontArc, m: &Metrics, size: f32) -> Self {
        let cell = m.cell(size);
        let mut masks = HashMap::new();
        for ch in std::iter::once('#').chain(TAGLINE.chars()) {
            if ch == ' ' || masks.contains_key(&ch) {
                continue;
            }
            let glyph = Glyph {
                id: font.glyph_id(ch),
                scale: PxScale::from(size),
                position: point(0.0, cell.ascent as f32),
            };
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let b = outline.px_bounds();
            let (w, h) = (b.width() as usize, b.height() as usize);
            let mut alpha = vec![0u8; w * h];
            outline.draw(|x, y, c| {
                let (x, y) = (x as usize, y as usize);
                if x < w && y < h {
                    alpha[y * w + x] = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            });
            masks.insert(
                ch,
                Mask {
                    dx: b.min.x as i32,
                    dy: b.min.y as i32,
                    w,
                    h,
                    alpha,
                },
            );
        }
        Self { cell, masks }
    }
}

/// A pixel buffer that covers `width` x `height` at (`x`, `y`) of the root.
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Canvas<'_> {
    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let (lx, ly) = (x - self.x, y - self.y);
        (lx >= 0 && ly >= 0 && (lx as u32) < self.width && (ly as u32) < self.height)
            .then(|| ly as usize * self.width as usize + lx as usize)
            .filter(|&i| i < self.pixels.len())
    }

    fn fill(&mut self, r: Rect, colour: Color) {
        let px = colour.pixel();
        for y in r.y..r.y + r.h as i32 {
            for x in r.x..r.x + r.w as i32 {
                if let Some(i) = self.index(x, y) {
                    self.pixels[i] = px;
                }
            }
        }
    }

    /// Stamp `mask` with its cell's top left at (`x`, `y`). With `scan`,
    /// every third pixel row (of the root) is left as it is: the scanline.
    fn stamp(&mut self, mask: &Mask, x: i32, y: i32, colour: Color, scan: bool) {
        for my in 0..mask.h {
            let py = y + mask.dy + my as i32;
            if scan && py.rem_euclid(3) == 2 {
                continue;
            }
            for mx in 0..mask.w {
                let a = u32::from(mask.alpha[my * mask.w + mx]);
                if a == 0 {
                    continue;
                }
                let Some(i) = self.index(x + mask.dx + mx as i32, py) else {
                    continue;
                };
                let dst = self.pixels[i];
                let inv = 255 - a;
                let ch = |shift: u32, c: u8| {
                    ((((dst >> shift) & 0xff) * inv + u32::from(c) * a) / 255) << shift
                };
                self.pixels[i] = ch(16, colour.r) | ch(8, colour.g) | ch(0, colour.b);
            }
        }
    }
}

/// The colours the banner uses.
#[derive(Clone, Copy, Debug)]
pub struct Colours {
    /// The banner and the finished cursor.
    pub accent: Color,
    /// The glow of the newest character and the cursor while typing.
    pub foreground: Color,
    /// The line under the banner.
    pub muted: Color,
}

/// The banner on one monitor.
pub struct Banner {
    /// The monitor it is centred on.
    pub area: Rect,
    spot: Spot,
    big: u32,
    tag: u32,
    /// Everything the banner, line and cursor ever cover.
    block: Rect,
    /// The background under `block`, to wipe cells with.
    under: Vec<u32>,
    drawn: Option<Reveal>,
    cursor: Option<Cursor>,
}

/// The banners on every monitor and the typing clock.
pub struct Ascii {
    /// `None` only in tests, which fill the glyph cache themselves.
    font: Option<FontArc>,
    pub metrics: Metrics,
    started: Instant,
    glyphs: HashMap<u32, Glyphs>,
    pub banners: Vec<Banner>,
    colours: Colours,
}

impl Ascii {
    pub fn new(font: FontArc, colours: Colours) -> Self {
        let metrics = Metrics::of(&font);
        Self {
            font: Some(font),
            metrics,
            started: Instant::now(),
            glyphs: HashMap::new(),
            banners: Vec::new(),
            colours,
        }
    }

    /// Pretend the typing finished (tests and screenshots).
    #[cfg(test)]
    pub fn skip_typing(&mut self) {
        if let Some(t) = Instant::now().checked_sub(typing_time()) {
            self.started = t;
        }
    }

    /// Forget every banner (before a new layout).
    pub fn clear(&mut self) {
        self.banners.clear();
    }

    /// Add a banner at `spot` on monitor `area`, capturing the background
    /// it is drawn over from `background(x, y)` (root coordinates).
    pub fn add(&mut self, area: Rect, spot: Spot, background: impl Fn(i32, i32) -> u32) {
        let (font, m) = (&self.font, self.metrics);
        let big = spot.size.to_bits();
        let tag = spot.tag_size.to_bits();
        for (key, size) in [(big, spot.size), (tag, spot.tag_size)] {
            self.glyphs.entry(key).or_insert_with(|| match font {
                Some(font) => Glyphs::new(font, &m, size),
                None => Glyphs {
                    cell: m.cell(size),
                    masks: HashMap::new(),
                },
            });
        }
        let (bc, tc) = (self.glyphs[&big].cell, self.glyphs[&tag].cell);
        let banner_right = spot.left + bc.adv * columns();
        // The finished cursor stands one space after the line.
        let tag_right = spot.tag_left + tc.adv * (TAGLINE.chars().count() as i32 + 2);
        let left = spot.left.min(spot.tag_left) - bc.adv;
        let right = banner_right.max(tag_right) + bc.adv;
        let bottom = spot.tag_top + tc.row + 2;
        let block = clip(
            Rect {
                x: left,
                y: spot.top,
                w: (right - left).max(1) as u32,
                h: (bottom - spot.top).max(1) as u32,
            },
            area,
        );
        let mut under = Vec::with_capacity(block.w as usize * block.h as usize);
        for y in block.y..block.y + block.h as i32 {
            for x in block.x..block.x + block.w as i32 {
                under.push(background(x, y));
            }
        }
        self.banners.push(Banner {
            area,
            spot,
            big,
            tag,
            block,
            under,
            drawn: None,
            cursor: None,
        });
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// When the banners next change.
    pub fn deadline(&self) -> Instant {
        Instant::now() + next_change(self.elapsed())
    }

    /// Draw banner `index` as it stands now, wiping its block first.
    pub fn paint(&mut self, index: usize, canvas: &mut Canvas) {
        self.paint_at(index, canvas, self.elapsed());
    }

    /// Bring banner `index` up to date, drawing only what changed. Returns
    /// the rectangle to present, if anything changed.
    pub fn tick(&mut self, index: usize, canvas: &mut Canvas) -> Option<Rect> {
        self.tick_at(index, canvas, self.elapsed())
    }

    fn paint_at(&mut self, index: usize, canvas: &mut Canvas, elapsed: Duration) {
        let r = reveal(elapsed);
        let Some(b) = self.banners.get_mut(index) else {
            return;
        };
        let (Some(big), Some(tag)) = (self.glyphs.get(&b.big), self.glyphs.get(&b.tag)) else {
            return;
        };
        let c = self.colours;
        wipe(canvas, b, b.block);
        let order = order();
        for (i, &(row, col)) in order.iter().enumerate().take(r.hashes) {
            let glow = i + 1 == r.hashes && r.hashes < order.len();
            let colour = if glow { c.foreground } else { c.accent };
            draw_hash(canvas, b, big, row, col, colour);
        }
        for (i, ch) in TAGLINE.chars().enumerate().take(r.line) {
            draw_tag(canvas, b, tag, i, ch, c.muted);
        }
        let at = r.cursor.then(|| cursor(r));
        if let Some(at) = at {
            canvas.fill(cursor_rect(b, big, tag, at), cursor_colour(r, &c));
        }
        b.drawn = Some(r);
        b.cursor = at;
    }

    fn tick_at(&mut self, index: usize, canvas: &mut Canvas, elapsed: Duration) -> Option<Rect> {
        let r = reveal(elapsed);
        let b = self.banners.get_mut(index)?;
        let before = b.drawn?;
        if r == before {
            return None;
        }
        let big = self.glyphs.get(&b.big)?;
        let tag = self.glyphs.get(&b.tag)?;
        let c = self.colours;
        let order = order();
        let old_cursor = b.cursor;
        let new_cursor = r.cursor.then(|| cursor(r));
        if let Some(at) = old_cursor {
            wipe(canvas, b, cursor_rect(b, big, tag, at));
        }
        // The character typed before settles into the accent; the newest
        // glows until the next one.
        if before.hashes > 0 && r.hashes > before.hashes {
            let (row, col) = order[before.hashes - 1];
            wipe(canvas, b, hash_rect(b, big, row, col));
            draw_hash(canvas, b, big, row, col, c.accent);
        }
        for (i, &(row, col)) in order.iter().enumerate().take(r.hashes).skip(before.hashes) {
            let glow = i + 1 == r.hashes && r.hashes < order.len();
            let colour = if glow { c.foreground } else { c.accent };
            draw_hash(canvas, b, big, row, col, colour);
        }
        for (i, ch) in TAGLINE.chars().enumerate().take(r.line).skip(before.line) {
            draw_tag(canvas, b, tag, i, ch, c.muted);
        }
        if let Some(at) = new_cursor {
            canvas.fill(cursor_rect(b, big, tag, at), cursor_colour(r, &c));
        }
        let blink_only = r.hashes == before.hashes
            && r.line == before.line
            && old_cursor.is_none_or(|at| at == cursor(r));
        b.drawn = Some(r);
        b.cursor = new_cursor;
        Some(if blink_only {
            // Only the blink, in the same cell: present that cell.
            clip(cursor_rect(b, big, tag, cursor(r)), b.block)
        } else {
            b.block
        })
    }
}

fn cursor_colour(r: Reveal, c: &Colours) -> Color {
    if r.done {
        c.accent
    } else {
        c.foreground
    }
}

fn hash_rect(b: &Banner, big: &Glyphs, row: usize, col: usize) -> Rect {
    let cell = big.cell;
    Rect {
        x: b.spot.left + cell.adv * col as i32,
        y: b.spot.top + cell.row * row as i32,
        w: cell.adv as u32,
        h: cell.row as u32,
    }
}

fn cursor_rect(b: &Banner, big: &Glyphs, tag: &Glyphs, at: Cursor) -> Rect {
    match at {
        Cursor::Banner(row, col) => {
            let cell = big.cell;
            let h = cell.row * 8 / 10;
            Rect {
                x: b.spot.left + cell.adv * col as i32,
                y: b.spot.top + cell.row * row as i32 + (cell.row - h) / 2,
                w: cell.adv as u32,
                h: h.max(1) as u32,
            }
        }
        Cursor::Line(i) => {
            let cell = tag.cell;
            let descent = cell.row - cell.ascent;
            Rect {
                x: b.spot.tag_left + cell.adv * i as i32,
                y: b.spot.tag_top + (cell.row - cell.ascent - descent) / 2,
                w: cell.adv as u32,
                h: (cell.ascent + descent / 2).max(1) as u32,
            }
        }
    }
}

fn draw_hash(canvas: &mut Canvas, b: &Banner, big: &Glyphs, row: usize, col: usize, fg: Color) {
    let Some(mask) = big.masks.get(&'#') else {
        return;
    };
    let r = hash_rect(b, big, row, col);
    canvas.stamp(mask, r.x, r.y, fg, big.cell.row >= SCANLINE_MIN_ROW);
}

fn draw_tag(canvas: &mut Canvas, b: &Banner, tag: &Glyphs, i: usize, ch: char, fg: Color) {
    let Some(mask) = tag.masks.get(&ch) else {
        return;
    };
    canvas.stamp(
        mask,
        b.spot.tag_left + tag.cell.adv * i as i32,
        b.spot.tag_top,
        fg,
        false,
    );
}

/// Put the captured background back over `r` (within the block).
fn wipe(canvas: &mut Canvas, b: &Banner, r: Rect) {
    let r = clip(r, b.block);
    for y in r.y..r.y + r.h as i32 {
        for x in r.x..r.x + r.w as i32 {
            let src = (y - b.block.y) as usize * b.block.w as usize + (x - b.block.x) as usize;
            if let (Some(i), Some(&px)) = (canvas.index(x, y), b.under.get(src)) {
                canvas.pixels[i] = px;
            }
        }
    }
}

fn clip(r: Rect, to: Rect) -> Rect {
    let x0 = r.x.max(to.x);
    let y0 = r.y.max(to.y);
    let x1 = (r.x + r.w as i32).min(to.x + to.w as i32);
    let y1 = (r.y + r.h as i32).min(to.y + to.h as i32);
    Rect {
        x: x0,
        y: y0,
        w: (x1 - x0).max(0) as u32,
        h: (y1 - y0).max(0) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn the_banner_is_rectangular_and_matches_the_lock_screen() {
        let w = BANNER[0].chars().count();
        assert!(BANNER.iter().all(|r| r.chars().count() == w));
        assert!(BANNER
            .iter()
            .all(|r| r.chars().all(|c| c == '#' || c == ' ')));
        assert_eq!(w, 44);
        assert_eq!(TAGLINE, "A MINIMAL X11 DESKTOP");
    }

    #[test]
    fn typing_is_slow_then_the_cursor_blinks() {
        assert_eq!(reveal(at(0)).hashes, 0);
        assert!(reveal(at(0)).cursor);
        let total = order().len();
        let banner = TYPE_BANNER * total as u32;
        assert_eq!(reveal(TYPE_BANNER * 5).hashes, 5);
        assert_eq!(reveal(banner + at(100)).hashes, total);
        assert_eq!(reveal(banner + at(100)).line, 0, "a breath first");
        assert_eq!(reveal(banner + BREATH).line, 1);
        assert_eq!(reveal(banner + BREATH + TYPE_LINE).line, 2);
        let end = typing_time();
        assert!(
            end > Duration::from_secs(5) && end < Duration::from_secs(10),
            "{end:?}"
        );
        let r = reveal(end);
        assert!(r.done && r.cursor && r.line == TAGLINE.chars().count());
        assert!(!reveal(end + BLINK).cursor);
        assert!(reveal(end + BLINK * 2).cursor);
        assert_eq!(next_change(at(10)), FRAME);
        assert_eq!(next_change(end + at(30)), BLINK - at(30));
    }

    #[test]
    fn typing_skips_spaces() {
        let order = order();
        assert_eq!(order[..3], [(0, 0), (0, 1), (0, 6)]);
        assert!(order.iter().all(|&(r, c)| BANNER[r].as_bytes()[c] == b'#'));
    }

    #[test]
    fn the_cursor_follows_the_text() {
        assert_eq!(cursor(reveal(at(0))), Cursor::Banner(0, 0));
        assert_eq!(cursor(reveal(TYPE_BANNER * 3)), Cursor::Banner(0, 7));
        let banner = TYPE_BANNER * order().len() as u32;
        assert_eq!(cursor(reveal(banner)), Cursor::Line(0));
        assert_eq!(
            cursor(reveal(typing_time())),
            Cursor::Line(TAGLINE.chars().count() + 1)
        );
    }

    #[test]
    fn the_banner_fits_every_monitor() {
        for (w, h) in [
            (1280, 800),
            (1920, 1080),
            (2560, 1440),
            (3840, 2160),
            (1080, 1920),
        ] {
            let size = banner_size(w, h, 0.5, 0.22, 0.6, 1.2);
            assert!(44.0 * 0.6 * size <= w as f32 * 0.51, "{w}x{h}: {size}");
            assert!(7.0 * 1.2 * size <= h as f32 * 0.23, "{w}x{h}: {size}");
        }
    }

    #[test]
    fn a_dim_accent_falls_back_to_the_text_colour() {
        let (fg, black) = (Color::new(0xe5, 0xe5, 0xe5), Color::new(0, 0, 0));
        let dim = Color::new(0x5a, 0x5a, 0x60);
        let green = Color::new(0x7f, 0xbf, 0x6a);
        assert_eq!(banner_colour(dim, fg, black), fg);
        assert_eq!(banner_colour(green, fg, black), green);
    }

    /// An `Ascii` with synthetic square glyphs, so the tests need no font.
    fn fake(area: Rect, row: i32) -> Ascii {
        let mut a = Ascii {
            font: None,
            metrics: Metrics {
                advance: 0.6,
                line: 1.0,
                ascent: 0.8,
            },
            started: Instant::now(),
            glyphs: HashMap::new(),
            banners: Vec::new(),
            colours: Colours {
                accent: Color::new(0, 200, 0),
                foreground: Color::new(255, 255, 255),
                muted: Color::new(100, 100, 100),
            },
        };
        let size = row as f32;
        let spot = Spot::centred(&a.metrics, area.x + area.w as i32 / 2, 20, size, 10.0, 8);
        for s in [spot.size, spot.tag_size] {
            let cell = a.metrics.cell(s);
            let full = Mask {
                dx: 0,
                dy: 0,
                w: cell.adv as usize,
                h: cell.row as usize,
                alpha: vec![255; (cell.adv * cell.row) as usize],
            };
            let mut masks = HashMap::new();
            masks.insert('#', full);
            for ch in TAGLINE.chars() {
                let m = Mask {
                    dx: 0,
                    dy: 0,
                    w: 1,
                    h: 1,
                    alpha: vec![255],
                };
                masks.insert(ch, m);
            }
            a.glyphs.insert(s.to_bits(), Glyphs { cell, masks });
        }
        // Background: a gradient, as a wallpaper would be.
        a.add(area, spot, |x, y| {
            ((x as u32 & 0xff) << 8) | (y as u32 & 0xff)
        });
        a
    }

    fn canvas(pixels: &mut [u32], area: Rect) -> Canvas<'_> {
        Canvas {
            pixels,
            x: area.x,
            y: area.y,
            width: area.w,
            height: area.h,
        }
    }

    #[test]
    fn a_large_banner_has_scanlines_only_on_its_glyphs() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 1200,
            h: 500,
        };
        let mut a = fake(area, 30);
        let bg = |x: i32, y: i32| ((x as u32 & 0xff) << 8) | (y as u32 & 0xff);
        let mut px: Vec<u32> = (0..area.h as i32)
            .flat_map(|y| (0..area.w as i32).map(move |x| bg(x, y)))
            .collect();
        a.paint_at(0, &mut canvas(&mut px, area), typing_time() + BLINK);
        let b = &a.banners[0];
        let cell = a.metrics.cell(30.0);
        // The first `#` cell: lit rows in the accent, every third row kept.
        let (x, y0) = (b.spot.left + 2, b.spot.top);
        for y in y0..y0 + cell.row {
            let p = px[(y as u32 * area.w + x as u32) as usize];
            if y % 3 == 2 {
                assert_eq!(p, bg(x, y), "scanline at {y}");
            } else {
                assert_eq!(p, Color::new(0, 200, 0).pixel(), "glyph at {y}");
            }
        }
        // A space in the banner is untouched background.
        let space_x = b.spot.left + cell.adv * 3;
        assert_eq!(
            px[((y0 + 5) as u32 * area.w + space_x as u32) as usize],
            bg(space_x, y0 + 5)
        );
    }

    #[test]
    fn a_small_banner_has_no_scanlines() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 600,
            h: 300,
        };
        let mut a = fake(area, 12);
        let mut px = vec![0u32; (area.w * area.h) as usize];
        a.paint_at(0, &mut canvas(&mut px, area), typing_time());
        let b = &a.banners[0];
        let x = b.spot.left + 1;
        for y in b.spot.top..b.spot.top + 12 {
            assert_eq!(px[(y as u32 * area.w + x as u32) as usize], 0x00c800);
        }
    }

    #[test]
    fn typing_draws_the_newest_character_white_then_accent() {
        let area = Rect {
            x: 100,
            y: 0,
            w: 1200,
            h: 500,
        };
        let mut a = fake(area, 20);
        let mut px = vec![0u32; (area.w * area.h) as usize];
        let mut c = canvas(&mut px, area);
        a.paint_at(0, &mut c, TYPE_BANNER * 2);
        let b_left = a.banners[0].spot.left;
        let top = a.banners[0].spot.top;
        let pick = |c: &Canvas, x: i32, y: i32| c.pixels[c.index(x, y).unwrap()];
        assert_eq!(pick(&c, b_left + 1, top + 1), 0x00c800, "settled");
        assert_eq!(pick(&c, b_left + 12 + 1, top + 1), 0xffffff, "newest glows");
        let damage = a.tick_at(0, &mut c, TYPE_BANNER * 3).unwrap();
        assert_eq!(pick(&c, b_left + 12 + 1, top + 1), 0x00c800, "settles");
        assert_eq!(damage, a.banners[0].block);
        assert_eq!(a.tick_at(0, &mut c, TYPE_BANNER * 3 + at(1)), None);
    }

    #[test]
    fn a_blink_presents_only_the_cursor_cell() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 1200,
            h: 500,
        };
        let mut a = fake(area, 30);
        let mut px = vec![0u32; (area.w * area.h) as usize];
        let mut c = canvas(&mut px, area);
        let end = typing_time();
        a.paint_at(0, &mut c, end);
        let before = c.pixels.to_vec();
        let damage = a.tick_at(0, &mut c, end + BLINK).unwrap();
        let tag = a.metrics.cell(10.0);
        // One cell of the line, not the block.
        assert_eq!(damage.w, tag.adv as u32);
        assert!(damage.h <= tag.row as u32);
        assert!(damage.w * damage.h < a.banners[0].block.w * a.banners[0].block.h / 100);
        // Nothing outside the damage changed, and the cursor is gone.
        for (i, (&p, &q)) in c.pixels.iter().zip(&before).enumerate() {
            let (x, y) = ((i as u32 % area.w) as i32, (i as u32 / area.w) as i32);
            let inside = x >= damage.x
                && y >= damage.y
                && x < damage.x + damage.w as i32
                && y < damage.y + damage.h as i32;
            if !inside {
                assert_eq!(p, q, "changed outside the cursor cell at {x},{y}");
            }
        }
        assert!(c.pixels[(damage.y as u32 * area.w + damage.x as u32) as usize] != 0x00c800);
        // And back on.
        let again = a.tick_at(0, &mut c, end + BLINK * 2).unwrap();
        assert_eq!(again, damage);
        assert_eq!(
            c.pixels[(damage.y as u32 * area.w + damage.x as u32) as usize],
            0x00c800
        );
    }

    #[test]
    fn a_real_font_rasterizes_every_glyph() {
        // Skipped on a machine without system fonts (CI installs them only
        // for the Xvfb smoke).
        let Ok(fonts) = crate::paint::Fonts::load() else {
            return;
        };
        let font = fonts.mono;
        let m = Metrics::of(&font);
        assert!(m.advance > 0.3 && m.advance < 1.0, "{m:?}");
        let g = Glyphs::new(&font, &m, 30.0);
        for ch in std::iter::once('#').chain(TAGLINE.chars().filter(|c| *c != ' ')) {
            let mask = &g.masks[&ch];
            assert!(mask.alpha.iter().any(|&a| a > 200), "{ch}");
            // Inside its cell.
            assert!(
                mask.dx >= -1 && mask.dx + mask.w as i32 <= g.cell.adv + 1,
                "{ch}"
            );
            assert!(
                mask.dy >= 0 && mask.dy + mask.h as i32 <= g.cell.row + 1,
                "{ch}"
            );
        }
    }
}
