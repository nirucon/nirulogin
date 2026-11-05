mod display;
mod greetd;
mod paint;
mod power;
mod theme;
mod users;

use chrono::Local;
use display::{Rect, Screen, X11};
use paint::{Fonts, Painter};
use power::{PowerAction, PowerResult};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};
use theme::{Color, Image, Theme};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    keyboard::{Key, NamedKey},
    window::WindowBuilder,
};
use zeroize::{Zeroize, Zeroizing};

const APP_NAME: &str = "NIRUX";
const TAGLINE: &str = "A minimal X11 desktop";
const DEFAULT_LAYOUT: &str = "se";
/// Reserved so typing never reallocates the password (a reallocation would
/// leave an unwiped copy of the prefix in freed memory).
const SECRET_CAPACITY: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Username,
    Password,
    Layout,
    PowerOff,
    Reboot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    Cancel,
    Confirm,
}

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

enum AuthOutcome {
    Success,
    Cancelled,
    AuthFailed,
    Error(String),
}

enum UserEvent {
    Prompt(PamPrompt),
    Info(String),
    Done(AuthOutcome),
    Power(PowerAction, PowerResult),
    ScreenChanged,
}

struct Ui {
    theme: Theme,
    username: String,
    password: Zeroizing<String>,
    focus: Field,
    error: String,
    /// Second line under the error: the last PAM/greetd message (faillock).
    detail: String,
    notice: String,
    pam_prompt: Option<PamPrompt>,
    pam_message: String,
    signing: bool,
    cancelling: bool,
    failed_attempts: u32,
    clock: String,
    date: String,
    caret_on: bool,
    caps_lock: bool,
    ctrl: bool,
    shift: bool,
    mouse: (f64, f64),
    layouts: Vec<String>,
    layout: String,
    pending_power: Option<PowerAction>,
    choice: Choice,
    powering: Option<PowerAction>,
}

