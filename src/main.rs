mod greetd;
mod paint;
mod theme;

use chrono::Local;
use paint::{Fonts, Painter};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PowerAction {
    PowerOff,
    Reboot,
}

struct Ui {
    username: String,
    password: String,
    focus: Field,
    error: String,
    signing: bool,
    clock: String,
    date: String,
    caret_on: bool,
    mouse: (f64, f64),
    layout: String,
    pending_power: Option<PowerAction>,
}

impl Ui {
    fn new() -> Self {
        let now = Local::now();
        let username = std::env::var("NIRULOGIN_USER").unwrap_or_else(|_| "niru".into());
        Self {
            username,
            password: String::new(),
            focus: Field::Password,
            error: String::new(),
            signing: false,
            clock: now.format("%H:%M").to_string(),
            date: now.format("%A %-d %B").to_string(),
            caret_on: true,
            mouse: (0.0, 0.0),
            layout: std::env::var("NIRULOGIN_LAYOUT").unwrap_or_else(|_| "se".into()),
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
            Field::Password => Field::Username,
        };
        self.caret_on = true;
    }

    fn active_text(&mut self) -> &mut String {
        match self.focus {
            Field::Username => &mut self.username,
            Field::Password => &mut self.password,
        }
    }

    fn type_text(&mut self, text: &str) {
        if self.signing {
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
        if self.signing {
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

        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            l.left,
            l.pass_label,
            "Password",
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
        let bullets = "•".repeat(self.password.chars().count().min(64));
        painter.text(
            &fonts.regular,
            15.0 * l.scale,
            (x + pad) as f32,
            (pass_y + field_h as i32 / 2 + (15.0 * l.scale) as i32 / 3) as f32,
            &bullets,
            FOREGROUND,
        );
        if self.focus == Field::Password && self.caret_on {
            let caret_x =
                (x + pad) as f32 + painter.text_width(&fonts.regular, 15.0 * l.scale, &bullets);
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
            "Sign in",
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
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            28.0 * l.scale,
            bottom,
            &format!("Keyboard: {}", self.layout.to_uppercase()),
            MUTED,
        );
        let off = self.power_rect(w, h, PowerAction::PowerOff);
        let reboot = self.power_rect(w, h, PowerAction::Reboot);
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            reboot.0 as f32,
            bottom,
            "Restart",
            MUTED,
        );
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            off.0 as f32,
            bottom,
            "Power off",
            MUTED,
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

fn focus_login_window(window: &winit::window::Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let raw = handle.as_raw();
    let xid = match raw {
        RawWindowHandle::Xlib(handle) => handle.window as u32,
        RawWindowHandle::Xcb(handle) => handle.window.get(),
        _ => return,
    };
    let Ok((connection, screen)) = x11rb::connect(None) else {
        return;
    };
    let _ = screen;
    let _ = connection.set_input_focus(
        x11rb::protocol::xproto::InputFocus::PARENT,
        xid,
        x11rb::CURRENT_TIME,
    );
    let _ = connection.flush();
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

    let layout = std::env::var("NIRULOGIN_LAYOUT").unwrap_or_else(|_| "se".into());
    let _ = Command::new("setxkbmap")
        .args(["-layout", &layout])
        .status();
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
    focus_login_window(&window);
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
    let mut ui = Ui::new();
    let mut next_tick = Instant::now() + Duration::from_millis(500);
    let mut exit_after_draw = false;
    let focus_until = Instant::now() + Duration::from_secs(3);

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
                        focus_login_window(&window);
                        let size = window.inner_size();
                        if let Some(action) = ui.hit_power(size.width, size.height, ui.mouse) {
                            ui.pending_power = Some(action);
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
                            );
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if event.state != ElementState::Pressed || ui.signing {
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
                            } else {
                                submit(
                                    &mut ui,
                                    preview,
                                    &window,
                                    &mut painter,
                                    &fonts,
                                    &mut exit_after_draw,
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
                let now = Instant::now();
                if now < focus_until {
                    focus_login_window(&window);
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
    let command = match action {
        PowerAction::PowerOff => "poweroff",
        PowerAction::Reboot => "reboot",
    };
    let result = Command::new("loginctl").arg(command).status();
    match result {
        Ok(status) if status.success() => {}
        _ => {
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
}

fn submit(
    ui: &mut Ui,
    preview: bool,
    window: &winit::window::Window,
    painter: &mut Painter,
    fonts: &Fonts,
    exit_after_draw: &mut bool,
) {
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
    ui.draw(painter, fonts);
    window.request_redraw();
    let username = ui.username.trim().to_string();
    let password = ui.password.clone();
    let configured_session = std::env::var("NIRULOGIN_SESSION")
        .unwrap_or_else(|_| format!("/home/{username}/.local/bin/niruwm-session"));
    let start_session = if Path::new("/usr/local/bin/nirulogin-session").is_file() {
        "/usr/local/bin/nirulogin-session".to_string()
    } else {
        configured_session.clone()
    };
    match greetd::login(
        &username,
        &password,
        &start_session,
        &ui.layout,
        &configured_session,
    ) {
        Ok(()) => {
            *exit_after_draw = true;
            window.request_redraw();
        }
        Err(error) => {
            ui.password.zeroize();
            ui.password.clear();
            ui.signing = false;
            ui.error = error;
            ui.set_focus(Field::Password);
            window.request_redraw();
        }
    }
}
