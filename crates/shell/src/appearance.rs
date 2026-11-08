//! The accent color, and making other apps match it.
//!
//! The accent is one of macOS's, any color, or one picked out of the wallpaper the way Material You
//! does it, favoring colors that are vivid and cover real area over a plain average. Everything else
//! follows from it: the shell's selections and switches, KDE's accent, and, if asked for, a whole
//! palette for other apps, generated from it with the same color science. That's a color scheme for
//! KDE and Qt, a style sheet for GTK, a theme for Alacritty and for Telegram, and whatever templates
//! are in `~/.config/makshell/templates`, each of which starts with a line naming its output, like
//! `# output: ~/.config/foot/colors.ini`, and uses `{{primary}}`, or `{{primary.rgb}}`.
//!
//! What it changes in other apps' settings is saved first, and put back when it's turned off.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use iced_widget::core::Color;
use material_colors::color::Rgb;
use material_colors::hct::Hct;
use material_colors::quantize::{Quantizer, QuantizerCelebi};
use material_colors::scheme::Scheme;
use material_colors::scheme::variant::SchemeContent;
use material_colors::score::Score;

use crate::config::Config;
use crate::icon_themes;
use crate::{dbus, places, wallpaper};

/// What the config file says when it hasn't been changed.
pub const DEFAULT_ACCENT: &str = "purple";
/// Close to macOS's, with its purple being the one measured from the user's screen.
pub const PRESETS: [(&str, [u8; 3]); 8] = [
    ("blue", [0, 122, 255]),
    ("purple", [179, 46, 176]),
    ("pink", [247, 79, 158]),
    ("red", [224, 56, 62]),
    ("orange", [247, 130, 27]),
    ("yellow", [255, 199, 38]),
    ("green", [98, 186, 70]),
    ("graphite", [140, 140, 146]),
];
const ACCENT_ALPHA: f32 = 0.95;
/// How light the accent can be, as a tone from 0 to 100.
const LIGHTEST_ACCENT: f64 = 68.0;
/// How many colors are offered from a wallpaper.
const WALLPAPER_COLORS: usize = 4;
/// How big a wallpaper is looked at. Far more detail than this doesn't change what its colors are.
const LOOK_AT: u32 = 128;
const DEFAULT_COLOR: [u8; 3] = [179, 46, 176];

const KDE_TEMPLATE: &str = include_str!("themes/kde.colors");
const GTK3_TEMPLATE: &str = include_str!("themes/gtk3.css");
const GTK4_TEMPLATE: &str = include_str!("themes/gtk4.css");
const ALACRITTY_TEMPLATE: &str = include_str!("themes/alacritty.toml");
const TELEGRAM_TEMPLATE: &str = include_str!("themes/telegram.tdesktop-theme");
const GTK_IMPORT: &str = "@import url(\"makshell.css\");";

static ACCENT: AtomicU32 = AtomicU32::new(bits(DEFAULT_COLOR));

const fn bits(rgb: [u8; 3]) -> u32 {
    (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32
}

/// The color of selections, switches and whatever else is the user's to choose.
pub fn accent() -> Color {
    let [_, red, green, blue] = ACCENT.load(Ordering::Relaxed).to_be_bytes();
    Color::from_rgba8(red, green, blue, ACCENT_ALPHA)
}

pub fn set_accent(rgb: [u8; 3]) {
    ACCENT.store(bits(behind_white_text(rgb)), Ordering::Relaxed);
}

/// The shell puts white text on its accent, so one that's too light for that is made darker. Its hue
/// stays. macOS's own colors, but for yellow, are already dark enough.
fn behind_white_text(rgb: [u8; 3]) -> [u8; 3] {
    let hct = Hct::new(Rgb::new(rgb[0], rgb[1], rgb[2]));
    if hct.get_tone() <= LIGHTEST_ACCENT {
        return rgb;
    }
    let darker: Rgb = Hct::from(hct.get_hue(), hct.get_chroma(), LIGHTEST_ACCENT).into();
    [darker.red, darker.green, darker.blue]
}

/// What the config file's `accent` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Color([u8; 3]),
    /// The color at this place among what's picked out of the wallpaper.
    Wallpaper(usize),
}

impl Choice {
    pub fn parse(text: &str) -> Self {
        if let Some((_, rgb)) = PRESETS.iter().find(|(name, _)| *name == text) {
            return Self::Color(*rgb);
        }
        if text == "wallpaper" {
            return Self::Wallpaper(0);
        }
        if let Some(place) = text.strip_prefix("wallpaper:").and_then(|n| n.parse().ok()) {
            return Self::Wallpaper(place);
        }
        Self::Color(parse_hex(text).unwrap_or(DEFAULT_COLOR))
    }
}

pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// A color from a wallpaper, as strong as a highlight behind white text has to be.
pub fn vivid(rgb: [u8; 3]) -> [u8; 3] {
    let hct = Hct::new(Rgb::new(rgb[0], rgb[1], rgb[2]));
    let strong: Rgb = Hct::from(hct.get_hue(), hct.get_chroma().max(48.0), 45.0).into();
    [strong.red, strong.green, strong.blue]
}

/// The accent this choice comes to, and the color a palette for other apps starts from, given what's
/// been picked out of the wallpaper.
pub fn resolve(choice: Choice, wallpaper: &[[u8; 3]]) -> ([u8; 3], [u8; 3]) {
    match choice {
        Choice::Color(rgb) => (rgb, rgb),
        Choice::Wallpaper(place) => match wallpaper.get(place).or_else(|| wallpaper.first()) {
            Some(rgb) => (vivid(*rgb), *rgb),
            None => (DEFAULT_COLOR, DEFAULT_COLOR),
        },
    }
}

fn home_dir(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| places::home().join(fallback))
}

fn config_home() -> PathBuf {
    home_dir("XDG_CONFIG_HOME", ".config")
}

pub fn data_home() -> PathBuf {
    home_dir("XDG_DATA_HOME", ".local/share")
}

pub fn state_dir() -> PathBuf {
    home_dir("XDG_STATE_HOME", ".local/state").join("makshell")
}

/// What's been picked out of this wallpaper before, if anything has since it last changed.
fn remembered_colors(path: &Path) -> Option<Vec<[u8; 3]>> {
    let saved = fs::read_to_string(state_dir().join("wallpaper-colors")).ok()?;
    let mut lines = saved.lines();
    if lines.next()? != wallpaper_key(path)? {
        return None;
    }
    lines.next()?.split(' ').map(parse_hex).collect()
}

fn wallpaper_key(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{} {}", since.as_secs(), path.display()))
}

/// The colors worth using in a wallpaper, best first. It's slow the first time.
pub fn wallpaper_colors(configured: Option<&str>) -> Vec<[u8; 3]> {
    let Some(path) = configured.and_then(wallpaper::resolve) else {
        return Vec::new();
    };
    if let Some(colors) = remembered_colors(&path) {
        return colors;
    }
    let Some(colors) = pick_colors(&path) else {
        return Vec::new();
    };
    if let Some(key) = wallpaper_key(&path) {
        let list: Vec<String> = colors.iter().map(|rgb| hex(*rgb)).collect();
        let saved = write_file(
            &state_dir().join("wallpaper-colors"),
            &format!("{key}\n{}\n", list.join(" ")),
        );
        if let Err(err) = saved {
            log::warn!("can't remember the wallpaper's colors: {err}");
        }
    }
    colors
}

