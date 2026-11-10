//! Spotlight: a search field with four round buttons, which grows into a panel of applications, files,
//! actions or the clipboard's history. It's a surface as big as the screen, so that a click anywhere
//! else closes it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};
use std::{env, fs};

use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings};
use iced_widget::core::text::{LineHeight, Wrapping};
use iced_widget::core::{
    Alignment, Background, Border, Color, ContentFit, Length, Padding, Point, Rectangle, Size,
};
use iced_widget::scrollable::{Direction, Scrollbar};
use iced_widget::{
    Id, Space, button, column, container, image, mouse_area, pin, row, scrollable, stack, text,
    text_input,
};

use crate::anim::{self, Motion, Spring};
use crate::app::{Element, Message};
use crate::apps::App;
use crate::clipboard::Content;
use crate::group::group;
use crate::icons::{self, Glyph};
use crate::menubar::SystemAction;
use crate::{fonts, material, places};

const WIDTH: f32 = 844.0;
const FIELD_HEIGHT: f32 = 54.0;
const FIELD_WIDTH: f32 = 390.0;
const CHIP_SIZE: f32 = 54.0;
const CHIP_GAP: f32 = 12.0;
const PANEL_HEIGHT: f32 = 576.0;
const PANEL_RADIUS: f32 = 28.0;
const PADDING: f32 = 14.0;
const ROW_HEIGHT: f32 = 44.0;
const CLIP_ROW_HEIGHT: f32 = 52.0;
const COLUMNS: usize = 7;
const CELL_HEIGHT: f32 = 102.0;
/// How long an app's name in the grid gets before it's cut short. In letters rather than measured,
/// since the grid is drawn often and has a lot of names.
const CELL_LABEL_LENGTH: usize = 18;
const ICON_SIZE: f32 = 26.0;
const GRID_ICON_SIZE: f32 = 62.0;
const TEXT_SIZE: f32 = 14.0;
const FIELD_TEXT_SIZE: f32 = 21.0;
const CAPTION_SIZE: f32 = 11.0;
/// How far down the screen its panel starts, as a share of its height, and how far the field is below
/// that.
const TOP: f32 = 0.2333;
const FIELD_DROP: f32 = 4.0;
const FIELD_PADDING: f32 = 16.0;
// All measured from a recording of Tahoe's, frame by frame, in seconds.
/// How the field grows into the panel and back: its height first, and its width a little after, which
/// is much of why it looks like liquid.
const EXPAND_SPRING: Spring = Spring::new(0.24, 0.92);
const WIDTH_LAG: f32 = 0.05;
/// How long what's in the panel takes to fade in.
const CONTENT_FADE: Duration = Duration::from_millis(120);
/// It starts as one long capsule, whose end draws in after a moment.
const PINCH_AT: f32 = 0.05;
const PINCH: Spring = Spring::new(0.13, 1.0);
/// What it leaves behind is the four buttons, as drops of the same liquid: thin at first and all in
/// one blob, which spreads out, setting off fast, going a little too far and coming back.
const DROPS_AT: f32 = 0.08;
const DROPS: Spring = Spring::new(0.42, 0.8);
const DROPS_SPEED: f32 = 7.0;
const DROPS_GROW: Spring = Spring::new(0.25, 1.0);
const DROP_SMALLEST: f32 = 0.4;
/// How far a drop reaches for what's before it at first, which is what makes the necks, and when each
/// has let go: the one by the field first, and then each of the others after it.
const DROP_REACH: f32 = 16.0;
const DROP_LETS_GO: [f32; 4] = [0.2, 0.3, 0.36, 0.42];
/// When their icons start to show, and how long that takes.
const CHIP_ICONS_AT: f32 = 0.14;
const CHIP_ICONS_FADE: f32 = 0.2;
/// They come into focus as they fade in, like Tahoe's: how far out of focus they start, in logical
/// pixels, and how long it takes.
const CHIP_ICONS_BLUR: f32 = 8.0;
const CHIP_ICONS_FOCUS: f32 = 0.44;
/// When the drops are all still, and can be buttons of their own.
const CHIPS_TIME: f32 = 0.62;
/// How long after the panel starts to shrink back the buttons come out again.
const CHIPS_RETURN: f32 = 0.1;
const DIM: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.55);

/// Which of its panels it's showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Just the field, or results for what's been typed in it.
    Search,
    Apps,
    Files,
    Actions,
    Clipboard,
}

/// The chips above the grid of applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Productivity,
    Social,
    Entertainment,
    Creativity,
    Development,
    Utilities,
}

impl Category {
    const ALL: [Self; 6] = [
        Self::Productivity,
        Self::Social,
        Self::Entertainment,
        Self::Creativity,
        Self::Development,
        Self::Utilities,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Productivity => "Productivity",
            Self::Social => "Social",
            Self::Entertainment => "Entertainment",
            Self::Creativity => "Creativity",
            Self::Development => "Development",
            Self::Utilities => "Utilities",
        }
    }

    /// Whether an app with these freedesktop categories belongs here.
    fn has(self, categories: &[String]) -> bool {
        let names: &[&str] = match self {
            Self::Productivity => &["Office", "Calendar", "Finance", "TextEditor"],
            Self::Social => &["Chat", "InstantMessaging", "Email", "Network", "WebBrowser"],
            Self::Entertainment => &["AudioVideo", "Audio", "Video", "Game", "Player"],
            Self::Creativity => &["Graphics", "Photography", "Art"],
            Self::Development => &["Development", "IDE"],
            Self::Utilities => &["Utility", "System", "Settings", "FileManager"],
        };
        categories
            .iter()
            .any(|category| names.contains(&category.as_str()))
    }
}

/// Something it can run that isn't an app of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Settings,
    Lock,
    Sleep,
    Restart,
    ShutDown,
    LogOut,
    EmptyTrash,
}

