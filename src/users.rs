//! Username prefill and home lookup.
//!
//! Order: `NIRULOGIN_USER`, then the last user who signed in successfully
//! (written only after greetd reported `Success`), then the only human account
//! when there is exactly one, otherwise an empty field.

use std::ffi::{CStr, CString};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const STATE_DIR: &str = "/var/lib/nirulogin";
const LAST_USER: &str = "last-user";

/// Usernames NIRULOGIN is willing to store or prefill. This matches what
/// useradd accepts by default and keeps the state file a single plain line.
pub fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '$'))
}

pub fn read_last_user(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(LAST_USER)).ok()?;
    let name = text.lines().next()?.trim();
    valid_username(name).then(|| name.to_string())
}

/// Atomically replace `last-user`. Called only after greetd's `Success`.
pub fn write_last_user(dir: &Path, name: &str) -> std::io::Result<()> {
    if !valid_username(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid username",
        ));
    }
    let tmp = dir.join(format!(".{LAST_USER}.tmp"));
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        writeln!(file, "{name}")?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(LAST_USER))
}

fn login_shell_ok(shell: &str, shells: Option<&str>) -> bool {
    if shell.is_empty() || shell.ends_with("/nologin") || shell.ends_with("/false") {
        return false;
    }
    match shells {
        Some(list) => list
            .lines()
            .map(|line| line.trim())
            .any(|line| !line.starts_with('#') && line == shell),
        None => true,
    }
}

/// Human accounts from passwd(5) text: uid ≥ 1000, not `nobody`/65534, and a
/// valid login shell (listed in /etc/shells when that file is readable).
pub fn human_users(passwd: &str, shells: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 {
            continue;
        }
        let (name, uid, shell) = (fields[0], fields[2], fields[6]);
        let Ok(uid) = uid.parse::<u32>() else {
            continue;
        };
        if uid < 1000 || uid == 65534 || name == "nobody" || !valid_username(name) {
            continue;
        }
        if login_shell_ok(shell, shells) && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// Choose the prefilled username from already-read inputs (testable).
pub fn choose_prefill(
    env_user: Option<String>,
    last_user: Option<String>,
    last_user_exists: impl Fn(&str) -> bool,
    humans: &[String],
) -> String {
    if let Some(user) = env_user {
        return user;
    }
    if let Some(user) = last_user {
        if last_user_exists(&user) {
            return user;
        }
    }
    if humans.len() == 1 {
        return humans[0].clone();
    }
    String::new()
}

pub fn prefill_username() -> String {
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let shells = std::fs::read_to_string("/etc/shells").ok();
    choose_prefill(
        std::env::var("NIRULOGIN_USER").ok(),
        read_last_user(Path::new(STATE_DIR)),
        |user| home_dir(user).is_some(),
        &human_users(&passwd, shells.as_deref()),
    )
}

/// The account's home directory from the system user database (NSS), so homes
/// outside `/home` and directory-service users resolve correctly.
pub fn home_dir(user: &str) -> Option<PathBuf> {
    let name = CString::new(user).ok()?;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live, correctly sized storage owned by
    // this frame; getpwnam_r only writes into `pwd` and `buf`.
    let rc = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into `buf`, NUL-terminated by getpwnam_r.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    let dir = dir.to_str().ok()?;
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
_greetd:x:104:108::/var/lib/greetd:/usr/sbin/nologin
alice:x:1000:1000:Alice:/srv/home/alice:/usr/bin/fish
nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin
svc:x:1001:1001::/var/svc:/bin/false
broken line
";
    const SHELLS: &str = "# /etc/shells\n/bin/sh\n/bin/bash\n/usr/bin/fish\n";

    #[test]
    fn single_human_is_found_in_a_passwd_fixture() {
        assert_eq!(human_users(PASSWD, Some(SHELLS)), ["alice"]);
        let two = format!("{PASSWD}bob:x:1002:1002::/home/bob:/bin/bash\n");
        assert_eq!(human_users(&two, Some(SHELLS)), ["alice", "bob"]);
        let unlisted = "carol:x:1003:1003::/home/carol:/opt/odd/shell\n";
        assert!(human_users(unlisted, Some(SHELLS)).is_empty());
        assert_eq!(human_users(unlisted, None), ["carol"]);
    }

    #[test]
    fn prefill_order_env_last_user_single_human_empty() {
        let one = vec!["alice".to_string()];
        let two = vec!["alice".to_string(), "bob".to_string()];
        let yes = |_: &str| true;
        let no = |_: &str| false;
        assert_eq!(
            choose_prefill(Some("env".into()), Some("bob".into()), yes, &one),
            "env"
        );
        assert_eq!(choose_prefill(None, Some("bob".into()), yes, &one), "bob");
        // A stored user that no longer exists falls through.
        assert_eq!(choose_prefill(None, Some("gone".into()), no, &one), "alice");
        assert_eq!(choose_prefill(None, None, yes, &two), "");
        assert_eq!(choose_prefill(None, None, yes, &[]), "");
    }

    #[test]
    fn last_user_round_trip_is_validated() {
        let dir = std::env::temp_dir().join(format!("nirulogin-users-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_last_user(&dir), None);
        write_last_user(&dir, "alice").unwrap();
        assert_eq!(read_last_user(&dir).as_deref(), Some("alice"));
        assert!(write_last_user(&dir, "bad\nname").is_err());
        std::fs::write(dir.join(LAST_USER), "../etc\n").unwrap();
        assert_eq!(read_last_user(&dir), None);
        use std::os::unix::fs::PermissionsExt;
        write_last_user(&dir, "bob").unwrap();
        let mode = std::fs::metadata(dir.join(LAST_USER))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn getpwnam_home_lookup() {
        assert_eq!(home_dir("root"), Some(PathBuf::from("/root")));
        assert_eq!(home_dir("no-such-user-nirulogin"), None);
        assert_eq!(home_dir("bad\0name"), None);
    }
}
