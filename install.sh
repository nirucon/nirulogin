#!/bin/sh
set -eu

PREFIX="${NIRULOGIN_PREFIX:-/usr/local}"
DEST="$PREFIX/bin/nirulogin"
CONF_DIR="${NIRULOGIN_CONFIG_DIR:-/etc/greetd}"
CONF="$CONF_DIR/config.toml"
GREETER_USER="${NIRULOGIN_GREETER_USER:-}"
ENABLE=0

for arg in "$@"; do
    case "$arg" in
        --enable) ENABLE=1 ;;
        --help)
            printf '%s\n' "Usage: ./install.sh [--enable]"
            exit 0
            ;;
        *) printf 'nirulogin install: unknown option %s\n' "$arg" >&2; exit 2 ;;
    esac
done

[ "$(id -u)" = 0 ] || { echo 'nirulogin install: run as root' >&2; exit 1; }
command -v greetd >/dev/null 2>&1 || { echo 'nirulogin install: greetd is missing' >&2; exit 1; }
command -v startx >/dev/null 2>&1 || { echo 'nirulogin install: startx is missing' >&2; exit 1; }

if [ -z "$GREETER_USER" ]; then
    if getent passwd _greetd >/dev/null 2>&1; then
        GREETER_USER=_greetd
    elif getent passwd greeter >/dev/null 2>&1; then
        GREETER_USER=greeter
    else
        echo 'nirulogin install: no greetd user found' >&2
        exit 1
    fi
fi

install -D -m 0755 target/release/nirulogin "$DEST"
install -d -m 0755 "$CONF_DIR"
if [ -f "$CONF" ] && ! grep -q '^# NIRULOGIN owner:' "$CONF"; then
    cp -a "$CONF" "$CONF.before-nirulogin"
fi
cat >"$CONF" <<CONFIG
# NIRULOGIN owner: $PREFIX
[terminal]
vt = 7

[general]
source_profile = false

[default_session]
command = "/usr/bin/startx $DEST -- :0 vt7"
user = "$GREETER_USER"
CONFIG

if [ "$ENABLE" = 1 ]; then
    if [ -f /etc/systemd/system/display-manager.service ] || [ -L /etc/systemd/system/display-manager.service ]; then
        rm -f /etc/systemd/system/display-manager.service
    fi
    ln -s /lib/systemd/system/greetd.service /etc/systemd/system/display-manager.service
    systemctl enable greetd.service >/dev/null 2>&1 || true
fi

printf 'Installed NIRULOGIN at %s\n' "$DEST"
if [ "$ENABLE" = 1 ]; then
    printf 'NIRULOGIN is the display-manager target. Reboot or restart greetd to use it.\n'
else
    printf 'NIRULOGIN is installed but not active. Re-run with --enable to make it default.\n'
fi