impl Action {
    const ALL: [Self; 7] = [
        Self::Settings,
        Self::Lock,
        Self::Sleep,
        Self::EmptyTrash,
        Self::LogOut,
        Self::Restart,
        Self::ShutDown,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Settings => "System Settings",
            Self::Lock => "Lock Screen",
            Self::Sleep => "Sleep",
            Self::Restart => "Restart",
            Self::ShutDown => "Shut Down",
            Self::LogOut => "Log Out",
            Self::EmptyTrash => "Empty Trash",
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Self::Settings => Glyph::Gear,
            Self::Lock => Glyph::Lock,
            Self::Sleep => Glyph::Sleep,
            Self::Restart => Glyph::Restart,
            Self::ShutDown => Glyph::Power,
            Self::LogOut => Glyph::Person,
            Self::EmptyTrash => Glyph::Trash,
        }
    }

    /// What it means to the shell.
    pub fn message(self) -> Message {
        match self {
            Self::Settings => Message::OpenSettings,
            Self::Lock => Message::System(SystemAction::Lock),
            Self::Sleep => Message::System(SystemAction::Sleep),
            Self::Restart => Message::System(SystemAction::Restart),
            Self::ShutDown => Message::System(SystemAction::ShutDown),
            Self::LogOut => Message::System(SystemAction::LogOut),
            Self::EmptyTrash => Message::DockMenu(crate::dock::MenuAction::ConfirmEmptyTrash),
        }
    }
}

/// A file it knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

impl FileHit {
    fn of(path: PathBuf) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let is_dir = path.is_dir();
        Some(Self { path, name, is_dir })
    }
}

/// Something that was copied, and when.
#[derive(Debug, Clone)]
pub struct Clip {
    pub content: Content,
    pub copied: chrono::DateTime<chrono::Local>,
    /// What was focused when it was copied, which most likely copied it.
    pub source: Option<Rc<App>>,
}

impl Clip {
    fn title(&self) -> String {
        match &self.content {
            Content::Text(text) => text.trim().lines().next().unwrap_or_default().to_owned(),
            Content::Files(files) => match files.as_slice() {
                [file] => file
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                files => format!("{} files", files.len()),
            },
            Content::Image { mime, .. } => match mime.as_str() {
                "image/jpeg" => String::from("JPEG image"),
                _ => String::from("PNG image"),
            },
        }
    }

    fn detail(&self) -> String {
        let kind = match &self.content {
            Content::Text(_) => "Text",
            Content::Files(_) => "File",
            Content::Image { .. } => "Image",
        };
        format!("{kind} · Copied {}", self.copied.format("%H:%M"))
    }
}

/// One thing that can be chosen.
#[derive(Debug, Clone)]
pub enum Hit {
    App(Rc<App>),
    /// A desktop action of an app: its command, its name, and the app.
    AppAction(Rc<App>, usize),
    File(FileHit),
    Action(Action),
    Clip(usize),
}

/// How well `name` matches what was typed, higher being better, if it does: the start of it, then
/// the start of a word, then anywhere, then its letters in order.
fn score(name: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(1);
    }
    let name = name.to_lowercase();
    let query = query.to_lowercase();
    if name.starts_with(&query) {
        return Some(400);
    }
    if let Some(at) = name.find(&query) {
        let word_start = !name[..at].ends_with(|c: char| c.is_alphanumeric());
        return Some(if word_start { 300 } else { 200 });
    }
    let mut letters = name.chars();
    query
        .chars()
        .all(|wanted| letters.any(|letter| letter == wanted))
        .then_some(100)
}

/// What's in the shell's list of apps, sorted by how well they match, then by name.
fn matching_apps(apps: &[Rc<App>], query: &str) -> Vec<Rc<App>> {
    let mut found: Vec<(u32, &Rc<App>)> = apps
        .iter()
        .filter_map(|app| Some((score(&app.name, query)?, app)))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().map(|(_, app)| app.clone()).collect()
}

