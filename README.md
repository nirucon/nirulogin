# NIRULOGIN

**Part of [NIRUX - A minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

Minimal graphical login for the NIRUX X11 desktop.

NIRULOGIN is a small X11 greeter for [greetd](https://github.com/kennylevinsen/greetd).
It has one job: sign in to NIRUWM. greetd owns PAM, authentication and the
session; NIRULOGIN owns the window, clock, prompt and error messages.

## Requirements

- Linux with X11
- `greetd`
- `xinit` / `startx`
- an Xorg server that can be started by the greeter user
- `setxkbmap` and `xkeyboard-config` for keyboard layouts

Install these with the distribution package manager. NIRUX installs and owns
the integration files.

## Build

```sh
cargo build --release
```

The release profile is LTO, stripped and self-contained apart from the system
X11/font libraries.

## Install

From a release directory, `./install.sh` uses the prebuilt `nirulogin` beside
it; from a source checkout it uses `target/release/nirulogin`.

```sh
sudo ./install.sh
```

The installer copies the binary to `/usr/local/bin/nirulogin`, installs the
polkit rule, creates `/var/lib/nirulogin` (owned by the greeter user, mode
0700) and writes a greetd configuration. It does not replace the active display manager unless
`--enable` is given.

```sh
sudo ./install.sh --enable
```

Keep SDDM/LightDM installed. They remain the fallback.

## Greetd configuration

```toml
[terminal]
vt = 7

[default_session]
command = "/usr/bin/startx /usr/local/bin/nirulogin -- :0 vt7"
user = "_greetd"
```

Debian's greeter user is `_greetd`; Arch uses `greeter`; Void uses `_greeter`.
NIRUX detects the user and writes the owned configuration.

The X server must be allowed to start for the greeter user. On Debian this is
`xserver-xorg-legacy` with an Xwrapper configuration; NIRUX manages that.

## Interaction

- **Username prefill:** `NIRULOGIN_USER` if set; otherwise the last user who
  signed in successfully (`/var/lib/nirulogin/last-user`, written by the greeter
  only after greetd reports `Success`); otherwise the only human account
  (uid ≥ 1000 with a valid login shell) when there is exactly one; otherwise the
  field is empty and has focus.
- The session path is resolved from the user database (`getpwnam`), so homes
  outside `/home` work.
- Password field has focus when a username is prefilled. Its placeholder shows
  the active layout, e.g. `Password · SE`.
- **Caps Lock is on** is shown under the password field while Caps Lock is
  active.
- Swedish (`se`) is the default layout and is passed to the session.
- The bottom-left keyboard indicator switches between the layouts in
  `/etc/nirulogin/layouts` (first entry is the default). Click it, focus it
  with `Tab` and press `Enter`/`Space`, or press `F2` or `Ctrl+Space` anywhere;
  the shortcut is never typed into a field. The selected layout is applied to
  the greeter and exported to the session.
- `Tab` / `Shift+Tab` move between username, password, keyboard layout,
  Power off and Restart.
- `Enter` signs in. While signing in the button is disabled and shows
  "Signing in…".
- After a failed sign-in the password is cleared, the username kept, the
  password focused, and the message shows the number of failed attempts in
  this greeter run plus PAM's own text when there is one (for example the
  faillock "account is locked" message).
- `Escape` clears the password, or cancels an extra PAM prompt (see below).
- **Power off** and **Restart** (icon and label, bottom right) open a
  confirmation dialog whose default button is **Cancel**; `Tab`/arrows switch
  buttons, `Enter` selects, `Escape` cancels. The action runs off the UI thread
  with a 10 s deadline and shows "Powering off…"/"Restarting…".
- Every monitor is covered with the background; the form is drawn on the
  RandR primary monitor, or on the monitor under the pointer when there is no
  primary, and is re-laid out when monitors change.
- The session is always NIRUWM.

## PAM conversation

greetd runs PAM; NIRULOGIN relays the conversation:

- The first secret prompt receives the password typed on the login screen.
- Any further prompt — secret (OTP, second factor, new password) or visible
  (shown as typed) — is shown in the password field as "Password / response"
  with PAM's prompt text above the button. `Enter` answers it.
- PAM info and error messages are shown under the button; an error message
  stays visible after a failed attempt.
- `Escape` during an extra prompt cancels the sign-in: NIRULOGIN sends
  `CancelSession` to greetd and shows "Sign-in cancelled".
- Every owned copy of a password or response is held in a zeroizing buffer and
  the serialized greetd message is wiped after it is written.

## Power actions and polkit

Power off and Restart use the login1 D-Bus API (with `systemctl`/`loginctl`
fallbacks). `nirulogin-power.rules` lets the local, active greeter user
(`_greetd`, `greeter` or `_greeter`) use `org.freedesktop.login1.power-off`
and `org.freedesktop.login1.reboot` without a password.

Since 0.2.0 the `*-multiple-sessions` actions are **not** granted. logind's
own policy allows them for any active local session, so the rule explicitly
requires administrator authentication for them; the greeter has no
authentication agent, so when other users are logged in (for example over SSH)
login1 refuses the request and the greeter shows "Other users are logged in.
Power off from a session instead."
Power off or restart from a logged-in session in that case.

## Theme

If `/etc/nirulogin/theme.toml` exists, its colors replace the built-in NIRUX
palette, and an optional PNG wallpaper is scaled to cover each monitor and
darkened. Colors are `"#rrggbb"` or `"rrggbb"`. The file must be readable by the greeter user; anything missing or
malformed falls back to the defaults, so without the file nothing changes.

```toml
background = "#0f0f10"
surface    = "#161618"
foreground = "#e5e5e5"
muted      = "#a8a8a8"
border     = "#2a2a2d"
accent     = "#5a5a60"
error      = "#d77a7a"
wallpaper  = "/usr/share/nirulogin/wallpaper.png"   # optional, PNG only
dim        = 0.45                                   # optional, 0 black .. 1 unchanged
```

NIRUX writes this file from the active theme during `nirux update`.
`NIRULOGIN_THEME=/path/to/theme.toml` selects another file (previews, tests).

## Scope

The greeter deliberately has no session picker, user avatars, animations,
network access or autologin UI. Keyboard layouts are a short curated list
rather than every XKB layout: edit `/etc/nirulogin/layouts` or set
`NIRULOGIN_LAYOUTS=se,us` for a display-manager-specific list.

## Test without switching the display manager

```sh
GREETD_SOCK=/tmp/fake.sock ./target/release/nirulogin
```

NIRULOGIN also accepts `--preview` for window-only testing. In preview mode the
password is accepted by the UI but authentication is disabled.

`cargo test` includes a fake greetd server on a temporary socket that covers
success, an auth error, extra secret/visible prompts, info/error messages,
Esc cancel (`CancelSession`), an oversized message and the `StartSession`
command and environment. CI also runs an Xvfb `--preview` screenshot smoke and
an MSRV (Rust 1.85) build.

## Keyboard layouts

`/etc/nirulogin/layouts` contains one XKB layout code per line; the first is the
default. NIRUX installs `se` followed by `us`, and never overwrites an existing
file. `NIRULOGIN_LAYOUTS` overrides the file for a single greeter configuration.
Only layout codes are accepted here; variants belong in the session's
`~/.config/niruwm/input.toml`.