impl Ui {
    fn new(theme: Theme, layouts: Vec<String>, layout: String, username: String) -> Self {
        let now = Local::now();
        let focus = if username.is_empty() {
            Field::Username
        } else {
            Field::Password
        };
        Self {
            theme,
            username,
            password: Zeroizing::new(String::with_capacity(SECRET_CAPACITY)),
            focus,
            error: String::new(),
            detail: String::new(),
            notice: String::new(),
            pam_prompt: None,
            pam_message: String::new(),
            signing: false,
            cancelling: false,
            failed_attempts: 0,
            clock: now.format("%H:%M").to_string(),
            date: now.format("%A %-d %B").to_string(),
            caret_on: true,
            caps_lock: false,
            ctrl: false,
            shift: false,
            mouse: (0.0, 0.0),
            layouts,
            layout,
            pending_power: None,
            choice: Choice::Cancel,
            powering: None,
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

    fn focus_order(&self) -> Vec<Field> {
        let mut order = vec![Field::Username, Field::Password];
        if self.layouts.len() > 1 {
            order.push(Field::Layout);
        }
        order.extend([Field::PowerOff, Field::Reboot]);
        order
    }

    fn move_focus(&mut self, backwards: bool) {
        let order = self.focus_order();
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if backwards {
            (at + order.len() - 1) % order.len()
        } else {
            (at + 1) % order.len()
        };
        self.set_focus(order[next]);
    }

    fn clear_password(&mut self) {
        self.password.zeroize();
        self.password.clear();
    }

    fn type_text(&mut self, text: &str) {
        if (self.signing && self.pam_prompt.is_none())
            || !matches!(self.focus, Field::Username | Field::Password)
        {
            return;
        }
        let target: &mut String = match self.focus {
            Field::Username => &mut self.username,
            _ => &mut self.password,
        };
        for ch in text.chars() {
            if !ch.is_control() && target.len() + ch.len_utf8() <= SECRET_CAPACITY {
                target.push(ch);
            }
        }
        self.caret_on = true;
        self.error.clear();
        self.detail.clear();
    }

    fn backspace(&mut self) {
        if (self.signing && self.pam_prompt.is_none())
            || !matches!(self.focus, Field::Username | Field::Password)
        {
            return;
        }
        match self.focus {
            Field::Username => {
                self.username.pop();
            }
            _ => {
                self.password.pop();
            }
        }
        self.caret_on = true;
        self.error.clear();
        self.detail.clear();
    }

    fn layout(&self, width: u32, height: u32) -> Layout {
        let scale = scale_for(height);
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
        let button_y = pass_y + field_h + 30.0 * scale;
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

    fn password_placeholder(&self) -> String {
        if self.pam_prompt.is_some() {
            "Response".into()
        } else {
            format!("Password · {}", self.layout.to_uppercase())
        }
    }

    fn draw(&mut self, painter: &mut Painter, fonts: &Fonts, backdrop: Option<&[u32]>) {
        let t = self.theme.clone();
        let w = painter.width();
        let h = painter.height();
        let l = self.layout(w, h);
        painter.clear_with(backdrop, t.background);
        let center = w as f32 / 2.0;

        painter.centered_text(
            &fonts.regular,
            44.0 * l.scale,
            center,
            l.clock_baseline,
            &self.clock,
            t.foreground,
        );
        painter.centered_text(
            &fonts.regular,
            14.0 * l.scale,
            center,
            l.date_baseline,
            &self.date,
            t.muted,
        );
        painter.centered_spaced_text(
            &fonts.bold,
            38.0 * l.scale,
            center,
            l.title_baseline,
            APP_NAME,
            t.foreground,
            4.0 * l.scale,
        );
        painter.centered_text(
            &fonts.regular,
            14.0 * l.scale,
            center,
            l.tagline_baseline,
            TAGLINE,
            t.muted,
        );

        let x = l.left as i32;
        let cw = l.column_w as u32;
        let field_h = l.field_h as u32;
        let pad = (14.0 * l.scale) as i32;
        let text_size = 15.0 * l.scale;

        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            l.left,
            l.user_label,
            "Username",
            t.muted,
        );
        let user_y = l.user_y as i32;
        painter.fill(x, user_y, cw, field_h, t.surface);
        painter.border(
            x,
            user_y,
            cw,
            field_h,
            if self.focus == Field::Username {
                t.accent
            } else {
                t.border
            },
        );
        let baseline = user_y + field_h as i32 / 2 + (15.0 * l.scale) as i32 / 3;
        painter.text(
            &fonts.regular,
            text_size,
            (x + pad) as f32,
            baseline as f32,
            &self.username,
            t.foreground,
        );
        if self.focus == Field::Username && self.caret_on {
            let caret_x =
                (x + pad) as f32 + painter.text_width(&fonts.regular, text_size, &self.username);
            painter.fill(
                caret_x as i32,
                user_y + (10.0 * l.scale) as i32,
                1,
                (field_h as f32 - 20.0 * l.scale) as u32,
                t.foreground,
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
            t.muted,
        );
        let pass_y = l.pass_y as i32;
        painter.fill(x, pass_y, cw, field_h, t.surface);
        painter.border(
            x,
            pass_y,
            cw,
            field_h,
            if self.focus == Field::Password {
                t.accent
            } else {
                t.border
            },
        );
        let visible = self.pam_prompt.as_ref().map(|p| !p.secret).unwrap_or(false);
        // A visible PAM prompt (not a secret) is shown as typed; the copy is
        // still wiped on drop.
        let shown = Zeroizing::new(if visible {
            self.password.as_str().to_owned()
        } else {
            "•".repeat(self.password.chars().count().min(64))
        });
        let pass_baseline = (pass_y + field_h as i32 / 2 + (15.0 * l.scale) as i32 / 3) as f32;
        if self.password.is_empty() {
            painter.text(
                &fonts.regular,
                text_size,
                (x + pad) as f32 + 2.0,
                pass_baseline,
                &self.password_placeholder(),
                t.border.mix(t.muted, 0.55),
            );
        } else {
            painter.text(
                &fonts.regular,
                text_size,
                (x + pad) as f32,
                pass_baseline,
                &shown,
                t.foreground,
            );
        }
        if self.focus == Field::Password && self.caret_on {
            let caret_x = (x + pad) as f32 + painter.text_width(&fonts.regular, text_size, &shown);
            painter.fill(
                caret_x as i32,
                pass_y + (10.0 * l.scale) as i32,
                1,
                (field_h as f32 - 20.0 * l.scale) as u32,
                t.foreground,
            );
        }
        if self.caps_lock {
            painter.text(
                &fonts.regular,
                11.0 * l.scale,
                l.left,
                pass_y as f32 + l.field_h + 19.0 * l.scale,
                "Caps Lock is on",
                t.error,
            );
        }

        let button_y = l.button_y as i32;
        let button_h = l.button_h as u32;
        let busy = self.signing && self.pam_prompt.is_none();
        if busy {
            painter.fill(x, button_y, cw, button_h, t.surface);
            painter.border(x, button_y, cw, button_h, t.border);
        } else {
            painter.fill(x, button_y, cw, button_h, t.accent);
        }
        painter.centered_text(
            &fonts.bold,
            15.0 * l.scale,
            center,
            (button_y + button_h as i32 / 2 + 5) as f32,
            if busy {
                "Signing in…"
            } else {
                "Press Enter to sign in"
            },
            if busy { t.muted } else { t.foreground },
        );
        let message_y = (button_y + button_h as i32 + 28) as f32;
        if !self.error.is_empty() {
            painter.centered_text(
                &fonts.regular,
                12.0 * l.scale,
                center,
                message_y,
                &self.error,
                t.error,
            );
            if !self.detail.is_empty() {
                painter.centered_text(
                    &fonts.regular,
                    12.0 * l.scale,
                    center,
                    message_y + 20.0 * l.scale,
                    &self.detail,
                    t.muted,
                );
            }
        } else if !self.notice.is_empty() {
            painter.centered_text(
                &fonts.regular,
                12.0 * l.scale,
                center,
                message_y,
                &self.notice,
                t.foreground,
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
                if self.focus == Field::Layout {
                    t.accent
                } else {
                    t.border
                },
            );
        }
        painter.text(
            &fonts.regular,
            12.0 * l.scale,
            28.0 * l.scale,
            bottom,
            &self.language_label(),
            if self.focus == Field::Layout {
                t.foreground
            } else {
                t.muted
            },
        );
        for (action, field, label) in [
            (PowerAction::PowerOff, Field::PowerOff, "Power off"),
            (PowerAction::Reboot, Field::Reboot, "Restart"),
        ] {
            let rect = self.power_rect(w, h, action);
            let focused = self.focus == field;
            if focused {
                painter.border(
                    rect.0,
                    rect.1,
                    rect.2.max(1) as u32,
                    rect.3.max(1) as u32,
                    t.accent,
                );
            }
            let color = if focused { t.foreground } else { t.muted };
            let icon_cx = rect.0 as f32 + 14.0 * l.scale;
            let icon_cy = rect.1 as f32 + rect.3 as f32 / 2.0;
            draw_power_icon(painter, action, icon_cx, icon_cy, l.scale, color);
            painter.text(
                &fonts.regular,
                12.0 * l.scale,
                rect.0 as f32 + 26.0 * l.scale,
                bottom,
                label,
                color,
            );
        }

        if let Some(action) = self.pending_power {
            let (px, py, pw, ph) = self.dialog_rect(w, h);
            painter.fill(px, py, pw as u32, ph as u32, t.surface);
            painter.border(px, py, pw as u32, ph as u32, t.border);
            let label = match action {
                PowerAction::PowerOff => "Power off?",
                PowerAction::Reboot => "Restart?",
            };
            painter.centered_text(
                &fonts.bold,
                22.0 * l.scale,
                center,
                py as f32 + 40.0 * l.scale,
                label,
                t.foreground,
            );
            painter.centered_text(
                &fonts.regular,
                12.0 * l.scale,
                center,
                py as f32 + 64.0 * l.scale,
                "Tab to choose · Enter to select · Esc to cancel",
                t.muted,
            );
            for choice in [Choice::Cancel, Choice::Confirm] {
                let (bx, by, bw, bh) = self.dialog_button(w, h, choice);
                let focused = self.choice == choice;
                painter.fill(
                    bx,
                    by,
                    bw as u32,
                    bh as u32,
                    if focused { t.accent } else { t.background },
                );
                painter.border(
                    bx,
                    by,
                    bw as u32,
                    bh as u32,
                    if focused { t.foreground } else { t.border },
                );
                let text = match (choice, action) {
                    (Choice::Cancel, _) => "Cancel",
                    (Choice::Confirm, PowerAction::PowerOff) => "Power off",
                    (Choice::Confirm, PowerAction::Reboot) => "Restart",
                };
                painter.centered_text(
                    &fonts.regular,
                    13.0 * l.scale,
                    bx as f32 + bw as f32 / 2.0,
                    by as f32 + bh as f32 / 2.0 + 4.5 * l.scale,
                    text,
                    if focused { t.foreground } else { t.muted },
                );
            }
        }
    }

    fn dialog_rect(&self, width: u32, height: u32) -> (i32, i32, i32, i32) {
        let scale = scale_for(height);
        let pw = 380.0 * scale;
        let ph = 140.0 * scale;
        (
            (width as f32 / 2.0 - pw / 2.0) as i32,
            (height as f32 / 2.0 - ph / 2.0) as i32,
            pw as i32,
            ph as i32,
        )
    }

    fn dialog_button(&self, width: u32, height: u32, choice: Choice) -> (i32, i32, i32, i32) {
        let scale = scale_for(height);
        let (px, py, pw, ph) = self.dialog_rect(width, height);
        let bw = (130.0 * scale) as i32;
        let bh = (34.0 * scale) as i32;
        let gap = (16.0 * scale) as i32;
        let left = px + (pw - 2 * bw - gap) / 2;
        let y = py + ph - bh - (18.0 * scale) as i32;
        match choice {
            Choice::Cancel => (left, y, bw, bh),
            Choice::Confirm => (left + bw + gap, y, bw, bh),
        }
    }

    fn hit_dialog(&self, width: u32, height: u32, pos: (f64, f64)) -> Option<Choice> {
        [Choice::Cancel, Choice::Confirm]
            .into_iter()
            .find(|c| inside(self.dialog_button(width, height, *c), pos))
    }

    fn language_rect(&self, width: u32, height: u32) -> (i32, i32, i32, i32) {
        let scale = scale_for(height);
        let x = (16.0 * scale) as i32;
        let y = height as i32 - (54.0 * scale) as i32;
        let w = ((300.0 * scale) as i32).min(width as i32 - x);
        (x, y, w, (30.0 * scale) as i32)
    }

    fn language_label(&self) -> String {
        let code = self.layout.to_uppercase();
        if self.layouts.len() > 1 {
            format!("Keyboard: {code} · click or F2 to change")
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
            self.detail.clear();
        } else {
            self.notice = format!(
                "Could not switch to keyboard layout {}",
                next.to_uppercase()
            );
        }
    }

    fn hit_language(&self, width: u32, height: u32, pos: (f64, f64)) -> bool {
        inside(self.language_rect(width, height), pos)
    }

    fn power_rect(&self, width: u32, height: u32, action: PowerAction) -> (i32, i32, i32, i32) {
        let scale = scale_for(height);
        let off_w = (100.0 * scale) as i32;
        let restart_w = (90.0 * scale) as i32;
        let gap = (12.0 * scale) as i32;
        let right = width as i32 - (20.0 * scale) as i32;
        let reboot_x = right - restart_w;
        let off_x = reboot_x - gap - off_w;
        let y = height as i32 - (52.0 * scale) as i32;
        let h = (28.0 * scale) as i32;
        match action {
            PowerAction::PowerOff => (off_x, y, off_w, h),
            PowerAction::Reboot => (reboot_x, y, restart_w, h),
        }
    }

    fn hit_power(&self, width: u32, height: u32, pos: (f64, f64)) -> Option<PowerAction> {
        [PowerAction::PowerOff, PowerAction::Reboot]
            .into_iter()
            .find(|a| inside(self.power_rect(width, height, *a), pos))
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

    fn open_power_dialog(&mut self, action: PowerAction) {
        self.focus = match action {
            PowerAction::PowerOff => Field::PowerOff,
            PowerAction::Reboot => Field::Reboot,
        };
        self.pending_power = Some(action);
        self.choice = Choice::Cancel;
    }

    fn failure_text(&mut self, outcome: AuthOutcome) {
        match outcome {
            AuthOutcome::AuthFailed => {
                self.failed_attempts += 1;
                self.error = if self.failed_attempts == 1 {
                    "Incorrect username or password".into()
                } else {
                    format!(
                        "Incorrect username or password · {} failed attempts",
                        self.failed_attempts
                    )
                };
            }
            AuthOutcome::Error(message) => self.error = message,
            AuthOutcome::Cancelled => {
                self.error.clear();
                self.notice = "Sign-in cancelled".into();
            }
            AuthOutcome::Success => {}
        }
    }
}

fn scale_for(height: u32) -> f32 {
    (height as f32 / 1200.0).clamp(0.72, 1.15)
}

fn inside(rect: (i32, i32, i32, i32), pos: (f64, f64)) -> bool {
    let (x, y, w, h) = rect;
    pos.0 >= f64::from(x)
        && pos.0 <= f64::from(x + w)
        && pos.1 >= f64::from(y)
        && pos.1 <= f64::from(y + h)
}

fn draw_power_icon(
    painter: &mut Painter,
    action: PowerAction,
    cx: f32,
    cy: f32,
    scale: f32,
    color: Color,
) {
    let r = 5.5 * scale;
    let t = 1.5 * scale;
    match action {
        PowerAction::PowerOff => {
            painter.ring(cx, cy, r, t, 0.0, 0.65, color);
            painter.fill(
                (cx - t / 2.0).round() as i32,
                (cy - r - 1.5 * scale).round() as i32,
                t.round().max(1.0) as u32,
                (r + 1.0 * scale).round() as u32,
                color,
            );
        }
        PowerAction::Reboot => {
            let gap_at = 0.35;
            let gap = 0.5;
            painter.ring(cx, cy, r, t, gap_at, gap, color);
            // Arrow head at the arc end just left of the gap, pointing clockwise.
            let theta: f32 = gap_at - gap;
            let (s, c) = theta.sin_cos();
            let p = (cx + r * s, cy - r * c);
            let tangent = (c, s);
            let normal = (s, -c);
            let head = 3.2 * scale;
            painter.triangle(
                (p.0 + tangent.0 * head, p.1 + tangent.1 * head),
                (p.0 + normal.0 * head, p.1 + normal.1 * head),
                (p.0 - normal.0 * head, p.1 - normal.1 * head),
                color,
            );
        }
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

/// The full window contents: every monitor's background, with the login
/// form drawn on `screen.form`.
struct Scene {
    screen: Screen,
    frame: Vec<u32>,
    form_backdrop: Option<Vec<u32>>,
    /// The whole frame must be presented (not only the form) next time.
    full: bool,
}

impl Scene {
    fn build(screen: Screen, theme: &Theme, wallpaper: Option<&Image>) -> Self {
        let (w, h) = (screen.width as usize, screen.height as usize);
        let mut frame = vec![theme.background.pixel(); w * h];
        let mut form_backdrop = None;
        if let Some(image) = wallpaper {
            for m in &screen.monitors {
                let scaled = theme::cover(image, m.w, m.h, theme.dim);
                blit(&mut frame, screen.width, m, &scaled);
                if *m == screen.form {
                    form_backdrop = Some(scaled);
                }
            }
        }
        Self {
            screen,
            frame,
            form_backdrop,
            full: true,
        }
    }
}

fn blit(dst: &mut [u32], dst_w: u32, at: &Rect, src: &[u32]) {
    for row in 0..at.h as usize {
        let d = (at.y as usize + row) * dst_w as usize + at.x as usize;
        let s = row * at.w as usize;
        dst[d..d + at.w as usize].copy_from_slice(&src[s..s + at.w as usize]);
    }
}

fn window_id(window: &winit::window::Window) -> Option<u32> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok(),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    }
}

fn current_screen(x11: Option<&X11>, window_size: PhysicalSize<u32>) -> Screen {
    let size = (window_size.width.max(1), window_size.height.max(1));
    match x11.and_then(|x| x.screen()) {
        // Only when the window really covers the root do monitor positions
        // map to window pixels; otherwise (a WM resized the preview) the
        // window is treated as one monitor.
        Some(screen) if (screen.width, screen.height) == size => screen,
        _ => display::plan_screen(size.0, size.1, &[], None),
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
    let theme = Theme::load();
    let wallpaper = theme.wallpaper.as_deref().and_then(theme::load_png);
    if theme.wallpaper.is_some() && wallpaper.is_none() {
        eprintln!("nirulogin: theme wallpaper is not a readable PNG; using colors only");
    }
    let event_loop = match EventLoopBuilder::<UserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("nirulogin: {error}");
            std::process::exit(1);
        }
    };
    let x11 = X11::connect();
    let initial = x11.as_ref().and_then(|x| x.screen());
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
    // Cover the whole X screen (every monitor); the form goes on one of them.
    if let Some(screen) = &initial {
        let _ = window.request_inner_size(PhysicalSize::new(screen.width, screen.height));
        window.set_outer_position(PhysicalPosition::new(0, 0));
    } else if let Some(monitor) = window
        .current_monitor()
        .or_else(|| event_loop.primary_monitor())
    {
        let _ = window.request_inner_size(monitor.size());
        window.set_outer_position(PhysicalPosition::new(
            monitor.position().x,
            monitor.position().y,
        ));
    }
    window.set_visible(true);
    let xid = window_id(&window);
    if let (Some(x), Some(id)) = (&x11, xid) {
        x.keep_focus(id);
    }
    window.set_cursor_visible(true);

    let proxy = event_loop.create_proxy();
    {
        let proxy = proxy.clone();
        thread::spawn(move || {
            display::watch_screen_changes(|| proxy.send_event(UserEvent::ScreenChanged).is_ok())
        });
    }

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
    let mut scene = Scene::build(
        current_screen(x11.as_ref(), window.inner_size()),
        &theme,
        wallpaper.as_ref(),
    );
    let mut painter = Painter::new(scene.screen.form.w, scene.screen.form.h);
    let username = users::prefill_username();
    let mut ui = Ui::new(theme, layouts, layout, username);
    ui.caps_lock = x11.as_ref().map(|x| x.caps_lock()).unwrap_or(false);
    let mut next_tick = Instant::now() + Duration::from_millis(500);
    let mut exit_after_draw = false;
    let mut reply_tx: Option<Sender<Zeroizing<String>>> = None;

    let _ = event_loop.run(move |event, elwt| {
        let _ = &context;
        elwt.set_control_flow(ControlFlow::WaitUntil(next_tick));
        let form = scene.screen.form;
        let local = |pos: (f64, f64)| (pos.0 - f64::from(form.x), pos.1 - f64::from(form.y));
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(_) => {
                    scene = Scene::build(
                        current_screen(x11.as_ref(), window.inner_size()),
                        &ui.theme,
                        wallpaper.as_ref(),
                    );
                    window.request_redraw();
                }
                WindowEvent::Focused(true) => {
                    if let Some(x) = &x11 {
                        ui.caps_lock = x.caps_lock();
                    }
                    window.request_redraw();
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    ui.ctrl = modifiers.state().control_key();
                    ui.shift = modifiers.state().shift_key();
                }
                WindowEvent::CursorMoved { position, .. } => {
                    ui.mouse = (position.x, position.y);
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if state != ElementState::Pressed || button != MouseButton::Left {
                        return;
                    }
                    if let (Some(x), Some(id)) = (&x11, xid) {
                        x.keep_focus(id);
                    }
                    if ui.powering.is_some() {
                        return;
                    }
                    let (w, h) = (form.w, form.h);
                    let pos = local(ui.mouse);
                    if let Some(action) = ui.pending_power {
                        match ui.hit_dialog(w, h, pos) {
                            Some(Choice::Confirm) => {
                                start_power(&mut ui, action, &proxy);
                            }
                            Some(Choice::Cancel) => ui.pending_power = None,
                            None => {}
                        }
                        window.request_redraw();
                    } else if let Some(action) = ui.hit_power(w, h, pos) {
                        ui.open_power_dialog(action);
                        window.request_redraw();
                    } else if ui.hit_language(w, h, pos) {
                        ui.cycle_layout();
                        window.request_redraw();
                    } else if let Some(field) = ui.hit_field(w, h, pos) {
                        ui.set_focus(field);
                        window.request_redraw();
                    } else if ui.hit_button(w, h, pos) {
                        submit(&mut ui, preview, &window, &proxy, &mut reply_tx);
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if let Some(x) = &x11 {
                        let caps = x.caps_lock();
                        if caps != ui.caps_lock {
                            ui.caps_lock = caps;
                            window.request_redraw();
                        }
                    }
                    if event.state != ElementState::Pressed || ui.powering.is_some() {
                        return;
                    }
                    if let Some(action) = ui.pending_power {
                        match event.logical_key {
                            Key::Named(
                                NamedKey::Tab
                                | NamedKey::ArrowLeft
                                | NamedKey::ArrowRight
                                | NamedKey::ArrowUp
                                | NamedKey::ArrowDown,
                            ) => {
                                ui.choice = match ui.choice {
                                    Choice::Cancel => Choice::Confirm,
                                    Choice::Confirm => Choice::Cancel,
                                };
                            }
                            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                                if ui.choice == Choice::Confirm {
                                    start_power(&mut ui, action, &proxy);
                                } else {
                                    ui.pending_power = None;
                                }
                            }
                            Key::Named(NamedKey::Escape) => ui.pending_power = None,
                            _ => {}
                        }
                        window.request_redraw();
                        return;
                    }
                    if ui.signing && ui.pam_prompt.is_none() {
                        return;
                    }
                    let layout_shortcut = matches!(event.logical_key, Key::Named(NamedKey::F2))
                        || (ui.ctrl && matches!(event.logical_key, Key::Named(NamedKey::Space)));
                    if layout_shortcut {
                        // Handled before text input, so it is never typed.
                        ui.cycle_layout();
                        window.request_redraw();
                        return;
                    }
                    let on_control =
                        matches!(ui.focus, Field::Layout | Field::PowerOff | Field::Reboot);
                    match event.logical_key {
                        Key::Named(NamedKey::Tab) => {
                            ui.move_focus(ui.shift);
                            window.request_redraw();
                        }
                        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) if on_control => {
                            match ui.focus {
                                Field::Layout => ui.cycle_layout(),
                                Field::PowerOff => ui.open_power_dialog(PowerAction::PowerOff),
                                Field::Reboot => ui.open_power_dialog(PowerAction::Reboot),
                                _ => {}
                            }
                            window.request_redraw();
                        }
                        Key::Named(NamedKey::Enter) => {
                            submit(&mut ui, preview, &window, &proxy, &mut reply_tx);
                        }
                        Key::Named(NamedKey::Backspace) => {
                            ui.backspace();
                            window.request_redraw();
                        }
                        Key::Named(NamedKey::Escape) => {
                            if ui.pam_prompt.is_some() {
                                // Dropping the reply channel makes the auth
                                // thread send CancelSession to greetd.
                                reply_tx = None;
                                ui.pam_prompt = None;
                                ui.clear_password();
                                ui.cancelling = true;
                                ui.notice = "Cancelling…".into();
                                ui.error.clear();
                                ui.detail.clear();
                            } else {
                                ui.clear_password();
                                ui.error.clear();
                                ui.detail.clear();
                                ui.set_focus(Field::Password);
                            }
                            window.request_redraw();
                        }
                        _ => {
                            if let Some(text) = event.text.as_ref() {
                                if !text.is_empty() && !ui.ctrl {
                                    ui.type_text(text);
                                    window.request_redraw();
                                }
                            }
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    let size = window.inner_size();
                    let (width, height) = (size.width.max(1), size.height.max(1));
                    if (width, height) != (scene.screen.width, scene.screen.height) {
                        scene = Scene::build(
                            current_screen(x11.as_ref(), size),
                            &ui.theme,
                            wallpaper.as_ref(),
                        );
                    }
                    let form = scene.screen.form;
                    if (form.w, form.h) != (painter.width(), painter.height()) {
                        painter.resize(form.w, form.h);
                    }
                    if surface
                        .resize(
                            NonZeroU32::new(width).unwrap(),
                            NonZeroU32::new(height).unwrap(),
                        )
                        .is_err()
                    {
                        return;
                    }
                    ui.draw(&mut painter, &fonts, scene.form_backdrop.as_deref());
                    if let Ok(mut buffer) = surface.buffer_mut() {
                        if buffer.len() == scene.frame.len() {
                            if scene.full || buffer.age() == 0 {
                                // The other monitors' backgrounds come from the
                                // cached frame; with one monitor the form is
                                // the whole frame and that copy is skipped.
                                let whole =
                                    form.w == scene.screen.width && form.h == scene.screen.height;
                                if whole {
                                    buffer.copy_from_slice(painter.pixels());
                                } else {
                                    buffer.copy_from_slice(&scene.frame);
                                    blit(&mut buffer, scene.screen.width, &form, painter.pixels());
                                }
                                scene.full = false;
                                let _ = buffer.present();
                            } else {
                                // Only the form monitor changes between frames.
                                blit(&mut buffer, scene.screen.width, &form, painter.pixels());
                                let damage = softbuffer::Rect {
                                    x: form.x as u32,
                                    y: form.y as u32,
                                    width: NonZeroU32::new(form.w).unwrap(),
                                    height: NonZeroU32::new(form.h).unwrap(),
                                };
                                let _ = buffer.present_with_damage(&[damage]);
                            }
                        }
                    }
                    if exit_after_draw {
                        elwt.exit();
                    }
                }
                _ => {}
            },
            Event::UserEvent(user_event) => {
                match user_event {
                    UserEvent::Prompt(prompt) => {
                        ui.pam_prompt = Some(prompt);
                        ui.clear_password();
                        ui.error.clear();
                        ui.detail.clear();
                        ui.signing = true;
                        ui.set_focus(Field::Password);
                    }
                    UserEvent::Info(message) => {
                        ui.notice = message.clone();
                        ui.pam_message = message;
                    }
                    UserEvent::Done(outcome) => {
                        reply_tx = None;
                        ui.clear_password();
                        ui.pam_prompt = None;
                        ui.cancelling = false;
                        match outcome {
                            AuthOutcome::Success => {
                                let name = ui.username.trim().to_string();
                                if let Err(error) =
                                    users::write_last_user(Path::new(users::STATE_DIR), &name)
                                {
                                    eprintln!("nirulogin: cannot remember last user: {error}");
                                }
                                exit_after_draw = true;
                            }
                            other => {
                                ui.signing = false;
                                ui.notice.clear();
                                ui.detail = std::mem::take(&mut ui.pam_message);
                                ui.failure_text(other);
                                ui.set_focus(Field::Password);
                            }
                        }
                    }
                    UserEvent::Power(action, result) => {
                        ui.powering = None;
                        ui.notice.clear();
                        if let Some(message) = power::message(action, &result) {
                            ui.error = message;
                            ui.detail.clear();
                        } else {
                            ui.notice = match action {
                                PowerAction::PowerOff => "Powering off…".into(),
                                PowerAction::Reboot => "Restarting…".into(),
                            };
                            // Keep input blocked while the system goes down.
                            ui.powering = Some(action);
                        }
                    }
                    UserEvent::ScreenChanged => {
                        if let Some(screen) = x11.as_ref().and_then(|x| x.screen()) {
                            let _ = window
                                .request_inner_size(PhysicalSize::new(screen.width, screen.height));
                            window.set_outer_position(PhysicalPosition::new(0, 0));
                        }
                        scene = Scene::build(
                            current_screen(x11.as_ref(), window.inner_size()),
                            &ui.theme,
                            wallpaper.as_ref(),
                        );
                    }
                }
                window.request_redraw();
            }
            Event::AboutToWait => {
                let now = Instant::now();
                if !ui.signing && ui.pending_power.is_none() {
                    if let (Some(x), Some(id)) = (&x11, xid) {
                        x.keep_focus(id);
                    }
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

fn start_power(ui: &mut Ui, action: PowerAction, proxy: &EventLoopProxy<UserEvent>) {
    ui.pending_power = None;
    ui.powering = Some(action);
    ui.error.clear();
    ui.detail.clear();
    ui.notice = match action {
        PowerAction::PowerOff => "Powering off…".into(),
        PowerAction::Reboot => "Restarting…".into(),
    };
    let proxy = proxy.clone();
    thread::spawn(move || {
        let result = power::run(action);
        let _ = proxy.send_event(UserEvent::Power(action, result));
    });
}

fn session_paths(username: &str) -> (String, String) {
    let configured = std::env::var("NIRULOGIN_SESSION").unwrap_or_else(|_| {
        users::home_dir(username)
            .map(|home| {
                home.join(".local/bin/niruwm-session")
                    .to_string_lossy()
                    .into_owned()
            })
            // Unknown here: nirulogin-session falls back to $HOME at start.
            .unwrap_or_default()
    });
    let start = if Path::new("/usr/local/bin/nirulogin-session").is_file() {
        "/usr/local/bin/nirulogin-session".to_string()
    } else {
        configured.clone()
    };
    (start, configured)
}

fn submit(
    ui: &mut Ui,
    preview: bool,
    window: &winit::window::Window,
    proxy: &EventLoopProxy<UserEvent>,
    reply_tx: &mut Option<Sender<Zeroizing<String>>>,
) {
    if ui.pam_prompt.is_some() {
        // Moved into the channel as Zeroizing: the auth thread wipes it after
        // it has been written to greetd.
        let answer = Zeroizing::new(ui.password.as_str().to_owned());
        ui.clear_password();
        let message = ui.pam_prompt.take().map(|p| p.message).unwrap_or_default();
        match reply_tx.take() {
            Some(tx) if tx.send(answer).is_ok() => {
                ui.notice = message;
                ui.error.clear();
                ui.detail.clear();
                ui.signing = true;
                ui.set_focus(Field::Password);
            }
            _ => {
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
        ui.detail.clear();
        ui.set_focus(Field::Username);
        window.request_redraw();
        return;
    }
    if ui.password.is_empty() {
        ui.error = "Enter your password".into();
        ui.detail.clear();
        ui.set_focus(Field::Password);
        window.request_redraw();
        return;
    }
    if preview {
        ui.error = "Preview mode: authentication is disabled".into();
        ui.detail.clear();
        window.request_redraw();
        return;
    }
    ui.signing = true;
    ui.error.clear();
    ui.detail.clear();
    ui.notice.clear();
    ui.pam_message.clear();
    window.request_redraw();
    let username = ui.username.trim().to_string();
    // The thread's copy; Zeroizing wipes it when the thread ends.
    let password = Zeroizing::new(ui.password.as_str().to_owned());
    let layout = ui.layout.clone();
    let (start_session, configured_session) = session_paths(&username);
    let (reply_sender, reply_receiver) = mpsc::channel::<Zeroizing<String>>();
    *reply_tx = Some(reply_sender);
    let events = proxy.clone();
    thread::spawn(move || {
        let request = greetd::LoginRequest {
            username: &username,
            password: &password,
            session: &start_session,
            layout: &layout,
            configured_session: &configured_session,
        };
        let result = greetd::login(&request, |prompt| {
            let (message, secret) = match prompt {
                greetd::Prompt::Info(message) | greetd::Prompt::Error(message) => {
                    let _ = events.send_event(UserEvent::Info(message));
                    return None;
                }
                greetd::Prompt::Secret(message) => (message, true),
                greetd::Prompt::Visible(message) => (message, false),
            };
            let _ = events.send_event(UserEvent::Prompt(PamPrompt { message, secret }));
            // Esc in the UI drops the sender: recv fails and we cancel.
            reply_receiver.recv().ok()
        });
        let outcome = match result {
            Ok(()) => AuthOutcome::Success,
            Err(greetd::LoginError::Cancelled) => AuthOutcome::Cancelled,
            Err(greetd::LoginError::Auth) => AuthOutcome::AuthFailed,
            Err(greetd::LoginError::Other(message)) => AuthOutcome::Error(message),
        };
        let _ = events.send_event(UserEvent::Done(outcome));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(layouts: &[&str], user: &str) -> Ui {
        Ui::new(
            Theme::default(),
            layouts.iter().map(|s| s.to_string()).collect(),
            layouts[0].to_string(),
            user.to_string(),
        )
    }

    #[test]
    fn layout_codes_are_normalised_and_comments_ignored() {
        assert_eq!(normalize_layout(" SE "), Some("se".into()));
        assert_eq!(normalize_layout("us-intl"), Some("us-intl".into()));
        assert_eq!(normalize_layout("se; rm -rf /"), None);
        let mut layouts = Vec::new();
        add_layouts(&mut layouts, "se # svenska\nus, gb\nse\n");
        assert_eq!(layouts, vec!["se", "us", "gb"]);
    }

    #[test]
    fn focus_starts_on_username_when_nothing_is_prefilled() {
        assert_eq!(ui(&["se"], "").focus, Field::Username);
        assert_eq!(ui(&["se"], "alice").focus, Field::Password);
    }

    #[test]
    fn tab_reaches_the_layout_switch_and_power_controls() {
        let mut u = ui(&["se", "us"], "alice");
        let mut seen = vec![u.focus];
        for _ in 0..5 {
            u.move_focus(false);
            seen.push(u.focus);
        }
        assert_eq!(
            seen,
            [
                Field::Password,
                Field::Layout,
                Field::PowerOff,
                Field::Reboot,
                Field::Username,
                Field::Password
            ]
        );
        u.move_focus(true);
        assert_eq!(u.focus, Field::Username);
        // With a single layout there is nothing to switch, so no stop.
        let mut single = ui(&["se"], "alice");
        single.move_focus(false);
        assert_eq!(single.focus, Field::PowerOff);
    }

    #[test]
    fn text_is_never_typed_into_controls() {
        let mut u = ui(&["se", "us"], "alice");
        u.set_focus(Field::Layout);
        u.type_text(" x");
        assert!(u.password.is_empty());
        u.set_focus(Field::Password);
        u.type_text("pw");
        assert_eq!(u.password.as_str(), "pw");
        assert_eq!(u.password_placeholder(), "Password · SE");
    }

    #[test]
    fn power_dialog_defaults_to_cancel() {
        let mut u = ui(&["se"], "alice");
        u.choice = Choice::Confirm;
        u.open_power_dialog(PowerAction::Reboot);
        assert_eq!(u.choice, Choice::Cancel);
        assert_eq!(u.focus, Field::Reboot);
        assert_eq!(u.pending_power, Some(PowerAction::Reboot));
    }

    #[test]
    fn failed_attempts_are_counted() {
        let mut u = ui(&["se"], "alice");
        u.failure_text(AuthOutcome::AuthFailed);
        assert_eq!(u.error, "Incorrect username or password");
        u.failure_text(AuthOutcome::AuthFailed);
        assert_eq!(
            u.error,
            "Incorrect username or password · 2 failed attempts"
        );
        u.failure_text(AuthOutcome::Cancelled);
        assert_eq!(u.notice, "Sign-in cancelled");
        assert_eq!(u.failed_attempts, 2);
    }

    #[test]
    fn scene_paints_every_monitor_and_places_the_form() {
        let left = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: 20,
        };
        let right = Rect {
            x: 40,
            y: 0,
            w: 30,
            h: 20,
        };
        let screen = display::plan_screen(70, 20, &[(left, false), (right, true)], None);
        let theme = Theme::default();
        let image = Image {
            width: 1,
            height: 1,
            pixels: vec![0xffffff],
        };
        let scene = Scene::build(screen, &theme, Some(&image));
        assert_eq!(scene.frame.len(), 70 * 20);
        assert_eq!(scene.screen.form, right);
        assert_eq!(scene.form_backdrop.as_ref().unwrap().len(), 30 * 20);
        // Both monitors carry the darkened wallpaper, not the bare root.
        assert!(scene.frame.iter().all(|&p| p == scene.frame[0] && p != 0));
        let plain = Scene::build(
            display::plan_screen(70, 20, &[(left, false), (right, true)], None),
            &theme,
            None,
        );
        assert!(plain.frame.iter().all(|&p| p == theme.background.pixel()));
    }
}
