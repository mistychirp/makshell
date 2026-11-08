//! The desktop: the Desktop folder's files as icons down the right of the screen, which can be
//! selected by clicking or with a rubber band, dragged around, opened and renamed, with a menu for
//! the desktop itself. It's its own surface above the wallpaper and below every window, so that the
//! wallpaper doesn't have to redraw along with a rubber band.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use std::{env, fs, io};

use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings};
use iced_widget::core::{Alignment, Background, Border, Color, Length, Point, Rectangle, Size};
use iced_widget::{
    Space, column, container, image, mouse_area, pin, slider, stack, text, text_input,
};
use serde::{Deserialize, Serialize};

use crate::app::{Element, Message};
use crate::icons::Glyph;
use crate::{dbus, fonts, menu, menubar, places};

// Measured from Tahoe's desktop.
const MARGIN_RIGHT: f32 = 18.0;
const MARGIN_TOP: f32 = 10.0;
/// Icons stop short of the bottom of the screen, where the dock is.
const DOCK_CLEARANCE: f32 = 90.0;
const BOX_RADIUS: f32 = 8.0;
const LABEL_GAP: f32 = 4.0;
/// How far apart the icons of a stack's pile are.
const STACK_OFFSET: f32 = 5.0;
const THUMBNAIL_SIZE: u32 = 128;
/// Files bigger than this keep their generic icon rather than being read for a thumbnail.
const MAX_THUMBNAIL_BYTES: u64 = 64 * 1024 * 1024;
/// How far a press has to move before it's a drag.
const DRAG_DISTANCE: f32 = 4.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub fn settings() -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        layer: Layer::Bottom,
        size: Some((0, 0)),
        exclusive_zone: Some(-1),
        // None at first, then on demand, so that clicking the desktop takes the keyboard from the
        // active window, like on macOS. KWin activates a new surface that takes the keyboard as it
        // appears, which after the shell restarts takes it from the window that had it, and leaves
        // the shell thinking no app is in front. One that only starts taking it later isn't.
        keyboard_interactivity: KeyboardInteractivity::None,
        namespace: Some(String::from("dock")),
        ..Default::default()
    }
}

/// How long after the desktop's surface opens it starts taking the keyboard: by then, it's been
/// shown, and KWin no longer activates it for that.
pub const KEYBOARD_AFTER: Duration = Duration::from_secs(1);

/// How big things are on the desktop, which View Options can change.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metrics {
    pub icon: f32,
    pub spacing: f32,
    pub text: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            icon: 52.0,
            spacing: 0.0,
            text: 12.0,
        }
    }
}

impl Metrics {
    fn box_size(self) -> f32 {
        self.icon + 16.0
    }

    fn line(self) -> f32 {
        self.text + 3.0
    }

    fn width(self) -> f32 {
        self.icon + 40.0 + self.spacing
    }

    fn label_width(self) -> f32 {
        self.width() - 4.0 - self.spacing
    }

    fn height(self) -> f32 {
        self.box_size() + LABEL_GAP + 2.0 * self.line() + 6.0 + self.spacing
    }
}

/// What a group of files is, when they're being kept in stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StackKind {
    Documents,
    Images,
    Movies,
    Music,
    PdfDocuments,
    Presentations,
    Screenshots,
    Spreadsheets,
    Other,
}

impl StackKind {
    fn of(entry: &Entry) -> Self {
        let extension = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match extension.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "avif" | "heic" | "tiff" => {
                if entry.name.to_lowercase().starts_with("screenshot") {
                    Self::Screenshots
                } else {
                    Self::Images
                }
            }
            "mp4" | "mkv" | "webm" | "mov" | "avi" => Self::Movies,
            "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" => Self::Music,
            "pdf" => Self::PdfDocuments,
            "odp" | "ppt" | "pptx" | "key" => Self::Presentations,
            "ods" | "xls" | "xlsx" | "csv" | "numbers" => Self::Spreadsheets,
            "txt" | "md" | "odt" | "doc" | "docx" | "rtf" | "pages" | "log" => Self::Documents,
            _ => Self::Other,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Documents => "Documents",
            Self::Images => "Images",
            Self::Movies => "Movies",
            Self::Music => "Music",
            Self::PdfDocuments => "PDF Documents",
            Self::Presentations => "Presentations",
            Self::Screenshots => "Screenshots",
            Self::Spreadsheets => "Spreadsheets",
            Self::Other => "Other",
        }
    }
}

/// A pile of files of a kind, when they're being kept in stacks.
struct Stack {
    kind: StackKind,
    position: Point,
    /// A few of the files' icons, for the pile.
    icons: Vec<image::Handle>,
}

/// What the menus on the desktop can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NewFolder,
    Open,
    Rename,
    MoveToTrash,
    GetInfo,
    ChangeWallpaper,
    EditWidgets,
    SortBy(Sort),
    CleanUp,
    CleanUpBy(Sort),
    UseStacks,
    Eject,
    ViewOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    /// Icons stay where they're put.
    #[default]
    None,
    Name,
    Kind,
    Modified,
    Size,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::None => "None",
            Sort::Name => "Name",
            Sort::Kind => "Kind",
            Sort::Modified => "Date Modified",
            Sort::Size => "Size",
        }
    }
}

/// A file in the Desktop folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub modified: Option<SystemTime>,
    pub len: u64,
    /// The device, if this is a mounted drive rather than a file in the Desktop folder.
    pub drive: Option<String>,
}

struct Item {
    entry: Entry,
    /// The top left of its cell.
    position: Point,
    icon: Option<image::Handle>,
    /// It's in a stack that isn't open.
    hidden: bool,
}