fn pick_colors(path: &Path) -> Option<Vec<[u8; 3]>> {
    let picture = image::open(path)
        .inspect_err(|err| log::warn!("can't read the wallpaper for its colors: {err}"))
        .ok()?;
    let small = picture.thumbnail(LOOK_AT, LOOK_AT).to_rgb8();
    let pixels: Vec<Rgb> = small
        .pixels()
        .map(|pixel| Rgb::new(pixel[0], pixel[1], pixel[2]))
        .collect();
    let found = QuantizerCelebi::quantize(&pixels, 128);
    let best = Score::score(
        &found.color_to_count,
        Some(WALLPAPER_COLORS),
        None,
        Some(true),
    );
    Some(
        best.into_iter()
            .map(|rgb| [rgb.red, rgb.green, rgb.blue])
            .collect(),
    )
}

/// Sets the accent from the config file, as far as that's possible without looking at the wallpaper,
/// which [`refresh`] does.
pub fn init() {
    let config = Config::load();
    let colors = config
        .desktop
        .wallpaper
        .as_deref()
        .and_then(wallpaper::resolve)
        .and_then(|path| remembered_colors(&path))
        .unwrap_or_default();
    set_accent(resolve(Choice::parse(&config.appearance.accent), &colors).0);
}

/// A palette for other apps: its roles, by name.
pub struct Palette(Vec<(&'static str, Rgb)>);

impl Palette {
    /// A palette from this color, for a light or a dark appearance.
    pub fn from_seed(seed: [u8; 3], dark: bool) -> Self {
        let source = Rgb::new(seed[0], seed[1], seed[2]);
        let s: Scheme = SchemeContent::new(Hct::new(source), dark, None)
            .scheme
            .into();
        // A palette's color at another tone, for terminals.
        let at_tone = |rgb: Rgb, tone: f64| -> Rgb {
            let hct: Hct = rgb.into();
            Hct::from(hct.get_hue(), hct.get_chroma(), tone).into()
        };
        // Text on a container color. The content scheme only gives it the least contrast that's
        // readable, which is tiring in a terminal's selection: this keeps its hue, and the way it
        // goes, lighter or darker than the container, and goes on until it's comfortable.
        let on_container = |text: Rgb, container: Rgb| -> Rgb {
            let text_hct: Hct = text.into();
            let container_hct: Hct = container.into();
            let behind = luminance([container.red, container.green, container.blue]);
            let contrast = |rgb: &Rgb| {
                let front = luminance([rgb.red, rgb.green, rgb.blue]);
                (front.max(behind) + 0.05) / (front.min(behind) + 0.05)
            };
            let lighter = text_hct.get_tone() > container_hct.get_tone();
            let tone = |tone: f64| -> Rgb {
                Hct::from(text_hct.get_hue(), text_hct.get_chroma(), tone).into()
            };
            let tones = if lighter {
                [90.0, 95.0, 98.0, 100.0]
            } else {
                [10.0, 5.0, 2.0, 0.0]
            };
            tones
                .into_iter()
                .map(tone)
                .find(|candidate| contrast(candidate) >= 7.0)
                // A vivid container, like macOS blue's, is too mid-toned for that either way:
                // then whichever end reads better.
                .unwrap_or_else(|| {
                    let (light, dark) = (tone(100.0), tone(0.0));
                    if contrast(&light) >= contrast(&dark) {
                        light
                    } else {
                        dark
                    }
                })
        };
        let mut roles = vec![
            ("primary", s.primary),
            ("on_primary", s.on_primary),
            ("primary_container", s.primary_container),
            (
                "on_primary_container",
                on_container(s.on_primary_container, s.primary_container),
            ),
            ("secondary", s.secondary),
            ("on_secondary", s.on_secondary),
            ("secondary_container", s.secondary_container),
            (
                "on_secondary_container",
                on_container(s.on_secondary_container, s.secondary_container),
            ),
            ("tertiary", s.tertiary),
            ("on_tertiary", s.on_tertiary),
            ("tertiary_container", s.tertiary_container),
            (
                "on_tertiary_container",
                on_container(s.on_tertiary_container, s.tertiary_container),
            ),
            ("error", s.error),
            ("on_error", s.on_error),
            ("error_container", s.error_container),
            (
                "on_error_container",
                on_container(s.on_error_container, s.error_container),
            ),
            ("surface", s.surface),
            ("surface_dim", s.surface_dim),
            ("surface_bright", s.surface_bright),
            ("surface_container_lowest", s.surface_container_lowest),
            ("surface_container_low", s.surface_container_low),
            ("surface_container", s.surface_container),
            ("surface_container_high", s.surface_container_high),
            ("surface_container_highest", s.surface_container_highest),
            ("surface_variant", s.surface_variant),
            ("on_surface", s.on_surface),
            ("on_surface_variant", s.on_surface_variant),
            ("background", s.background),
            ("on_background", s.on_background),
            ("outline", s.outline),
            ("outline_variant", s.outline_variant),
            ("inverse_surface", s.inverse_surface),
            ("inverse_on_surface", s.inverse_on_surface),
            ("inverse_primary", s.inverse_primary),
            ("shadow", s.shadow),
            ("scrim", s.scrim),
            ("source", source),
        ];
        // A terminal's black is what's darkest and its white what's lightest, whichever the
        // background is.
        if dark {
            roles.extend([
                ("term_black", s.surface_container_high),
                ("term_white", s.on_surface_variant),
                ("term_bright_black", s.outline),
                ("term_bright_white", s.on_surface),
            ]);
        } else {
            roles.extend([
                ("term_black", s.on_surface),
                ("term_white", s.surface_container_highest),
                ("term_bright_black", s.outline),
                ("term_bright_white", s.surface_container_low),
            ]);
        }
        // A terminal's colors are the palette's own, all shades of the wallpaper, like Noctalia's:
        // red is still the palette's error color, so errors read as errors, and the rest are its
        // primary, secondary and tertiary colors, each at its own tone so that no two are alike.
        // On a light background, they have to be dark to be read; bright is a step further from
        // the background.
        let (light, middle, deep) = if dark {
            ((80.0, 90.0), (74.0, 85.0), (68.0, 78.0))
        } else {
            ((40.0, 30.0), (45.0, 35.0), (50.0, 40.0))
        };
        for (name, bright, rgb, (tone, bright_tone)) in [
            ("term_red", "term_bright_red", s.error, light),
            ("term_green", "term_bright_green", s.primary, light),
            ("term_yellow", "term_bright_yellow", s.tertiary, light),
            // Secondary is primary's hue, greyer: at the same tone, the two would be alike.
            ("term_blue", "term_bright_blue", s.secondary, middle),
            ("term_magenta", "term_bright_magenta", s.primary, deep),
            ("term_cyan", "term_bright_cyan", s.tertiary, deep),
        ] {
            roles.push((name, at_tone(rgb, tone)));
            roles.push((bright, at_tone(rgb, bright_tone)));
        }
        Self(roles)
    }

    /// The color of a role, for showing it.
    pub fn role(&self, name: &str) -> Option<[u8; 3]> {
        self.0
            .iter()
            .find(|(role, _)| *role == name)
            .map(|(_, rgb)| [rgb.red, rgb.green, rgb.blue])
    }

    /// Fills in a template: `{{role}}` is `#rrggbb`, and `{{role.rgb}}` is `r,g,b`. What it doesn't
    /// know stays as it is, which is easier to notice than nothing.
    pub fn render(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            rest = &rest[start..];
            let Some(end) = rest.find("}}") else {
                break;
            };
            let tag = rest[2..end].trim();
            let (name, csv) = match tag.strip_suffix(".rgb") {
                Some(name) => (name, true),
                None => (tag, false),
            };
            match self.0.iter().find(|(role, _)| *role == name) {
                Some((_, rgb)) if csv => {
                    out.push_str(&format!("{},{},{}", rgb.red, rgb.green, rgb.blue));
                }
                Some((_, rgb)) => out.push_str(&hex([rgb.red, rgb.green, rgb.blue])),
                None => out.push_str(&rest[..end + 2]),
            }
            rest = &rest[end + 2..];
        }
        out.push_str(rest);
        out
    }
}

/// What bringing apps in line with the config file came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub accent: [u8; 3],
    /// The icon theme changed, which the shell's own icons come from too.
    pub icons_changed: bool,
}

