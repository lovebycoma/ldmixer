#!/bin/sh
# `ld-mixer uninstall`: remove lD Mixer and everything it created, in one go.
#
# This script is embedded in the ld-mixer binary and run through `sh -c`;
# the binary passes its own path in LDMIXER_SELF.
#
#   1. Per-user data of the calling user: settings, presets, state, cache.
#   2. The installed program itself, through whatever installed it:
#      pacman (Arch and derivatives), rpm/dnf (Fedora), Nix, or a manual
#      `make install` (removed using its manifest).
#
# Options:
#   -y, --yes          do not ask for confirmation
#   --keep-user-data   leave settings and presets in place
set -eu

assume_yes=0
keep_user=0
for arg in "$@"; do
    case "$arg" in
        -y|--yes) assume_yes=1 ;;
        --keep-user-data) keep_user=1 ;;
        -h|--help)
            echo "usage: ld-mixer uninstall [-y|--yes] [--keep-user-data]"
            echo "Removes the lD Mixer package (pacman, dnf/rpm, Nix or a manual install)"
            echo "and your lD Mixer settings and presets."
            exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

# When run through sudo, clean the invoking user's home, not root's.
if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    user_home=$(getent passwd "$SUDO_USER" | cut -d: -f6)
    config_home="$user_home/.config"
    state_home="$user_home/.local/state"
    cache_home="$user_home/.cache"
else
    config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
    state_home="${XDG_STATE_HOME:-$HOME/.local/state}"
    cache_home="${XDG_CACHE_HOME:-$HOME/.cache}"
fi

user_paths="$config_home/ld-mixer $state_home/ld-mixer $cache_home/ld-mixer \
$config_home/autostart/com.lovebycoma.ldmixer.desktop \
$config_home/autostart/io.github.lovebycoma.LdMixer.desktop"

self=$(readlink -f "${LDMIXER_SELF:-$0}")
as_root() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

# Work out who owns this script, and therefore how to remove the program.
method=manual
if command -v pacman >/dev/null 2>&1 && pacman -Qqo "$self" >/dev/null 2>&1; then
    method=pacman; pkg=$(pacman -Qqo "$self")
elif command -v rpm >/dev/null 2>&1 && rpm -qf "$self" >/dev/null 2>&1; then
    method=rpm; pkg=$(rpm -qf --qf '%{NAME}\n' "$self")
else
    case "$self" in /nix/store/*) method=nix ;; esac
fi
manifest="$(dirname "$self")/../share/ld-mixer/manifest"

echo "This removes lD Mixer:"
case "$method" in
    pacman) echo "  - package '$pkg' via: pacman -Rns $pkg" ;;
    rpm)    echo "  - package '$pkg' via: dnf remove $pkg" ;;
    nix)    echo "  - the Nix profile entry 'ld-mixer'" ;;
    manual) echo "  - files listed in $(readlink -f "$manifest" 2>/dev/null || echo "$manifest")" ;;
esac
if [ "$keep_user" -eq 0 ]; then
    for p in $user_paths; do [ -e "$p" ] && echo "  - $p"; done
fi

if [ "$assume_yes" -eq 0 ]; then
    printf 'Continue? [y/N] '
    read -r answer
    case "$answer" in y|Y|yes|YES) ;; *) echo "Aborted."; exit 1 ;; esac
fi

# Stop the GUI and the background service so nothing holds the device or
# rewrites settings; stopping the service also removes its virtual devices.
# The bracket keeps this pattern from matching this script's command line.
pkill -f 'ld-mixe[r] (gui|daemon)' 2>/dev/null || true

if [ "$keep_user" -eq 0 ]; then
    for p in $user_paths; do
        if [ -e "$p" ]; then rm -rf -- "$p"; fi
    done
fi

case "$method" in
    pacman)
        as_root pacman -Rns --noconfirm "$pkg" ;;
    rpm)
        if command -v dnf >/dev/null 2>&1; then as_root dnf remove -y "$pkg"
        else as_root rpm -e "$pkg"; fi ;;
    nix)
        if nix profile list 2>/dev/null | grep -q ld-mixer; then
            nix profile remove ld-mixer
        else
            echo "lD Mixer was installed declaratively (NixOS or Home Manager):"
            echo "remove it from your configuration and rebuild."
        fi ;;
    manual)
        if [ ! -f "$manifest" ]; then
            echo "No package or manifest found; nothing else to remove." >&2
            exit 1
        fi
        files=$(cat "$manifest")
        # A user-prefix install (e.g. PREFIX=~/.local) needs no root.
        run=as_root
        if [ -w "$(dirname "$manifest")" ]; then run=""; fi
        # shellcheck disable=SC2086
        $run rm -f $files
        $run rmdir "$(dirname "$manifest")" "$(dirname "$manifest")/../licenses/ld-mixer" 2>/dev/null || true
        if command -v udevadm >/dev/null 2>&1 && [ -n "$run" ]; then
            as_root udevadm control --reload || true
        fi ;;
esac

echo "lD Mixer has been removed."