enum Gesture {
    /// A rubber band, and what was selected before it, which it adds to.
    Rubber { start: Point },
    /// The selected icons were pressed, and might be dragged.
    Icons {
        start: Point,
        from: Vec<(PathBuf, Point)>,
    },
    /// The selected icons are being dragged, as a drag and drop, so they can go anywhere.
    Dragging {
        start: Point,
        from: Vec<(PathBuf, Point)>,
    },
}

/// What's kept between sessions.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    sort: Sort,
    /// By file name, in distance from the right and the top, so that they survive a change of
    /// screen.
    positions: BTreeMap<String, (f32, f32)>,
    use_stacks: bool,
    metrics: Metrics,
}

pub struct Rename {
    pub path: PathBuf,
    pub value: String,
    /// Its field has yet to be given the keyboard. It can't have it until the icon's on screen.
    pub wants_focus: bool,
}

#[derive(Default)]
pub struct Desktop {
    folder: PathBuf,
    items: Vec<Item>,
    positions: BTreeMap<String, (f32, f32)>,
    sort: Sort,
    size: Size,
    pointer: Point,
    gesture: Option<Gesture>,
    selected: HashSet<PathBuf>,
    last_press: Option<(PathBuf, Instant)>,
    pub rename: Option<Rename>,
    /// Ctrl or Shift, which add to the selection.
    pub adding: bool,
    /// The folder a drag would go in, if dropped now.
    drop_folder: Option<PathBuf>,
    metrics: Metrics,
    use_stacks: bool,
    stacks: Vec<Stack>,
    open_stack: Option<StackKind>,
    /// The grid changed size while stacks were deciding where things go.
    grid_changed: bool,
}

/// What a press on the desktop turned out to be about.
pub enum Pressed {
    Nothing,
    /// An icon was double-clicked.
    Open,
}

