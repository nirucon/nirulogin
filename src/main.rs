mod greetd;
mod paint;
mod theme;

use chrono::Local;
use paint::{Fonts, Painter};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};
use theme::*;
use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::WindowBuilder,
};
use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};
use zeroize::Zeroize;

const APP_NAME: &str = "NIRUX";
const TAGLINE: &str = "A minimal X11 desktop";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Username,
    Password,
    PowerOff,
    Reboot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PowerAction {
    PowerOff,
    Reboot,
}

const DEFAULT_LAYOUT: &str = "se";

fn normalize_layout(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 32
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(value)
}

fn add_layouts(out: &mut Vec<String>, text: &str) {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        for token in line.split(|c: char| c.is_whitespace() || c == ',') {
            if let Some(layout) = normalize_layout(token) {
                if !out.iter().any(|existing| existing == &layout) {
                    out.push(layout);
                }
            }
        }
    }
}

/// Layouts offered at the login screen. NIRUX writes `/etc/nirulogin/layouts`
/// with Swedish first; administrators can edit it without touching the binary.
/// `NIRULOGIN_LAYOUTS` is an explicit per-greeter override for tests and custom
/// display-manager configurations. Only XKB layout codes are accepted here;
/// variants stay in the session's input settings.
fn login_layouts() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(value) = std::env::var("NIRULOGIN_LAYOUTS") {
        add_layouts(&mut out, &value);
    }
    if let Ok(value) = std::fs::read_to_string("/etc/nirulogin/layouts") {
        add_layouts(&mut out, &value);
    }
    if let Ok(value) = std::env::var("NIRULOGIN_LAYOUT") {
        if let Some(layout) = normalize_layout(&value) {
            out.retain(|item| item != &layout);
            out.insert(0, layout);
        }
    }
    if out.is_empty() {
        out.push(DEFAULT_LAYOUT.to_string());
    }
    out
}

fn apply_layout(layout: &str) -> bool {
    Command::new("setxkbmap")
        .args(["-layout", layout])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}
struct PamPrompt {
    message: String,
    secret: bool,
}

enum AuthEvent {
    Prompt(PamPrompt),
    Info(String),
    Done(Result<(), String>),
}

struct Ui {
    username: String,
    password: String,
    focus: Field,
    error: String,
    notice: String,
    pam_prompt: Option<PamPrompt>,
    signing: bool,
    clock: String,
    date: String,
    caret_on: bool,
    mouse: (f64, f64),
    layouts: Vec<String>,
    layout: String,
    pending_power: Option<PowerAction>,
}

impl Ui {
    fn new(layouts: Vec<String>, layout: String) -> Self {
        let now = Local::now();
        let username = std::env::var("NIRULOGIN_USER").unwrap_or_else(|_| "niru".into());
        Self {
            username,
            password: String::new(),
            focus: Field::Password,
            error: String::new(),
            notice: String::new(),
            pam_prompt: None,
            signing: false,
            clock: now.format("%H:%M").to_string(),
            date: now.format("%A %-d %B").to_string(),
            caret_on: true,
            mouse: (0.0, 0.0),
            layouts,
            layout,
            pending_power: None,
        }
    }

    fn update_clock(&mut self) -> bool {
        let now = Local::now();
        let clock = now.format("%H:%M").to_string();
        let date = now.format("%A %-d %B").to_string();
        let changed = clock != self.clock || date != self.date;
        self.clock = clock;
        self.date = date;
        changed
    }

    fn set_focus(&mut self, field: Field) {
        self.focus = field;
        self.caret_on = true;
    }

    fn next_focus(&mut self) {
        self.focus = match self.focus {
            Field::Username => Field::Password,
            Field::Password => Field::PowerOff,
            Field::PowerOff => Field::Reboot,
            Field::Reboot => Field::Username,
        };
        self.caret_on = true;
    }

    fn active_text(&mut self) -> &mut String {
        match self.focus {
            Field::Username => &mut self.username,
            _ => &mut self.password,
        }
    }

    fn type_text(&mut self, text: &str) {
        if (self.signing && self.pam_prompt.is_none())
            || matches!(self.focus, Field::PowerOff | Field::Reboot)
        {
            return;
        }
        let target = self.active_text();
        for ch in text.chars() {
            if !ch.is_control() {
                target.push(ch);
            }
        }
        self.caret_on = true;
        self.error.clear();
    }