/// Brings everything in line with the config file, as far as that's changed since last time.
pub async fn refresh() -> Option<Applied> {
    let (accent, changed_colors, icons_changed) = tokio::task::spawn_blocking(sync)
        .await
        .inspect_err(|err| log::warn!("couldn't update the colors: {err}"))
        .ok()?;
    if changed_colors {
        tell_kde_apps(false).await;
    }
    if icons_changed {
        tell_kde_apps(true).await;
    }
    Some(Applied {
        accent,
        icons_changed,
    })
}

/// After the MacOS-Like icons were remade: KDE apps load them again, and if they've only just
/// arrived, apps get them.
pub async fn icons_remade() -> Option<Applied> {
    let applied = refresh().await;
    tell_kde_apps(true).await;
    applied
}

/// One at a time, each going by the config file as it is by then, so the last one is right however
/// quickly the accent is changed.
static SYNCING: Mutex<()> = Mutex::new(());

/// What's been done, as a line in a file: whether apps have a palette made for them, light or dark,
/// with the accent, and from what and which templates.
fn applied_state(
    generated: bool,
    light: bool,
    accent: [u8; 3],
    seed: [u8; 3],
    icons: IconChoice,
) -> String {
    let templates = if generated {
        templates_fingerprint()
    } else {
        0
    };
    format!(
        "{} {} {} {} {templates:x} {}",
        if generated { "generated" } else { "own" },
        if light { "light" } else { "dark" },
        hex(accent),
        if generated {
            hex(seed)
        } else {
            String::from("-")
        },
        icons.word(),
    )
}

/// Which icons apps get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IconChoice {
    /// MacOS-Like's, which are there.
    Macos,
    /// MacOS-Like's, once they're built; till then, whatever there is.
    Waiting,
    /// The ones they had before.
    Own,
}

impl IconChoice {
    fn of(config: &Config) -> Self {
        if !config.appearance.macos_icons {
            Self::Own
        } else if icon_themes::installed() {
            Self::Macos
        } else {
            Self::Waiting
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Macos => "macos-icons",
            Self::Waiting => "waiting-icons",
            Self::Own => "own-icons",
        }
    }
}

/// What the last line said, read the way it's written now or was before: whether apps had a palette
/// made for them, whether that was light, and the accent.
fn read_state(line: &str) -> (bool, bool, Option<String>) {
    let words: Vec<&str> = line.split(' ').collect();
    let generated = matches!(words.first(), Some(&"generated" | &"matching"));
    match words.get(1) {
        // Before there was light, the accent came second.
        Some(word) if word.starts_with('#') => (generated, false, Some((*word).to_owned())),
        Some(mode) => (
            generated,
            *mode == "light",
            words.get(2).map(|word| (*word).to_owned()),
        ),
        None => (generated, false, None),
    }
}

/// Goes up when the palette's roles come out differently for the same seed, so the files made
/// from it are written again.
const PALETTE_VERSION: u32 = 4;

/// Changes when a template does, built in or the user's, or the palette, which means files have to
/// be written again.
fn templates_fingerprint() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    PALETTE_VERSION.hash(&mut hasher);
    for template in [
        KDE_TEMPLATE,
        GTK3_TEMPLATE,
        GTK4_TEMPLATE,
        ALACRITTY_TEMPLATE,
        TELEGRAM_TEMPLATE,
    ] {
        template.hash(&mut hasher);
    }
    let mut own: Vec<_> = user_templates().into_iter().collect();
    own.sort();
    own.hash(&mut hasher);
    hasher.finish()
}

fn sync() -> ([u8; 3], bool, bool) {
    let _one_at_a_time = SYNCING.lock();
    let config = Config::load();
    let colors = wallpaper_colors(config.desktop.wallpaper.as_deref());
    let (accent, seed) = resolve(Choice::parse(&config.appearance.accent), &colors);
    let matching = config.appearance.match_apps;
    let light = Mode::parse(&config.appearance.mode).is_light(chrono::Local::now());
    // A light appearance takes a palette made for it, even when apps otherwise keep their own: it's
    // their own scheme's light twin, from its accent.
    let generated = matching || light;
    let own_accent = own_kde_accent().unwrap_or(accent);
    let (palette_seed, kde_accent) = if matching {
        (seed, accent)
    } else {
        (own_accent, own_accent)
    };

    let icons = IconChoice::of(&config);
    let state_file = state_dir().join("colors-applied");
    let state = applied_state(generated, light, accent, palette_seed, icons);
    let before = fs::read_to_string(&state_file).ok();
    let before = before.as_deref().map(str::trim);
    if before == Some(state.as_str()) {
        return (accent, false, false);
    }
    let (was_generated, was_light, was_accent) = read_state(before.unwrap_or_default());
    // The first time, there's nothing to go by, and KDE's accent is left as it's been set.
    let accent_changed = was_accent.is_some_and(|old| old != hex(accent));

    let mut changed_colors = false;
    let mut done = true;
    if generated {
        match match_apps(palette_seed, kde_accent, light, accent) {
            Ok(()) => changed_colors = true,
            Err(err) => {
                log::warn!("can't make apps match: {err}");
                done = false;
            }
        }
    } else {
        if was_generated {
            match stop_matching_apps(accent) {
                Ok(()) => changed_colors = true,
                Err(err) => {
                    log::warn!("can't put apps' colors back: {err}");
                    done = false;
                }
            }
        }
        if accent_changed {
            match set_kde_accent(accent) {
                Ok(()) => changed_colors = true,
                Err(err) => {
                    log::warn!("can't set KDE's accent: {err}");
                    done = false;
                }
            }
        }
    }
    let mut icons_changed = false;
    if before.is_none() || light != was_light {
        icons_changed = set_appearance(light);
    }
    let was_icons = before.and_then(|line| line.split(' ').nth(5));
    if was_icons != Some(icons.word()) {
        icons_changed |= choose_icons(icons, light);
    }
    if done && let Err(err) = write_file(&state_file, &state) {
        log::warn!("can't remember what colors were applied: {err}");
    }
    (accent, changed_colors, icons_changed)
}

/// Light or dark, or whichever the sun says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
    Auto,
}

impl Mode {
    pub fn parse(text: &str) -> Self {
        match text {
            "light" => Self::Light,
            "auto" => Self::Auto,
            _ => Self::Dark,
        }
    }

    /// Whether it's light now. Auto is light between sunrise and sunset where the weather widget
    /// is, and from seven to seven without a place to go by.
    pub fn is_light(self, now: chrono::DateTime<chrono::Local>) -> bool {
        match self {
            Self::Light => true,
            Self::Dark => false,
            Self::Auto => match place()
                .and_then(|(latitude, longitude)| sun_times(now.date_naive(), latitude, longitude))
            {
                Some((sunrise, sunset)) => {
                    let now = now.timestamp();
                    sunrise <= now && now < sunset
                }
                None => {
                    use chrono::Timelike as _;
                    (7..19).contains(&now.hour())
                }
            },
        }
    }
}