impl Desktop {
    pub fn load() -> Self {
        let saved: Saved = fs::read_to_string(saved_path())
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            folder: desktop_dir(),
            positions: saved.positions,
            sort: saved.sort,
            use_stacks: saved.use_stacks,
            metrics: saved.metrics,
            ..Default::default()
        }
    }

    /// Where the pointer is, in whole logical pixels.
    pub fn pointer(&self) -> (i32, i32) {
        (self.pointer.x as i32, self.pointer.y as i32)
    }

    pub fn folder(&self) -> PathBuf {
        self.folder.clone()
    }

    pub fn set_size(&mut self, size: Size) {
        self.size = size;
        self.arrange();
    }

    /// Whether the field for renaming should be given the keyboard now. Asks once, as soon as its
    /// icon is on screen.
    pub fn take_rename_focus(&mut self) -> bool {
        let items = &self.items;
        match &mut self.rename {
            Some(rename)
                if rename.wants_focus
                    && items.iter().any(|item| item.entry.path == rename.path) =>
            {
                rename.wants_focus = false;
                true
            }
            _ => false,
        }
    }

    /// Takes in what's in the folder now. `icon` finds a generic icon for what's new. Returns the
    /// new files worth reading a thumbnail from.
    pub fn merge(
        &mut self,
        entries: Vec<Entry>,
        mut icon: impl FnMut(&Entry) -> Option<image::Handle>,
    ) -> Vec<PathBuf> {
        let mut old: HashMap<PathBuf, Item> = self
            .items
            .drain(..)
            .map(|item| (item.entry.path.clone(), item))
            .collect();
        let mut wanted = Vec::new();
        self.items = entries
            .into_iter()
            .map(|entry| match old.remove(&entry.path) {
                Some(mut item) => {
                    item.entry = entry;
                    item
                }
                None => {
                    if entry.len <= MAX_THUMBNAIL_BYTES && has_thumbnail(&entry.path) {
                        wanted.push(entry.path.clone());
                    }
                    Item {
                        icon: icon(&entry),
                        entry,
                        position: Point::ORIGIN,
                        hidden: false,
                    }
                }
            })
            .collect();
        // What's gone can't stay selected.
        let items = &self.items;
        self.selected
            .retain(|path| items.iter().any(|item| item.entry.path == *path));
        self.arrange();
        wanted
    }

    pub fn set_thumbnail(&mut self, path: &Path, thumbnail: image::Handle) {
        if let Some(item) = self.items.iter_mut().find(|item| item.entry.path == path) {
            item.icon = Some(thumbnail);
        }
    }

    /// Puts icons where they belong: all of them in order if they're being sorted, and otherwise
    /// where they were left, with any new ones in the first free cell.
    fn arrange(&mut self) {
        if self.size.width <= 0.0 {
            return;
        }
        if self.use_stacks {
            self.arrange_in_stacks();
            return;
        }
        self.stacks.clear();
        for item in &mut self.items {
            item.hidden = false;
        }
        if self.sort != Sort::None {
            self.sort_into_cells(self.sort);
            return;
        }
        let width = self.size.width;
        let mut unplaced = Vec::new();
        for (index, item) in self.items.iter_mut().enumerate() {
            match self.positions.get(&item.entry.name) {
                Some(&(from_right, y)) => {
                    item.position = Point::new(width - from_right, y);
                }
                None => unplaced.push(index),
            }
        }
        let any_new = !unplaced.is_empty();
        for index in unplaced {
            let position = self.first_free_cell();
            self.place(index, position);
        }
        if any_new {
            self.save();
        }
    }

    /// Drives, then a stack for each kind of file there's more than one of, with an open stack's
    /// files right after it, then everything else. Where icons were left is kept for when stacks
    /// are turned off.
    fn arrange_in_stacks(&mut self) {
        let mut by_kind: BTreeMap<StackKind, Vec<usize>> = BTreeMap::new();
        for (index, item) in self.items.iter().enumerate() {
            if !item.entry.is_dir && item.entry.drive.is_none() {
                by_kind
                    .entry(StackKind::of(&item.entry))
                    .or_default()
                    .push(index);
            }
        }
        by_kind.retain(|_, members| members.len() > 1);
        let name = |index: &usize, items: &[Item]| items[*index].entry.name.to_lowercase();
        let stacked: HashSet<usize> = by_kind.values().flatten().copied().collect();

        let mut cell = 0;
        let mut drives: Vec<usize> = (0..self.items.len())
            .filter(|index| self.items[*index].entry.drive.is_some())
            .collect();
        drives.sort_by_key(|index| name(index, &self.items));
        for index in drives {
            self.items[index].position = self.cell(cell);
            self.items[index].hidden = false;
            cell += 1;
        }

        let mut stacks = Vec::new();
        for (kind, mut members) in by_kind {
            members.sort_by_key(|index| name(index, &self.items));
            stacks.push(Stack {
                kind,
                position: self.cell(cell),
                icons: members
                    .iter()
                    .take(3)
                    .filter_map(|index| self.items[*index].icon.clone())
                    .collect(),
            });
            cell += 1;
            let open = self.open_stack == Some(kind);
            for index in members {
                self.items[index].hidden = !open;
                if open {
                    self.items[index].position = self.cell(cell);
                    cell += 1;
                }
            }
        }
        self.stacks = stacks;

        let mut rest: Vec<usize> = (0..self.items.len())
            .filter(|index| !stacked.contains(index) && self.items[*index].entry.drive.is_none())
            .collect();
        rest.sort_by_key(|index| name(index, &self.items));
        for index in rest {
            self.items[index].position = self.cell(cell);
            self.items[index].hidden = false;
            cell += 1;
        }
    }

    fn stack_at(&self, point: Point) -> Option<StackKind> {
        self.stacks
            .iter()
            .find(|stack| {
                self.hit_rects(stack.position)
                    .iter()
                    .any(|rect| rect.contains(point))
            })
            .map(|stack| stack.kind)
    }

    pub fn toggle_stacks(&mut self) {
        self.use_stacks = !self.use_stacks;
        self.open_stack = None;
        self.arrange();
        // Where icons were left was on a grid of another size.
        if !self.use_stacks && std::mem::take(&mut self.grid_changed) && self.sort == Sort::None {
            self.clean_up();
        }
        self.save();
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Icons keep their cells, as near as they can, when the grid changes size.
    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.metrics = metrics;
        if self.use_stacks {
            self.grid_changed = true;
        } else if self.sort == Sort::None {
            self.clean_up();
        }
        self.arrange();
        self.save();
    }

    /// The device behind a mounted drive's icon.
    pub fn drive_device(&self, path: &Path) -> Option<String> {
        self.items
            .iter()
            .find(|item| item.entry.path == path)
            .and_then(|item| item.entry.drive.clone())
    }

    fn cell(&self, index: usize) -> Point {
        let top = menubar::HEIGHT as f32 + MARGIN_TOP;
        let (width, height) = (self.metrics.width(), self.metrics.height());
        let rows = (((self.size.height - top - DOCK_CLEARANCE) / height) as usize).max(1);
        Point::new(
            self.size.width - MARGIN_RIGHT - ((index / rows) + 1) as f32 * width,
            top + (index % rows) as f32 * height,
        )
    }

    fn first_free_cell(&self) -> Point {
        let width = self.size.width;
        let taken = |cell: Point| {
            self.positions.values().any(|&(from_right, y)| {
                ((width - from_right) - cell.x).abs() < self.metrics.width() / 2.0
                    && (y - cell.y).abs() < self.metrics.height() / 2.0
            })
        };
        (0..)
            .map(|index| self.cell(index))
            .find(|&cell| !taken(cell) || cell.x < 0.0)
            .unwrap_or(Point::ORIGIN)
    }

    fn place(&mut self, index: usize, position: Point) {
        let item = &mut self.items[index];
        item.position = position;
        self.positions.insert(
            item.entry.name.clone(),
            (self.size.width - position.x, position.y),
        );
    }

    fn sort_into_cells(&mut self, sort: Sort) {
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        let items = &self.items;
        let name_key = |index: &usize| items[*index].entry.name.to_lowercase();
        match sort {
            Sort::None | Sort::Name => order.sort_by_key(name_key),
            Sort::Kind => order.sort_by_key(|index| {
                let entry = &items[*index].entry;
                (
                    !entry.is_dir,
                    entry
                        .path
                        .extension()
                        .map(|extension| extension.to_ascii_lowercase()),
                    name_key(index),
                )
            }),
            // Newest and biggest first.
            Sort::Modified => {
                order.sort_by_key(|index| std::cmp::Reverse(items[*index].entry.modified));
            }
            Sort::Size => order.sort_by_key(|index| std::cmp::Reverse(items[*index].entry.len)),
        }
        for (cell, index) in order.into_iter().enumerate() {
            let position = self.cell(cell);
            self.place(index, position);
        }
    }

    /// Snaps every icon to the nearest free cell, keeping them about where they are.
    fn clean_up(&mut self) {
        let mut taken: Vec<Point> = Vec::new();
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        let items = &self.items;
        order.sort_by(|a, b| {
            let (a, b) = (items[*a].position, items[*b].position);
            (a.y, a.x)
                .partial_cmp(&(b.y, b.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let cells: Vec<Point> = (0..(self.items.len() * 2 + 64))
            .map(|index| self.cell(index))
            .filter(|cell| cell.x >= 0.0)
            .collect();
        for index in order {
            let at = self.items[index].position;
            let nearest = cells
                .iter()
                .filter(|cell| !taken.contains(cell))
                .min_by(|a, b| {
                    a.distance(at)
                        .partial_cmp(&b.distance(at))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .copied();
            if let Some(cell) = nearest {
                taken.push(cell);
                self.place(index, cell);
            }
        }
    }

    pub fn clean_up_action(&mut self, sort: Option<Sort>) {
        match sort {
            Some(sort) => self.sort_into_cells(sort),
            None => self.clean_up(),
        }
        self.save();
    }

    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
        self.arrange();
        self.save();
    }

    fn save(&self) {
        let saved = Saved {
            sort: self.sort,
            positions: self.positions.clone(),
            use_stacks: self.use_stacks,
            metrics: self.metrics,
        };
        let path = saved_path();
        let result = toml::to_string(&saved)
            .map_err(io::Error::other)
            .and_then(|text| {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, text)
            });
        if let Err(err) = result {
            log::warn!("can't save the desktop's layout: {err}");
        }
    }

    /// An icon's box and its label, in surface coordinates, for its cell's top left.
    fn hit_rects(&self, position: Point) -> [Rectangle; 2] {
        let metrics = self.metrics;
        let center = position.x + metrics.width() / 2.0;
        [
            Rectangle {
                x: center - metrics.box_size() / 2.0,
                y: position.y,
                width: metrics.box_size(),
                height: metrics.box_size(),
            },
            Rectangle {
                x: center - metrics.label_width() / 2.0,
                y: position.y + metrics.box_size() + LABEL_GAP,
                width: metrics.label_width(),
                height: 2.0 * metrics.line(),
            },
        ]
    }

    fn item_at(&self, point: Point) -> Option<usize> {
        // Topmost first, which is the last one drawn.
        self.items.iter().rposition(|item| {
            !item.hidden
                && self
                    .hit_rects(item.position)
                    .iter()
                    .any(|rect| rect.contains(point))
        })
    }

    /// Returns whether a drag of the selection should start.
    pub fn pointer_moved(&mut self, point: Point) -> bool {
        self.pointer = point;
        match &mut self.gesture {
            Some(Gesture::Rubber { start }) => {
                let band = rect_between(*start, point);
                let adding = self.adding;
                let mut selected = if adding {
                    self.selected.clone()
                } else {
                    HashSet::new()
                };
                for item in self.items.iter().filter(|item| !item.hidden) {
                    if self
                        .hit_rects(item.position)
                        .iter()
                        .any(|rect| rect.intersects(&band))
                    {
                        selected.insert(item.entry.path.clone());
                    }
                }
                self.selected = selected;
            }
            Some(Gesture::Icons { start, from }) => {
                let delta = point - *start;
                if delta.x.hypot(delta.y) >= DRAG_DISTANCE {
                    self.gesture = Some(Gesture::Dragging {
                        start: *start,
                        from: std::mem::take(from),
                    });
                    return true;
                }
            }
            Some(Gesture::Dragging { .. }) | None => {}
        }
        false
    }

    /// The icons of what's selected, for a picture of them being dragged.
    pub fn selected_icons(&self) -> Vec<image::Handle> {
        self.items
            .iter()
            .filter(|item| self.selected.contains(&item.entry.path))
            .filter_map(|item| item.icon.clone())
            .collect()
    }

    /// A drag of the selection is over, wherever it went.
    pub fn drag_ended(&mut self) {
        if matches!(self.gesture, Some(Gesture::Dragging { .. })) {
            self.gesture = None;
        }
        self.drop_folder = None;
    }

    /// The selection was dragged to here on the desktop itself.
    pub fn moved_selection_to(&mut self, point: Point) {
        let Some(Gesture::Dragging { start, from }) = self.gesture.take() else {
            return;
        };
        // Stacks decide where everything goes.
        if self.use_stacks {
            return;
        }
        let delta = point - start;
        for (path, origin) in from {
            if let Some(index) = self.items.iter().position(|item| item.entry.path == path) {
                self.place(index, origin + delta);
            }
        }
        // Sorted icons go back to their order, like on macOS.
        self.arrange();
        self.save();
    }

    /// The folder under this point that isn't itself being dragged, if there is one.
    pub fn folder_at(&self, point: Point) -> Option<PathBuf> {
        let item = &self.items[self.item_at(point)?];
        let dragged = matches!(self.gesture, Some(Gesture::Dragging { .. }))
            && self.selected.contains(&item.entry.path);
        (item.entry.is_dir && !dragged).then(|| item.entry.path.clone())
    }

    /// A drag's over this point, which lights up a folder it could go in.
    pub fn drag_over(&mut self, point: Option<Point>) {
        self.drop_folder = point.and_then(|point| self.folder_at(point));
    }

    /// Files that were just dropped go around this point, one cell apart.
    pub fn place_dropped(&mut self, names: &[String], point: Point) {
        let columns = 4;
        for (index, name) in names.iter().enumerate() {
            let metrics = self.metrics;
            let x = point.x - metrics.width() / 2.0 - (index % columns) as f32 * metrics.width();
            let y =
                point.y - metrics.box_size() / 2.0 + (index / columns) as f32 * metrics.height();
            self.positions
                .insert(name.clone(), (self.size.width - x, y));
        }
        self.save();
    }

    /// The left button went down at the pointer.
    pub fn pressed(&mut self) -> Pressed {
        let point = self.pointer;
        if let Some(kind) = self.stack_at(point) {
            self.open_stack = if self.open_stack == Some(kind) {
                None
            } else {
                Some(kind)
            };
            self.selected.clear();
            self.arrange();
            return Pressed::Nothing;
        }
        let Some(index) = self.item_at(point) else {
            if !self.adding {
                self.selected.clear();
            }
            self.gesture = Some(Gesture::Rubber { start: point });
            self.last_press = None;
            return Pressed::Nothing;
        };
        let path = self.items[index].entry.path.clone();
        let now = Instant::now();
        let double = self
            .last_press
            .as_ref()
            .is_some_and(|(last, at)| *last == path && now - *at < DOUBLE_CLICK);
        self.last_press = Some((path.clone(), now));

        if self.adding {
            if !self.selected.remove(&path) {
                self.selected.insert(path);
            }
        } else if !self.selected.contains(&path) {
            self.selected = HashSet::from([path]);
        }
        if double {
            return Pressed::Open;
        }
        let from = self
            .items
            .iter()
            .filter(|item| self.selected.contains(&item.entry.path))
            .map(|item| (item.entry.path.clone(), item.position))
            .collect();
        self.gesture = Some(Gesture::Icons { start: point, from });
        Pressed::Nothing
    }

    /// The left button came up.
    pub fn released(&mut self) {
        if let Some(Gesture::Icons { from, .. }) = &self.gesture
            && from.len() > 1
            && !self.adding
        {
            // A plain click on one of several selected icons narrows it down to that one.
            if let Some(index) = self.item_at(self.pointer) {
                self.selected = HashSet::from([self.items[index].entry.path.clone()]);
            }
        }
        // A drag and drop ends when it says so, and the pointer's elsewhere by then anyway.
        if !matches!(self.gesture, Some(Gesture::Dragging { .. })) {
            self.gesture = None;
        }
    }

    /// The right button went down: whatever's under the pointer becomes the selection. Returns the
    /// menu for that, and whether it's on an icon.
    pub fn context_menu(&mut self) -> Vec<menu::Entry> {
        match self.item_at(self.pointer) {
            Some(index) => {
                let path = self.items[index].entry.path.clone();
                if !self.selected.contains(&path) {
                    self.selected = HashSet::from([path]);
                }
                let any_drive = self.items.iter().any(|item| {
                    item.entry.drive.is_some() && self.selected.contains(&item.entry.path)
                });
                let mut entries = vec![desktop_item("Open", Action::Open), menu::Entry::Separator];
                if any_drive {
                    entries.push(desktop_item("Eject", Action::Eject));
                } else {
                    entries.push(
                        desktop_item("Move to Trash", Action::MoveToTrash)
                            .icon(menu::Icon::Glyph(Glyph::Trash)),
                    );
                }
                entries.push(menu::Entry::Separator);
                entries.push(
                    desktop_item("Get Info", Action::GetInfo).icon(menu::Icon::Glyph(Glyph::Info)),
                );
                if self.selected.len() == 1 && !any_drive {
                    entries.push(desktop_item("Rename", Action::Rename));
                }
                entries
            }
            None => {
                self.selected.clear();
                let sorts = |current: Option<Sort>, action: fn(Sort) -> Action, none: bool| {
                    [
                        Sort::None,
                        Sort::Name,
                        Sort::Kind,
                        Sort::Modified,
                        Sort::Size,
                    ]
                    .into_iter()
                    .filter(|sort| none || *sort != Sort::None)
                    .map(|sort| {
                        desktop_item(sort.label(), action(sort)).checked(current == Some(sort))
                    })
                    .collect::<Vec<_>>()
                };
                vec![
                    desktop_item("New Folder", Action::NewFolder)
                        .icon(menu::Icon::Glyph(Glyph::NewFolder)),
                    menu::Entry::Separator,
                    desktop_item("Get Info", Action::GetInfo).icon(menu::Icon::Glyph(Glyph::Info)),
                    desktop_item("Change Wallpaper…", Action::ChangeWallpaper),
                    desktop_item("Edit Widgets…", Action::EditWidgets),
                    menu::Entry::Separator,
                    desktop_item(
                        if self.use_stacks {
                            "Stop Using Stacks"
                        } else {
                            "Use Stacks"
                        },
                        Action::UseStacks,
                    )
                    .icon(menu::Icon::Glyph(Glyph::Stacks)),
                    desktop_item("Sort By", Action::SortBy(self.sort))
                        .icon(menu::Icon::Glyph(Glyph::Sort))
                        .enabled(!self.use_stacks)
                        .submenu(sorts(Some(self.sort), Action::SortBy, true)),
                    desktop_item("Clean Up", Action::CleanUp)
                        .icon(menu::Icon::Glyph(Glyph::Grid))
                        .enabled(self.sort == Sort::None && !self.use_stacks),
                    desktop_item("Clean Up By", Action::CleanUp)
                        .enabled(!self.use_stacks)
                        .submenu(sorts(None, Action::CleanUpBy, false)),
                    desktop_item("Show View Options", Action::ViewOptions)
                        .icon(menu::Icon::Glyph(Glyph::Gear)),
                ]
            }
        }
    }

    /// The icons on show.
    pub fn icons(&self) -> impl Iterator<Item = &image::Handle> {
        self.items.iter().filter_map(|item| item.icon.as_ref())
    }

    pub fn selection(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .filter(|item| self.selected.contains(&item.entry.path))
            .map(|item| item.entry.path.clone())
            .collect()
    }

    /// Selects a file the moment it's known about, like a new folder.
    pub fn select_only(&mut self, path: PathBuf) {
        self.selected = HashSet::from([path]);
    }

    pub fn start_rename(&mut self, path: PathBuf) {
        let value = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.rename = Some(Rename {
            path,
            value,
            wants_focus: true,
        });
    }

    /// Finishes renaming. The file keeps its name if the new one can't be had.
    pub fn commit_rename(&mut self) {
        let Some(Rename { path, value, .. }) = self.rename.take() else {
            return;
        };
        let value = value.trim();
        let Some(parent) = path.parent() else {
            return;
        };
        let target = parent.join(value);
        if value.is_empty() || value.contains('/') || target == path || target.exists() {
            return;
        }
        if let Err(err) = fs::rename(&path, &target) {
            log::warn!("can't rename {}: {err}", path.display());
            return;
        }
        let old_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        if let Some(position) = old_name.and_then(|name| self.positions.remove(&name)) {
            self.positions.insert(value.to_owned(), position);
        }
        if let Some(item) = self.items.iter_mut().find(|item| item.entry.path == path) {
            item.entry.path = target.clone();
            item.entry.name = value.to_owned();
        }
        if self.selected.remove(&path) {
            self.selected.insert(target);
        }
        self.save();
    }

    /// What decides whether the surface has to redraw.
    pub fn content_key(&self, active: bool) -> String {
        let items: Vec<_> = self
            .items
            .iter()
            .filter(|item| !item.hidden)
            .map(|item| (&item.entry.name, item.position, item.icon.is_some()))
            .collect();
        let mut selected: Vec<_> = self.selected.iter().collect();
        selected.sort();
        format!(
            "{items:?} {selected:?} {:?} {:?} {active} {:?} {} {:?} {:?}",
            self.rubber_band(),
            self.rename.as_ref().map(|rename| &rename.value),
            self.drop_folder,
            self.is_dragging(),
            self.stacks
                .iter()
                .map(|stack| (stack.kind, stack.position, stack.icons.len()))
                .collect::<Vec<_>>(),
            self.metrics,
        )
    }

    fn is_dragging(&self) -> bool {
        matches!(self.gesture, Some(Gesture::Dragging { .. }))
    }

    fn rubber_band(&self) -> Option<Rectangle> {
        match &self.gesture {
            Some(Gesture::Rubber { start }) if *start != self.pointer => {
                Some(rect_between(*start, self.pointer))
            }
            _ => None,
        }
    }
}

fn desktop_item(label: &str, action: Action) -> menu::Entry {
    menu::Entry::item(label, Message::Desktop(action))
}

fn rect_between(a: Point, b: Point) -> Rectangle {
    Rectangle {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        width: (a.x - b.x).abs(),
        height: (a.y - b.y).abs(),
    }
}

/// The widget that takes the keyboard while renaming.
pub fn rename_field() -> iced_widget::Id {
    iced_widget::Id::new("desktop-rename")
}

/// `active`: the desktop has the keyboard, rather than some window. Its selection shows in the
/// accent color then, and in grey otherwise.
pub fn view<'a>(desktop: &'a Desktop, active: bool, below: Vec<Element<'a>>) -> Element<'a> {
    let mut layers = below;
    for stack in &desktop.stacks {
        layers.push(
            pin(stack_view(
                stack,
                desktop.metrics,
                desktop.open_stack == Some(stack.kind),
            ))
            .x(stack.position.x)
            .y(stack.position.y)
            .into(),
        );
    }
    for item in desktop.items.iter().filter(|item| !item.hidden) {
        let in_selection = desktop.selected.contains(&item.entry.path);
        // A folder that would take what's being dragged lights up like it's selected.
        let selected = in_selection || desktop.drop_folder.as_ref() == Some(&item.entry.path);
        let dimmed = in_selection && desktop.is_dragging();
        let renaming = desktop
            .rename
            .as_ref()
            .filter(|rename| rename.path == item.entry.path);
        layers.push(
            pin(item_view(
                item,
                desktop.metrics,
                selected,
                dimmed,
                active,
                renaming,
            ))
            .x(item.position.x)
            .y(item.position.y)
            .into(),
        );
    }
    if let Some(band) = desktop.rubber_band() {
        layers.push(
            pin(container(Space::new())
                .width(band.width)
                .height(band.height)
                .style(|_| container::Style {
                    // Measured from Tahoe's.
                    background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.15))),
                    border: Border {
                        color: Color::from_rgba(1.0, 1.0, 1.0, 0.55),
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    ..Default::default()
                }))
            .x(band.x)
            .y(band.y)
            .into(),
        );
    }
    mouse_area(
        container(stack(layers))
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Message::DesktopPressed)
    .on_right_press(Message::DesktopRightPressed)
    .into()
}

/// A pile of the icons of a stack's files.
fn stack_view(stack: &Stack, metrics: Metrics, open: bool) -> Element<'_> {
    let size = metrics.icon - 2.0 * STACK_OFFSET;
    let mut pile = Vec::new();
    for (index, icon) in stack.icons.iter().enumerate().rev() {
        let inset = 8.0 + index as f32 * STACK_OFFSET;
        pile.push(
            pin(image(icon.clone()).width(size).height(size))
                .x(inset)
                .y(inset)
                .into(),
        );
    }
    let tile = container(iced_widget::Stack::with_children(pile))
        .width(metrics.box_size())
        .height(metrics.box_size())
        .style(move |_| {
            if open {
                container::Style {
                    background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.12))),
                    border: Border {
                        radius: BOX_RADIUS.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            } else {
                container::Style::default()
            }
        });
    container(
        column![
            tile,
            Space::new().height(LABEL_GAP),
            label_view(stack.kind.label(), metrics, false, false),
        ]
        .align_x(Alignment::Center)
        .width(metrics.width()),
    )
    .into()
}