fn matching_actions(apps: &[Rc<App>], query: &str, limit: usize) -> Vec<Hit> {
    let mut found: Vec<(u32, Hit)> = Action::ALL
        .iter()
        .filter_map(|action| Some((word_score(action.name(), query)?, Hit::Action(*action))))
        .collect();
    for app in apps {
        for (index, action) in app.actions.iter().enumerate() {
            let text = format!("{} {}", action.name, app.name);
            if let Some(found_score) = word_score(&text, query) {
                found.push((found_score, Hit::AppAction(app.clone(), index)));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().take(limit).map(|(_, hit)| hit).collect()
}

pub struct Spotlight {
    pub id: iced_layershell::reexport::IcedId,
    pub material: Option<material::MaterialSurface>,
    pub mode: Mode,
    pub query: String,
    pub category: Option<Category>,
    pub selected: usize,
    /// What the last finished search of files found, and what it was for.
    pub files: Vec<FileHit>,
    pub files_query: String,
    pub recent_files: Vec<FileHit>,
    /// The last shapes sent, to tell when they change, and how opaque they were.
    pub shapes: Vec<material::Shape>,
    pub opacities: Vec<f32>,
    /// How far it's grown from the field to the panel.
    expansion: Motion,
    /// When it started out as a capsule, which the buttons' coming out is timed from.
    chips_started: Instant,
    /// Whether the field's starting out wide, which only happens when it first opens.
    starts_wide: bool,
    /// When what's in the panel last changed, to fade it in.
    content_changed: Instant,
}

impl Spotlight {
    pub fn new(id: iced_layershell::reexport::IcedId) -> Self {
        Self {
            id,
            material: None,
            mode: Mode::Search,
            query: String::new(),
            category: None,
            selected: 0,
            files: Vec::new(),
            files_query: String::new(),
            recent_files: Vec::new(),
            shapes: Vec::new(),
            opacities: Vec::new(),
            expansion: Motion::at(0.0, EXPAND_SPRING, 0.002),
            chips_started: anim::now(),
            starts_wide: true,
            content_changed: anim::now() - CONTENT_FADE,
        }
    }

    /// Starts growing or shrinking, if what it's showing calls for that.
    pub fn sync_expansion(&mut self) {
        let target = if self.expanded() { 1.0 } else { 0.0 };
        if target != self.expansion.target() {
            self.expansion.go_to(target);
            self.content_changed = anim::now();
            if target == 0.0 {
                // The buttons come back out as it gets there.
                self.chips_started = anim::now() + Duration::from_secs_f32(CHIPS_RETURN)
                    - Duration::from_secs_f32(DROPS_AT);
                self.starts_wide = false;
            }
        }
    }

    /// Starts the timeline over, now that there's glass to show it on.
    pub fn glass_arrived(&mut self) {
        self.chips_started = anim::now();
    }

    /// Says that what's in the panel changed, which fades it in.
    pub fn content_changed(&mut self) {
        self.content_changed = anim::now();
    }

    /// How far it's grown from the field to the panel, going a little past on the way.
    fn expansion(&self) -> f32 {
        self.expansion.value()
    }

    /// How long it's been since it started out as a capsule.
    fn timeline(&self) -> f32 {
        anim::seconds_since(self.chips_started)
    }

    /// Whether the round buttons are out, and on their own.
    fn chips_are_out(&self) -> bool {
        self.timeline() >= CHIPS_TIME
    }

    pub fn is_animating(&self) -> bool {
        self.expansion.is_moving_delayed(WIDTH_LAG)
            || anim::seconds_since(self.content_changed) < CONTENT_FADE.as_secs_f32()
            || !self.chips_are_out()
    }

    /// The panel's rectangle, which all the glass is drawn in, so that it never has to be set up
    /// again, and the field's and the first button's, on a screen of this size.
    fn geometry(width: f32, height: f32) -> (Rectangle, Rectangle) {
        // A small screen has to fit it.
        let top = (height * TOP)
            .round()
            .min(height - PANEL_HEIGHT - 20.0)
            .max(0.0);
        let panel = Rectangle {
            x: ((width - WIDTH) / 2.0).round(),
            y: top,
            width: WIDTH,
            height: PANEL_HEIGHT,
        };
        let total = FIELD_WIDTH + 4.0 * (CHIP_GAP + CHIP_SIZE);
        let field = Rectangle {
            x: ((width - total) / 2.0).round(),
            y: top + FIELD_DROP,
            width: FIELD_WIDTH,
            height: FIELD_HEIGHT,
        };
        (panel, field)
    }

    /// The glass as it is now, between the field and the panel, and its corner radius.
    fn glass_now(&self, panel: Rectangle, field: Rectangle) -> (Rectangle, f32) {
        let tall = self.expansion.value();
        let wide = self.expansion.delayed(WIDTH_LAG);
        let along = |from: f32, to: f32, by: f32| from + (to - from) * by;
        let mut rect = Rectangle {
            x: along(field.x, panel.x, wide),
            y: along(field.y, panel.y, tall),
            width: along(field.width, panel.width, wide).max(1.0),
            height: along(field.height, panel.height, tall).max(1.0),
        };
        // It starts out as long as it'll be with its buttons, and draws in from the end.
        if self.starts_wide && tall.abs() < 0.001 {
            let drawn_in = PINCH.progress(self.timeline() - PINCH_AT);
            rect.width += 4.0 * (CHIP_GAP + CHIP_SIZE) * (1.0 - drawn_in);
        }
        (rect, along(FIELD_HEIGHT / 2.0, PANEL_RADIUS, tall).max(0.0))
    }

    /// The middle of the round button at `index`.
    fn chip_center(field: Rectangle, index: usize) -> Point {
        Point::new(
            field.x
                + FIELD_WIDTH
                + CHIP_GAP
                + CHIP_SIZE / 2.0
                + index as f32 * (CHIP_SIZE + CHIP_GAP),
            field.y + FIELD_HEIGHT / 2.0,
        )
    }

    /// Where a round button is now: on its way out of the field's end as a drop, or in its place.
    fn chip_center_now(&self, field: Rectangle, index: usize) -> Point {
        let end = Self::chip_center(field, index);
        if self.chips_are_out() {
            return end;
        }
        let since = (self.timeline() - DROPS_AT).max(0.0);
        let out = DROPS.progress_from(DROPS_SPEED, since);
        let from = field.x + FIELD_WIDTH - CHIP_SIZE / 2.0;
        Point::new(from + (end.x - from) * out, end.y)
    }

    /// How big a round button is now, as a share of its size: drops grow as they come out.
    fn chip_size_now(&self) -> f32 {
        if self.chips_are_out() {
            return 1.0;
        }
        let since = (self.timeline() - DROPS_AT).max(0.0);
        DROP_SMALLEST + (1.0 - DROP_SMALLEST) * DROPS_GROW.progress(since)
    }

    /// How much of the round buttons shows, once they're out on their own.
    fn chips_show(&self) -> f32 {
        (1.0 - self.expansion().max(0.0) * 4.0).clamp(0.0, 1.0)
    }

    pub fn field_id() -> Id {
        Id::new("spotlight-field")
    }

    pub fn results_id() -> Id {
        Id::new("spotlight-results")
    }

    /// Puts the keyboard back in the field. Going between just the field and the whole panel makes
    /// a new one, which doesn't have it.
    pub fn refocus<T: Send + 'static>() -> iced_layershell::reexport::Task<T> {
        iced_layershell::reexport::Task::batch([
            iced_runtime::widget::operation::focus(Self::field_id()),
            iced_runtime::widget::operation::move_cursor_to_end(Self::field_id()),
        ])
    }

    /// Whether it's a whole panel rather than just the field and its buttons.
    pub fn expanded(&self) -> bool {
        self.mode != Mode::Search || !self.query.trim().is_empty()
    }

    /// What can be chosen now, in order.
    pub fn hits(&self, apps: &[Rc<App>], clips: &VecDeque<Clip>) -> Vec<Hit> {
        let query = self.query.trim();
        match self.mode {
            Mode::Search if query.is_empty() => Vec::new(),
            Mode::Search => {
                // What really matches comes first, and apps that only have the letters in order last.
                let (strong, loose): (Vec<Rc<App>>, Vec<Rc<App>>) = matching_apps(apps, query)
                    .into_iter()
                    .partition(|app| word_score(&app.name, query).is_some());
                let mut hits: Vec<Hit> = strong.into_iter().take(6).map(Hit::App).collect();
                hits.extend(matching_actions(apps, query, 4));
                hits.extend(self.file_hits().into_iter().take(8).map(Hit::File));
                hits.extend(loose.into_iter().take(3).map(Hit::App));
                hits
            }
            Mode::Apps => {
                let category = self.category;
                matching_apps(apps, query)
                    .into_iter()
                    .filter(|app| category.is_none_or(|category| category.has(&app.categories)))
                    .map(Hit::App)
                    .collect()
            }
            Mode::Files => self.file_hits().into_iter().map(Hit::File).collect(),
            Mode::Actions => matching_actions(apps, query, 200),
            Mode::Clipboard => clips
                .iter()
                .enumerate()
                .filter(|(_, clip)| score(&clip.title(), query).is_some())
                .map(|(index, _)| Hit::Clip(index))
                .collect(),
        }
    }

    fn file_hits(&self) -> Vec<FileHit> {
        if self.query.trim().is_empty() {
            self.recent_files.clone()
        } else {
            self.files.clone()
        }
    }

    /// Whether what's typed now needs a search of the files.
    pub fn wants_file_search(&self) -> Option<String> {
        let query = self.query.trim();
        let searching = matches!(self.mode, Mode::Search | Mode::Files);
        (searching && !query.is_empty() && query != self.files_query).then(|| query.to_owned())
    }

    /// Which way the arrow keys move in what's showing.
    fn columns(&self) -> usize {
        if self.mode == Mode::Apps { COLUMNS } else { 1 }
    }

    /// Moves the selection, by `rows` down and `across` to the right.
    pub fn select_by(&mut self, rows: i32, across: i32, count: usize) {
        if count == 0 {
            self.selected = 0;
            return;
        }
        let columns = self.columns() as i32;
        let moved = self.selected as i32 + rows * columns + across;
        self.selected = moved.clamp(0, count as i32 - 1) as usize;
    }

    /// How far down its list the selection is, from 0 to 1, for scrolling to it.
    pub fn scroll_share(&self, count: usize) -> f32 {
        let columns = self.columns();
        let rows = count.div_ceil(columns);
        if rows <= 1 {
            0.0
        } else {
            (self.selected / columns) as f32 / (rows - 1) as f32
        }
    }

    /// Where the glass goes on a screen of this size.
    pub fn material_shapes(&self, width: f32, height: f32) -> Vec<material::Shape> {
        let (panel, field) = Self::geometry(width, height);
        let (rect, radius) = self.glass_now(panel, field);
        let milli = |value: f32| (value * 1000.0).round() as i32;
        let drop_of = |center: Point, radius: f32, blend: f32| {
            [
                milli(center.x - panel.x),
                milli(center.y - panel.y),
                milli(radius),
                milli(blend),
            ]
        };
        // While they're coming out, the buttons are drops of the same liquid as the field.
        let timeline = self.timeline();
        let mut drops = [[0; 4]; 4];
        let on_their_own = self.chips_are_out();
        if !on_their_own && self.chips_show() > 0.0 && timeline >= DROPS_AT {
            let size = self.chip_size_now();
            for (index, drop) in drops.iter_mut().enumerate() {
                let center = self.chip_center_now(field, index);
                let reach = DROP_REACH * (1.0 - timeline / DROP_LETS_GO[index]).max(0.0);
                *drop = drop_of(center, CHIP_SIZE / 2.0 * size, reach);
            }
        }
        let mut shapes = vec![material::Shape {
            rect: (
                panel.x as i32,
                panel.y as i32,
                panel.width as i32,
                panel.height as i32,
            ),
            radius: 0,
            style: material::Style::CardBlob(
                [
                    milli(rect.x + rect.width / 2.0 - panel.x),
                    milli(rect.y + rect.height / 2.0 - panel.y),
                    milli(rect.width),
                    milli(rect.height),
                    milli(radius),
                ],
                drops,
            ),
        }];
        if on_their_own {
            for index in 0..4 {
                let center = Self::chip_center(field, index);
                shapes.push(material::Shape {
                    rect: (
                        (center.x - CHIP_SIZE / 2.0) as i32,
                        (center.y - CHIP_SIZE / 2.0) as i32,
                        CHIP_SIZE as i32,
                        CHIP_SIZE as i32,
                    ),
                    radius: (CHIP_SIZE / 2.0) as i32,
                    style: material::Style::Card,
                });
            }
        }
        shapes
    }

    /// Which parts of it are out of focus: the buttons' icons, as they come out.
    pub fn content_blurs(&self, width: f32, height: f32) -> Vec<material::ContentBlur> {
        let since = self.timeline() - CHIP_ICONS_AT;
        if !(0.0..CHIP_ICONS_FOCUS).contains(&since) || self.chips_show() <= 0.0 {
            return Vec::new();
        }
        let unfocused = 1.0 - since / CHIP_ICONS_FOCUS;
        // Tenths of a pixel are as fine as it shows.
        let radius = (CHIP_ICONS_BLUR * unfocused * unfocused * 10.0).round() / 10.0;
        if radius <= 0.0 {
            return Vec::new();
        }
        let (_, field) = Self::geometry(width, height);
        let (first, last) = (
            self.chip_center_now(field, 0),
            self.chip_center_now(field, 3),
        );
        vec![material::ContentBlur {
            rect: (
                (first.x - CHIP_SIZE / 2.0) as i32,
                (first.y - CHIP_SIZE / 2.0) as i32,
                (last.x - first.x + CHIP_SIZE) as i32,
                CHIP_SIZE as i32,
            ),
            radius,
        }]
    }

    /// How opaque each of [`Spotlight::material_shapes`] is: the buttons go as the panel grows.
    pub fn material_opacities(&self) -> Vec<f32> {
        let mut opacities = vec![1.0];
        if self.chips_are_out() {
            opacities.extend([self.chips_show(); 4]);
        }
        opacities
    }

    /// Everything's always in the same place in the tree, whether it's the field or the panel, so
    /// that the field keeps the keyboard, and what's typed, all the way through.
    pub fn view<'a>(
        &'a self,
        apps: &'a [Rc<App>],
        clips: &'a VecDeque<Clip>,
        file_icons: &'a FileIcons,
        width: f32,
        height: f32,
    ) -> Element<'a> {
        let (panel, field) = Self::geometry(width, height);
        let (rect, _) = self.glass_now(panel, field);
        let e = self.expansion();

        // It takes presses, so that pressing it doesn't count as pressing outside it.
        let catcher = mouse_area(
            container(Space::new())
                .width(rect.width)
                .height(rect.height),
        )
        .on_press(Message::Nothing);

        let (glyph, placeholder) = match self.mode {
            Mode::Search => (Glyph::Search, "Spotlight Search"),
            Mode::Apps => (Glyph::Grid, "Applications"),
            Mode::Files => (Glyph::Folder, "Files"),
            Mode::Actions => (Glyph::Stacks, "Actions"),
            Mode::Clipboard => (Glyph::Clipboard, "Clipboard"),
        };
        let field_row = container(self.field(glyph, placeholder))
            .width((rect.width - 2.0 * FIELD_PADDING).max(1.0))
            .height(FIELD_HEIGHT)
            .center_y(FIELD_HEIGHT);

        let content_fade =
            (anim::seconds_since(self.content_changed) / CONTENT_FADE.as_secs_f32()).min(1.0);
        let shows = ((e - 0.5) / 0.5).clamp(0.0, 1.0) * content_fade;
        let body: Element<'a> = if self.expanded() || e > 0.01 {
            let line = container(Space::new())
                .width(Length::Fill)
                .height(1)
                .style(|_| container::Style {
                    background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.1))),
                    ..Default::default()
                });
            let hits = self.hits(apps, clips);
            container(column![line, self.body(hits, clips, file_icons)])
                .width(WIDTH)
                .height(PANEL_HEIGHT - FIELD_HEIGHT)
                .into()
        } else {
            Space::new().into()
        };
        let clip = Size::new(rect.width, (rect.height - FIELD_HEIGHT).max(0.0));

        let icons_show = ((self.timeline() - CHIP_ICONS_AT) / CHIP_ICONS_FADE).clamp(0.0, 1.0)
            * self.chips_show();
        let mut layers: Vec<Element<'a>> = vec![
            container(Space::new()).width(width).height(height).into(),
            pin(catcher).x(rect.x).y(rect.y).into(),
        ];
        // They can't be pressed where they can't be seen, which is under the panel. Each rides its
        // drop out of the field, growing with it, like Tahoe's.
        if icons_show > 0.0 {
            let icon_size = 22.0 * self.chip_size_now();
            for (index, (mode, glyph)) in [
                (Mode::Apps, Glyph::Grid),
                (Mode::Files, Glyph::Folder),
                (Mode::Actions, Glyph::Stacks),
                (Mode::Clipboard, Glyph::Clipboard),
            ]
            .into_iter()
            .enumerate()
            {
                let center = self.chip_center_now(field, index);
                layers.push(
                    pin(group(chip(mode, glyph, icon_size)).opacity(icons_show))
                        .x(center.x - CHIP_SIZE / 2.0)
                        .y(center.y - CHIP_SIZE / 2.0)
                        .into(),
                );
            }
        }
        layers.push(
            pin(group(body).opacity(shows).clip(clip))
                .x(rect.x)
                .y(rect.y + FIELD_HEIGHT)
                .into(),
        );
        layers.push(pin(field_row).x(rect.x + FIELD_PADDING).y(rect.y).into());

        mouse_area(stack(layers).width(width).height(height))
            .on_press(Message::SpotlightClose)
            .into()
    }

    fn field<'a>(&'a self, glyph: Glyph, placeholder: &'a str) -> Element<'a> {
        row![
            icons::icon(glyph, 20.0, DIM),
            text_input(placeholder, &self.query)
                .id(Self::field_id())
                .on_input(Message::SpotlightTyped)
                .size(FIELD_TEXT_SIZE)
                .padding(0)
                .style(|_, _| text_input::Style {
                    background: Background::Color(Color::TRANSPARENT),
                    border: Border::default(),
                    icon: Color::WHITE,
                    placeholder: DIM,
                    value: Color::WHITE,
                    selection: crate::appearance::accent(),
                }),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
    }

    fn body<'a>(
        &'a self,
        hits: Vec<Hit>,
        clips: &'a VecDeque<Clip>,
        file_icons: &'a FileIcons,
    ) -> Element<'a> {
        let list: Element<'a> =
            if hits.is_empty() {
                let message = match self.mode {
                    Mode::Clipboard if self.query.trim().is_empty() => "Nothing copied yet",
                    Mode::Files if self.query.trim().is_empty() => "No recent files",
                    _ if self.query.trim().is_empty() => "",
                    _ => "No results",
                };
                container(text(message).size(TEXT_SIZE).color(DIM))
                    .center_x(Length::Fill)
                    .center_y(160)
                    .into()
            } else if self.mode == Mode::Apps {
                self.grid(&hits)
            } else {
                column(hits.iter().enumerate().map(|(index, hit)| {
                    hit_row(hit, index, index == self.selected, clips, file_icons)
                }))
                .spacing(2)
                .into()
            };
        let scroll = scrollable(container(list).padding(Padding {
            top: 8.0,
            right: PADDING - 6.0,
            bottom: PADDING,
            left: PADDING - 6.0,
        }))
        .id(Self::results_id())
        .direction(Direction::Vertical(
            Scrollbar::new().width(0).scroller_width(0),
        ))
        .width(Length::Fill)
        .height(Length::Fill);
        if self.mode == Mode::Apps {
            column![self.category_chips(), scroll].into()
        } else {
            scroll.into()
        }
    }

    fn category_chips(&self) -> Element<'_> {
        let chips = Category::ALL.iter().map(|category| {
            let selected = self.category == Some(*category);
            let category = *category;
            button(text(category.name()).size(12.0))
                .padding([3, 10])
                .on_press(Message::SpotlightCategory((!selected).then_some(category)))
                .style(move |_, status| {
                    let hovered =
                        matches!(status, button::Status::Hovered | button::Status::Pressed);
                    let background = if selected {
                        crate::appearance::accent()
                    } else {
                        Color::from_rgba(1.0, 1.0, 1.0, if hovered { 0.18 } else { 0.08 })
                    };
                    button::Style {
                        background: Some(Background::Color(background)),
                        text_color: Color::WHITE,
                        border: Border {
                            radius: 11.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                })
                .into()
        });
        container(iced_widget::Row::with_children(chips).spacing(6))
            .padding(Padding {
                top: 10.0,
                right: PADDING,
                bottom: 0.0,
                left: PADDING,
            })
            .into()
    }

    fn grid<'a>(&self, hits: &[Hit]) -> Element<'a> {
        let selected = self.selected;
        let rows = hits.chunks(COLUMNS).enumerate().map(|(row_index, chunk)| {
            let cells = chunk.iter().enumerate().map(|(column_index, hit)| {
                let index = row_index * COLUMNS + column_index;
                let Hit::App(app) = hit else {
                    return Space::new().into();
                };
                grid_cell(app.clone(), index, index == selected)
            });
            iced_widget::Row::with_children(cells).into()
        });
        iced_widget::Column::with_children(rows).into()
    }
}