    fn backspace(&mut self) {
        if (self.signing && self.pam_prompt.is_none())
            || matches!(self.focus, Field::PowerOff | Field::Reboot)
        {
            return;
        }
        self.active_text().pop();
        self.caret_on = true;
        self.error.clear();
    }

    fn layout(&self, width: u32, height: u32) -> Layout {
        let scale = (height as f32 / 1200.0).clamp(0.72, 1.15);
        let column_w = (420.0 * scale).min(width as f32 - 64.0).max(280.0);
        let field_h = 44.0 * scale;
        let gap = 14.0 * scale;
        let top_block = 48.0 * scale + 24.0 * scale + 28.0 * scale;
        let content_h = top_block
            + 46.0 * scale
            + 24.0 * scale
            + 18.0 * scale
            + field_h
            + gap
            + 18.0 * scale
            + field_h
            + 28.0 * scale
            + 46.0 * scale
            + 24.0 * scale;
        let top = ((height as f32 - content_h) / 2.0).max(36.0);
        let left = (width as f32 - column_w) / 2.0;
        let clock_baseline = top + 42.0 * scale;
        let date_baseline = clock_baseline + 26.0 * scale;
        let title_baseline = date_baseline + 54.0 * scale;
        let tagline_baseline = title_baseline + 24.0 * scale;
        let user_label = tagline_baseline + 42.0 * scale;
        let user_y = user_label + 10.0 * scale;
        let pass_label = user_y + field_h + 22.0 * scale;
        let pass_y = pass_label + 10.0 * scale;
        let button_y = pass_y + field_h + 26.0 * scale;
        Layout {
            scale,
            left,
            column_w,
            field_h,
            clock_baseline,
            date_baseline,
            title_baseline,
            tagline_baseline,
            user_label,
            user_y,
            pass_label,
            pass_y,
            button_y,
            button_h: 46.0 * scale,
        }
    }

    fn draw(&mut self, painter: &mut Painter, fonts: &Fonts) {
        let w = painter.width();
        let h = painter.height();
        let l = self.layout(w, h);
        painter.clear(BACKGROUND);
        let center = w as f32 / 2.0;

        painter.centered_text(
            &fonts.regular,
            44.0 * l.scale,
            center,
            l.clock_baseline,
            &self.clock,
            FOREGROUND,
        );
        painter.centered_text(
            &fonts.regular,
            14.0 * l.scale,
            center,
            l.date_baseline,
            &self.date,
            MUTED,
        );
        painter.centered_spaced_text(
            &fonts.bold,
            38.0 * l.scale,
            center,
            l.title_baseline,
            APP_NAME,
            FOREGROUND,
            4.0 * l.scale,
        );
        painter.centered_text(
            &fonts.regular,
            14.0 * l.scale,
            center,
            l.tagline_baseline,
            TAGLINE,
            MUTED,
        );

        let x = l.left as i32;
        let cw = l.column_w as u32;
        let field_h = l.field_h as u32;

        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            l.left,
            l.user_label,
            "Username",
            MUTED,
        );
        let user_y = l.user_y as i32;
        painter.fill(x, user_y, cw, field_h, SURFACE);
        painter.border(
            x,
            user_y,
            cw,
            field_h,
            if self.focus == Field::Username {
                ACCENT
            } else {
                BORDER
            },
        );
        let pad = (14.0 * l.scale) as i32;
        let baseline = user_y + field_h as i32 / 2 + (15.0 * l.scale) as i32 / 3;
        painter.text(
            &fonts.regular,
            15.0 * l.scale,
            (x + pad) as f32,
            baseline as f32,
            &self.username,
            FOREGROUND,
        );
        if self.focus == Field::Username && self.caret_on {
            let caret_x = (x + pad) as f32
                + painter.text_width(&fonts.regular, 15.0 * l.scale, &self.username);
            painter.fill(
                caret_x as i32,
                user_y + (10.0 * l.scale) as i32,
                1,
                (field_h as f32 - 20.0 * l.scale) as u32,
                FOREGROUND,
            );
        }

