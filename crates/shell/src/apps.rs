//! Resolves window app ids and pinned ids to desktop entries: names, icons and launch commands.

use std::borrow::Cow;
use std::collections::HashMap;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::thread;
use std::{env, fs};

use freedesktop_desktop_entry::unicase::Ascii;
use freedesktop_desktop_entry::{DesktopEntry, find_app_by_id};

use crate::appearance;
use iced_widget::image;
use resvg::{tiny_skia, usvg};

/// Pixel size SVG icons are rasterized at: enough for a 128px magnified dock icon at 2x scale,
/// so the GPU only ever scales them down.
const SVG_RASTER_SIZE: u32 = 256;
/// Menu icons are small, even at a high scale, and there can be many of them to draw at once.
const MENU_ICON_RASTER_SIZE: u32 = 48;

#[derive(Debug)]
pub struct App {
    /// Desktop entry id (e.g. `org.kde.dolphin`), or the raw app id when no entry matched.
    pub id: String,
    pub name: String,
    pub icon: Option<image::Handle>,
    pub command: Option<Vec<String>>,
    /// Whether its desktop entry says it's not for listing: it's a background service, say.
    pub unlisted: bool,
    /// What its desktop entry says it is, like `Office` or `Game`.
    pub categories: Vec<String>,
    /// Extra actions from the desktop entry, like "New Private Window".
    pub actions: Vec<DesktopAction>,
}

#[derive(Debug)]
pub struct DesktopAction {
    pub name: String,
    pub command: Vec<String>,
}

pub struct AppIndex {
    entries: Vec<DesktopEntry>,
    locales: Vec<String>,
    icon_theme: String,
    cache: HashMap<String, Rc<App>>,
    /// Theme icons looked up by name, like the dock's places and file types.
    themed_icons: HashMap<String, Option<image::Handle>>,
    menu_icons: HashMap<String, Option<image::Handle>>,
}

impl AppIndex {
    pub fn load() -> Self {
        let locales = freedesktop_desktop_entry::get_languages_from_env();
        let entries = freedesktop_desktop_entry::desktop_entries(&locales);
        let mut icon_theme = icon_theme();
        // Looking in a theme that isn't there is slow, and finds nothing.
        if !appearance::icon_theme_exists(&icon_theme) {
            let instead = FALLBACK_THEMES
                .into_iter()
                .find(|theme| appearance::icon_theme_exists(theme))
                .unwrap_or("hicolor");
            log::warn!("icon theme {icon_theme} isn't installed, using {instead}");
            icon_theme = instead.to_owned();
        }
        log::info!(
            "indexed {} desktop entries, icon theme {icon_theme}",
            entries.len()
        );

        Self {
            entries,
            locales,
            icon_theme,
            cache: HashMap::new(),
            themed_icons: HashMap::new(),
            menu_icons: HashMap::new(),
        }
    }