fn item_view<'a>(
    item: &'a Item,
    metrics: Metrics,
    selected: bool,
    dimmed: bool,
    active: bool,
    renaming: Option<&'a Rename>,
) -> Element<'a> {
    let icon: Element<'a> = match &item.icon {
        Some(handle) => image(handle.clone())
            .width(metrics.icon)
            .height(metrics.icon)
            .opacity(if dimmed { 0.4_f32 } else { 1.0_f32 })
            .into(),
        None => Space::new().width(metrics.icon).height(metrics.icon).into(),
    };
    let tile = container(icon)
        .center_x(metrics.box_size())
        .center_y(metrics.box_size())
        .style(move |_| {
            if selected {
                container::Style {
                    background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.4))),
                    border: Border {
                        color: Color::from_rgba(1.0, 1.0, 1.0, 0.35),
                        width: 2.0,
                        radius: BOX_RADIUS.into(),
                    },
                    ..Default::default()
                }
            } else {
                container::Style::default()
            }
        });

    let label: Element<'a> = match renaming {
        Some(rename) => text_input("", &rename.value)
            .id(rename_field())
            .on_input(Message::DesktopRenameChanged)
            .size(metrics.text)
            .padding([1, 4])
            .style(|_, _| text_input::Style {
                background: Background::Color(Color::WHITE),
                border: Border {
                    color: crate::appearance::accent(),
                    width: 2.0,
                    radius: 4.0.into(),
                },
                icon: Color::BLACK,
                placeholder: Color::from_rgb(0.5, 0.5, 0.5),
                value: Color::BLACK,
                selection: Color::from_rgba(0.7, 0.18, 0.69, 0.35),
            })
            .into(),
        None => label_view(&item.entry.name, metrics, selected, active),
    };

    container(
        column![tile, Space::new().height(LABEL_GAP), label]
            .align_x(Alignment::Center)
            .width(metrics.width()),
    )
    .into()
}

