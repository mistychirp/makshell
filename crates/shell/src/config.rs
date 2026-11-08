//! User configuration, read from `$XDG_CONFIG_HOME/makshell/config.toml`.

use std::path::PathBuf;
use std::{env, fs, io};

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: AppearanceConfig,
    pub desktop: DesktopConfig,
    pub dock: DockConfig,
    pub menubar: MenuBarConfig,
    pub notifications: NotificationsConfig,
    pub clipboard: ClipboardConfig,
    /// Desktop entry id of the app that opens folders and the trash. Defaults to the system's
    /// default app for folders.
    pub file_manager: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// A color's name, like `blue`, or `#rrggbb`, or `wallpaper`, or `wallpaper:1` for the second
    /// color picked out of it.
    pub accent: String,
    /// Give other apps a palette made from the accent.
    pub match_apps: bool,
    /// `light`, `dark`, or `auto`, which goes by the sun: what apps look like.
    pub mode: String,
    /// Give apps macOS's icons, from the MacOS-Like themes the shell builds.
    pub macos_icons: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            accent: crate::appearance::DEFAULT_ACCENT.to_owned(),
            match_apps: false,
            mode: String::from("dark"),
            macos_icons: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DesktopConfig {
    /// Path of the wallpaper image, which may start with `~/`. Without one, the shell leaves the
    /// background to whatever else draws it.
    pub wallpaper: Option<String>,
    /// Keep widgets as glass even when the desktop has focus, instead of in color.
    pub always_glassy_widgets: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DockConfig {
    /// Desktop entry ids (or app ids) kept in the dock even when they aren't running.
    pub pinned: Vec<String>,
    /// How big its icons are, in logical pixels.
    pub icon_size: f32,
    /// Keep the last few apps that aren't kept in it, in a section of their own.
    pub show_recents: bool,
    /// Grow icons near the pointer.
    pub magnification: bool,
    /// Size of the icon right under the pointer, in logical pixels (icons are 48).
    pub magnified_size: f32,
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            pinned: Vec::new(),
            icon_size: crate::dock::DEFAULT_ICON,
            show_recents: true,
            magnification: true,
            magnified_size: 80.0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    /// How many copied things the history keeps.
    pub max_items: usize,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self { max_items: 50 }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    /// The apps whose notifications are turned off, by desktop entry id or name.
    pub off: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct MenuBarConfig {
    /// Icon name or path of the logo that opens the system menu. Defaults to the distro's.
    pub logo: Option<String>,
    /// Status items, each opening what Control Center's module for it opens to.
    pub show_wifi: bool,
    pub show_bluetooth: bool,
    pub show_sound: bool,
}

impl Default for MenuBarConfig {
    fn default() -> Self {
        Self {
            logo: None,
            show_wifi: true,
            show_bluetooth: true,
            show_sound: true,
        }
    }
}

/// Rewrites one of `[menubar]`'s switches, the same way as the others.
pub fn save_menubar_switch(key: &str, on: bool) -> io::Result<()> {
    save("menubar", key, toml_edit::value(on))
}

/// Rewrites `[dock] pinned` in the config file, keeping the rest of it (comments included).
pub fn save_pinned(pinned: &[String]) -> io::Result<()> {
    edit(|doc| {
        if !doc.contains_table("dock") {
            doc["dock"] = toml_edit::table();
        }
        doc["dock"]["pinned"] = toml_edit::value(pinned.iter().collect::<toml_edit::Array>());
    })
}

/// Rewrites `[desktop] wallpaper` in the config file, the same way.
pub fn save_wallpaper(wallpaper: &std::path::Path) -> io::Result<()> {
    edit(|doc| {
        if !doc.contains_table("desktop") {
            doc["desktop"] = toml_edit::table();
        }
        doc["desktop"]["wallpaper"] = toml_edit::value(wallpaper.to_string_lossy().as_ref());
    })
}

/// Rewrites `[dock] magnification`, the same way.
pub fn save_magnification(magnification: bool) -> io::Result<()> {
    save("dock", "magnification", toml_edit::value(magnification))
}

/// Rewrites `[dock] magnified_size`, the same way.
pub fn save_show_recents(show: bool) -> io::Result<()> {
    save("dock", "show_recents", toml_edit::value(show))
}

pub fn save_icon_size(size: f32) -> io::Result<()> {
    save("dock", "icon_size", toml_edit::value(f64::from(size)))
}

pub fn save_magnified_size(size: f32) -> io::Result<()> {
    save("dock", "magnified_size", toml_edit::value(f64::from(size)))
}

/// Rewrites `[notifications] off`, the same way.
pub fn save_notifications_off(off: &[String]) -> io::Result<()> {
    edit(|doc| {
        if !doc.contains_table("notifications") {
            doc["notifications"] = toml_edit::table();
        }
        doc["notifications"]["off"] = toml_edit::value(off.iter().collect::<toml_edit::Array>());
    })
}

/// Rewrites `[appearance] accent`, the same way.
pub fn save_accent(accent: &str) -> io::Result<()> {
    save("appearance", "accent", toml_edit::value(accent))
}

/// Rewrites `[appearance] mode`, the same way.
pub fn save_appearance_mode(mode: &str) -> io::Result<()> {
    save("appearance", "mode", toml_edit::value(mode))
}

/// Rewrites `[appearance] match_apps`, the same way.
pub fn save_match_apps(on: bool) -> io::Result<()> {
    save("appearance", "match_apps", toml_edit::value(on))
}

/// Rewrites `[clipboard] max_items`, the same way.
pub fn save_clipboard_max(max_items: usize) -> io::Result<()> {
    save("clipboard", "max_items", toml_edit::value(max_items as i64))
}

/// Rewrites `[appearance] macos_icons`, the same way.
pub fn save_macos_icons(on: bool) -> io::Result<()> {
    save("appearance", "macos_icons", toml_edit::value(on))
}

/// Rewrites `[desktop] always_glassy_widgets`, the same way.
pub fn save_always_glassy_widgets(always: bool) -> io::Result<()> {
    save("desktop", "always_glassy_widgets", toml_edit::value(always))
}

fn save(table: &str, key: &str, value: toml_edit::Item) -> io::Result<()> {
    edit(|doc| {
        if !doc.contains_table(table) {
            doc[table] = toml_edit::table();
        }
        doc[table][key] = value;
    })
}

fn edit(change: impl FnOnce(&mut toml_edit::DocumentMut)) -> io::Result<()> {
    let path = config_path().ok_or_else(|| io::Error::other("no config directory"))?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };

    let mut doc: toml_edit::DocumentMut = text.parse().map_err(io::Error::other)?;
    change(&mut doc);

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, doc.to_string())
}

/// The config file as it is, or nothing if there isn't one.
pub fn read_text() -> io::Result<String> {
    let path = config_path().ok_or_else(|| io::Error::other("no config directory"))?;
    match fs::read_to_string(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        result => result,
    }
}

/// Replaces the config file.
pub fn write_text(text: &str) -> io::Result<()> {
    let path = config_path().ok_or_else(|| io::Error::other("no config directory"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, text)
}

/// A file next to the config file.
pub fn sibling(name: &str) -> Option<PathBuf> {
    Some(config_path()?.parent()?.join(name))
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };

        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                log::error!("invalid {}: {err}", path.display());
                Self::default()
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                log::error!("can't read {}: {err}", path.display());
                Self::default()
            }
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("makshell/config.toml"))
}