/// When the sun rises and sets on a day, as Unix times, by the sunrise equation (as NOAA's): within a
/// minute or two, which is plenty. `None` when it doesn't, near the poles.
pub fn sun_times(day: chrono::NaiveDate, latitude: f64, longitude: f64) -> Option<(i64, i64)> {
    use std::f64::consts::PI;
    let radians = |degrees: f64| degrees * PI / 180.0;
    let degrees = |radians: f64| radians * 180.0 / PI;
    let noon = day.and_hms_opt(12, 0, 0)?.and_utc().timestamp() as f64;
    let julian_day = noon / 86400.0 + 2440587.5;
    let n = (julian_day - 2451545.0 + 0.0008).round();
    let mean_solar_time = n - longitude / 360.0;
    let anomaly = (357.5291 + 0.98560028 * mean_solar_time).rem_euclid(360.0);
    let m = radians(anomaly);
    let center = 1.9148 * m.sin() + 0.02 * (2.0 * m).sin() + 0.0003 * (3.0 * m).sin();
    let ecliptic = radians((anomaly + center + 180.0 + 102.9372).rem_euclid(360.0));
    let transit = 2451545.0 + mean_solar_time + 0.0053 * m.sin() - 0.0069 * (2.0 * ecliptic).sin();
    let declination = (ecliptic.sin() * radians(23.4397).sin()).asin();
    let phi = radians(latitude);
    let cos_hour_angle =
        (radians(-0.833).sin() - phi.sin() * declination.sin()) / (phi.cos() * declination.cos());
    if !(-1.0..=1.0).contains(&cos_hour_angle) {
        return None;
    }
    let hour_angle = degrees(cos_hour_angle.acos());
    let unix = |julian: f64| ((julian - 2440587.5) * 86400.0).round() as i64;
    Some((
        unix(transit - hour_angle / 360.0),
        unix(transit + hour_angle / 360.0),
    ))
}

/// Where the weather widget is, which is where the sun's looked at for Auto. It's looked up once for
/// each city, and kept.
fn place() -> Option<(f64, f64)> {
    let city = crate::widgets::saved_city()?;
    let saved = state_dir().join("place");
    if let Ok(text) = fs::read_to_string(&saved)
        && let Some((name, at)) = text.trim().split_once('\t')
        && name == city
        && let Some((latitude, longitude)) = at.split_once(' ')
        && let (Ok(latitude), Ok(longitude)) = (latitude.parse(), longitude.parse())
    {
        return Some((latitude, longitude));
    }
    let (latitude, longitude) = crate::widgets::locate(&city)?;
    if let Err(err) = write_file(&saved, &format!("{city}\t{latitude} {longitude}\n")) {
        log::warn!("can't remember where the weather widget is: {err}");
    }
    Some((latitude, longitude))
}

/// The accent of KDE's own color scheme, from before a palette was made for apps if one has been.
fn own_kde_accent() -> Option<[u8; 3]> {
    let kept = saved_dir().join("kdeglobals");
    let path = if kept.exists() {
        kept
    } else {
        config_home().join("kdeglobals")
    };
    let text = fs::read_to_string(path).ok()?;
    let general = kde_groups(&text)
        .into_iter()
        .find(|(header, _)| header == "[General]")?;
    let value = general
        .1
        .iter()
        .find(|line| key_of(line) == "AccentColor")?
        .split_once('=')?
        .1
        .to_owned();
    let mut channels = value
        .split(',')
        .map(|channel| channel.trim().parse::<u8>().ok());
    Some([channels.next()??, channels.next()??, channels.next()??])
}

/// The parts of a light or dark appearance that aren't colors: what GTK is asked for, and the icon
/// theme's light or dark twin, if it has one. Returns whether the icon theme changed.
fn set_appearance(light: bool) -> bool {
    let scheme = if light { "prefer-light" } else { "prefer-dark" };
    let gtk_theme = if light { "Adwaita" } else { "Adwaita-dark" };
    gsettings("color-scheme", scheme);
    gsettings("gtk-theme", gtk_theme);

    let path = config_home().join("kdeglobals");
    let globals = fs::read_to_string(&path).unwrap_or_default();
    let Some(current) = kde_icon_theme(&globals) else {
        return false;
    };
    let Some(twin) = icon_twin(&current, light, icon_theme_exists) else {
        return false;
    };
    if twin == current {
        return false;
    }
    set_icon_theme(&path, &globals, &twin)
}

/// `[Icons] Theme` in this `kdeglobals`, where KDE only writes it when it isn't Breeze.
fn kde_icon_theme(globals: &str) -> Option<String> {
    kde_groups(globals)
        .into_iter()
        .find(|(header, _)| header == "[Icons]")
        .and_then(|(_, lines)| {
            lines
                .iter()
                .find(|line| key_of(line) == "Theme")
                .and_then(|line| line.split_once('=').map(|(_, theme)| theme.to_owned()))
        })
}

/// Gives KDE apps, and GTK's, this icon theme. A `kdeglobals` that's a link, as home-manager makes
/// them, stays as it is.
fn set_icon_theme(path: &Path, globals: &str, theme: &str) -> bool {
    if fs::symlink_metadata(path).is_err() || can_change(path) {
        let mut groups = kde_groups(globals);
        merge_kde_keys(&format!("[Icons]\nTheme={theme}\n"), &mut groups);
        if let Err(err) = write_file(path, &kde_text(&groups)) {
            log::warn!("can't change the icon theme: {err}");
            return false;
        }
    }
    gsettings("icon-theme", theme);
    true
}

/// Switches apps to MacOS-Like's icons, keeping what they had to go back to, or back to that.
fn choose_icons(choice: IconChoice, light: bool) -> bool {
    let path = config_home().join("kdeglobals");
    let globals = fs::read_to_string(&path).unwrap_or_default();
    let current = kde_icon_theme(&globals);
    let ours = current.as_deref().is_some_and(icon_themes::is_ours);
    let before = state_dir().join("icons-before");
    let next = match choice {
        IconChoice::Macos => {
            if !ours {
                // Empty when KDE's default was in use.
                if let Err(err) = write_file(&before, current.as_deref().unwrap_or_default()) {
                    log::warn!("can't remember the icon theme: {err}");
                    return false;
                }
            }
            icon_themes::theme(light).to_owned()
        }
        IconChoice::Own if ours => {
            let old = fs::read_to_string(&before)
                .ok()
                .map(|theme| theme.trim().to_owned())
                .filter(|theme| !theme.is_empty() && !icon_themes::is_ours(theme))
                .unwrap_or_else(|| String::from("breeze"));
            icon_twin(&old, light, icon_theme_exists).unwrap_or(old)
        }
        IconChoice::Own | IconChoice::Waiting => return false,
    };
    if current.as_deref() == Some(next.as_str()) {
        return false;
    }
    log::info!("icon theme {next}");
    set_icon_theme(&path, &globals, &next)
}

/// The light or dark twin of an icon theme, by its name: `-Light` and `-Dark`, or without either
/// for the light one, as long as it's there.
fn icon_twin(theme: &str, light: bool, exists: impl Fn(&str) -> bool) -> Option<String> {
    let lower = theme.to_ascii_lowercase();
    let base = ["-dark", "-light"]
        .iter()
        .find(|suffix| lower.ends_with(**suffix))
        .map_or(theme, |suffix| &theme[..theme.len() - suffix.len()]);
    let candidates: Vec<String> = if light {
        vec![
            format!("{base}-Light"),
            format!("{base}-light"),
            base.to_owned(),
        ]
    } else {
        vec![format!("{base}-Dark"), format!("{base}-dark")]
    };
    candidates.into_iter().find(|name| exists(name))
}