/// A name broken over at most two lines, and shortened on the second if it's still too long.
fn two_lines(name: &str, metrics: Metrics) -> String {
    let width = metrics.label_width() - 12.0;
    let fits = |words: &str| menu::text_width(words, metrics.text, fonts::BOLD) <= width;
    if fits(name) {
        return name.to_owned();
    }
    // The most that fits on the first line, between words if it can.
    let mut end = 0;
    let mut last_space = None;
    for (index, character) in name.char_indices() {
        if !fits(&name[..index + character.len_utf8()]) {
            break;
        }
        end = index + character.len_utf8();
        if character == ' ' {
            last_space = Some(index);
        }
    }
    let (first, rest) = match last_space {
        Some(space) => (&name[..space], name[space + 1..].trim_start()),
        None => name.split_at(end),
    };
    format!(
        "{first}\n{}",
        menu::fit(rest, metrics.text, fonts::BOLD, width)
    )
}

fn label_view(name: &str, metrics: Metrics, selected: bool, active: bool) -> Element<'_> {
    let shown = two_lines(name, metrics);
    let label_width = metrics.label_width();
    let words = |color: Color| {
        text(shown.clone())
            .size(metrics.text)
            .line_height(iced_widget::core::text::LineHeight::Absolute(
                metrics.line().into(),
            ))
            .font(fonts::BOLD)
            .align_x(iced_widget::core::alignment::Horizontal::Center)
            .color(color)
    };
    if selected {
        // Measured from Tahoe: its accent when the desktop's active, and light grey when not.
        let (background, ink) = if active {
            (Color::from_rgb8(128, 54, 130), Color::WHITE)
        } else {
            (
                Color::from_rgb8(200, 200, 205),
                Color::from_rgb8(70, 70, 70),
            )
        };
        return container(words(ink))
            .padding([1, 6])
            .max_width(label_width)
            .style(move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border {
                    radius: 5.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into();
    }
    // A shadow, so that names stay legible on a bright wallpaper.
    container(stack![
        pin(words(Color::from_rgba(0.0, 0.0, 0.0, 0.6)).width(label_width)).y(1.0),
        words(Color::WHITE).width(label_width),
    ])
    .width(label_width)
    .into()
}

pub fn has_thumbnail(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg"
            )
        })
}