/// A round button that opens a panel.
fn chip<'a>(mode: Mode, glyph: Glyph, icon_size: f32) -> Element<'a> {
    button(
        container(icons::icon(glyph, icon_size, Color::WHITE))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(CHIP_SIZE)
    .height(CHIP_SIZE)
    .padding(0)
    .on_press(Message::SpotlightMode(mode))
    .style(|_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: hovered.then_some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.14))),
            text_color: Color::WHITE,
            border: Border {
                radius: (CHIP_SIZE / 2.0).into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

fn app_picture<'a>(app: &App, size: f32) -> Element<'a> {
    match &app.icon {
        Some(handle) => image(handle.clone())
            .width(size)
            .height(size)
            .content_fit(ContentFit::Contain)
            .into(),
        None => container(
            text(
                app.name
                    .chars()
                    .next()
                    .map_or_else(String::new, |c| c.to_uppercase().to_string()),
            )
            .size(size * 0.5),
        )
        .center_x(size)
        .center_y(size)
        .style(move |_| container::Style {
            background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.16))),
            border: Border {
                radius: (size * 0.22).into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into(),
    }
}

fn cell_label(name: &str) -> String {
    if name.chars().count() <= CELL_LABEL_LENGTH {
        return name.to_owned();
    }
    let mut short: String = name.chars().take(CELL_LABEL_LENGTH - 1).collect();
    short.push('…');
    short
}

fn grid_cell<'a>(app: Rc<App>, index: usize, selected: bool) -> Element<'a> {
    let name = text(cell_label(&app.name))
        .size(CAPTION_SIZE)
        .wrapping(Wrapping::None)
        .center();
    let cell = container(
        column![
            app_picture(&app, GRID_ICON_SIZE),
            container(name)
                .width(Length::Fill)
                .center_x(Length::Fill)
                .clip(true),
        ]
        .spacing(6)
        .align_x(Alignment::Center),
    )
    .width(Length::FillPortion(1))
    .height(CELL_HEIGHT)
    .padding([8, 4])
    .style(move |_| selection_style(selected, 14.0));
    mouse_area(cell)
        .on_enter(Message::SpotlightHovered(index))
        .on_press(Message::SpotlightChosen(index))
        .into()
}

fn selection_style(selected: bool, radius: f32) -> container::Style {
    container::Style {
        background: selected.then_some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.14))),
        text_color: Some(Color::WHITE),
        border: Border {
            radius: radius.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn glyph_picture<'a>(glyph: Glyph) -> Element<'a> {
    container(icons::icon(glyph, 16.0, Color::WHITE))
        .center_x(ICON_SIZE)
        .center_y(ICON_SIZE)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.14))),
            border: Border {
                radius: 7.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn hit_row<'a>(
    hit: &Hit,
    index: usize,
    selected: bool,
    clips: &'a VecDeque<Clip>,
    file_icons: &'a FileIcons,
) -> Element<'a> {
    let (picture, title, detail, hint): (Element<'a>, String, Option<String>, &str) = match hit {
        Hit::App(app) => (app_picture(app, ICON_SIZE), app.name.clone(), None, "Open"),
        Hit::AppAction(app, action) => (
            app_picture(app, ICON_SIZE),
            app.actions
                .get(*action)
                .map_or_else(String::new, |action| action.name.clone()),
            Some(app.name.clone()),
            "Run",
        ),
        Hit::File(file) => (
            file_icons.picture(file),
            file.name.clone(),
            Some(short_path(&file.path)),
            "Open",
        ),
        Hit::Action(action) => (
            glyph_picture(action.glyph()),
            action.name().to_owned(),
            None,
            "Run",
        ),
        Hit::Clip(clip) => {
            let Some(clip) = clips.get(*clip) else {
                return Space::new().into();
            };
            let picture: Element<'a> = match &clip.content {
                Content::Image { picture, .. } => image(picture.clone())
                    .width(ICON_SIZE + 6.0)
                    .height(ICON_SIZE + 6.0)
                    .content_fit(ContentFit::Cover)
                    .border_radius(6.0)
                    .into(),
                Content::Files(_) => glyph_picture(Glyph::Folder),
                Content::Text(_) => match &clip.source {
                    Some(app) if app.icon.is_some() => app_picture(app, ICON_SIZE),
                    _ => glyph_picture(Glyph::Clipboard),
                },
            };
            (picture, clip.title(), Some(clip.detail()), "Copy")
        }
    };
    let words: Element<'a> = match detail {
        Some(detail) => column![
            line_of(title, TEXT_SIZE, Color::WHITE),
            line_of(detail, CAPTION_SIZE, DIM),
        ]
        .into(),
        None => line_of(title, TEXT_SIZE, Color::WHITE),
    };
    let mut content = row![
        container(picture)
            .center_x(ICON_SIZE + 6.0)
            .center_y(ICON_SIZE + 6.0),
        container(words).width(Length::Fill).clip(true),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if selected {
        content = content.push(text(hint).size(CAPTION_SIZE).color(DIM));
    }
    let height = if matches!(hit, Hit::Clip(_)) {
        CLIP_ROW_HEIGHT
    } else {
        ROW_HEIGHT
    };
    mouse_area(
        container(content)
            .width(Length::Fill)
            .height(height)
            .padding([0, 10])
            .center_y(height)
            .style(move |_| selection_style(selected, 12.0)),
    )
    .on_enter(Message::SpotlightHovered(index))
    .on_press(Message::SpotlightChosen(index))
    .into()
}

