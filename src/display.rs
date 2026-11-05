//! X11 helpers: monitor geometry (RandR), Caps Lock state and input focus.
//!
//! Everything here is event driven. The RandR watcher blocks in
//! `wait_for_event` on its own connection and wakes the UI only when the
//! screen layout changes; Caps Lock is read once per key/focus event.

use x11rb::connection::Connection;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, InputFocus, KeyButMask, Window};
use x11rb::rust_connection::RustConnection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w as i32 && py < self.y + self.h as i32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// Size of the X root window; the greeter window covers all of it.
    pub width: u32,
    pub height: u32,
    pub monitors: Vec<Rect>,
    /// The monitor that carries the login form.
    pub form: Rect,
}

/// Choose the form monitor: the RandR primary, else the one under the
/// pointer, else the first. Monitors are clipped to the root so a stale or
/// odd RandR layout can never place the form off screen.
pub fn plan_screen(
    width: u32,
    height: u32,
    monitors: &[(Rect, bool)],
    pointer: Option<(i32, i32)>,
) -> Screen {
    let root = Rect {
        x: 0,
        y: 0,
        w: width.max(1),
        h: height.max(1),
    };
    let clipped: Vec<(Rect, bool)> = monitors
        .iter()
        .filter_map(|(rect, primary)| clip(*rect, root).map(|r| (r, *primary)))
        .collect();
    if clipped.is_empty() {
        return Screen {
            width: root.w,
            height: root.h,
            monitors: vec![root],
            form: root,
        };
    }
    let form = clipped
        .iter()
        .find(|(_, primary)| *primary)
        .or_else(|| pointer.and_then(|(px, py)| clipped.iter().find(|(r, _)| r.contains(px, py))))
        .unwrap_or(&clipped[0])
        .0;
    Screen {
        width: root.w,
        height: root.h,
        monitors: clipped.into_iter().map(|(r, _)| r).collect(),
        form,
    }
}

fn clip(rect: Rect, root: Rect) -> Option<Rect> {
    let x0 = rect.x.max(root.x);
    let y0 = rect.y.max(root.y);
    let x1 = (rect.x + rect.w as i32).min(root.x + root.w as i32);
    let y1 = (rect.y + rect.h as i32).min(root.y + root.h as i32);
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0,
        y: y0,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    })
}

pub fn caps_lock_from_mask(mask: u16) -> bool {
    mask & u16::from(KeyButMask::LOCK) != 0
}

pub struct X11 {
    conn: RustConnection,
    root: Window,
    randr: bool,
}

impl X11 {
    pub fn connect() -> Option<Self> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        let randr = conn
            .randr_query_version(1, 5)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|v| v.major_version > 1 || (v.major_version == 1 && v.minor_version >= 5))
            .unwrap_or(false);
        Some(Self { conn, root, randr })
    }

    pub fn screen(&self) -> Option<Screen> {
        let geometry = self.conn.get_geometry(self.root).ok()?.reply().ok()?;
        let mut monitors = Vec::new();
        if self.randr {
            if let Ok(Ok(reply)) = self
                .conn
                .randr_get_monitors(self.root, true)
                .map(|c| c.reply())
            {
                for m in reply.monitors {
                    monitors.push((
                        Rect {
                            x: i32::from(m.x),
                            y: i32::from(m.y),
                            w: u32::from(m.width),
                            h: u32::from(m.height),
                        },
                        m.primary,
                    ));
                }
            }
        }
        let pointer = self
            .conn
            .query_pointer(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|p| (i32::from(p.root_x), i32::from(p.root_y)));
        Some(plan_screen(
            u32::from(geometry.width),
            u32::from(geometry.height),
            &monitors,
            pointer,
        ))
    }

    pub fn caps_lock(&self) -> bool {
        self.conn
            .query_pointer(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|p| caps_lock_from_mask(u16::from(p.mask)))
            .unwrap_or(false)
    }

    /// Keep keyboard focus on the login window without spawning a helper:
    /// one round trip, and a SetInputFocus only when focus moved away.
    pub fn keep_focus(&self, window: Window) {
        let Ok(Ok(focus)) = self.conn.get_input_focus().map(|c| c.reply()) else {
            return;
        };
        if focus.focus == window {
            return;
        }
        if let Ok(cookie) =
            self.conn
                .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
        {
            // Checked, so a BadMatch before the window is viewable is consumed
            // here instead of queueing on a connection nobody reads events from.
            let _ = cookie.check();
        }
    }
}

/// Block on RandR screen-change notifications and call `notify` for each.
/// Runs on its own thread and connection; returns when X goes away.
pub fn watch_screen_changes(notify: impl Fn() -> bool) {
    let Some(x) = X11::connect() else {
        return;
    };
    if !x.randr {
        return;
    }
    let mask = randr::NotifyMask::SCREEN_CHANGE
        | randr::NotifyMask::CRTC_CHANGE
        | randr::NotifyMask::OUTPUT_CHANGE;
    if x.conn
        .randr_select_input(x.root, mask)
        .ok()
        .and_then(|c| c.check().ok())
        .is_none()
    {
        return;
    }
    let _ = x.conn.flush();
    while let Ok(event) = x.conn.wait_for_event() {
        use x11rb::protocol::Event;
        if matches!(
            event,
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_)
        ) {
            // Coalesce a burst (one change emits several notifications).
            while let Ok(Some(_)) = x.conn.poll_for_event() {}
            if !notify() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn primary_monitor_carries_the_form() {
        let left = r(0, 0, 2560, 1440);
        let right = r(2560, 0, 2560, 1440);
        let s = plan_screen(5120, 1440, &[(left, false), (right, true)], Some((10, 10)));
        assert_eq!(s.width, 5120);
        assert_eq!(s.form, right);
        assert_eq!(s.monitors, vec![left, right]);
    }

    #[test]
    fn pointer_monitor_is_used_without_a_primary() {
        let left = r(0, 0, 1920, 1080);
        let right = r(1920, 0, 2560, 1440);
        let s = plan_screen(
            4480,
            1440,
            &[(left, false), (right, false)],
            Some((3000, 700)),
        );
        assert_eq!(s.form, right);
        let s = plan_screen(4480, 1440, &[(left, false), (right, false)], None);
        assert_eq!(s.form, left);
    }

    #[test]
    fn no_randr_means_one_monitor_covering_the_root() {
        let s = plan_screen(1366, 768, &[], None);
        assert_eq!(s.monitors, vec![r(0, 0, 1366, 768)]);
        assert_eq!(s.form, r(0, 0, 1366, 768));
    }

    #[test]
    fn monitors_are_clipped_to_the_root() {
        let s = plan_screen(
            1920,
            1080,
            &[
                (r(-100, 0, 2020, 1080), true),
                (r(5000, 0, 100, 100), false),
            ],
            None,
        );
        assert_eq!(s.monitors, vec![r(0, 0, 1920, 1080)]);
        assert_eq!(s.form, r(0, 0, 1920, 1080));
    }

    #[test]
    fn caps_lock_state_from_the_core_modifier_mask() {
        let lock = u16::from(KeyButMask::LOCK);
        let shift = u16::from(KeyButMask::SHIFT);
        assert!(caps_lock_from_mask(lock));
        assert!(caps_lock_from_mask(lock | shift));
        assert!(!caps_lock_from_mask(shift));
        assert!(!caps_lock_from_mask(0));
    }
}