/// Drives mounted for the user, from the kernel's list of mounts.
fn mounted_drives() -> Vec<Entry> {
    let (Ok(mounts), Some(user)) = (
        fs::read_to_string("/proc/self/mounts"),
        env::var_os("USER").map(|user| user.to_string_lossy().into_owned()),
    ) else {
        return Vec::new();
    };
    parse_drives(&mounts, &user)
}

fn parse_drives(mounts: &str, user: &str) -> Vec<Entry> {
    let prefix = format!("/run/media/{user}/");
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let device = fields.next()?;
            let mount = unescape_mount(fields.next()?);
            let name = mount.strip_prefix(&prefix)?;
            (!name.is_empty() && !name.contains('/')).then(|| Entry {
                path: PathBuf::from(&mount),
                name: name.to_owned(),
                is_dir: true,
                modified: None,
                len: 0,
                drive: Some(device.to_owned()),
            })
        })
        .collect()
}

/// The kernel's mount list writes spaces and such as octal, like `\\040`.
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut unescaped = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let octal = (bytes[index] == b'\\')
            .then(|| bytes.get(index + 1..index + 4))
            .flatten()
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        match octal {
            Some(byte) => {
                unescaped.push(byte);
                index += 4;
            }
            None => {
                unescaped.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&unescaped).into_owned()
}

