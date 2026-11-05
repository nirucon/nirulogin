//! greetd IPC client.
//!
//! greetd owns PAM and the session. This module only relays the conversation:
//! it never authenticates by itself and it starts a session only after greetd
//! reported `Success` for the authentication.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use zeroize::Zeroizing;

/// Largest message accepted from greetd. Real messages are a few hundred bytes.
const MAX_MESSAGE: usize = 1024 * 1024;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request<'a> {
    CreateSession { username: &'a str },
    // Borrowed so the secret is not copied into another owned String.
    PostAuthMessageResponse { response: Option<&'a str> },
    StartSession { cmd: Vec<String>, env: Vec<String> },
    CancelSession,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Success,
    Error {
        error_type: ErrorType,
        description: String,
    },
    AuthMessage {
        auth_message_type: AuthMessageType,
        auth_message: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorType {
    AuthError,
    Error,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

pub enum Prompt {
    Secret(String),
    Visible(String),
    Info(String),
    Error(String),
}

/// The answer to an interactive prompt. `None` cancels the sign-in.
pub type Answer = Option<Zeroizing<String>>;

#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    /// The user cancelled an extra prompt; `CancelSession` was sent.
    Cancelled,
    /// PAM rejected the credentials.
    Auth,
    /// greetd or the transport reported an error.
    Other(String),
}

pub struct LoginRequest<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub session: &'a str,
    pub layout: &'a str,
    pub configured_session: &'a str,
}

impl LoginRequest<'_> {
    pub fn start_cmd(&self) -> Vec<String> {
        vec!["/usr/bin/startx".into(), self.session.into()]
    }

    pub fn start_env(&self) -> Vec<String> {
        vec![
            "XDG_SESSION_TYPE=x11".into(),
            "XDG_CURRENT_DESKTOP=NIRU".into(),
            "DESKTOP_SESSION=niruwm".into(),
            format!("NIRULOGIN_LAYOUT={}", self.layout),
            format!("NIRULOGIN_SESSION={}", self.configured_session),
        ]
    }
}

fn write_message(stream: &mut UnixStream, request: &Request) -> Result<(), String> {
    // Reserve up front so serde_json does not reallocate (and leave a partial
    // copy of a secret response in freed memory) while it serializes. The
    // buffer itself is wiped when `payload` drops.
    let mut payload = Zeroizing::new(Vec::with_capacity(4096));
    serde_json::to_writer(&mut *payload, request).map_err(|e| e.to_string())?;
    let len = u32::try_from(payload.len()).map_err(|_| "greetd message too long".to_string())?;
    stream
        .write_all(&len.to_ne_bytes())
        .and_then(|_| stream.write_all(&payload))
        .map_err(|e| e.to_string())
}

fn read_message(stream: &mut UnixStream) -> Result<Response, String> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).map_err(|e| e.to_string())?;
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err("greetd sent an oversized message".into());
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
    serde_json::from_slice(&payload).map_err(|e| e.to_string())
}

fn cancel(stream: &mut UnixStream) {
    if write_message(stream, &Request::CancelSession).is_ok() {
        // greetd acknowledges the cancel; the reply carries nothing we need.
        let _ = read_message(stream);
    }
}

/// Sign in through the greetd socket named by `GREETD_SOCK`.
pub fn login<F>(request: &LoginRequest, prompt: F) -> Result<(), LoginError>
where
    F: FnMut(Prompt) -> Answer,
{
    let socket = std::env::var_os("GREETD_SOCK")
        .ok_or_else(|| LoginError::Other("NIRULOGIN must be started by greetd".into()))?;
    login_at(Path::new(&socket), request, prompt)
}

pub fn login_at<F>(socket: &Path, request: &LoginRequest, prompt: F) -> Result<(), LoginError>
where
    F: FnMut(Prompt) -> Answer,
{
    let mut stream = UnixStream::connect(socket).map_err(|e| LoginError::Other(e.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(30))))
        .map_err(|e| LoginError::Other(e.to_string()))?;
    let result = converse(&mut stream, request, prompt);
    if let Err(error) = &result {
        // greetd's own Error reply has already ended its side; every other
        // failure (cancel, transport, oversized message) leaves a session
        // open that must be cancelled before the socket is dropped.
        if !matches!(error, LoginError::Auth) {
            cancel(&mut stream);
        }
    }
    result
}