/// Whether an icon theme by this id is installed anywhere icons are looked for.
pub fn icon_theme_exists(name: &str) -> bool {
    let mut roots = vec![data_home().join("icons"), places::home().join(".icons")];
    // Unset, it's the spec's default.
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| String::from("/usr/local/share:/usr/share"));
    roots.extend(
        dirs.split(':')
            .filter(|dir| !dir.is_empty())
            .map(|dir| PathBuf::from(dir).join("icons")),
    );
    roots
        .iter()
        .any(|root| root.join(name).join("index.theme").is_file())
}

/// GNOME's interface settings, which GTK reads and watches, and which is all that GTK 3 apps know of
/// light and dark.
fn gsettings(key: &str, value: &str) {
    let result = std::process::Command::new("gsettings")
        .args(["set", "org.gnome.desktop.interface", key, value])
        .output();
    match result {
        Ok(output) if !output.status.success() => log::debug!(
            "gsettings didn't take {key}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Ok(_) => {}
        Err(err) => log::debug!("can't run gsettings: {err}"),
    }
}

/// KDE's accent, on its own, leaving the rest of its colors as they are. There's a tool for this,
/// `plasma-apply-colorscheme --accent-color`, but it applies the current color scheme again along
/// with it, and when it can't find that scheme's file, which only has to be named differently, it
/// blanks every color there is. It doesn't save the accent as the setting, either.
fn set_kde_accent(accent: [u8; 3]) -> io::Result<()> {
    let path = config_home().join("kdeglobals");
    if !can_change(&path) {
        return Ok(());
    }
    // These colors have no way back otherwise, and nothing else can tell what they were.
    let original = state_dir().join("kdeglobals-before-accent");
    if !original.exists() {
        fs::create_dir_all(state_dir())?;
        fs::copy(&path, original)?;
    }
    write_file(&path, &with_kde_accent(&fs::read_to_string(&path)?, accent))
}

/// `kdeglobals` with this accent, in the colors that follow it, as KDE's own settings do it.
fn with_kde_accent(globals: &str, accent: [u8; 3]) -> String {
    let [red, green, blue] = accent;
    let color = format!("{red},{green},{blue}");
    let darker = [red, green, blue].map(|channel| (f32::from(channel) * 0.75) as u8);
    let darker = format!("{},{},{}", darker[0], darker[1], darker[2]);
    let ink = if luminance(accent) > 0.45 {
        "0,0,0"
    } else {
        "255,255,255"
    };
    let mut groups = kde_groups(globals);
    for (header, lines) in &mut groups {
        if header.starts_with("[Colors:") {
            for key in ["DecorationFocus", "DecorationHover", "ForegroundActive"] {
                set_kde_key(lines, key, &color);
            }
        }
        if header == "[Colors:Selection]" {
            set_kde_key(lines, "BackgroundNormal", &color);
            set_kde_key(lines, "BackgroundAlternate", &darker);
            set_kde_key(lines, "ForegroundNormal", ink);
            set_kde_key(lines, "ForegroundInactive", ink);
        }
    }
    let general = format!("[General]\nAccentColor={color}\naccentColorFromWallpaper=false\n");
    merge_kde_keys(&general, &mut groups);
    kde_text(&groups)
}

fn set_kde_key(lines: &mut Vec<String>, key: &str, value: &str) {
    let line = format!("{key}={value}");
    match lines.iter().position(|old| key_of(old) == key) {
        Some(at) => lines[at] = line,
        None => lines.push(line),
    }
}

/// How bright a color looks, from 0 to 1.
fn luminance(rgb: [u8; 3]) -> f32 {
    let linear = |channel: u8| {
        let c = f32::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

/// Running KDE apps redo their colors, or their icons, when they hear this.
async fn tell_kde_apps(icons: bool) {
    let Some(connection) = dbus::session().await else {
        return;
    };
    // KGlobalSettings' palette change, or its icon change.
    let kind: i32 = if icons { 4 } else { 0 };
    let sent = connection
        .emit_signal(
            None::<&str>,
            "/KGlobalSettings",
            "org.kde.KGlobalSettings",
            "notifyChange",
            &(kind, 0i32),
        )
        .await;
    if let Err(err) = sent {
        log::warn!("can't tell KDE apps about their colors: {err}");
    }
    // What KConfigWatcher hears, as KWin's window decorations do: which keys changed, by group.
    let globals = fs::read_to_string(config_home().join("kdeglobals")).unwrap_or_default();
    let changed = changed_kde_keys(&globals, icons);
    if !changed.is_empty() {
        let sent = connection
            .emit_signal(
                None::<&str>,
                "/kdeglobals",
                "org.kde.kconfig.notify",
                "ConfigChanged",
                &(changed,),
            )
            .await;
        if let Err(err) = sent {
            log::warn!("can't tell KWin about the colors: {err}");
        }
    }
    if icons {
        let sent = connection
            .emit_signal(
                None::<&str>,
                "/KIconLoader",
                "org.kde.KIconLoader",
                "iconChanged",
                &(0i32,),
            )
            .await;
        if let Err(err) = sent {
            log::warn!("can't tell KDE apps about their icons: {err}");
        }
    }
}

/// The keys in `kdeglobals` that a color change touches, by group, named the way KConfig sends
/// them: a group inside another one is `Outer\x1dInner`. Or the icon theme, for an icon change.
fn changed_kde_keys(globals: &str, icons: bool) -> HashMap<String, Vec<Vec<u8>>> {
    let mut changed = HashMap::new();
    for (header, lines) in kde_groups(globals) {
        let wanted = if icons {
            header == "[Icons]"
        } else {
            scheme_owns(&header) || header == "[General]"
        };
        if !wanted || lines.is_empty() {
            continue;
        }
        let name = header
            .trim_start_matches('[')
            .trim_end_matches(']')
            .replace("][", "\x1d");
        let keys = lines
            .iter()
            .filter(|line| line.contains('='))
            .map(|line| key_of(line).trim().as_bytes().to_vec())
            .collect::<Vec<_>>();
        if !keys.is_empty() {
            changed.insert(name, keys);
        }
    }
    changed
}

/// Writes a file, unless it's what's already there, making the folder it goes in. Apps read these
/// whenever they like, so it's never half written. A link stays a link, and the file it's to changes.
fn write_file(path: &Path, content: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = target.clone().into_os_string();
    temporary.push(".makshell-new");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, content)?;
    if let Ok(meta) = fs::metadata(&target) {
        fs::set_permissions(&temporary, meta.permissions())?;
    }
    fs::rename(temporary, target)
}

/// Whether this is a file of the user's to change, rather than a link into the Nix store.
fn can_change(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

fn gtk_css(version: u8) -> PathBuf {
    config_home().join(format!("gtk-{version}.0/gtk.css"))
}

fn alacritty_config() -> PathBuf {
    config_home().join("alacritty/alacritty.toml")
}

fn saved_dir() -> PathBuf {
    state_dir().join("before-matching")
}

/// Keeps what's about to be changed, unless that's already been done.
fn save_before_matching(accent: [u8; 3]) -> io::Result<()> {
    let saved = saved_dir();
    if saved.exists() {
        return Ok(());
    }
    fs::create_dir_all(&saved)?;
    fs::write(saved.join("accent"), hex(accent))?;
    for (from, name) in [
        (config_home().join("kdeglobals"), "kdeglobals"),
        (gtk_css(3), "gtk3.css"),
        (gtk_css(4), "gtk4.css"),
        (alacritty_config(), "alacritty.toml"),
    ] {
        if can_change(&from) {
            fs::copy(from, saved.join(name))?;
        }
    }
    Ok(())
}

/// `accent` is KDE's, and `shell_accent` the shell's, which is what's kept to tell, when apps get
/// their own colors back, whether it changed since.
fn match_apps(
    seed: [u8; 3],
    accent: [u8; 3],
    light: bool,
    shell_accent: [u8; 3],
) -> io::Result<()> {
    save_before_matching(shell_accent)?;
    let palette = Palette::from_seed(seed, !light);

    // KDE and Qt. Applying a scheme by name does nothing if that's the name already in use, so it goes
    // straight into the settings, which is all that does anyway.
    let scheme = palette.render(KDE_TEMPLATE);
    write_file(&data_home().join("color-schemes/Makshell.colors"), &scheme)?;
    let globals_path = config_home().join("kdeglobals");
    if can_change(&globals_path) || !globals_path.exists() {
        let globals = fs::read_to_string(&globals_path).unwrap_or_default();
        let [red, green, blue] = accent;
        let ours = format!(
            "{scheme}\n[General]\nAccentColor={red},{green},{blue}\naccentColorFromWallpaper=false\n"
        );
        write_file(&globals_path, &merge_kde(&ours, &globals))?;
    } else {
        log::warn!("{} isn't ours to change", globals_path.display());
    }

    // GTK. Apps that are open pick this up when they next start.
    for (version, template) in [(3, GTK3_TEMPLATE), (4, GTK4_TEMPLATE)] {
        let css = gtk_css(version);
        write_file(
            &config_home().join(format!("gtk-{version}.0/makshell.css")),
            &palette.render(template),
        )?;
        if can_change(&css) || !css.exists() {
            let old = fs::read_to_string(&css).unwrap_or_default();
            write_file(&css, &with_gtk_import(&old))?;
        } else {
            log::warn!("{} isn't ours to change", css.display());
        }
    }

    // Alacritty takes this up by itself.
    let alacritty = alacritty_config();
    if can_change(&alacritty) {
        let theme = config_home().join("alacritty/themes/makshell.toml");
        write_file(&theme, &palette.render(ALACRITTY_TEMPLATE))?;
        let old = fs::read_to_string(&alacritty)?;
        match importing(&old, &theme.to_string_lossy()) {
            Some(new) => write_file(&alacritty, &new)?,
            None => log::warn!("can't read {}", alacritty.display()),
        }
    }

    // Telegram only takes a theme when it's loaded in it.
    if data_home().join("TelegramDesktop").exists()
        || config_home().join("telegram-desktop").exists()
    {
        write_file(
            &config_home().join("telegram-desktop/themes/makshell.tdesktop-theme"),
            &palette.render(TELEGRAM_TEMPLATE),
        )?;
    }

    render_user_templates(&palette);
    Ok(())
}

/// The templates in makshell's config folder, as where they go and what's in them.
fn user_templates() -> Vec<(String, String)> {
    let Ok(templates) = fs::read_dir(config_home().join("makshell/templates")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for template in templates.flatten() {
        let Ok(text) = fs::read_to_string(template.path()) else {
            continue;
        };
        let (first, body) = text.split_once('\n').unwrap_or((&text, ""));
        match first.split_once("output:") {
            Some((_, output)) => found.push((output.trim().to_owned(), body.to_owned())),
            None => log::warn!(
                "{} doesn't start with where it goes",
                template.path().display()
            ),
        }
    }
    found
}

fn render_user_templates(palette: &Palette) {
    for (output, body) in user_templates() {
        let path = match output.strip_prefix("~/") {
            Some(rest) => places::home().join(rest),
            None => PathBuf::from(&output),
        };
        if let Err(err) = write_file(&path, &palette.render(&body)) {
            log::warn!("can't write {}: {err}", path.display());
        }
    }
}

fn stop_matching_apps(accent: [u8; 3]) -> io::Result<()> {
    let saved = saved_dir();
    if !saved.exists() {
        return Ok(());
    }
    let accent_then = fs::read_to_string(saved.join("accent")).ok();
    if let Ok(before) = fs::read_to_string(saved.join("kdeglobals")) {
        let path = config_home().join("kdeglobals");
        let now = fs::read_to_string(&path).unwrap_or_default();
        write_file(&path, &kde_colors_restored(&now, &before))?;
    }
    for (version, name) in [(3, "gtk3.css"), (4, "gtk4.css")] {
        let css = gtk_css(version);
        match fs::read_to_string(saved.join(name)) {
            Ok(before) => write_file(&css, &before)?,
            // It wasn't there before; what's left is only what we added.
            Err(_) if can_change(&css) => fs::remove_file(&css)?,
            Err(_) => {}
        }
    }
    if let Ok(before) = fs::read_to_string(saved.join("alacritty.toml")) {
        let alacritty = alacritty_config();
        if can_change(&alacritty) {
            let now = fs::read_to_string(&alacritty)?;
            if let Some(restored) = imports_restored(&now, &before) {
                write_file(&alacritty, &restored)?;
            }
        }
    }
    // What's come back has the accent of back then. If it's another one by now, that's the one.
    if accent_then.as_deref() != Some(hex(accent).as_str()) {
        set_kde_accent(accent)?;
    }
    fs::remove_dir_all(saved)
}

/// A file like `kdeglobals`, as its groups, in order: each one's header, and its lines.
fn kde_groups(text: &str) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
    for line in text.lines() {
        if line.starts_with('[') {
            groups.push((line.to_owned(), Vec::new()));
        } else if !line.trim().is_empty() {
            groups
                .last_mut()
                .expect("there's always one")
                .1
                .push(line.to_owned());
        }
    }
    groups
}

fn kde_text(groups: &[(String, Vec<String>)]) -> String {
    let mut text = String::new();
    for (header, lines) in groups {
        if header.is_empty() && lines.is_empty() {
            continue;
        }
        if !header.is_empty() {
            text.push_str(header);
            text.push('\n');
        }
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        text.push('\n');
    }
    text
}

/// A color scheme's groups are all the scheme's. Others are shared, so only its keys change.
fn scheme_owns(header: &str) -> bool {
    header.starts_with("[Colors:") || header.starts_with("[ColorEffects:") || header == "[WM]"
}

fn key_of(line: &str) -> &str {
    line.split_once('=').map_or(line, |(key, _)| key)
}

/// Puts a color scheme into the contents of `kdeglobals`, the way applying it does.
fn merge_kde(scheme: &str, globals: &str) -> String {
    let mut groups = kde_groups(globals);
    groups.retain(|(header, _)| !scheme_owns(header));
    merge_kde_keys(scheme, &mut groups);
    kde_text(&groups)
}

/// Adds what's in `from` to these groups: whole groups that belong to a color scheme, and otherwise
/// one key at a time.
fn merge_kde_keys(from: &str, groups: &mut Vec<(String, Vec<String>)>) {
    for (header, lines) in kde_groups(from) {
        if header.is_empty() {
            continue;
        }
        if scheme_owns(&header) {
            groups.push((header, lines));
            continue;
        }
        let index = match groups.iter().position(|(name, _)| *name == header) {
            Some(index) => index,
            None => {
                groups.push((header, Vec::new()));
                groups.len() - 1
            }
        };
        for line in lines {
            if let Some((key, value)) = line.split_once('=') {
                set_kde_key(&mut groups[index].1, key, value);
            }
        }
    }
}

/// `kdeglobals` with its colors as they were in `before`, and everything else as it is now.
fn kde_colors_restored(now: &str, before: &str) -> String {
    const GENERAL: [&str; 4] = [
        "ColorScheme",
        "Name",
        "AccentColor",
        "accentColorFromWallpaper",
    ];
    let mut groups = kde_groups(now);
    groups.retain(|(header, _)| !scheme_owns(header));
    let before = kde_groups(before);
    groups.extend(
        before
            .iter()
            .filter(|(header, _)| scheme_owns(header))
            .cloned(),
    );
    let was: Vec<String> = before
        .iter()
        .find(|(header, _)| header == "[General]")
        .map_or_else(Vec::new, |(_, lines)| lines.clone());
    if let Some((_, lines)) = groups.iter_mut().find(|(header, _)| header == "[General]") {
        lines.retain(|line| !GENERAL.contains(&key_of(line)));
        lines.extend(
            was.into_iter()
                .filter(|line| GENERAL.contains(&key_of(line))),
        );
    }
    kde_text(&groups)
}

/// A GTK style sheet importing our colors instead of the last theme's.
fn with_gtk_import(css: &str) -> String {
    let mut lines: Vec<&str> = css
        .lines()
        .filter(|line| !(line.contains("@import") && line.contains("noctalia.css")))
        .filter(|line| line.trim() != GTK_IMPORT)
        .collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.push(GTK_IMPORT);
    lines.join("\n") + "\n"
}

/// Alacritty's settings with our theme as the theme, in place of the last one, keeping the comments.
fn importing(config: &str, theme: &str) -> Option<String> {
    let mut doc: toml_edit::DocumentMut = config.parse().ok()?;
    let general = doc
        .entry("general")
        .or_insert_with(toml_edit::table)
        .as_table_mut()?;
    let mut imports = toml_edit::Array::new();
    if let Some(old) = general.get("import").and_then(|item| item.as_array()) {
        for entry in old.iter().filter_map(|entry| entry.as_str()) {
            if !entry.contains("/themes/") {
                imports.push(entry);
            }
        }
    }
    imports.push(theme);
    general["import"] = toml_edit::value(imports);
    Some(doc.to_string())
}

/// Alacritty's settings with the imports they had before, and everything else as it is now.
fn imports_restored(now: &str, before: &str) -> Option<String> {
    let mut doc: toml_edit::DocumentMut = now.parse().ok()?;
    let before: toml_edit::DocumentMut = before.parse().ok()?;
    match before
        .get("general")
        .and_then(|general| general.get("import"))
    {
        Some(imports) => doc["general"]["import"] = imports.clone(),
        None => {
            doc.get_mut("general")
                .and_then(|general| general.as_table_mut())
                .map(|general| general.remove("import"));
        }
    }
    Some(doc.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn color_changes_name_their_groups_and_keys() {
        let globals = "[General]\nAccentColor=1,2,3\n[Colors:Header]\nBackgroundNormal=1,1,1\n\
                       [Colors:Header][Inactive]\nBackgroundNormal=2,2,2\n[Icons]\nTheme=X\n\
                       [KDE]\nwidgetStyle=Breeze\n";
        let colors = super::changed_kde_keys(globals, false);
        assert_eq!(colors.len(), 3);
        assert_eq!(
            colors["Colors:Header\x1dInactive"],
            vec![b"BackgroundNormal".to_vec()]
        );
        assert_eq!(colors["General"], vec![b"AccentColor".to_vec()]);
        let icons = super::changed_kde_keys(globals, true);
        assert_eq!(icons.keys().collect::<Vec<_>>(), ["Icons"]);
    }

    use super::*;

    #[test]
    fn choices_read_the_way_theyre_written() {
        assert_eq!(Choice::parse("blue"), Choice::Color([0, 122, 255]));
        assert_eq!(Choice::parse("#102030"), Choice::Color([16, 32, 48]));
        assert_eq!(Choice::parse("wallpaper"), Choice::Wallpaper(0));
        assert_eq!(Choice::parse("wallpaper:2"), Choice::Wallpaper(2));
        // What makes no sense is the default.
        assert_eq!(Choice::parse("nonsense"), Choice::Color(DEFAULT_COLOR));
        assert_eq!(parse_hex("#1020"), None);
        assert_eq!(parse_hex("зеленый"), None);
    }

    #[test]
    fn the_accent_is_the_default_until_its_set() {
        let (red, green, blue) = (DEFAULT_COLOR[0], DEFAULT_COLOR[1], DEFAULT_COLOR[2]);
        assert_eq!(accent(), Color::from_rgba8(red, green, blue, ACCENT_ALPHA));
    }

    #[test]
    fn a_wallpaper_with_no_colors_falls_back() {
        let (accent, seed) = resolve(Choice::Wallpaper(0), &[]);
        assert_eq!((accent, seed), (DEFAULT_COLOR, DEFAULT_COLOR));
        // Past the end, it's the best one.
        let colors = [[10, 200, 30]];
        assert_eq!(resolve(Choice::Wallpaper(3), &colors).1, colors[0]);
    }

    #[test]
    fn a_color_from_a_wallpaper_is_strong_enough_for_white_text() {
        // Even from something washed out.
        let [red, green, blue] = vivid([200, 190, 195]);
        let luminance =
            0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue);
        assert!(luminance < 140.0, "{red} {green} {blue}");
    }

    #[test]
    fn a_light_accent_gets_dark_enough_for_white_text_and_others_stay() {
        let rgb = |name: &str| PRESETS.iter().find(|(n, _)| *n == name).unwrap().1;
        for name in [
            "blue", "purple", "pink", "red", "orange", "green", "graphite",
        ] {
            assert_eq!(behind_white_text(rgb(name)), rgb(name), "{name}");
        }
        assert!(luminance(behind_white_text(rgb("yellow"))) < luminance(rgb("yellow")));
        assert!(luminance(behind_white_text([255, 255, 255])) < 0.45);
    }

    #[test]
    fn kdes_accent_changes_only_what_follows_it() {
        let globals = "[Colors:Selection]\nBackgroundNormal=1,1,1\nDecorationFocus=1,1,1\nForegroundLink=7,7,7\n\n[Colors:Window]\nBackgroundNormal=2,2,2\nDecorationHover=2,2,2\n\n[General]\nAccentColor=3,3,3\naccentColorFromWallpaper=true\nfixed=Mono\n\n[WM]\nactiveBackground=4,4,4\n";
        let new = with_kde_accent(globals, [0, 122, 255]);
        let group = |name: &str| {
            new.split(name)
                .nth(1)
                .unwrap()
                .split("\n\n")
                .next()
                .unwrap()
                .to_owned()
        };
        let selection = group("[Colors:Selection]");
        assert!(selection.contains("BackgroundNormal=0,122,255"));
        assert!(selection.contains("ForegroundLink=7,7,7"));
        assert!(selection.contains("ForegroundNormal=255,255,255"));
        let window = group("[Colors:Window]");
        assert!(window.contains("BackgroundNormal=2,2,2"));
        assert!(window.contains("DecorationHover=0,122,255"));
        let general = group("[General]");
        assert!(general.contains("AccentColor=0,122,255"));
        assert!(general.contains("accentColorFromWallpaper=false"));
        assert!(general.contains("fixed=Mono"));
        assert!(new.contains("activeBackground=4,4,4"));
        // Blank values are what breaks a theme, and none may ever appear.
        assert!(!new.lines().any(|line| line.ends_with('=')));
        // On something light, the text turns dark.
        assert!(with_kde_accent(globals, [255, 240, 120]).contains("ForegroundNormal=0,0,0"));
    }

    #[test]
    fn a_palette_stays_readable_whatever_it_starts_from() {
        for (dark, (_, seed)) in [true, false]
            .into_iter()
            .flat_map(|dark| PRESETS.into_iter().map(move |preset| (dark, preset)))
        {
            let palette = Palette::from_seed(seed, dark);
            let contrast = |a: &str, b: &str| {
                let (a, b) = (
                    luminance(palette.role(a).unwrap()),
                    luminance(palette.role(b).unwrap()),
                );
                (a.max(b) + 0.05) / (a.min(b) + 0.05)
            };
            assert!(contrast("on_surface", "surface") > 7.0);
            assert!(contrast("on_primary", "primary") > 4.5);
            for role in ["primary", "secondary", "tertiary", "error"] {
                let on = format!("on_{role}_container");
                assert!(contrast(&on, &format!("{role}_container")) > 4.5);
            }
            assert!(contrast("term_red", "surface") > 4.5);
        }
    }

    #[test]
    fn a_selection_is_comfortable_to_read() {
        // The dark purple a wallpaper gave, whose container is dark too: a terminal's selection.
        let palette = Palette::from_seed([0x32, 0x1a, 0x66], true);
        let (text, behind) = (
            luminance(palette.role("on_primary_container").unwrap()),
            luminance(palette.role("primary_container").unwrap()),
        );
        assert!((text.max(behind) + 0.05) / (text.min(behind) + 0.05) >= 7.0);
    }

    #[test]
    fn the_sun_rises_and_sets_when_it_does() {
        // London at midsummer: 03:43 and 20:21 UTC.
        let day = chrono::NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let (sunrise, sunset) = sun_times(day, 51.5074, -0.1278).unwrap();
        let at = |hour: u32, minute: u32| {
            day.and_hms_opt(hour, minute, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        };
        assert!((sunrise - at(3, 43)).abs() < 5 * 60, "{sunrise}");
        assert!((sunset - at(20, 21)).abs() < 5 * 60, "{sunset}");
        // Tromsø in midwinter has no sunrise at all.
        let winter = chrono::NaiveDate::from_ymd_opt(2026, 12, 21).unwrap();
        assert_eq!(sun_times(winter, 69.65, 18.96), None);
    }

    #[test]
    fn an_icon_theme_has_a_twin_only_if_its_there() {
        let installed = [
            "MacOS-Like-Light",
            "MacOS-Like-Dark",
            "WhiteSur",
            "WhiteSur-dark",
        ];
        let exists = |name: &str| installed.contains(&name);
        assert_eq!(
            icon_twin("MacOS-Like-Dark", true, exists).as_deref(),
            Some("MacOS-Like-Light")
        );
        assert_eq!(
            icon_twin("MacOS-Like-Light", false, exists).as_deref(),
            Some("MacOS-Like-Dark")
        );
        assert_eq!(
            icon_twin("WhiteSur", false, exists).as_deref(),
            Some("WhiteSur-dark")
        );
        assert_eq!(
            icon_twin("WhiteSur-dark", true, exists).as_deref(),
            Some("WhiteSur")
        );
        assert_eq!(icon_twin("breeze", false, exists), None);
    }

    #[test]
    fn what_was_applied_reads_the_way_it_was_written_before_too() {
        assert_eq!(
            read_state("matching #e0383e 54027c"),
            (true, false, Some("#e0383e".into()))
        );
        assert_eq!(
            read_state("own #62ba46 0"),
            (false, false, Some("#62ba46".into()))
        );
        let now = applied_state(true, true, [1, 2, 3], [4, 5, 6], IconChoice::Macos);
        assert_eq!(read_state(&now), (true, true, Some("#010203".into())));
        assert_eq!(now.split(' ').nth(5), Some(IconChoice::Macos.word()));
        assert_eq!(read_state(""), (false, false, None));
    }

    #[test]
    fn templates_get_filled_in_and_what_isnt_known_stays() {
        let palette = Palette::from_seed([0, 122, 255], true);
        let primary = hex(palette.role("primary").unwrap());
        assert_eq!(palette.render("a {{primary}} b"), format!("a {primary} b"));
        assert!(!palette.render("{{primary.rgb}}").contains('#'));
        assert_eq!(palette.render("{{nothing}} {{"), "{{nothing}} {{");
    }

    #[test]
    fn every_role_in_the_templates_is_known() {
        let palette = Palette::from_seed([0, 122, 255], true);
        for template in [
            KDE_TEMPLATE,
            GTK3_TEMPLATE,
            GTK4_TEMPLATE,
            ALACRITTY_TEMPLATE,
            TELEGRAM_TEMPLATE,
        ] {
            let rendered = palette.render(template);
            assert!(
                !rendered.contains("{{"),
                "{}",
                &rendered[..rendered.find("{{").unwrap() + 40]
            );
        }
    }

    #[test]
    fn a_scheme_replaces_the_colors_and_leaves_the_rest() {
        let globals = "[General]\nColorScheme=Old\nfixed=Mono\n\n[Colors:View]\nBackgroundNormal=1,2,3\nOnlyOld=1\n\n[KDE]\nSingleClick=true\n";
        let scheme = "[General]\nColorScheme=Makshell\n\n[Colors:View]\nBackgroundNormal=4,5,6\n\n[KDE]\ncontrast=4\n";
        let merged = merge_kde(scheme, globals);
        assert!(merged.contains("ColorScheme=Makshell"));
        assert!(!merged.contains("ColorScheme=Old"));
        assert!(merged.contains("fixed=Mono"));
        assert!(merged.contains("BackgroundNormal=4,5,6"));
        assert!(!merged.contains("OnlyOld"));
        assert!(merged.contains("SingleClick=true"));
        assert!(merged.contains("contrast=4"));
    }

    #[test]
    fn turning_it_off_puts_the_colors_back_and_keeps_later_changes() {
        let before = "[General]\nColorScheme=Noctalia\nAccentColor=1,2,3\n\n[Colors:View]\nBackgroundNormal=1,1,1\n";
        let now = "[General]\nColorScheme=Makshell\nAccentColor=9,9,9\nfixed=Later\n\n[Colors:View]\nBackgroundNormal=9,9,9\n";
        let restored = kde_colors_restored(now, before);
        assert!(restored.contains("ColorScheme=Noctalia"));
        assert!(restored.contains("AccentColor=1,2,3"));
        assert!(restored.contains("BackgroundNormal=1,1,1"));
        assert!(restored.contains("fixed=Later"));
        assert!(!restored.contains("9,9,9"));
    }

    #[test]
    fn gtk_imports_ours_in_place_of_the_last_theme_and_only_once() {
        let css = "@import 'colors.css';\n\n@import url(\"noctalia.css\");\n";
        let once = with_gtk_import(css);
        assert_eq!(once, format!("@import 'colors.css';\n{GTK_IMPORT}\n"));
        assert_eq!(with_gtk_import(&once), once);
    }

    #[test]
    fn alacritty_swaps_its_theme_and_keeps_everything_else() {
        let config = "[general]\nimport = [\"~/.config/alacritty/themes/noctalia.toml\", \"~/keys.toml\"]\n\n# Mine\n[window]\nopacity = 0.85\n";
        let new = importing(config, "/x/themes/makshell.toml").unwrap();
        assert!(new.contains("~/keys.toml"));
        assert!(new.contains("makshell.toml"));
        assert!(!new.contains("noctalia"));
        assert!(new.contains("# Mine"));
        let restored = imports_restored(&new, config).unwrap();
        assert!(restored.contains("noctalia.toml"));
        assert!(!restored.contains("makshell.toml"));
        assert!(restored.contains("opacity = 0.85"));
    }
}
