//! Power off / restart through login1, off the UI thread.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    PowerOff,
    Reboot,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PowerResult {
    /// login1 accepted the request; the system is going down.
    Started,
    /// polkit refused, normally because other users are logged in
    /// (`*-multiple-sessions` needs authentication).
    Denied,
    Failed,
    TimedOut,
}

pub const DEADLINE: Duration = Duration::from_secs(10);

/// Errors that mean "not authorized" rather than "broken".
pub fn is_denial(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("interactiveauthorizationrequired")
        || s.contains("accessdenied")
        || s.contains("not authorized")
        || s.contains("interactive authentication required")
        || s.contains("access denied")
        || s.contains("authentication is required")
        || s.contains("users logged in")
        || s.contains("other users")
}

enum Outcome {
    Ok,
    Err(String),
    TimedOut,
}

fn run_until(mut command: Command, deadline: Instant) -> Outcome {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child: Child = match child {
        Ok(child) => child,
        Err(error) => return Outcome::Err(error.to_string()),
    };
    // Bounded wait on this worker thread only, for at most DEADLINE once per
    // confirmed power action; the UI thread never waits here.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return if status.success() {
                    Outcome::Ok
                } else {
                    Outcome::Err(stderr)
                };
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Outcome::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Outcome::Err(error.to_string()),
        }
    }
}

/// Ask login1 (then systemctl, then loginctl) within one overall deadline.
pub fn run(action: PowerAction) -> PowerResult {
    let deadline = Instant::now() + DEADLINE;
    let (method, verb) = match action {
        PowerAction::PowerOff => ("PowerOff", "poweroff"),
        PowerAction::Reboot => ("Reboot", "reboot"),
    };
    let mut dbus = Command::new("dbus-send");
    dbus.args([
        "--system",
        "--print-reply",
        "--reply-timeout=9000",
        "--dest=org.freedesktop.login1",
        "/org/freedesktop/login1",
        &format!("org.freedesktop.login1.Manager.{method}"),
        // interactive=true lets polkit decide; with no agent at the greeter a
        // rule-less action is refused immediately instead of hanging.
        "boolean:true",
    ]);
    let mut denied = false;
    let mut systemctl = Command::new("systemctl");
    systemctl.arg(verb);
    let mut loginctl = Command::new("loginctl");
    loginctl.arg(verb);
    for command in [dbus, systemctl, loginctl] {
        match run_until(command, deadline) {
            Outcome::Ok => return PowerResult::Started,
            Outcome::TimedOut => return PowerResult::TimedOut,
            Outcome::Err(stderr) => denied |= is_denial(&stderr),
        }
        if Instant::now() >= deadline {
            return PowerResult::TimedOut;
        }
    }
    if denied {
        PowerResult::Denied
    } else {
        PowerResult::Failed
    }
}

pub fn message(action: PowerAction, result: &PowerResult) -> Option<String> {
    let verb = match action {
        PowerAction::PowerOff => "power off",
        PowerAction::Reboot => "restart",
    };
    match result {
        PowerResult::Started => None,
        PowerResult::Denied => Some(match action {
            PowerAction::PowerOff => {
                "Other users are logged in. Power off from a session instead.".into()
            }
            PowerAction::Reboot => {
                "Other users are logged in. Restart from a session instead.".into()
            }
        }),
        PowerResult::Failed => Some(format!("Could not {verb}")),
        PowerResult::TimedOut => Some(format!("Could not {verb}: no answer within 10 s")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polkit_refusals_are_recognised() {
        assert!(is_denial(
            "Error org.freedesktop.DBus.Error.InteractiveAuthorizationRequired: Interactive authentication required."
        ));
        assert!(is_denial(
            "Failed to power off system via logind: Access denied"
        ));
        assert!(is_denial(
            "There are other users logged in, and they need to be logged out first"
        ));
        assert!(!is_denial(
            "Failed to connect to bus: No such file or directory"
        ));
    }

    #[test]
    fn denial_message_names_the_session_route() {
        assert_eq!(
            message(PowerAction::PowerOff, &PowerResult::Denied).unwrap(),
            "Other users are logged in. Power off from a session instead."
        );
        assert_eq!(message(PowerAction::Reboot, &PowerResult::Started), None);
    }

    #[test]
    fn a_hung_command_is_killed_at_the_deadline() {
        let started = Instant::now();
        let mut command = Command::new("sleep");
        command.arg("30");
        let outcome = run_until(command, Instant::now() + Duration::from_millis(300));
        assert!(matches!(outcome, Outcome::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// The shipped polkit rule must not grant the multiple-sessions actions.
    #[test]
    fn polkit_rule_allows_only_single_session_actions() {
        let rule = include_str!("../nirulogin-power.rules");
        let start = rule.find("var allowed").expect("allow list");
        let end = start + rule[start..].find("];").expect("allow list end");
        let allowed = &rule[start..end];
        assert!(!allowed.contains("multiple-sessions"), "{allowed}");
        assert!(allowed.contains("\"org.freedesktop.login1.power-off\""));
        assert!(allowed.contains("\"org.freedesktop.login1.reboot\""));
        assert!(!rule.contains("-multiple-sessions\""), "{rule}");
        assert!(rule.contains("subject.local") && rule.contains("subject.active"));
    }
}