    /// The ids of the apps there are to launch, like a launcher would list them.
    pub fn listed_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| !entry.no_display() && entry.exec().is_some())
            .map(|entry| entry.id().to_owned())
            .collect()
    }

    pub fn resolve(&mut self, app_id: &str) -> Rc<App> {
        if let Some(app) = self.cache.get(app_id) {
            return app.clone();
        }

        let app = Rc::new(match find_app_by_id(&self.entries, Ascii::new(app_id)) {
            Some(entry) => App {
                id: entry.id().to_owned(),
                name: entry
                    .name(&self.locales)
                    .map_or_else(|| fallback_name(app_id), Cow::into_owned),
                icon: entry
                    .icon()
                    .and_then(|icon| self.find_icon(icon))
                    .and_then(|path| load_icon(&path)),
                command: entry.parse_exec().ok(),
                unlisted: entry.no_display(),
                categories: entry
                    .categories()
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                actions: entry
                    .actions()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|action| {
                        Some(DesktopAction {
                            name: entry.action_name(action, &self.locales)?.into_owned(),
                            command: entry.parse_exec_action(action).ok()?,
                        })
                    })
                    .collect(),
            },
            None => App {
                id: app_id.to_owned(),
                name: fallback_name(app_id),
                icon: self.find_icon(app_id).and_then(|path| load_icon(&path)),
                command: None,
                unlisted: false,
                categories: Vec::new(),
                actions: Vec::new(),
            },
        });

        log::debug!("resolved {app_id}: {app:?}");
        self.cache.insert(app_id.to_owned(), app.clone());
        app
    }

    /// An icon from the theme by name, like `user-trash`.
    pub fn themed_icon(&mut self, name: &str) -> Option<image::Handle> {
        if let Some(icon) = self.themed_icons.get(name) {
            return icon.clone();
        }
        let icon = self.find_icon(name).and_then(|path| load_icon(&path));
        self.themed_icons.insert(name.to_owned(), icon.clone());
        icon
    }

    /// The first of `names` the theme has, as a silhouette in `color` (straight RGBA), the way
    /// macOS draws the Apple logo. Names can also be absolute paths.
    pub fn silhouette(&self, names: &[String], color: [u8; 4]) -> Option<image::Handle> {
        let path = names.iter().find_map(|name| self.find_icon(name))?;
        let (width, height, mut rgba) = load_rgba(&path)?;
        for pixel in rgba.chunks_exact_mut(4) {
            let alpha = u16::from(pixel[3]) * u16::from(color[3]) / 255;
            pixel.copy_from_slice(&[color[0], color[1], color[2], alpha as u8]);
        }
        Some(image::Handle::from_rgba(width, height, rgba))
    }

    /// An icon for a menu: small, and white, like macOS's monochrome ones.
    pub fn menu_icon(&mut self, name: &str) -> Option<image::Handle> {
        if let Some(icon) = self.menu_icons.get(name) {
            return icon.clone();
        }
        let path = self.lookup(name, 16);
        let icon = path.and_then(|path| {
            let (width, height, mut rgba) = if path.extension().is_some_and(|ext| ext == "svg") {
                rasterize_svg_at(&path, MENU_ICON_RASTER_SIZE)?
            } else {
                load_rgba(&path)?
            };
            for pixel in rgba.chunks_exact_mut(4) {
                pixel[0] = 255;
                pixel[1] = 255;
                pixel[2] = 255;
            }
            Some(image::Handle::from_rgba(width, height, rgba))
        });
        self.menu_icons.insert(name.to_owned(), icon.clone());
        icon
    }

    /// Where the app's desktop entry is.
    pub fn entry_path(&self, app_id: &str) -> Option<PathBuf> {
        find_app_by_id(&self.entries, Ascii::new(app_id)).map(|entry| entry.path.clone())
    }

    /// The command that opens `uris` with the app `app_id`.
    pub fn command_with_uris(&self, app_id: &str, uris: &[&str]) -> Option<Vec<String>> {
        find_app_by_id(&self.entries, Ascii::new(app_id))?
            .parse_exec_with_uris(uris, &self.locales)
            .ok()
    }

    fn find_icon(&self, name: &str) -> Option<PathBuf> {
        let path = Path::new(name);
        if path.is_absolute() {
            return path.exists().then(|| path.to_owned());
        }
        self.lookup(name, 128)
    }

    /// The icon in the user's theme, or else in the themes most desktops have: a theme needn't
    /// have every icon, and one that doesn't say it inherits from Breeze would leave apps without.
    fn lookup(&self, name: &str, size: u16) -> Option<PathBuf> {
        std::iter::once(self.icon_theme.as_str())
            .chain(
                FALLBACK_THEMES
                    .iter()
                    .copied()
                    .filter(|theme| *theme != self.icon_theme),
            )
            .find_map(|theme| {
                freedesktop_icons::lookup(name)
                    .with_theme(theme)
                    .with_size(size)
                    .with_cache()
                    .find()
            })
    }
}

/// Turns an app id like `org.gnome.Nautilus` into `Nautilus`.
fn fallback_name(app_id: &str) -> String {
    let last = app_id.rsplit('.').next().unwrap_or(app_id);
    let mut chars = last.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::from(app_id),
    }
}

