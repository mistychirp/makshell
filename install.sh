#!/bin/sh
# Builds and installs makshell outside Nix: the shell, its KWin plugins, and a "makshell" login
# session. On NixOS, use the flake's module instead (see README.md).
#
#   ./install.sh build              as yourself: builds into target/ and build/
#   ./install.sh update             as yourself: gets the latest makshell, and builds it
#   sudo ./install.sh install       copies it into PREFIX, replacing what an older install put there
#   sudo ./install.sh uninstall     removes what install copied
#
# PREFIX is /usr/local unless it's set, for both build and install. DESTDIR, if set, is put in front
# of every installed path, for packaging.
set -eu

PREFIX=${PREFIX:-/usr/local}
DESTDIR=${DESTDIR:-}
ROOT=$(cd "$(dirname "$0")" && pwd)
BUILD="$ROOT/build/kwin"
MANIFEST="$PREFIX/share/makshell/installed-files"

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

build() {
    [ "$(id -u)" -ne 0 ] || die "build as yourself, not as root; then install with sudo"
    command -v cargo >/dev/null || die "cargo is missing (install Rust 1.88 or newer, e.g. with rustup)"
    command -v cmake >/dev/null || die "cmake is missing"
    say "Building the shell"
    (cd "$ROOT" && cargo build --release --locked)
    say "Building the KWin plugins"
    cmake -S "$ROOT/kwin" -B "$BUILD" -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$PREFIX"
    cmake --build "$BUILD" --parallel
    say "Built. Now: sudo ./install.sh install"
}

# Copies a template into the staging folder, filling in @prefix@ and friends.
stage() { # stage MODE SOURCE DESTINATION
    mkdir -p "$STAGE$(dirname "$3")"
    sed -e "s|@prefix@|$PREFIX|g" -e "s|@makshell@|$PREFIX/bin/makshell|g" \
        -e "s|@qtplugins@|$QT_PLUGINS|g" "$2" > "$STAGE$3"
    chmod "$1" "$STAGE$3"
}

install_all() {
    [ -x "$ROOT/target/release/makshell" ] && [ -f "$BUILD/CMakeCache.txt" ] \
        || die "nothing built yet: run ./install.sh build first, as yourself"
    [ -f "$BUILD/bin/kwin/effects/plugins/makshell.so" ] \
        || die "the KWin plugins didn't build: see what ./install.sh build says"
    # Everything goes together in a staging folder first, and then into place.
    STAGE=$(mktemp -d)
    trap 'rm -rf "$STAGE" "$STAGE.files"' EXIT

    say "Installing the KWin plugins"
    DESTDIR="$STAGE" cmake --install "$BUILD" > /dev/null
    effect=$(find "$STAGE" -path '*/kwin/effects/plugins/makshell.so' | head -n 1)
    [ -n "$effect" ] || die "the KWin effect wasn't built"
    # Where Qt plugins go under PREFIX, which the session tells KWin.
    QT_PLUGINS=$(dirname "$(dirname "$(dirname "$(dirname "${effect#"$STAGE"}")")")")

    say "Installing the shell and the session"
    mkdir -p "$STAGE$PREFIX/bin"
    install -m 0755 "$ROOT/target/release/makshell" "$STAGE$PREFIX/bin/makshell"
    printf '#!/bin/sh\nexec %s/bin/makshell settings "$@"\n' "$PREFIX" > "$STAGE$PREFIX/bin/makshell-settings"
    chmod 0755 "$STAGE$PREFIX/bin/makshell-settings"
    # KWin only lets the shell manage windows if a desktop entry naming its binary says so.
    stage 0644 "$ROOT/contrib/makshell.desktop" "$PREFIX/share/applications/makshell.desktop"
    stage 0644 "$ROOT/contrib/makshell-settings.desktop" "$PREFIX/share/applications/makshell-settings.desktop"
    stage 0755 "$ROOT/contrib/session/makshell-session" "$PREFIX/bin/makshell-session"
    stage 0755 "$ROOT/contrib/session/makshell-startup" "$PREFIX/libexec/makshell-startup"
    stage 0644 "$ROOT/contrib/session/makshell.desktop" "$PREFIX/share/wayland-sessions/makshell.desktop"
    stage 0644 "$ROOT/contrib/session/makshell-session.target" "$PREFIX/lib/systemd/user/makshell-session.target"

    # Each file goes in as a new one, not over the old one: a running session has the plugins loaded
    # and the startup script open, and keeps the old ones until it ends.
    (cd "$STAGE" && find . -type f | sed 's|^\.||' | sort) > "$STAGE.files"
    while IFS= read -r file; do
        install -D -m "$(stat -c %a "$STAGE$file")" "$STAGE$file" "$DESTDIR$file"
    done < "$STAGE.files"
    # What the last install had and this one doesn't.
    if [ -f "$DESTDIR$MANIFEST" ]; then
        sort "$DESTDIR$MANIFEST" | comm -23 - "$STAGE.files" | while IFS= read -r gone; do
            rm -f "$DESTDIR$gone"
        done
    fi
    mkdir -p "$DESTDIR$(dirname "$MANIFEST")"
    cp "$STAGE.files" "$DESTDIR$MANIFEST"
    rm -f "$STAGE.files"

    say "Installed into $PREFIX. Log out and back in, choosing \"makshell\" as the session."
}

update() {
    [ "$(id -u)" -ne 0 ] || die "update as yourself, not as root; then install with sudo"
    say "Getting the latest makshell"
    git -C "$ROOT" pull --ff-only
    build
}

uninstall_all() {
    [ -f "$DESTDIR$MANIFEST" ] || die "no record of an install in $PREFIX"
    while IFS= read -r file; do
        rm -f "$DESTDIR$file"
    done < "$DESTDIR$MANIFEST"
    rm -f "$DESTDIR$MANIFEST"
    # Folders this left empty, deepest first.
    for dir in "$PREFIX/share/makshell" "$PREFIX/share/plasma/shells/org.makshell.desktop/contents/lockscreen" \
               "$PREFIX/share/plasma/shells/org.makshell.desktop/contents/fonts" \
               "$PREFIX/share/plasma/shells/org.makshell.desktop/contents" "$PREFIX/share/plasma/shells/org.makshell.desktop" \
               "$PREFIX/share/kwin-wayland/tabbox/makshell/contents/ui" "$PREFIX/share/kwin-wayland/tabbox/makshell/contents" \
               "$PREFIX/share/kwin-wayland/tabbox/makshell"; do
        rmdir "$DESTDIR$dir" 2>/dev/null || true
    done
    say "Removed makshell from $PREFIX. Your settings in ~/.config/makshell stay."
}

case "${1:-}" in
    build) build ;;
    update) update ;;
    install) install_all ;;
    uninstall) uninstall_all ;;
    *) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