/// Unmounts a drive, so that it's safe to pull out.
pub fn eject(device: &str) {
    crate::apps::spawn(vec![
        String::from("udisksctl"),
        String::from("unmount"),
        String::from("-b"),
        device.to_owned(),
    ]);
}

/// The panel for changing how big things are on the desktop.
pub fn view_options(metrics: Metrics) -> Element<'static> {
    let row_of = |label: &'static str,
                  range: std::ops::RangeInclusive<f32>,
                  value: f32,
                  step: f32,
                  change: fn(Metrics, f32) -> Metrics| {
        column![
            text(label).size(12.0),
            slider(range, value, move |value| Message::DesktopMetrics(change(
                metrics, value
            )))
            .step(step)
            .height(18.0)
            .style(|_, _| slider::Style {
                rail: slider::Rail {
                    backgrounds: (
                        Background::Color(crate::appearance::accent()),
                        Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.2)),
                    ),
                    width: 4.0,
                    border: Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                },
                handle: slider::Handle {
                    shape: slider::HandleShape::Circle { radius: 7.0 },
                    background: Background::Color(Color::WHITE),
                    border_width: 0.0,
                    border_color: Color::TRANSPARENT,
                },
            }),
        ]
        .spacing(4)
    };
    container(
        column![
            text("Desktop").size(13.0).font(fonts::BOLD),
            row_of(
                "Icon size",
                32.0..=128.0,
                metrics.icon,
                4.0,
                |metrics, icon| { Metrics { icon, ..metrics } }
            ),
            row_of(
                "Grid spacing",
                0.0..=60.0,
                metrics.spacing,
                4.0,
                |metrics, spacing| { Metrics { spacing, ..metrics } }
            ),
            row_of(
                "Text size",
                10.0..=16.0,
                metrics.text,
                1.0,
                |metrics, text| { Metrics { text, ..metrics } }
            ),
        ]
        .spacing(10),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_| container::Style {
        text_color: Some(Color::WHITE),
        ..Default::default()
    })
    .into()
}