        let pass_label = if self.pam_prompt.is_some() {
            "Password / response"
        } else {
            "Password"
        };
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            l.left,
            l.pass_label,
            pass_label,
            MUTED,
        );
        let pass_y = l.pass_y as i32;
        painter.fill(x, pass_y, cw, field_h, SURFACE);
        painter.border(
            x,
            pass_y,
            cw,
            field_h,
            if self.focus == Field::Password {
                ACCENT
            } else {
                BORDER
            },
        );
        let visible = self.pam_prompt.as_ref().map(|p| !p.secret).unwrap_or(false);
        let shown = if visible {
            self.password.clone()
        } else {
            "•".repeat(self.password.chars().count().min(64))
        };
        painter.text(
            &fonts.regular,
            15.0 * l.scale,
            (x + pad) as f32,
            (pass_y + field_h as i32 / 2 + (15.0 * l.scale) as i32 / 3) as f32,
            &shown,
            FOREGROUND,
        );
        if self.focus == Field::Password && self.caret_on {
            let caret_x =
                (x + pad) as f32 + painter.text_width(&fonts.regular, 15.0 * l.scale, &shown);
            painter.fill(
                caret_x as i32,
                pass_y + (10.0 * l.scale) as i32,
                1,
                (field_h as f32 - 20.0 * l.scale) as u32,
                FOREGROUND,
            );
        }

        let button_y = l.button_y as i32;
        let button_h = l.button_h as u32;
        painter.fill(x, button_y, cw, button_h, ACCENT);
        painter.centered_text(
            &fonts.bold,
            15.0 * l.scale,
            center,
            (button_y + button_h as i32 / 2 + 5) as f32,
            "Press Enter to sign in",
            FOREGROUND,
        );
        if !self.error.is_empty() {
            painter.centered_text(
                &fonts.regular,
                12.0 * l.scale,
                center,
                (button_y + button_h as i32 + 28) as f32,
                &self.error,
                ERROR,
            );
        } else if !self.notice.is_empty() {
            painter.centered_text(
                &fonts.regular,
                12.0 * l.scale,
                center,
                (button_y + button_h as i32 + 28) as f32,
                &self.notice,
                FOREGROUND,
            );
        } else if self.signing {
            painter.centered_text(
                &fonts.regular,
                12.0 * l.scale,
                center,
                (button_y + button_h as i32 + 28) as f32,
                "Signing in…",
                MUTED,
            );
        }

        let bottom = h as f32 - 34.0 * l.scale;
        let (language_x, language_y, language_w, language_h) = self.language_rect(w, h);
        if self.layouts.len() > 1 {
            painter.border(
                language_x,
                language_y,
                language_w as u32,
                language_h as u32,
                BORDER,
            );
        }
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            28.0 * l.scale,
            bottom,
            &self.language_label(),
            MUTED,
        );
        let off = self.power_rect(w, h, PowerAction::PowerOff);
        let reboot = self.power_rect(w, h, PowerAction::Reboot);
        for (rect, focused) in [
            (off, self.focus == Field::PowerOff),
            (reboot, self.focus == Field::Reboot),
        ] {
            if focused {
                painter.border(
                    rect.0,
                    rect.1,
                    rect.2.max(1) as u32,
                    rect.3.max(1) as u32,
                    ACCENT,
                );
            }
        }
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            reboot.0 as f32,
            bottom,
            "Restart",
            if self.focus == Field::Reboot {
                FOREGROUND
            } else {
                MUTED
            },
        );
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            off.0 as f32,
            bottom,
            "Power off",
            if self.focus == Field::PowerOff {
                FOREGROUND
            } else {
                MUTED
            },
        );

        if let Some(action) = self.pending_power {
            let panel_w = 360.0 * l.scale;
            let panel_h = 112.0 * l.scale;
            let px = (center - panel_w / 2.0) as i32;
            let py = (h as f32 / 2.0 - panel_h / 2.0) as i32;
            painter.fill(px, py, panel_w as u32, panel_h as u32, SURFACE);
            painter.border(px, py, panel_w as u32, panel_h as u32, BORDER);
            let label = match action {
                PowerAction::PowerOff => "Power off?",
                PowerAction::Reboot => "Restart?",
            };
            painter.centered_text(
                &fonts.bold,
                22.0 * l.scale,
                center,
                py as f32 + 42.0 * l.scale,
                label,
                FOREGROUND,
            );
            painter.centered_text(
                &fonts.regular,
                12.0 * l.scale,
                center,
                py as f32 + 70.0 * l.scale,
                "Enter to confirm · Esc to cancel",
                MUTED,
            );
        }
    }

    fn language_rect(&self, width: u32, height: u32) -> (i32, i32, i32, i32) {
        let scale = (height as f32 / 1200.0).clamp(0.72, 1.15);
        let x = (16.0 * scale) as i32;
        let y = height as i32 - (54.0 * scale) as i32;
        let w = ((300.0 * scale) as i32).min(width as i32 - x);
        (x, y, w, (30.0 * scale) as i32)
    }

    fn language_label(&self) -> String {
        let code = self.layout.to_uppercase();
        if self.layouts.len() > 1 {
            format!("Keyboard: {code} · click to change")
        } else {
            format!("Keyboard: {code}")
        }
    }

    fn cycle_layout(&mut self) {
        if self.layouts.len() < 2 {
            return;
        }
        let current = self
            .layouts
            .iter()
            .position(|layout| layout == &self.layout)
            .unwrap_or(0);
        let next = self.layouts[(current + 1) % self.layouts.len()].clone();
        if apply_layout(&next) {
            self.layout = next;
            self.notice = format!("Keyboard layout: {}", self.layout.to_uppercase());
            self.error.clear();
        } else {
            self.notice = format!(
                "Could not switch to keyboard layout {}",
                next.to_uppercase()
            );
        }
    }

    fn hit_language(&self, width: u32, height: u32, pos: (f64, f64)) -> bool {
        let (x, y, w, h) = self.language_rect(width, height);
        pos.0 >= f64::from(x)
            && pos.0 <= f64::from(x + w)
            && pos.1 >= f64::from(y)
            && pos.1 <= f64::from(y + h)
    }

    fn power_rect(&self, width: u32, height: u32, action: PowerAction) -> (i32, i32, i32, i32) {
        let scale = (height as f32 / 1200.0).clamp(0.72, 1.15);
        let font = 12.0 * scale;
        let _ = font;
        let off_w = (76.0 * scale) as i32;
        let restart_w = (58.0 * scale) as i32;
        let gap = (24.0 * scale) as i32;
        let right = width as i32 - (28.0 * scale) as i32;
        let reboot_x = right - restart_w;
        let off_x = reboot_x - gap - off_w;
        let y = height as i32 - (48.0 * scale) as i32;
        let h = (24.0 * scale) as i32;
        match action {
            PowerAction::PowerOff => (off_x, y, off_w, h),
            PowerAction::Reboot => (reboot_x, y, restart_w, h),
        }
    }

    fn hit_power(&self, width: u32, height: u32, pos: (f64, f64)) -> Option<PowerAction> {
        for action in [PowerAction::PowerOff, PowerAction::Reboot] {
            let (x, y, w, h) = self.power_rect(width, height, action);
            if pos.0 >= f64::from(x)
                && pos.0 <= f64::from(x + w)
                && pos.1 >= f64::from(y)
                && pos.1 <= f64::from(y + h)
            {
                return Some(action);
            }
        }
        None
    }

    fn hit_field(&self, width: u32, height: u32, pos: (f64, f64)) -> Option<Field> {
        let l = self.layout(width, height);
        let x = l.left as f64;
        let w = l.column_w as f64;
        if pos.0 < x || pos.0 > x + w {
            return None;
        }
        if pos.1 >= l.user_y as f64 && pos.1 <= (l.user_y + l.field_h) as f64 {
            Some(Field::Username)
        } else if pos.1 >= l.pass_y as f64 && pos.1 <= (l.pass_y + l.field_h) as f64 {
            Some(Field::Password)
        } else {
            None
        }
    }

    fn hit_button(&self, width: u32, height: u32, pos: (f64, f64)) -> bool {
        let l = self.layout(width, height);
        pos.0 >= l.left as f64
            && pos.0 <= (l.left + l.column_w) as f64
            && pos.1 >= l.button_y as f64
            && pos.1 <= (l.button_y + l.button_h) as f64
    }
}