/// Loads an icon as an image the GPU can scale smoothly. SVGs are rasterized once here, since
/// drawing them as SVGs would re-rasterize them for every size the dock animates through.
fn load_icon(path: &Path) -> Option<image::Handle> {
    if path.extension().is_none_or(|ext| ext != "svg") {
        return Some(image::Handle::from_path(path));
    }
    let (width, height, rgba) = rasterize_svg(path)?;
    Some(image::Handle::from_rgba(width, height, rgba))
}

/// Decodes an icon to straight-alpha RGBA pixels: `(width, height, pixels)`.
fn load_rgba(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    if path.extension().is_some_and(|ext| ext == "svg") {
        return rasterize_svg(path);
    }
    let decoded = ::image::open(path)
        .inspect_err(|err| log::warn!("can't decode {}: {err}", path.display()))
        .ok()?
        .into_rgba8();
    Some((decoded.width(), decoded.height(), decoded.into_raw()))
}

fn rasterize_svg(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    rasterize_svg_at(path, SVG_RASTER_SIZE)
}

fn rasterize_svg_at(path: &Path, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let data = fs::read(path)
        .inspect_err(|err| log::warn!("can't read {}: {err}", path.display()))
        .ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default())
        .inspect_err(|err| log::warn!("can't parse {}: {err}", path.display()))
        .ok()?;

    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let svg_size = tree.size();
    let scale = size as f32 / svg_size.width().max(svg_size.height());
    let offset_x = (size as f32 - svg_size.width() * scale) / 2.0;
    let offset_y = (size as f32 - svg_size.height() * scale) / 2.0;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, offset_x, offset_y),
        &mut pixmap.as_mut(),
    );

    // tiny-skia stores premultiplied alpha; iced expects straight alpha.
    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    Some((size, size, rgba))
}

/// Starts a program apart from the shell: in its own process group, so it outlives the shell
/// and doesn't get its signals, and reaped in the background. It gets no terminal to read from:
/// the shell's is the console the session started on, which KWin has, and nobody can type into.
/// Something asking there, like loginctl's password prompt, would wait forever.
pub fn spawn(command: Vec<String>) {
    let Some((program, args)) = command.split_first() else {
        return;
    };
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(mut child) => {
            thread::spawn(move || child.wait());
        }
        Err(err) => log::error!("can't start {program}: {err}"),
    }
}

/// The desktop entry id of the default app for `mime`, like `org.kde.dolphin` for folders.
pub fn default_app(mime: &str) -> Option<String> {
    let output = Command::new("xdg-mime")
        .args(["query", "default", mime])
        .output()
        .ok()?;
    let desktop_file = String::from_utf8(output.stdout).ok()?;
    let id = desktop_file.trim().strip_suffix(".desktop")?;
    (!id.is_empty()).then(|| id.to_owned())
}

/// Where an icon the user's theme lacks is looked for next, in order.
const FALLBACK_THEMES: [&str; 3] = ["breeze-dark", "breeze", "Adwaita"];

/// The id of the icon theme, like `breeze-dark`: KDE's setting, then GTK's, then KDE's default.
fn icon_theme() -> String {
    kde_icon_theme()
        .or_else(gtk_icon_theme)
        .unwrap_or_else(|| String::from("breeze"))
}

/// `[Icons] Theme` from kdeglobals, where KDE only writes it if it differs from the default.
fn kde_icon_theme() -> Option<String> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))?;
    ["kdeglobals", "kdedefaults/kdeglobals"]
        .iter()
        .find_map(|file| {
            let text = fs::read_to_string(config.join(file)).ok()?;
            let group = text.split("[Icons]").nth(1)?;
            let group = group.split("\n[").next()?;
            let theme = group.lines().find_map(|line| line.strip_prefix("Theme="))?;
            (!theme.trim().is_empty()).then(|| theme.trim().to_owned())
        })
}

/// freedesktop_icons' `default_theme_gtk` returns the theme's display name, which lookups don't
/// accept, so this asks for its id.
fn gtk_icon_theme() -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()?;
    let theme = String::from_utf8(output.stdout).ok()?;
    let theme = theme.trim().trim_matches('\'');
    (!theme.is_empty()).then(|| theme.to_owned())
}