pub const VIEW_OPTIONS_SIZE: (u32, u32) = (260, 206);

/// What's in the Desktop folder, without what's hidden, and the mounted drives.
pub async fn list(folder: PathBuf) -> Vec<Entry> {
    let drives = mounted_drives();
    let Ok(entries) = fs::read_dir(&folder) else {
        return drives;
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some(Entry {
                path: entry.path(),
                name,
                is_dir: metadata.is_dir(),
                modified: metadata.modified().ok(),
                len: metadata.len(),
                drive: None,
            })
        })
        .chain(drives)
        .collect()
}

/// Reads an image into a small picture of itself, on a thread of its own.
pub async fn thumbnail(path: PathBuf) -> Option<image::Handle> {
    tokio::task::spawn_blocking(move || {
        let picture = ::image::open(&path).ok()?;
        let small = picture.thumbnail(THUMBNAIL_SIZE, THUMBNAIL_SIZE).to_rgba8();
        let (width, height) = small.dimensions();
        Some(image::Handle::from_rgba(width, height, small.into_raw()))
    })
    .await
    .ok()
    .flatten()
}

/// Makes a folder called "untitled folder", with a number if that's taken.
pub fn new_folder(folder: &Path) -> io::Result<PathBuf> {
    for number in 1.. {
        let name = if number == 1 {
            String::from("untitled folder")
        } else {
            format!("untitled folder {number}")
        };
        let path = folder.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    unreachable!("there's always another number")
}

/// Moves files into a folder, without replacing anything: a name that's taken gets a number. Between
/// disks, it copies them instead, like on macOS. Returns the names they ended up with.
pub async fn move_into(paths: Vec<PathBuf>, folder: PathBuf) -> Vec<String> {
    tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .filter_map(|path| {
                move_one(path, &folder)
                    .inspect_err(|err| log::warn!("can't move {}: {err}", path.display()))
                    .ok()
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn move_one(path: &Path, folder: &Path) -> io::Result<String> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("it has no name"))?;
    // Already there.
    if path.parent() == Some(folder) {
        return Ok(name.to_string_lossy().into_owned());
    }
    if folder.starts_with(path) {
        return Err(io::Error::other("a folder can't go inside itself"));
    }
    let target = free_name(folder, name);
    match fs::rename(path, &target) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => copy_all(path, &target)?,
        Err(err) => return Err(err),
    }
    Ok(target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default())
}

/// A path in `folder` for a file called `name` that nothing's using yet.
fn free_name(folder: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let plain = folder.join(name);
    if !plain.exists() && !plain.is_symlink() {
        return plain;
    }
    let original = Path::new(name);
    let stem = original.file_stem().unwrap_or(name).to_string_lossy();
    let extension = original
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|number| folder.join(format!("{stem} {number}{extension}")))
        .find(|path| !path.exists() && !path.is_symlink())
        .unwrap_or(plain)
}

fn copy_all(from: &Path, to: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(from)?;
    if metadata.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if metadata.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

/// Shows the file manager's properties window for these.
pub async fn get_info(paths: Vec<PathBuf>) {
    let result = async {
        let connection = dbus::session().await.ok_or(zbus::Error::Unsupported)?;
        let uris: Vec<String> = paths.iter().map(|path| places::file_uri(path)).collect();
        connection
            .call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItemProperties",
                &(uris, ""),
            )
            .await?;
        zbus::Result::Ok(())
    }
    .await;
    if let Err(err) = result {
        log::warn!("can't show a file's properties: {err}");
    }
}

/// The Desktop folder from the XDG user dirs, or `~/Desktop`.
fn desktop_dir() -> PathBuf {
    places::user_dir("XDG_DESKTOP_DIR").unwrap_or_else(|| places::home().join("Desktop"))
}

fn saved_path() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| places::home().join(".local/state"))
        .join("makshell/desktop.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_drives_mounted_for_the_user() {
        let mounts = "/dev/nvme0n1p2 / btrfs rw 0 0\n\
            /dev/sda1 /run/media/misty/MACOS hfsplus rw 0 0\n\
            /dev/sdb1 /run/media/misty/My\\040Disk vfat rw 0 0\n\
            /dev/sdc1 /run/media/someone/Theirs vfat rw 0 0\n\
            tmpfs /run/user/1000 tmpfs rw 0 0\n";
        let drives = parse_drives(mounts, "misty");
        let names: Vec<_> = drives.iter().map(|drive| drive.name.as_str()).collect();
        assert_eq!(names, ["MACOS", "My Disk"]);
        assert_eq!(drives[1].path, PathBuf::from("/run/media/misty/My Disk"));
        assert_eq!(drives[0].drive.as_deref(), Some("/dev/sda1"));
    }

    #[test]
    fn groups_files_by_what_they_are() {
        let entry = |name: &str| Entry {
            path: PathBuf::from(name),
            name: name.to_owned(),
            is_dir: false,
            modified: None,
            len: 0,
            drive: None,
        };
        assert_eq!(StackKind::of(&entry("cat.PNG")), StackKind::Images);
        assert_eq!(
            StackKind::of(&entry("Screenshot from 2026.png")),
            StackKind::Screenshots
        );
        assert_eq!(StackKind::of(&entry("a.pdf")), StackKind::PdfDocuments);
        assert_eq!(StackKind::of(&entry("archive.zip")), StackKind::Other);
    }
}
