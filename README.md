# NIRU Login

Minimal X11 greeter for greetd that signs you in to NIRUWM.

**Status: alpha** · Part of the [NIRUX](https://github.com/nirucon/nirux) suite · [MIT License](LICENSE)

## About

NIRU Login (`nirulogin`) is a small X11 greeter for
[greetd](https://github.com/kennylevinsen/greetd). It has one job: sign in to
NIRUWM. greetd owns PAM, authentication and the session; NIRU Login owns the
window, clock, prompt and error messages.

It is the default login screen of a new NIRUX installation. It deliberately has
no session picker, user avatars, animations, network access or autologin.

## Features

- Username prefill: `NIRULOGIN_USER` if set, otherwise the last user who signed
  in successfully, otherwise the only human account (uid ≥ 1000 with a valid
  login shell), otherwise an empty, focused field.
- Password field with the active keyboard layout as placeholder
  (`Password · SE`) and a **Caps Lock is on** warning.
- Clickable keyboard layout indicator from a short curated list; the chosen
  layout is applied to the greeter and exported to the session.
- Full PAM conversation through greetd: extra secret or visible prompts (OTP,
  second factor, new password), PAM info and error messages, cancel with
  `Escape`.
- Failed attempts clear the password, keep the username and show the attempt
  count plus PAM's own message (for example a faillock "account is locked").
- **Power off** and **Restart** behind a confirmation dialog whose default
  button is **Cancel**.
- Covers every monitor with the background; the form is drawn on the RandR
  primary monitor (or the monitor under the pointer) and re-laid out when
  monitors change.
- Optional theme file with colours and a darkened PNG wallpaper; NIRUX writes
  it from the active suite theme.
- Passwords and responses are held in zeroizing buffers, and the serialized
  greetd message is wiped after it is written.

## Requirements

- Linux with X11, x86_64 (Debian, Arch and Void glibc).
- `greetd`, `xinit`/`startx`, an Xorg server that the greeter user may start,
  `setxkbmap` and `xkeyboard-config`.
- Debian additionally needs `xserver-xorg-legacy` with an Xwrapper
  configuration (`allowed_users=anybody`, `needs_root_rights=yes`); NIRUX
  writes it and keeps the old file as `Xwrapper.config.before-nirulogin`.
- The greeter user: `_greetd` on Debian, `greeter` on Arch, `_greeter` on Void.

## Installation

### With NIRUX

NIRU Login is a core component. A new `nirux install` on a machine without a
display manager selects it by default; it can also be chosen explicitly:

```sh
./nirux install --display-manager nirulogin
```

NIRUX installs the greetd and Xorg packages, `/usr/local/bin/nirulogin`,
`/usr/local/bin/nirulogin-session`, the polkit rule, `/etc/nirulogin/layouts`
and an owned `/etc/greetd/config.toml`, and keeps the greeter theme in sync on
`nirux update`. It refuses to replace another active display manager unless you
disable that one first. See the NIRUX
[installation guide](https://github.com/nirucon/nirux/blob/main/docs/INSTALL.md).

### Standalone

Each release ships `nirulogin`, `install.sh`, `nirulogin-session`,
`nirulogin-power.rules`, `nirulogin-layouts` and `SHA256SUMS`. Put them in one
directory (or use a source checkout after `cargo build --release`) and run:

```sh
sudo ./install.sh            # install, but leave the current display manager active
sudo ./install.sh --enable   # also make greetd the display manager (systemd)
```

The installer copies the binary to `/usr/local/bin/nirulogin` (override with
`NIRULOGIN_PREFIX`), installs the polkit rule, creates `/var/lib/nirulogin`
(owned by the greeter user, mode 0700), installs `/etc/nirulogin/layouts` if
missing and writes `/etc/greetd/config.toml` (an existing foreign file is kept
as `config.toml.before-nirulogin`). `NIRULOGIN_GREETER_USER`,
`NIRULOGIN_CONFIG_DIR` and `NIRULOGIN_BIN` override the detected values.
`--enable` only switches the systemd `display-manager.service` link.

> Keep SDDM, LightDM or a TTY login available while you validate a new login
> release. A machine must always be able to log in.

## Usage

NIRU Login is started by greetd. The written greetd configuration is:

```toml
[terminal]
vt = 7

[general]
source_profile = false

[default_session]
command = "/usr/bin/startx /usr/local/bin/nirulogin -- :0 vt7"
user = "_greetd"
```

| Key | Action |
|---|---|
| `Enter` | Sign in (the button shows "Signing in…" meanwhile), or answer an extra PAM prompt |
| `Tab` / `Shift+Tab` | Move between username, password, keyboard layout, Power off and Restart |
| `F2` or `Ctrl+Space` | Switch keyboard layout (never typed into a field) |
| `Enter` / `Space` on the layout indicator | Switch keyboard layout |
| `Escape` | Clear the password, or cancel an extra PAM prompt ("Sign-in cancelled") |
| Power dialog: `Tab` / arrows, `Enter`, `Escape` | Switch buttons, select, cancel |

Power actions use the login1 D-Bus API (falling back to `systemctl` and
`loginctl`), run off the UI thread with a 10 s deadline and show
"Powering off…" or "Restarting…".

The session is always NIRUWM: `nirulogin-session` applies the chosen layout
with `setxkbmap` and runs `~/.local/bin/niruwm-session` of the user who signed
in (the home directory comes from the user database, so homes outside `/home`
work).

Command line:

```sh
nirulogin --version
nirulogin --help
nirulogin --preview     # window-only test mode; authentication is disabled
```

## Configuration

### Keyboard layouts

`/etc/nirulogin/layouts` holds one XKB layout code per line (commas and
whitespace also separate codes, `#` starts a comment); the first is the default.
NIRUX installs `se` followed by `us` and never overwrites an existing file. Only
layout codes are accepted; variants belong in the session's
`~/.config/niruwm/input.toml`.

Environment variables in the greetd command line:

- `NIRULOGIN_LAYOUTS=se,us` lists layouts before those from the file.
- `NIRULOGIN_LAYOUT=us` makes one layout the default (moved to the front).
- `NIRULOGIN_USER=name` prefills the username.
- `NIRULOGIN_SESSION=/path` starts another session command instead of
  `~/.local/bin/niruwm-session`.
- `NIRULOGIN_THEME=/path/to/theme.toml` selects another theme file (previews,
  tests).

### Theme

If `/etc/nirulogin/theme.toml` exists, its colours replace the built-in NIRUX
palette and an optional PNG wallpaper is scaled to cover each monitor and
darkened. Colours are `"#rrggbb"` or `"rrggbb"`. The file must be readable by the
greeter user; anything missing or malformed falls back to the defaults.

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

### Power actions and polkit

`nirulogin-power.rules` (installed as
`/etc/polkit-1/rules.d/49-nirulogin-power.rules`) lets the local, active greeter
user use `org.freedesktop.login1.power-off` and `org.freedesktop.login1.reboot`
without a password. The `*-multiple-sessions` actions are **not** granted: they
require administrator authentication, and the greeter has no authentication
agent. When other users are logged in (for example over SSH) the greeter shows
"Other users are logged in. Power off from a session instead."

## Files and paths

| Path | Purpose |
|---|---|
| `/usr/local/bin/nirulogin` | The greeter |
| `/usr/local/bin/nirulogin-session` | Session wrapper: layout, then `~/.local/bin/niruwm-session` |
| `/etc/greetd/config.toml` | greetd configuration (marked `# NIRULOGIN owner:`) |
| `/etc/nirulogin/layouts` | Keyboard layouts offered at login |
| `/etc/nirulogin/theme.toml` | Optional theme, written by NIRUX |
| `/var/lib/nirulogin/last-user` | Last user who signed in successfully (written only after greetd `Success`) |
| `/etc/polkit-1/rules.d/49-nirulogin-power.rules` | Power off / restart for the greeter user |
| `/etc/X11/Xwrapper.config` | Debian: lets the greeter user start Xorg (NIRUX-managed) |

## Troubleshooting

- **Black screen or greetd restarting:** check that the greeter user may start
  Xorg (Debian: `xserver-xorg-legacy` and the Xwrapper configuration) and read
  `journalctl -u greetd`. Switch to a TTY and re-enable the previous display
  manager if needed.
- **Power off says other users are logged in:** expected while another session
  (for example SSH) is open; power off from a session instead.
- **Wrong default layout:** edit `/etc/nirulogin/layouts`; the first line wins.
- **Test without switching the display manager:** run
  `nirulogin --preview` in a normal X session, or point it at a fake greetd
  socket with `GREETD_SOCK=/tmp/fake.sock ./target/release/nirulogin`.

## Uninstall

With NIRUX, `nirux uninstall` removes NIRU Login, the owned greetd configuration
and the polkit rules, and restores the previously recorded display manager.

A standalone installation is removed by hand: re-enable your previous display
manager, then delete `/usr/local/bin/nirulogin`,
`/usr/local/bin/nirulogin-session`,
`/etc/polkit-1/rules.d/49-nirulogin-power.rules`, `/var/lib/nirulogin` and
`/etc/nirulogin`, and restore `/etc/greetd/config.toml.before-nirulogin` if it
exists.

## Building from source

```sh
cargo build --release
cargo test
```

The release profile is LTO and stripped; the binary depends only on the system
X11 and font libraries. The minimum Rust version is 1.85 (Debian 13). `cargo test`
includes a fake greetd server on a temporary socket that covers success, an auth
error, extra secret/visible prompts, info/error messages, `Escape` cancel
(`CancelSession`), an oversized message and the `StartSession` command and
environment. CI runs `cargo fmt --check`, clippy with `-D warnings`, the tests,
an Xvfb `--preview` screenshot smoke and an MSRV build.

## Status, disclaimer and how it's made

**Status: alpha.** It logs me in every day, but a login screen is the one place
where a bug locks you out, so keep a fallback display manager or a TTY login.

NIRUX and every niru* project are made by me, Nicklas Rudolfsson
([@nirucon](https://github.com/nirucon)), for me. They are tailored to my own
needs, preferences and workflows, on my own machines. If they fit yours too,
that's a nice bonus.

- **Free and open source.** Anyone is free to use, study, change and share
  it under the [MIT License](LICENSE).
- **No guarantees.** There is no warranty of any kind. Use it at your own
  risk, read what you install and keep backups of anything that matters.
- **How it's made.** Some of these projects I built from scratch by hand. In
  more recent releases I have used Claude, Anthropic's AI, to do much of the
  heavy lifting under my direction: I decide what to build and how it should
  behave, and I test it on my own machines. It saves me a lot of time, and
  the result is quite OK.
- **Alpha, beta, dev, and maybe forever.** Most things here are alpha, beta
  or dev, and may stay that way. Maybe one day there will be a real stable
  release, but given how fast development moves, and how I am as a person,
  maybe never :)

| Label | Meaning |
|---|---|
| dev | Under active construction; may not build, install or work yet. |
| alpha | Works on my machines, but incomplete or rough; expect breaking changes. |
| beta | In daily use on my machines and mostly complete; rough edges remain. |

## License

MIT License, see [LICENSE](LICENSE). Copyright (c) 2026 Nicklas Rudolfsson.