fn line_of<'a>(content: String, size: f32, color: Color) -> Element<'a> {
    text(content)
        .size(size)
        .font(fonts::REGULAR)
        .color(color)
        .line_height(LineHeight::Absolute((size * 1.3).into()))
        .wrapping(Wrapping::None)
        .into()
}

/// Theme icons for files, by what kind of file they are.
#[derive(Debug, Default)]
pub struct FileIcons {
    pub folder: Option<image::Handle>,
    pub image: Option<image::Handle>,
    pub video: Option<image::Handle>,
    pub audio: Option<image::Handle>,
    pub pdf: Option<image::Handle>,
    pub text: Option<image::Handle>,
}

impl FileIcons {
    fn picture<'a>(&self, file: &FileHit) -> Element<'a> {
        let extension = Path::new(&file.name)
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let handle = if file.is_dir {
            &self.folder
        } else {
            match extension.as_str() {
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "bmp" => &self.image,
                "mp4" | "mkv" | "webm" | "mov" | "avi" => &self.video,
                "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" => &self.audio,
                "pdf" => &self.pdf,
                _ => &self.text,
            }
        };
        match handle {
            Some(handle) => image(handle.clone())
                .width(ICON_SIZE)
                .height(ICON_SIZE)
                .content_fit(ContentFit::Contain)
                .into(),
            None => glyph_picture(if file.is_dir {
                Glyph::Folder
            } else {
                Glyph::Info
            }),
        }
    }
}

