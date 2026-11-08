# makshell

A macOS Tahoe-style desktop for [KWin](https://invent.kde.org/plasma/kwin), KDE's compositor,
running as its own login session without the rest of Plasma.

- **Dock**, with magnification, recent apps, minimized windows and a Downloads stack that fans out.
- **Menu bar**, with the focused app's menus, tray icons, and Wi-Fi, Bluetooth and Sound items.
- **Control Center**, **Notification Center** with grouped notifications, and **Spotlight**, with a
  clipboard history that outlasts the session.
- **Desktop** icons and widgets: calendar, clock, weather and photos.
- **Liquid glass** behind all of it, rendered by a KWin plugin, and blur behind see-through windows.
- **Window decorations** with traffic lights, a macOS-style **Alt+Tab** and **lock screen**.
- **Settings**, with the accent color, Light, Dark and Auto for apps, and colors for other apps
  generated from the accent or the wallpaper (KDE, GTK, Alacritty and Telegram).
- **macOS app icons** for every app, built for the apps you have, and the macOS cursor.

### Works with KDE's System Settings

Underneath, it's KWin with KDE Frameworks, so KDE's System Settings works as it does in Plasma:
displays, keyboard layouts, shortcuts, input devices, window behavior and the rest. makshell's own
Settings app opens KDE's pages for Wi-Fi, Bluetooth, Sound, Displays and Keyboard. The session shares
KWin's configuration with Plasma, so you can switch between them.

## Requirements

- **KWin 6.7 or 6.8** on Wayland. makshell's KWin plugins use KWin's internal API, which changes
  between releases: they're tested with KWin 6.7.5 and build with 6.8.2. Check yours with
  `kwin_wayland --version`.
- KDE Frameworks 6 and Qt 6.
- A GPU with Vulkan or OpenGL.
- For the menu bar and Control Center: NetworkManager, BlueZ and WirePlumber.
- For the app icons: Python 3 with Pillow, and `rsvg-convert`.

## Installation

### NixOS

The flake has a module that adds a "makshell" login session, built from your system's own packages,
so the plugins always match the KWin they load into.

```nix
# flake.nix
inputs.makshell.url = "github:mistychirp/makshell";

# your configuration
imports = [ inputs.makshell.nixosModules.default ];
programs.makshell.enable = true;
```

Rebuild, log out, and choose **makshell** at the login screen.

### Arch Linux

```sh
sudo pacman -S --needed base-devel git rust cmake extra-cmake-modules pkgconf \
    kwin kdecoration qt6-base qt6-declarative qt6-wayland \
    kconfig kcoreaddons kglobalaccel kwindowsystem \
    wayland libepoxy libdrm vulkan-headers libxkbcommon \
    kscreenlocker xdg-desktop-portal-kde polkit-kde-agent kwallet-pam \
    networkmanager bluez wireplumber python-pillow librsvg curl
```

Then [build and install](#build-and-install). Rebuild makshell after KWin updates (see
[Updating](#updating)).

### Debian and Ubuntu

You need a release with KWin 6.7 or 6.8, so check `kwin_wayland --version` first: Debian 13, for one, has
an older KWin and won't work. Rust from the distro is often too old, so install it with
[rustup](https://rustup.rs) (makshell needs Rust 1.88 or newer).

```sh
sudo apt install build-essential git cmake extra-cmake-modules pkg-config \
    kwin-dev libkdecorations3-dev qt6-base-dev qt6-base-private-dev \
    qt6-declarative-dev qt6-wayland-dev qt6-wayland-dev-tools \
    libkf6config-dev libkf6coreaddons-dev libkf6globalaccel-dev libkf6windowsystem-dev \
    libwayland-dev libepoxy-dev libdrm-dev libvulkan-dev libxkbcommon-dev \
    kwin-wayland kscreenlocker xdg-desktop-portal-kde polkit-kde-agent-1 \
    network-manager bluez wireplumber python3-pil librsvg2-bin libgtk-3-bin curl
```

Package names shift between releases; if one isn't found, `apt search` for it. Then
[build and install](#build-and-install).

### Build and install

```sh
git clone https://github.com/mistychirp/makshell
cd makshell
./install.sh build           # as yourself
sudo ./install.sh install    # into /usr/local
```

`build` also downloads [MacOS-Like](https://github.com/Scrimas/MacOS-Like), which the icons are
built with, and the macOS cursor.

Log out, and choose **makshell** at the login screen. SDDM and GDM find it in `/usr/local`. If yours
doesn't list it, install into `/usr` instead, with `PREFIX=/usr` for both commands.

To remove it: `sudo ./install.sh uninstall`. Your settings in `~/.config/makshell` stay.

### Updating

```sh
./install.sh update          # gets the latest makshell and builds it
sudo ./install.sh install
```

It's safe to do from inside the makshell session; the new version starts when you log out and back
in. Run `./install.sh build` and `sudo ./install.sh install` after every KWin update too: the plugins
must be built against the KWin they load into, and if they don't match, KWin won't load them and the
desktop runs without liquid glass.

## Configuration

Settings (System Settings… in the logo menu, or `makshell-settings`) covers most of it. Everything is kept in
`~/.config/makshell/config.toml`, which can also be edited by hand:

```toml
# Desktop entry ID of the app that opens folders and the trash. Defaults to the system's.
file_manager = "org.kde.dolphin"

[appearance]
accent = "blue"          # a macOS color, "#rrggbb", or "wallpaper"
mode = "dark"            # "light", "dark" or "auto", for apps
match_apps = false       # give other apps colors from the accent
macos_icons = true       # give apps macOS's icons

[desktop]
wallpaper = "~/Pictures/wallpaper.png"

[dock]
pinned = ["org.kde.dolphin", "firefox", "Alacritty"]   # desktop entry IDs, in order
icon_size = 48
magnification = true
magnified_size = 80
show_recents = true

[menubar]
logo = "archlinux-logo"  # icon name or path; defaults to the distro's logo
show_wifi = true
show_bluetooth = true
show_sound = true

[notifications]
off = ["org.example.noisy"]   # apps whose notifications are turned off

[clipboard]
max_items = 50           # how many copied things Spotlight's clipboard history keeps
```

The clipboard history is kept in `~/.local/state/makshell/clipboard`, which only you can read.
Anything a password manager marks as secret isn't kept.

To give apps the generated colors that makshell doesn't know about, put templates in
`~/.config/makshell/templates`. A template's first line names where the result goes, as in
`# output: ~/.config/foot/colors.ini`, and `{{primary}}` or `{{primary.rgb}}` stand for colors.

## Development

```sh
nix develop
cargo run                    # the shell
cargo run -- settings        # the Settings app
cargo test
```

Without Nix: the [build dependencies](#arch-linux) above, then `cargo run`, and
`cmake -S kwin -B build/kwin && cmake --build build/kwin` for the plugins.

The layout:

- `crates/shell`: the shell and the Settings app, in Rust with [iced](https://iced.rs) on
  layer-shell. It talks to KWin over Wayland protocols and D-Bus.
- `kwin`: KWin plugins for what the shell can't do from outside: the glass effect, the genie, the
  window decoration, the window switcher and the lock screen.
- `protocols/makshell-glass-v1.xml`: how the shell asks for glass. On a compositor without it, the
  shell falls back to blur (`ext-background-effect-v1`), or to a plain fill.

To try changes without leaving your session, run KWin nested. It needs
`wp_single_pixel_buffer_manager_v1` and pointer constraints from its host, which a nested sway has.

Notes:

- The shell depends on iced's component crates (`iced_widget`, `iced_futures`, `iced_renderer`)
  instead of `iced`, which hard-depends on winit. That means turning on by hand what `iced` would:
  the `wgpu-bare` renderer (without it, rendering is silently on the CPU) and the PNG codec.
- `iced_layershell` and `iced_wgpu` are vendored with small patches (see
  [vendor/README.md](vendor/README.md)).
- `winit-core` and `winit-common` are pinned to `0.31.0-beta.2` in `Cargo.lock`, because
  `iced_exdevtools` 0.19.1 doesn't compile against beta.3. Don't `cargo update` them until
  iced_layershell ships a fix.

## Credits

- Some of the glass shader, the genie's curve and Control Center's opening follow
  [MacShell](https://github.com/0-ss/MacShell) ([MIT](LICENSES/MacShell-MIT.txt)).
- The color templates for other apps come from [Noctalia](https://github.com/noctalia-dev/noctalia)
  ([MIT](LICENSES/Noctalia-MIT.txt)).
- [Inter](https://rsms.me/inter/), the typeface, is under the [OFL](assets/fonts/OFL.txt).
- The app icons are [MacTahoe](https://github.com/vinceliuice/MacTahoe-icon-theme)'s, as
  [MacOS-Like](https://github.com/Scrimas/MacOS-Like) builds them, and the cursor is
  [apple_cursor](https://github.com/ful1e5/apple_cursor), all GPL-3.0. makshell downloads them;
  they aren't part of this repository.
- The KWin plugins are GPL-3.0-or-later, as their source headers say.

makshell isn't affiliated with Apple. macOS is a trademark of Apple Inc.
