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
- `setxkbmap` for the Swedish default layout

Install these with the distribution package manager. NIRUX installs and owns
the integration files.

## Build

```sh
cargo build --release
```

The release profile is LTO, stripped and self-contained apart from the system
X11/font libraries.

## Install

```sh
sudo ./install.sh
```

The installer copies the binary to `/usr/local/bin/nirulogin` and writes a
greetd configuration. It does not replace the active display manager unless
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

- Username is prefilled from `NIRULOGIN_USER` or `niru`.
- Password field has focus at startup.
- Swedish keyboard layout (`se`) is applied at startup and passed to the session.
- `Tab` moves between username and password.
- `Enter` signs in.
- `Escape` clears the password.
- **Power off** and **Restart** are available at the bottom right and require
  confirmation.
- The session is always NIRUWM.

## Scope

The current release deliberately has no session picker, wallpaper decoding,
animations, network access or autologin UI. The background is the NIRUX theme
background colour, currently `#0f0f10`. Power actions use the login1 D-Bus
capability with systemctl/loginctl fallbacks and a narrow polkit rule for the
greetd greeter user.

The password field handles the normal secret PAM prompt. Additional interactive
PAM conversations (visible prompts, extra authentication steps or expired
password changes) are rejected with a clear error and the session is cancelled;
the password is never sent to an unknown visible prompt. This limitation is
tracked in the suite planning roadmap.

## Test without switching the display manager

```sh
GREETD_SOCK=/tmp/fake.sock ./target/release/nirulogin
```

NIRULOGIN also accepts `--preview` for window-only testing. In preview mode the
password is accepted by the UI but authentication is disabled.
