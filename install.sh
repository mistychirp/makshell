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
# Folders installed whole, links and all: MacOS-Like and the cursor.
TREES="$PREFIX/share/makshell/installed-folders"

# MacOS-Like (github.com/Scrimas/MacOS-Like), which the shell builds the app icons with, at the
# commit the flake pins. The macOS cursor (github.com/ful1e5/apple_cursor), as released.
MACOS_LIKE_REPO=https://github.com/Scrimas/MacOS-Like
CURSOR_URL=https://github.com/ful1e5/apple_cursor/releases/download/v2.0.1/macOS.tar.xz
CURSOR_SHA256=9c6e5e13b068ce51a9e90a9abd6ce232ec7d25d4ceb05aa8ac76efbb99c76762

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
    fetch_icons
    fetch_cursor
    say "Built. Now: sudo ./install.sh install"
    check_runtime
}

fetch_icons() {
    command -v git >/dev/null || die "git is missing"
    command -v python3 >/dev/null || die "python3 is missing"
    rev=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["nodes"]["macos-like"]["locked"]["rev"])' "$ROOT/flake.lock")
    dir="$ROOT/build/macos-like"
    if [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" != "$rev" ]; then
        say "Getting MacOS-Like, for the app icons"
        rm -rf "$dir"
        mkdir -p "$dir"
        git -C "$dir" init -q
        git -C "$dir" fetch -q --depth 1 "$MACOS_LIKE_REPO" "$rev"
        git -C "$dir" checkout -q FETCH_HEAD
    fi
}

# What makshell needs at runtime that a build doesn't, which a newer version can add to: the
# packages, by this distro's names, said where they can't be missed, at the end of the build.
check_runtime() {
    if command -v pacman >/dev/null; then
        pillow=python-pillow rsvg=librsvg install="sudo pacman -S --needed"
    elif command -v apt-get >/dev/null; then
        pillow=python3-pil rsvg=librsvg2-bin install="sudo apt install"
    elif command -v dnf >/dev/null; then
        pillow=python3-pillow rsvg=librsvg2-tools install="sudo dnf install"
    else
        pillow="Pillow" rsvg=rsvg-convert install=""
    fi
    missing=""
    python3 -c 'import PIL' 2>/dev/null || missing="$pillow"
    command -v rsvg-convert >/dev/null || missing="${missing:+$missing }$rsvg"
    if [ -n "$missing" ]; then
        printf '\n'
        say "The macOS app icons can't be made without: $missing"
        if [ -n "$install" ]; then
            say "Install it, then log in again:  $install $missing"
        else
            say "Install Python's Pillow and rsvg-convert with your package manager, then log in again."
        fi
    fi

    # Settings opens KDE's own pages for these, which come with Plasma's packages, by the same
    # names on Arch, Debian, Ubuntu and Fedora.
    pages=""
    if command -v kcmshell6 >/dev/null; then
        # Without a display, as over SSH or on a console, it aborts unless told there isn't one.
        modules=$(QT_QPA_PLATFORM=offscreen kcmshell6 --list 2>/dev/null || true)
        for page in kcm_networkmanagement:plasma-nm kcm_bluetooth:bluedevil kcm_pulseaudio:plasma-pa \
                    kcm_kscreen:kscreen kcm_keyboard:plasma-desktop; do
            printf '%s\n' "$modules" | grep -q "^${page%%:*} " || pages="${pages:+$pages }${page#*:}"
        done
    else
        pages="systemsettings plasma-nm bluedevil plasma-pa kscreen plasma-desktop"
    fi
    if [ -n "$pages" ]; then
        printf '\n'
        say "Settings' Wi-Fi, Bluetooth, Sound, Displays and Keyboard pages need: $pages"
        if [ -n "$install" ]; then
            say "Install them with:  $install $pages"
        fi
    fi
    [ -n "$missing$pages" ] && printf '\n'
    return 0
}

fetch_cursor() {
    dir="$ROOT/build/cursor"
    [ -f "$dir/macOS/index.theme" ] && return
    say "Getting the macOS cursor"
    rm -rf "$dir"
    mkdir -p "$dir"
    if command -v curl >/dev/null; then
        curl -fsSL -o "$dir/macOS.tar.xz" "$CURSOR_URL"
    elif command -v wget >/dev/null; then
        wget -q -O "$dir/macOS.tar.xz" "$CURSOR_URL"
    else
        die "curl or wget is needed to get the cursor"
    fi
    echo "$CURSOR_SHA256  $dir/macOS.tar.xz" | sha256sum -c --quiet - \
        || die "the cursor's download isn't what it should be"
    tar -xJf "$dir/macOS.tar.xz" -C "$dir"
    rm "$dir/macOS.tar.xz"
}

# Copies a whole folder into place, links and all, swapping it in at once.
install_tree() { # install_tree SOURCE DESTINATION
    new="$DESTDIR$2.new"
    old="$DESTDIR$2.old"
    rm -rf "$new" "$old"
    mkdir -p "$(dirname "$new")"
    cp -R "$1" "$new"
    [ ! -e "$DESTDIR$2" ] || mv "$DESTDIR$2" "$old"
    mv "$new" "$DESTDIR$2"
    rm -rf "$old"
    echo "$2" >> "$STAGE.trees"
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
    [ -f "$ROOT/build/macos-like/setup.sh" ] && [ -f "$ROOT/build/cursor/macOS/index.theme" ] \
        || die "the icons or the cursor are missing: run ./install.sh build again, as yourself"
    # Everything goes together in a staging folder first, and then into place.
    STAGE=$(mktemp -d)
    trap 'rm -rf "$STAGE" "$STAGE.files" "$STAGE.trees" "$STAGE.trees.sorted"' EXIT

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
    stage 0755 "$ROOT/contrib/makshell-icons" "$PREFIX/libexec/makshell-icons"

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

    say "Installing the app icons' builder and the cursor"
    : > "$STAGE.trees"
    install_tree "$ROOT/build/macos-like" "$PREFIX/share/makshell/macos-like"
    rm -rf "$DESTDIR$PREFIX/share/makshell/macos-like/.git" "$DESTDIR$PREFIX/share/makshell/macos-like/screenshots"
    install_tree "$ROOT/build/cursor/macOS" "$PREFIX/share/icons/macOS"
    # What the last install had and this one doesn't.
    sort "$STAGE.trees" > "$STAGE.trees.sorted"
    if [ -f "$DESTDIR$TREES" ]; then
        sort "$DESTDIR$TREES" | comm -23 - "$STAGE.trees.sorted" | while IFS= read -r gone; do
            rm -rf "$DESTDIR$gone"
        done
    fi
    cp "$STAGE.trees.sorted" "$DESTDIR$TREES"

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
    if [ -f "$DESTDIR$TREES" ]; then
        while IFS= read -r tree; do
            rm -rf "$DESTDIR$tree"
        done < "$DESTDIR$TREES"
        rm -f "$DESTDIR$TREES"
    fi
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