fn converse<F>(
    stream: &mut UnixStream,
    request: &LoginRequest,
    mut prompt: F,
) -> Result<(), LoginError>
where
    F: FnMut(Prompt) -> Answer,
{
    let other = LoginError::Other;
    write_message(
        stream,
        &Request::CreateSession {
            username: request.username,
        },
    )
    .map_err(other)?;

    let mut answered_password = false;
    let mut authenticated = false;
    loop {
        match read_message(stream).map_err(other)? {
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => match auth_message_type {
                AuthMessageType::Secret | AuthMessageType::Visible => {
                    let secret = matches!(auth_message_type, AuthMessageType::Secret);
                    if !answered_password && secret {
                        answered_password = true;
                        write_message(
                            stream,
                            &Request::PostAuthMessageResponse {
                                response: Some(request.password),
                            },
                        )
                        .map_err(other)?;
                    } else {
                        let requested = if secret {
                            Prompt::Secret(auth_message)
                        } else {
                            Prompt::Visible(auth_message)
                        };
                        let Some(answer) = prompt(requested) else {
                            return Err(LoginError::Cancelled);
                        };
                        write_message(
                            stream,
                            &Request::PostAuthMessageResponse {
                                response: Some(answer.as_str()),
                            },
                        )
                        .map_err(other)?;
                    }
                }
                AuthMessageType::Info | AuthMessageType::Error => {
                    let message = if matches!(auth_message_type, AuthMessageType::Info) {
                        Prompt::Info(auth_message)
                    } else {
                        Prompt::Error(auth_message)
                    };
                    let _ = prompt(message);
                    write_message(stream, &Request::PostAuthMessageResponse { response: None })
                        .map_err(other)?;
                }
            },
            Response::Success => {
                if authenticated {
                    return Ok(());
                }
                authenticated = true;
                write_message(
                    stream,
                    &Request::StartSession {
                        cmd: request.start_cmd(),
                        env: request.start_env(),
                    },
                )
                .map_err(other)?;
            }
            Response::Error {
                error_type,
                description,
            } => {
                return match error_type {
                    ErrorType::AuthError => {
                        // greetd ends the session itself after an auth error,
                        // but cancelling keeps the socket state unambiguous.
                        cancel(stream);
                        Err(LoginError::Auth)
                    }
                    ErrorType::Error => Err(LoginError::Other(if description.is_empty() {
                        "The login service reported an error".into()
                    } else {
                        description
                    })),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct FakeGreetd {
        path: PathBuf,
        handle: Option<thread::JoinHandle<Vec<Value>>>,
    }

    fn send(stream: &mut UnixStream, value: &Value) {
        let bytes = serde_json::to_vec(value).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_ne_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
    }

    fn recv(stream: &mut UnixStream) -> Option<Value> {
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).ok()?;
        let mut payload = vec![0u8; u32::from_ne_bytes(len) as usize];
        stream.read_exact(&mut payload).ok()?;
        serde_json::from_slice(&payload).ok()
    }

    fn auth(kind: &str, message: &str) -> Value {
        json!({"type": "auth_message", "auth_message_type": kind, "auth_message": message})
    }

    fn success() -> Value {
        json!({"type": "success"})
    }

    /// A greetd stand-in on a temporary socket (the path a real greeter gets in
    /// `GREETD_SOCK`). `script` receives every request and returns the reply,
    /// or `None` to hang up. All requests are returned for assertions.
    impl FakeGreetd {
        fn start<S>(mut script: S) -> Self
        where
            S: FnMut(&Value) -> Option<Value> + Send + 'static,
        {
            let path = std::env::temp_dir().join(format!(
                "nirulogin-test-{}-{}.sock",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).unwrap();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut seen = Vec::new();
                while let Some(request) = recv(&mut stream) {
                    let reply = script(&request);
                    seen.push(request);
                    match reply {
                        Some(reply) => send(&mut stream, &reply),
                        None => break,
                    }
                }
                seen
            });
            Self {
                path,
                handle: Some(handle),
            }
        }

        fn finish(mut self) -> Vec<Value> {
            self.handle.take().unwrap().join().unwrap()
        }
    }

    impl Drop for FakeGreetd {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn request() -> LoginRequest<'static> {
        LoginRequest {
            username: "alice",
            password: "correct horse",
            session: "/usr/local/bin/nirulogin-session",
            layout: "se",
            configured_session: "/srv/home/alice/.local/bin/niruwm-session",
        }
    }

    fn kinds(seen: &[Value]) -> Vec<String> {
        seen.iter()
            .map(|v| v["type"].as_str().unwrap().to_string())
            .collect()
    }

    /// Standard script: password prompt, then accept, then start the session.
    fn normal(request: &Value) -> Option<Value> {
        match request["type"].as_str().unwrap() {
            "create_session" => Some(auth("secret", "Password: ")),
            "post_auth_message_response" => Some(success()),
            "start_session" => Some(success()),
            "cancel_session" => Some(success()),
            _ => None,
        }
    }

    #[test]
    fn success_with_one_secret_prompt_and_start_session_payload() {
        let server = FakeGreetd::start(normal);
        let mut prompts = 0;
        let result = login_at(&server.path, &request(), |_| {
            prompts += 1;
            None
        });
        assert_eq!(result, Ok(()));
        assert_eq!(prompts, 0, "the first secret prompt takes the password");
        let seen = server.finish();
        assert_eq!(
            kinds(&seen),
            [
                "create_session",
                "post_auth_message_response",
                "start_session"
            ]
        );
        assert_eq!(seen[0]["username"], "alice");
        assert_eq!(seen[1]["response"], "correct horse");
        assert_eq!(
            seen[2]["cmd"],
            json!(["/usr/bin/startx", "/usr/local/bin/nirulogin-session"])
        );
        assert_eq!(
            seen[2]["env"],
            json!([
                "XDG_SESSION_TYPE=x11",
                "XDG_CURRENT_DESKTOP=NIRU",
                "DESKTOP_SESSION=niruwm",
                "NIRULOGIN_LAYOUT=se",
                "NIRULOGIN_SESSION=/srv/home/alice/.local/bin/niruwm-session"
            ])
        );
    }

    #[test]
    fn auth_error_is_reported_and_no_session_is_started() {
        let server = FakeGreetd::start(|request| match request["type"].as_str().unwrap() {
            "create_session" => Some(auth("secret", "Password: ")),
            "post_auth_message_response" => Some(json!({
                "type": "error", "error_type": "auth_error", "description": "pam_authenticate: AUTH_ERR"
            })),
            "cancel_session" => Some(success()),
            _ => None,
        });
        let result = login_at(&server.path, &request(), |_| None);
        assert_eq!(result, Err(LoginError::Auth));
        let seen = server.finish();
        assert!(!kinds(&seen).contains(&"start_session".to_string()));
    }

    #[test]
    fn extra_secret_and_visible_prompts_reach_the_ui() {
        let mut step = 0;
        let server = FakeGreetd::start(move |request| {
            step += 1;
            match (step, request["type"].as_str().unwrap()) {
                (1, "create_session") => Some(auth("secret", "Password: ")),
                (2, _) => Some(auth("visible", "Username for OTP: ")),
                (3, _) => Some(auth("secret", "OTP code: ")),
                (4, _) => Some(success()),
                (5, "start_session") => Some(success()),
                _ => None,
            }
        });
        let mut asked = Vec::new();
        let result = login_at(&server.path, &request(), |prompt| {
            let (kind, message, answer) = match prompt {
                Prompt::Visible(m) => ("visible", m, "alice-otp"),
                Prompt::Secret(m) => ("secret", m, "123456"),
                _ => unreachable!(),
            };
            asked.push((kind, message));
            Some(Zeroizing::new(answer.to_string()))
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            asked,
            [
                ("visible", "Username for OTP: ".to_string()),
                ("secret", "OTP code: ".to_string())
            ]
        );
        let seen = server.finish();
        assert_eq!(seen[1]["response"], "correct horse");
        assert_eq!(seen[2]["response"], "alice-otp");
        assert_eq!(seen[3]["response"], "123456");
        assert_eq!(kinds(&seen).last().unwrap(), "start_session");
    }

    #[test]
    fn info_and_error_messages_are_relayed_without_a_response() {
        let mut step = 0;
        let server = FakeGreetd::start(move |_| {
            step += 1;
            match step {
                1 => Some(auth("info", "Welcome")),
                2 => Some(auth(
                    "error",
                    "The account is locked due to 3 failed logins.",
                )),
                3 => Some(auth("secret", "Password: ")),
                4 | 5 => Some(success()),
                _ => None,
            }
        });
        let mut messages = Vec::new();
        let result = login_at(&server.path, &request(), |prompt| {
            match prompt {
                Prompt::Info(m) => messages.push(format!("info:{m}")),
                Prompt::Error(m) => messages.push(format!("error:{m}")),
                _ => unreachable!(),
            }
            None
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            messages,
            [
                "info:Welcome",
                "error:The account is locked due to 3 failed logins."
            ]
        );
        let seen = server.finish();
        assert_eq!(seen[1]["response"], Value::Null);
        assert_eq!(seen[2]["response"], Value::Null);
        assert_eq!(seen[3]["response"], "correct horse");
    }

    #[test]
    fn escape_on_an_extra_prompt_sends_cancel_session() {
        let server = FakeGreetd::start(|request| match request["type"].as_str().unwrap() {
            "create_session" => Some(auth("secret", "Password: ")),
            "post_auth_message_response" => Some(auth("secret", "OTP code: ")),
            "cancel_session" => Some(success()),
            _ => None,
        });
        // The UI drops its reply channel on Esc; the closure then answers None.
        let result = login_at(&server.path, &request(), |_| None);
        assert_eq!(result, Err(LoginError::Cancelled));
        let seen = server.finish();
        assert_eq!(
            kinds(&seen),
            [
                "create_session",
                "post_auth_message_response",
                "cancel_session"
            ]
        );
    }

    #[test]
    fn oversized_message_is_rejected_and_cancelled() {
        let path = std::env::temp_dir().join(format!(
            "nirulogin-test-big-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let first = recv(&mut stream).unwrap();
            stream
                .write_all(&((MAX_MESSAGE as u32) + 1).to_ne_bytes())
                .unwrap();
            let second = recv(&mut stream);
            (first, second)
        });
        let result = login_at(&path, &request(), |_| None);
        let (first, second) = handle.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            result,
            Err(LoginError::Other("greetd sent an oversized message".into()))
        );
        assert_eq!(first["type"], "create_session");
        assert_eq!(second.unwrap()["type"], "cancel_session");
    }

    #[test]
    fn missing_socket_variable_is_a_clear_error() {
        if std::env::var_os("GREETD_SOCK").is_some() {
            return;
        }
        assert_eq!(
            login(&request(), |_| None),
            Err(LoginError::Other(
                "NIRULOGIN must be started by greetd".into()
            ))
        );
    }
}