struct Layout {
    scale: f32,
    left: f32,
    column_w: f32,
    field_h: f32,
    clock_baseline: f32,
    date_baseline: f32,
    title_baseline: f32,
    tagline_baseline: f32,
    user_label: f32,
    user_y: f32,
    pass_label: f32,
    pass_y: f32,
    button_y: f32,
    button_h: f32,
}

fn focus_login_window() {
    if Command::new("xdotool")
        .args(["search", "--name", "NIRUX Login", "windowfocus"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        return;
    }
    let Ok((connection, screen)) = x11rb::connect(None) else {
        return;
    };
    let root = connection.setup().roots[screen].root;
    let Ok(tree_cookie) = connection.query_tree(root) else {
        return;
    };
    let Ok(tree) = tree_cookie.reply() else {
        return;
    };
    for window in tree.children {
        let Ok(property) = connection.get_property(
            false,
            window,
            x11rb::protocol::xproto::AtomEnum::WM_NAME,
            x11rb::protocol::xproto::AtomEnum::STRING,
            0,
            64,
        ) else {
            continue;
        };
        let Ok(name) = property.reply() else {
            continue;
        };
        if name.value == b"NIRUX Login" {
            let _ = connection.set_input_focus(
                x11rb::protocol::xproto::InputFocus::PARENT,
                window,
                x11rb::CURRENT_TIME,
            );
            let _ = connection.flush();
            return;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let preview = args.iter().any(|arg| arg == "--preview");
    if args.iter().any(|arg| arg == "--version") {
        println!("NIRULOGIN {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|arg| arg == "--help") {
        println!("Usage: nirulogin [--preview]");
        println!("NIRULOGIN is normally started by greetd.");
        return;
    }

    let layouts = login_layouts();
    let layout = layouts
        .first()
        .cloned()
        .unwrap_or_else(|| DEFAULT_LAYOUT.into());
    if !apply_layout(&layout) {
        eprintln!("nirulogin: cannot apply keyboard layout {layout}");
    }
    let fonts = match Fonts::load() {
        Ok(fonts) => fonts,
        Err(error) => {
            eprintln!("nirulogin: {error}");
            std::process::exit(1);
        }
    };
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("nirulogin: {error}");
            std::process::exit(1);
        }
    };
    let window = match WindowBuilder::new()
        .with_title("NIRUX Login")
        .with_decorations(false)
        .with_resizable(false)
        .with_visible(false)
        .build(&event_loop)
    {
        Ok(window) => window,
        Err(error) => {
            eprintln!("nirulogin: {error}");
            std::process::exit(1);
        }
    };
    if let Some(monitor) = window
        .current_monitor()
        .or_else(|| event_loop.primary_monitor())
    {
        let size = monitor.size();
        let _ = window.request_inner_size(size);
        window.set_outer_position(PhysicalPosition::new(
            monitor.position().x,
            monitor.position().y,
        ));
    }
    window.set_visible(true);
    focus_login_window();
    window.set_cursor_visible(true);

    let window = Rc::new(window);
    let context = match softbuffer::Context::new(window.clone()) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("nirulogin: {error}");
            std::process::exit(1);
        }
    };
    let mut surface = match softbuffer::Surface::new(&context, window.clone()) {
        Ok(surface) => surface,
        Err(error) => {
            eprintln!("nirulogin: {error}");
            std::process::exit(1);
        }
    };
    let size = window.inner_size();
    let width = size.width.max(1);
    let height = size.height.max(1);
    let mut painter = Painter::new(width, height);
    let mut ui = Ui::new(layouts, layout);
    let mut next_tick = Instant::now() + Duration::from_millis(500);
    let mut exit_after_draw = false;
    let (auth_tx, auth_rx) = mpsc::channel::<AuthEvent>();
    let mut reply_tx: Option<Sender<String>> = None;

    let _ = event_loop.run(move |event, elwt| {
        let _ = &context;
        elwt.set_control_flow(ControlFlow::WaitUntil(next_tick));
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(size) => {
                    let width = size.width.max(1);
                    let height = size.height.max(1);
                    let _ = surface.resize(
                        NonZeroU32::new(width).unwrap(),
                        NonZeroU32::new(height).unwrap(),
                    );
                    painter.resize(width, height);
                    window.request_redraw();
                }
                WindowEvent::CursorMoved { position, .. } => {
                    ui.mouse = (position.x, position.y);
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if state == ElementState::Pressed && button == MouseButton::Left {
                        focus_login_window();
                        let size = window.inner_size();
                        if let Some(action) = ui.hit_power(size.width, size.height, ui.mouse) {
                            ui.focus = match action {
                                PowerAction::PowerOff => Field::PowerOff,
                                PowerAction::Reboot => Field::Reboot,
                            };
                            ui.pending_power = Some(action);
                            window.request_redraw();
                        } else if ui.hit_language(size.width, size.height, ui.mouse) {
                            ui.cycle_layout();
                            window.request_redraw();
                        } else if let Some(field) = ui.hit_field(size.width, size.height, ui.mouse)
                        {
                            ui.set_focus(field);
                            window.request_redraw();
                        } else if ui.hit_button(size.width, size.height, ui.mouse) {
                            submit(
                                &mut ui,
                                preview,
                                &window,
                                &mut painter,
                                &fonts,
                                &mut exit_after_draw,
                                &auth_tx,
                                &mut reply_tx,
                            );
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if event.state != ElementState::Pressed
                        || (ui.signing && ui.pam_prompt.is_none())
                    {
                        return;
                    }
                    match event.logical_key {
                        Key::Named(NamedKey::Tab) => {
                            ui.next_focus();
                            window.request_redraw();
                        }
                        Key::Named(NamedKey::Enter) => {
                            if let Some(action) = ui.pending_power.take() {
                                run_power(action, &mut ui, &window);
                            } else if ui.focus == Field::PowerOff {
                                ui.pending_power = Some(PowerAction::PowerOff);
                                window.request_redraw();
                            } else if ui.focus == Field::Reboot {
                                ui.pending_power = Some(PowerAction::Reboot);
                                window.request_redraw();
                            } else {
                                submit(
                                    &mut ui,
                                    preview,
                                    &window,
                                    &mut painter,
                                    &fonts,
                                    &mut exit_after_draw,
                                    &auth_tx,
                                    &mut reply_tx,
                                );
                            }
                        }
                        Key::Named(NamedKey::Backspace) => {
                            ui.backspace();
                            window.request_redraw();
                        }
                        Key::Named(NamedKey::Escape) => {
                            if ui.pending_power.take().is_some() {
                                window.request_redraw();
                            } else {
                                ui.password.zeroize();
                                ui.password.clear();
                                ui.error.clear();
                                ui.set_focus(Field::Password);
                                window.request_redraw();
                            }
                        }
                        _ => {
                            if let Some(text) = event.text.as_ref() {
                                if !text.is_empty() {
                                    ui.type_text(text);
                                    window.request_redraw();
                                }
                            }
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    let size = window.inner_size();
                    let width = size.width.max(1);
                    let height = size.height.max(1);
                    if width != painter.width() || height != painter.height() {
                        painter.resize(width, height);
                    }
                    ui.draw(&mut painter, &fonts);
                    if let Ok(mut buffer) = surface.buffer_mut() {
                        let len = buffer.len().min(painter.pixels().len());
                        buffer[..len].copy_from_slice(&painter.pixels()[..len]);
                        let _ = buffer.present();
                    }
                    if exit_after_draw {
                        elwt.exit();
                    }
                }
                _ => {}
            },
            Event::AboutToWait => {
                while let Ok(auth_event) = auth_rx.try_recv() {
                    match auth_event {
                        AuthEvent::Prompt(prompt) => {
                            ui.pam_prompt = Some(prompt);
                            ui.password.zeroize();
                            ui.password.clear();
                            ui.error.clear();
                            ui.notice.clear();
                            ui.signing = true;
                            ui.set_focus(Field::Password);
                            window.request_redraw();
                        }
                        AuthEvent::Info(message) => {
                            ui.notice = message;
                            window.request_redraw();
                        }
                        AuthEvent::Done(result) => {
                            match result {
                                Ok(()) => exit_after_draw = true,
                                Err(error) => {
                                    ui.password.zeroize();
                                    ui.password.clear();
                                    ui.pam_prompt = None;
                                    ui.signing = false;
                                    ui.error = error;
                                    ui.notice.clear();
                                    ui.set_focus(Field::Password);
                                }
                            }
                            window.request_redraw();
                        }
                    }
                }
                let now = Instant::now();
                if !ui.signing && ui.pending_power.is_none() {
                    focus_login_window();
                }
                if now >= next_tick {
                    let clock_changed = ui.update_clock();
                    ui.caret_on = !ui.caret_on;
                    next_tick = now + Duration::from_millis(500);
                    if clock_changed || !ui.signing {
                        window.request_redraw();
                    }
                }
            }
            _ => {}
        }
    });
}

fn run_power(action: PowerAction, ui: &mut Ui, window: &winit::window::Window) {
    let (method, verb) = match action {
        PowerAction::PowerOff => ("PowerOff", "poweroff"),
        PowerAction::Reboot => ("Reboot", "reboot"),
    };
    let mut ok = false;
    if Command::new("dbus-send")
        .args([
            "--system",
            "--print-reply",
            "--reply-timeout=120000",
            "--dest=org.freedesktop.login1",
            "/org/freedesktop/login1",
            &format!("org.freedesktop.login1.Manager.{method}"),
            "boolean:true",
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        ok = true;
    }
    if !ok
        && Command::new("systemctl")
            .arg(verb)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    {
        ok = true;
    }
    if !ok
        && Command::new("loginctl")
            .arg(verb)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    {
        ok = true;
    }
    if !ok {
        ui.error = format!(
            "Could not {}",
            if action == PowerAction::PowerOff {
                "power off"
            } else {
                "restart"
            }
        );
        window.request_redraw();
    }
}

#[allow(clippy::too_many_arguments)]
fn submit(
    ui: &mut Ui,
    preview: bool,
    window: &winit::window::Window,
    painter: &mut Painter,
    fonts: &Fonts,
    _exit_after_draw: &mut bool,
    auth_tx: &Sender<AuthEvent>,
    reply_tx: &mut Option<Sender<String>>,
) {
    if let Some(prompt) = ui.pam_prompt.as_ref() {
        let answer = ui.password.clone();
        let message = prompt.message.clone();
        match reply_tx.take() {
            Some(tx) if tx.send(answer).is_ok() => {
                ui.password.zeroize();
                ui.password.clear();
                ui.pam_prompt = None;
                ui.notice = message;
                ui.error.clear();
                ui.signing = true;
                ui.set_focus(Field::Password);
            }
            _ => {
                ui.pam_prompt = None;
                ui.password.zeroize();
                ui.password.clear();
                ui.signing = false;
                ui.error = "Authentication session ended".into();
            }
        }
        window.request_redraw();
        return;
    }
    if ui.signing {
        return;
    }
    if ui.username.trim().is_empty() {
        ui.error = "Enter your username".into();
        ui.set_focus(Field::Username);
        window.request_redraw();
        return;
    }
    if ui.password.is_empty() {
        ui.error = "Enter your password".into();
        ui.set_focus(Field::Password);
        window.request_redraw();
        return;
    }
    if preview {
        ui.error = "Preview mode: authentication is disabled".into();
        window.request_redraw();
        return;
    }
    ui.signing = true;
    ui.error.clear();
    ui.notice.clear();
    ui.draw(painter, fonts);
    window.request_redraw();
    let username = ui.username.trim().to_string();
    let password = ui.password.clone();
    let layout = ui.layout.clone();
    let configured_session = std::env::var("NIRULOGIN_SESSION")
        .unwrap_or_else(|_| format!("/home/{username}/.local/bin/niruwm-session"));
    let start_session = if Path::new("/usr/local/bin/nirulogin-session").is_file() {
        "/usr/local/bin/nirulogin-session".to_string()
    } else {
        configured_session.clone()
    };
    let (reply_sender, reply_receiver) = mpsc::channel::<String>();
    *reply_tx = Some(reply_sender);
    let events = auth_tx.clone();
    thread::spawn(move || {
        let result = greetd::login(
            &username,
            &password,
            &start_session,
            &layout,
            &configured_session,
            |prompt| match prompt {
                greetd::Prompt::Info(message) | greetd::Prompt::Error(message) => {
                    let _ = events.send(AuthEvent::Info(message));
                    Ok(None)
                }
                greetd::Prompt::Secret(message) => {
                    let _ = events.send(AuthEvent::Prompt(PamPrompt {
                        message,
                        secret: true,
                    }));
                    reply_receiver.recv().map(Some).map_err(|e| e.to_string())
                }
                greetd::Prompt::Visible(message) => {
                    let _ = events.send(AuthEvent::Prompt(PamPrompt {
                        message,
                        secret: false,
                    }));
                    reply_receiver.recv().map(Some).map_err(|e| e.to_string())
                }
            },
        );
        let _ = events.send(AuthEvent::Done(result));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_codes_are_normalised_and_comments_ignored() {
        assert_eq!(normalize_layout(" SE "), Some("se".into()));
        assert_eq!(normalize_layout("us-intl"), Some("us-intl".into()));
        assert_eq!(normalize_layout("se; rm -rf /"), None);
        let mut layouts = Vec::new();
        add_layouts(&mut layouts, "se # svenska\nus, gb\nse\n");
        assert_eq!(layouts, vec!["se", "us", "gb"]);
    }
}
