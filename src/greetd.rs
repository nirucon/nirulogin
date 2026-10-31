use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
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

fn write_message(stream: &mut UnixStream, request: &Request) -> Result<(), String> {
    let payload = serde_json::to_vec(request).map_err(|e| e.to_string())?;
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
    if len > 1024 * 1024 {
        return Err("greetd sent an oversized message".into());
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
    serde_json::from_slice(&payload).map_err(|e| e.to_string())
}

pub fn login<F>(
    username: &str,
    password: &str,
    session: &str,
    layout: &str,
    configured_session: &str,
    mut prompt: F,
) -> Result<(), String>
where
    F: FnMut(Prompt) -> Result<Option<String>, String>,
{
    let socket = std::env::var("GREETD_SOCK")
        .map_err(|_| "NIRULOGIN must be started by greetd".to_string())?;
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(30))))
        .map_err(|e| e.to_string())?;
    write_message(
        &mut stream,
        &Request::CreateSession {
            username: username.to_string(),
        },
    )?;

    let mut answered_password = false;
    let mut authenticated = false;
    loop {
        match read_message(&mut stream)? {
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                let response = match auth_message_type {
                    AuthMessageType::Secret | AuthMessageType::Visible => {
                        let secret = matches!(auth_message_type, AuthMessageType::Secret);
                        let answer = if !answered_password && secret {
                            answered_password = true;
                            password.to_string()
                        } else {
                            let requested = if secret {
                                Prompt::Secret(auth_message)
                            } else {
                                Prompt::Visible(auth_message)
                            };
                            match prompt(requested)? {
                                Some(value) => value,
                                None => {
                                    let _ = write_message(&mut stream, &Request::CancelSession);
                                    return Err("Authentication cancelled".into());
                                }
                            }
                        };
                        Some(answer)
                    }
                    AuthMessageType::Info => {
                        prompt(Prompt::Info(auth_message))?;
                        None
                    }
                    AuthMessageType::Error => {
                        prompt(Prompt::Error(auth_message))?;
                        None
                    }
                };
                write_message(&mut stream, &Request::PostAuthMessageResponse { response })?;
            }
            Response::Success => {
                if authenticated {
                    return Ok(());
                }
                authenticated = true;
                write_message(
                    &mut stream,
                    &Request::StartSession {
                        cmd: vec!["/usr/bin/startx".into(), session.into()],
                        env: vec![
                            "XDG_SESSION_TYPE=x11".into(),
                            "XDG_CURRENT_DESKTOP=NIRU".into(),
                            "DESKTOP_SESSION=niruwm".into(),
                            format!("NIRULOGIN_LAYOUT={layout}"),
                            format!("NIRULOGIN_SESSION={configured_session}"),
                        ],
                    },
                )?;
            }
            Response::Error {
                error_type,
                description,
            } => {
                let _ = write_message(&mut stream, &Request::CancelSession);
                return match error_type {
                    ErrorType::AuthError => Err("Incorrect username or password".into()),
                    ErrorType::Error => Err(if description.is_empty() {
                        "The login service reported an error".into()
                    } else {
                        description
                    }),
                };
            }
        }
    }
}