/// A path, with the home folder as `~`.
fn short_path(path: &Path) -> String {
    let parent = path.parent().unwrap_or(path);
    match parent.strip_prefix(places::home()) {
        Ok(rest) if rest.as_os_str().is_empty() => String::from("~"),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => parent.display().to_string(),
    }
}

/// Adds what was just copied to the top, moving it there if it's already in the history, which
/// keeps no more than `max` things.
pub fn remember_clip(clips: &mut VecDeque<Clip>, clip: Clip, max: usize) {
    clips.retain(|known| known.content != clip.content);
    clips.push_front(clip);
    clips.truncate(max);
}

pub fn settings() -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((0, 0)),
        exclusive_zone: Some(-1),
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        namespace: Some(String::from("dock")),
        ..Default::default()
    }
}

/// How deep a search goes, and how much it looks at before giving up.
const SEARCH_DEPTH: usize = 7;
const SEARCH_ENTRIES: usize = 60_000;
const SEARCH_RESULTS: usize = 60;
/// Folders that are full of files nobody's looking for by name.
const SKIPPED_FOLDERS: [&str; 6] = [
    "node_modules",
    "target",
    "__pycache__",
    "venv",
    "build",
    "dist",
];

/// Files under the home folder with names like `query`, best first. For another thread.
pub fn search_files(query: &str) -> Vec<FileHit> {
    let home = places::home();
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    let mut folders = VecDeque::from([(home, 0usize)]);
    let mut seen = 0;
    while let Some((folder, depth)) = folders.pop_front() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > SEARCH_ENTRIES {
                folders.clear();
                break;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            // Not following links, which can go around in circles.
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if let Some(found_score) = word_score(&name, query) {
                found.push((found_score, path.clone()));
            }
            if is_dir && depth < SEARCH_DEPTH && !SKIPPED_FOLDERS.contains(&name.as_ref()) {
                folders.push_back((path, depth + 1));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found
        .into_iter()
        .take(SEARCH_RESULTS)
        .filter_map(|(_, path)| FileHit::of(path))
        .collect()
}

/// Like [`score`], without the looser letters-in-order match, which finds too much in long names.
fn word_score(name: &str, query: &str) -> Option<u32> {
    score(name, query).filter(|found| *found > 100)
}

/// What Qt, GTK and others list as recently used, newest first.
pub fn recent_files() -> Vec<FileHit> {
    let path = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| places::home().join(".local/share"))
        .join("recently-used.xbel");
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_recent(&text)
}

fn parse_recent(text: &str) -> Vec<FileHit> {
    let mut entries: Vec<(SystemTime, PathBuf)> = text
        .match_indices("<bookmark ")
        .filter_map(|(at, _)| {
            let tag = &text[at..at + text[at..].find('>')?];
            let href = tag.split("href=\"").nth(1)?.split('"').next()?;
            let path = crate::portal::path_of(href)?;
            let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok()?;
            Some((modified, path))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries
        .into_iter()
        .take(30)
        .filter_map(|(_, path)| FileHit::of(path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_grows_into_the_panel_and_back_from_wherever_it_got_to() {
        let mut spotlight = Spotlight::new(iced_layershell::reexport::IcedId::unique());
        assert_eq!(spotlight.expansion(), 0.0);
        spotlight.mode = Mode::Apps;
        spotlight.sync_expansion();
        spotlight.expansion = spotlight.expansion.started_ago(2.0);
        assert_eq!(spotlight.expansion(), 1.0);
        spotlight.mode = Mode::Search;
        spotlight.sync_expansion();
        assert!((spotlight.expansion() - 1.0).abs() < 0.05);
        spotlight.expansion = spotlight.expansion.started_ago(2.0);
        assert_eq!(spotlight.expansion(), 0.0);
    }

    #[test]
    fn its_glass_all_stays_in_one_rectangle_and_the_shapes_match_their_opacities() {
        let mut spotlight = Spotlight::new(iced_layershell::reexport::IcedId::unique());
        let (panel, field) = Spotlight::geometry(1280.0, 720.0);
        let check = |spotlight: &Spotlight| {
            let shapes = spotlight.material_shapes(1280.0, 720.0);
            assert_eq!(shapes.len(), spotlight.material_opacities().len());
            assert_eq!(
                shapes[0].rect,
                (
                    panel.x as i32,
                    panel.y as i32,
                    panel.width as i32,
                    panel.height as i32
                )
            );
        };
        check(&spotlight);
        spotlight.chips_started = Instant::now() - Duration::from_secs(2);
        check(&spotlight);
        assert_eq!(spotlight.material_shapes(1280.0, 720.0).len(), 5);
        // All four buttons fit inside the panel's rectangle.
        let last = Spotlight::chip_center(field, 3);
        assert!(last.x + CHIP_SIZE / 2.0 <= panel.x + panel.width);
        assert!(field.x >= panel.x);
    }

    #[test]
    fn scores_the_start_first() {
        assert_eq!(score("Discord", "dis"), Some(400));
        assert_eq!(score("Visual Studio Code", "stu"), Some(300));
        assert_eq!(score("Discord", "cor"), Some(200));
        assert_eq!(score("Discord", "dcd"), Some(100));
        assert_eq!(score("Discord", "xyz"), None);
        assert_eq!(score("Discord", ""), Some(1));
    }

    #[test]
    fn cuts_long_names_for_the_grid() {
        assert_eq!(cell_label("Alacritty"), "Alacritty");
        let short = cell_label("KDE System Settings Module");
        assert_eq!(short.chars().count(), CELL_LABEL_LENGTH);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn loose_matches_come_after_real_ones() {
        let mut spotlight = Spotlight::new(iced_layershell::reexport::IcedId::unique());
        spotlight.query = String::from("notes");
        spotlight.files = vec![FileHit {
            path: PathBuf::from("/home/me/notes"),
            name: String::from("notes"),
            is_dir: true,
        }];
        spotlight.files_query = String::from("notes");
        let app = |name: &str| {
            Rc::new(App {
                id: name.to_owned(),
                name: name.to_owned(),
                icon: None,
                command: Some(vec![String::from("x")]),
                unlisted: false,
                categories: Vec::new(),
                actions: Vec::new(),
            })
        };
        let apps = vec![app("No Terminal Editors"), app("Notes")];
        let hits = spotlight.hits(&apps, &VecDeque::new());
        assert!(matches!(&hits[0], Hit::App(found) if found.name == "Notes"));
        assert!(matches!(&hits[1], Hit::File(_)));
        assert!(matches!(&hits[2], Hit::App(found) if found.name == "No Terminal Editors"));
    }

    #[test]
    fn files_need_a_real_match() {
        assert_eq!(word_score("notes.txt", "notes"), Some(400));
        assert_eq!(word_score("notes.txt", "nts"), None);
    }

    #[test]
    fn moves_around_a_grid_and_stops_at_its_ends() {
        let mut spotlight = Spotlight::new(iced_layershell::reexport::IcedId::unique());
        spotlight.mode = Mode::Apps;
        spotlight.select_by(1, 0, 20);
        assert_eq!(spotlight.selected, COLUMNS);
        spotlight.select_by(0, 1, 20);
        assert_eq!(spotlight.selected, COLUMNS + 1);
        spotlight.select_by(9, 0, 20);
        assert_eq!(spotlight.selected, 19);
        spotlight.select_by(-9, 0, 20);
        assert_eq!(spotlight.selected, 0);
    }

    #[test]
    fn a_copied_thing_moves_to_the_top_instead_of_repeating() {
        let clip = |text: &str| Clip {
            content: Content::Text(text.to_owned()),
            copied: chrono::Local::now(),
            source: None,
        };
        let mut clips = VecDeque::new();
        remember_clip(&mut clips, clip("a"), 50);
        remember_clip(&mut clips, clip("b"), 50);
        remember_clip(&mut clips, clip("a"), 50);
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].content, Content::Text(String::from("a")));
        // No more than it's set to keep, the oldest going first.
        remember_clip(&mut clips, clip("c"), 2);
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[1].content, Content::Text(String::from("a")));
    }

    #[test]
    fn reads_recent_files_that_still_exist() {
        let file = env::current_exe().unwrap();
        let text = format!(
            "<xbel><bookmark href=\"file://{}\" added=\"x\"><info/></bookmark>\
             <bookmark href=\"file:///no/such/file\"></bookmark></xbel>",
            file.display()
        );
        let found = parse_recent(&text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, file);
    }
}
