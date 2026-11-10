//! The Downloads stack, as a fan: the newest downloads rise out of its tile in an arc, leaning more as
//! they go, each with its name in a capsule. They can be opened, or dragged anywhere.
//!
//! Iced only rotates images, so a name is drawn into a picture once, when the fan opens. That's also
//! why hovering is worked out here rather than by widgets: they can't tell they've been rotated.

use std::path::PathBuf;

use iced_layershell::reexport::{
    Anchor, IcedId, KeyboardInteractivity, Layer, NewLayerShellSettings,
};
use iced_widget::core::{ContentFit, Length, Point, Radians, Rotation};
use iced_widget::graphics::text::{cosmic_text, font_system, to_attributes, to_shaping};
use iced_widget::{Space, container, image, mouse_area, pin};
use wayland_client::protocol::wl_surface::WlSurface;

use crate::anim::{Motion, Spring};
use crate::app::{Element, Message};
use crate::icons::Glyph;
use crate::{dock, fonts, material, menu};

/// How far apart the rows are, and how big a file's picture is.
const ROW_STEP: f32 = 64.0;
const PICTURE_SIZE: f32 = 58.0;
const PICTURE_RADIUS: f32 = 5.0;
/// From the top of the space above the dock to the middle of the first row.
const BOTTOM_ROW: f32 = 55.0;
const LABEL_GAP: f32 = 14.0;
const LABEL_HEIGHT: f32 = 26.0;
const LABEL_PADDING: f32 = 14.0;
const LABEL_TEXT: f32 = 13.5;
const LABEL_MAX_WIDTH: f32 = 470.0;
/// How many times finer than a logical pixel names are drawn, to stay sharp when rotated.
const LABEL_SCALE: f32 = 3.0;
const MAX_ROWS: usize = 11;
/// How far right the arc has gone and how far it leans, at the tenth row, measured from Tahoe's.
const ARC_REACH: f32 = 104.0;
const ARC_LEAN: f32 = 13.0;
const LEAN_AT_START: f32 = 1.0;
// Measured from a recording of Tahoe's: half way in about 110ms, and there by about 230ms, with the
// rows moving almost as one.
const OUT_SPRING: Spring = Spring::new(0.33, 0.9);
const BACK_SPRING: Spring = Spring::new(0.26, 1.0);
/// How far behind the bottom row the top one is, in seconds.
const STAGGER: f32 = 0.025;
/// How far the pointer moves with a row pressed before it's a drag.
const DRAG_DISTANCE: f32 = 4.0;

/// With glass under it, a name needs nothing behind it. Without, it needs a dark capsule.
const LABEL_ON_GLASS: [u8; 4] = [0, 0, 0, 0];
const LABEL_COLOR: [u8; 4] = [30, 22, 40, 170];
const LABEL_HOVER_COLOR: [u8; 4] = [178, 46, 176, 245];

/// A name in a capsule, drawn into a picture with room around it for its lean: iced cuts a rotated
/// image off at its box, and a `pin` shrinks whatever would stick out of the surface, which would pull
/// a square box off center near the bottom.
#[derive(Debug, Clone)]
struct Label {
    content: String,
    plain: image::Handle,
    /// Drawn the first time it's hovered, since most never are.
    hovered: Option<image::Handle>,
    /// The capsule's own width, and the size of the picture that holds it at its lean.
    width: f32,
    holds: (f32, f32),
    lean: f32,
    height: f32,
}

impl Label {
    fn new(content: &str, plain_color: [u8; 4], lean: f32, height: f32) -> Self {
        let (plain, width, holds) = draw_label(content, plain_color, height, lean);
        Self {
            content: content.to_owned(),
            plain,
            hovered: None,
            width,
            holds,
            lean,
            height,
        }
    }

    fn make_hovered(&mut self) {
        if self.hovered.is_none() {
            let (hovered, ..) =
                draw_label(&self.content, LABEL_HOVER_COLOR, self.height, self.lean);
            self.hovered = Some(hovered);
        }
    }
}

/// A file, in the fan.
/// What can be done with one of its files, from its menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Open,
    /// The folder it's in, with it selected.
    ShowInFiles,
    GetInfo,
    MoveToTrash,
}

/// Shows a file in the file manager, selected in its folder, or returns false if there's no file
/// manager that does that.
pub async fn show_in_files(path: PathBuf) -> bool {
    let Some(connection) = crate::dbus::session().await else {
        return false;
    };
    connection
        .call_method(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            Some("org.freedesktop.FileManager1"),
            "ShowItems",
            &(vec![crate::places::file_uri(&path)], ""),
        )
        .await
        .inspect_err(|err| log::debug!("no file manager to show a file in: {err}"))
        .is_ok()
}

pub struct Row {
    pub path: PathBuf,
    label: Label,
    pub picture: Option<image::Handle>,
}

impl Place {
    /// How far it's leaning now: it only leans as it spreads out.
    fn turn(self) -> f32 {
        self.lean * self.out.min(1.0).powi(2)
    }

    /// How much of its capsule and name shows.
    fn shows(self) -> f32 {
        // After the rows have come apart, since a stack of glass goes dark.
        ((self.out - 0.7) / 0.3).clamp(0.0, 1.0)
    }
}

/// Where something's drawn, right now.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Place {
    center: Point,
    /// Clockwise, in radians, once it's out.
    lean: f32,
    /// How far along its way out of the tile it is, from 0 to 1, which it can go a little past.
    out: f32,
}

pub struct Fan {
    pub id: IcedId,
    pub surface: Option<WlSurface>,
    pub rows: Vec<Row>,
    /// The way to the whole folder, which is the row after the last.
    more: Label,
    more_button: Label,
    /// The middle of the tile it comes out of, and the height of its surface.
    tile_x: f32,
    height: f32,
    pub hovered: Option<usize>,
    /// The row pressed, and where.
    pub pressed: Option<(usize, Point)>,
    pub dragging: bool,
    /// How far out it is, from 0 to 1.
    out: Motion,
    closing: bool,
    /// The glass behind its capsules, and what was last sent.
    pub material: Option<material::MaterialSurface>,
    pub shapes: Vec<material::Shape>,
    pub shape_opacities: Vec<f32>,
}

impl Fan {
    pub fn new(
        id: IcedId,
        rows: Vec<(PathBuf, String, Option<image::Handle>)>,
        more_count: usize,
        tile_x: f32,
        height: f32,
        glass: bool,
    ) -> Self {
        let plain_color = if glass { LABEL_ON_GLASS } else { LABEL_COLOR };
        let lean_of = |index: usize| (LEAN_AT_START + ARC_LEAN * index as f32 / 11.0).to_radians();
        let rows: Vec<Row> = rows
            .into_iter()
            .enumerate()
            .map(|(index, (path, name, picture))| Row {
                path,
                label: Label::new(&shorten(&name), plain_color, lean_of(index), LABEL_HEIGHT),
                picture,
            })
            .collect();
        let more = if more_count == 0 {
            String::from("Open in Files")
        } else {
            format!("{more_count} More in Files")
        };
        let last = lean_of(rows.len());
        Self {
            id,
            surface: None,
            rows,
            more: Label::new(&more, plain_color, last, LABEL_HEIGHT),
            more_button: Label::new("→", plain_color, 0.0, PICTURE_SIZE),
            tile_x,
            height,
            hovered: None,
            pressed: None,
            dragging: false,
            out: {
                let mut out = Motion::at(0.0, OUT_SPRING, 0.002);
                // Otherwise it waits for its glass to show up.
                if !glass {
                    out.go_to(1.0);
                }
                out
            },
            closing: false,
            material: None,
            shapes: Vec::new(),
            shape_opacities: Vec::new(),
        }
    }

    /// How many rows fit above the dock, the way to the folder aside.
    pub fn rows_that_fit(height: f32) -> usize {
        (((height - BOTTOM_ROW - ROW_STEP) / ROW_STEP).floor() as usize).clamp(1, MAX_ROWS)
    }

    /// Sets off, now that there's glass to show it on.
    pub fn arrive(&mut self) {
        if !self.closing {
            self.out.go_to(1.0);
        }
    }

    pub fn is_animating(&self) -> bool {
        self.out.is_moving_delayed(STAGGER)
    }

    pub fn is_closing(&self) -> bool {
        self.closing
    }

    pub fn has_closed(&self) -> bool {
        self.closing && !self.out.is_moving_delayed(STAGGER)
    }

    pub fn close(&mut self) {
        self.closing = true;
        self.out.go_to_on(0.0, BACK_SPRING);
    }

    /// Follows the tile it came out of.
    pub fn set_tile(&mut self, x: f32) {
        self.tile_x = x;
    }

    /// Where the row at `index` ends up.
    fn place_of(&self, index: usize) -> Place {
        let along = index as f32 / 11.0;
        Place {
            center: Point::new(
                self.tile_x + ARC_REACH * along.powf(1.8),
                self.height - BOTTOM_ROW - index as f32 * ROW_STEP,
            ),
            lean: (LEAN_AT_START + ARC_LEAN * along).to_radians(),
            out: 1.0,
        }
    }

    /// The rectangle its glass is drawn in, which is the same size all the way out so that its
    /// textures last, and the middle of the capsule, on whole pixels of that.
    fn capsule_rect(&self, index: usize, place: Place) -> (i32, i32, i32, i32, Point) {
        let label = self.label(index);
        let (sin, cos) = place.turn().sin_cos();
        let reach = PICTURE_SIZE / 2.0 + LABEL_GAP + label.width / 2.0;
        let center_x = place.center.x - cos * reach;
        let center_y = place.center.y - sin * reach;
        let (holds_width, holds_height) = (label.holds.0 as i32, label.holds.1 as i32);
        let x = center_x.round() as i32 - holds_width / 2;
        let y = center_y.round() as i32 - holds_height / 2;
        let center = Point::new((x + holds_width / 2) as f32, (y + holds_height / 2) as f32);
        (x, y, holds_width, holds_height, center)
    }

    fn label(&self, index: usize) -> &Label {
        self.rows.get(index).map_or(&self.more, |row| &row.label)
    }

    /// Draws the hovered look of a row, the first time it's needed.
    pub fn prepare_hovered(&mut self, index: usize) {
        match self.rows.get_mut(index) {
            Some(row) => row.label.make_hovered(),
            None => {
                self.more.make_hovered();
                self.more_button.make_hovered();
            }
        }
    }

    /// The glass behind the capsules, and behind the round button to the folder, as they are now.
    pub fn material_shapes(&self) -> Vec<material::Shape> {
        let mut shapes = Vec::with_capacity(self.rows.len() + 2);
        for index in 0..=self.rows.len() {
            let place = self.place_now(index);
            let (x, y, holds_width, holds_height, _) = self.capsule_rect(index, place);
            shapes.push(material::Shape {
                rect: (x, y, holds_width, holds_height),
                radius: (LABEL_HEIGHT / 2.0) as i32,
                style: material::Style::RotatedCard([
                    self.label(index).width.round() as i32,
                    LABEL_HEIGHT as i32,
                    (place.turn().to_degrees() * 1000.0).round() as i32,
                ]),
            });
        }
        let button = self.place_now(self.rows.len());
        shapes.push(material::Shape {
            rect: (
                (button.center.x - PICTURE_SIZE / 2.0).round() as i32,
                (button.center.y - PICTURE_SIZE / 2.0).round() as i32,
                PICTURE_SIZE as i32,
                PICTURE_SIZE as i32,
            ),
            radius: (PICTURE_SIZE / 2.0) as i32,
            style: material::Style::Card,
        });
        shapes
    }

    /// How opaque each of [`Fan::material_shapes`] is: capsules fade in at their full length, as the
    /// column of files rises and spreads, like Tahoe's.
    pub fn material_opacities(&self) -> Vec<f32> {
        let mut opacities: Vec<f32> = (0..=self.rows.len())
            .map(|index| self.place_now(index).shows())
            .collect();
        opacities.push(self.place_now(self.rows.len()).shows());
        opacities
    }

    /// Where the row at `index` is now, part of the way out of the tile while it opens or closes. It
    /// rises first, and spreads into the arc as it gets there.
    fn place_now(&self, index: usize) -> Place {
        let end = self.place_of(index);
        let count = (self.rows.len() + 1) as f32;
        let out = self.out.delayed(STAGGER * index as f32 / count);
        let spread = out.max(0.0).min(1.0).powi(2);
        let start = Point::new(self.tile_x, self.height + PICTURE_SIZE / 2.0);
        Place {
            center: Point::new(
                start.x + (end.center.x - start.x) * spread,
                start.y + (end.center.y - start.y) * out,
            ),
            lean: end.lean,
            out: out.max(0.0),
        }
    }

    fn label_width(&self, index: usize) -> f32 {
        self.rows
            .get(index)
            .map_or(self.more.width, |row| row.label.width)
    }

    /// The row under this point, if any, which is the rows' own leaning shapes, not their boxes.
    pub fn row_at(&self, point: Point) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for index in 0..=self.rows.len() {
            let place = self.place_of(index);
            let (dx, dy) = (point.x - place.center.x, point.y - place.center.y);
            let (sin, cos) = place.lean.sin_cos();
            // Into the row's own coordinates, where it's level.
            let along = dx * cos + dy * sin;
            let across = -dx * sin + dy * cos;
            let left = -(PICTURE_SIZE / 2.0 + LABEL_GAP + self.label_width(index));
            if across.abs() <= ROW_STEP / 2.0
                && (left..=PICTURE_SIZE / 2.0).contains(&along)
                && best.is_none_or(|(_, closest)| across.abs() < closest)
            {
                best = Some((index, across.abs()));
            }
        }
        best.map(|(index, _)| index)
    }

    /// What a file's row offers, like a file on the desktop.
    pub fn file_menu(&self, index: usize) -> Option<Vec<menu::Entry>> {
        let path = &self.rows.get(index)?.path;
        let item = |label: &str, action: FileAction| {
            menu::Entry::item(label, Message::FanFile(action, path.clone()))
        };
        Some(vec![
            item("Open", FileAction::Open),
            item("Show in Files", FileAction::ShowInFiles),
            menu::Entry::Separator,
            item("Get Info", FileAction::GetInfo).icon(menu::Icon::Glyph(Glyph::Info)),
            menu::Entry::Separator,
            item("Move to Trash", FileAction::MoveToTrash),
        ])
    }

    /// Whether this row is a file, rather than the way to the folder.
    pub fn is_file(&self, index: usize) -> bool {
        index < self.rows.len()
    }

    /// Whether the pointer has moved far enough for the press to be a drag.
    pub fn should_drag(&self, point: Point) -> bool {
        self.pressed.is_some_and(|(index, from)| {
            self.is_file(index)
                && !self.dragging
                && (point.x - from.x).hypot(point.y - from.y) > DRAG_DISTANCE
        })
    }

    pub fn view(&self) -> Element<'_> {
        let mut layers: Vec<Element<'_>> = vec![
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        ];
        for index in 0..=self.rows.len() {
            let place = self.place_now(index);
            if place.out <= 0.0 {
                continue;
            }
            let hovered = self.hovered == Some(index);
            let opacity = place.shows();
            let rotation = Rotation::Floating(Radians(place.turn()));
            let label = self.label(index);

            let (.., capsule) = self.capsule_rect(index, place);
            let plain_or_hovered = |label: &Label| match (&label.hovered, hovered) {
                (Some(drawn), true) => drawn.clone(),
                _ => label.plain.clone(),
            };
            if opacity > 0.0 {
                layers.push(
                    pin(image(plain_or_hovered(label))
                        .width(label.holds.0)
                        .height(label.holds.1)
                        .content_fit(ContentFit::Contain)
                        .rotation(rotation)
                        .opacity(opacity))
                    .x(capsule.x - label.holds.0 / 2.0)
                    .y(capsule.y - label.holds.1 / 2.0)
                    .into(),
                );
            }

            let is_file = self.is_file(index);
            let picture = if is_file {
                self.rows.get(index).and_then(|row| row.picture.clone())
            } else {
                Some(plain_or_hovered(&self.more_button))
            };
            if let Some(picture) = picture {
                let size = if is_file {
                    PICTURE_SIZE
                } else {
                    self.more_button.holds.0
                };
                layers.push(
                    pin(image(picture)
                        .width(size)
                        .height(size)
                        .content_fit(ContentFit::Contain)
                        .border_radius(if is_file { PICTURE_RADIUS } else { 0.0 })
                        .rotation(rotation))
                    .x(place.center.x - size / 2.0)
                    .y(place.center.y - size / 2.0)
                    .into(),
                );
            }
        }
        mouse_area(
            iced_widget::Stack::with_children(layers)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_move(Message::FanMoved)
        .on_press(Message::FanPressed)
        .on_release(Message::FanReleased)
        .on_right_press(Message::FanRightPressed)
        .into()
    }
}

/// A name that fits in a capsule, shortened in the middle, so that its start and extension stay.
fn shorten(name: &str) -> String {
    if menu::text_width(name, LABEL_TEXT, fonts::REGULAR) <= LABEL_MAX_WIDTH {
        return name.to_owned();
    }
    let chars: Vec<char> = name.chars().collect();
    let mut keep = chars.len().min(70);
    while keep > 8 {
        let half = keep / 2;
        let start: String = chars[..half].iter().collect();
        let end: String = chars[chars.len() - half..].iter().collect();
        let short = format!("{start}…{end}");
        if menu::text_width(&short, LABEL_TEXT, fonts::REGULAR) <= LABEL_MAX_WIDTH {
            return short;
        }
        keep -= 2;
    }
    menu::fit(name, LABEL_TEXT, fonts::REGULAR, LABEL_MAX_WIDTH)
}

/// Draws words in a capsule of this color, in a picture with room for it to lean. Returns the picture,
/// the capsule's width, and the size of the picture, all in logical pixels.
fn draw_label(
    content: &str,
    color: [u8; 4],
    height: f32,
    lean: f32,
) -> (image::Handle, f32, (f32, f32)) {
    let mut system = font_system().write().expect("text system");
    let fonts = system.raw();
    let size = LABEL_TEXT * LABEL_SCALE;
    let line = size * 1.25;
    let mut buffer = cosmic_text::Buffer::new(fonts, cosmic_text::Metrics::new(size, line));
    buffer.set_size(fonts, None, None);
    buffer.set_text(
        fonts,
        content,
        &to_attributes(fonts::REGULAR),
        to_shaping(iced_widget::core::text::Shaping::Advanced, content),
        None,
    );
    buffer.shape_until_scroll(fonts, false);
    let words = buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0_f32, f32::max);

    let width = if content.chars().count() == 1 {
        height
    } else {
        (words / LABEL_SCALE + 2.0 * LABEL_PADDING).ceil()
    };
    let (sin, cos) = lean.sin_cos();
    // Even, so that its middle stays on a whole pixel, and a little over, to hold the antialiasing.
    let even = |size: f32| ((size / 2.0).ceil() + 1.0) * 2.0;
    // Big enough for it both level, which is how it's drawn, and leaning, which is how it's shown. A
    // long one takes up less room across once it leans.
    let holds = (
        even((width * cos + height * sin).max(width)),
        even((width * sin + height * cos).max(height)),
    );

    let canvas_width = (holds.0 * LABEL_SCALE) as u32;
    let canvas_height = (holds.1 * LABEL_SCALE) as u32;
    let capsule_width = (width * LABEL_SCALE) as u32;
    let capsule_height = (height * LABEL_SCALE) as u32;
    let left = (canvas_width - capsule_width) / 2;
    let top = (canvas_height - capsule_height) / 2;
    let mut pixels = vec![0_u8; (canvas_width * canvas_height * 4) as usize];
    let body = capsule(capsule_width, capsule_height, color);
    for row in 0..capsule_height {
        let from = (row * capsule_width * 4) as usize;
        let to = (((row + top) * canvas_width + left) * 4) as usize;
        let length = (capsule_width * 4) as usize;
        pixels[to..to + length].copy_from_slice(&body[from..from + length]);
    }

    let x = (canvas_width as f32 / 2.0 - words / 2.0) as i32;
    let y = ((canvas_height as f32 - line) / 2.0) as i32;
    let mut swash = cosmic_text::SwashCache::new();
    let ink = cosmic_text::Color::rgb(255, 255, 255);
    buffer.draw(fonts, &mut swash, ink, |glyph_x, glyph_y, w, h, glyph| {
        for gy in 0..h as i32 {
            for gx in 0..w as i32 {
                blend(
                    &mut pixels,
                    canvas_width,
                    canvas_height,
                    x + glyph_x + gx,
                    y + glyph_y + gy,
                    glyph.a(),
                );
            }
        }
    });
    (
        image::Handle::from_rgba(canvas_width, canvas_height, pixels),
        width,
        holds,
    )
}

/// A capsule filling a picture of this size, with a faint rim, in straight alpha.
fn capsule(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let radius = height as f32 / 2.0;
    let rim = LABEL_SCALE;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5 - radius;
            let dx = (radius - px).max(px - (width as f32 - radius)).max(0.0);
            let distance = dx.hypot(py) - radius;
            let coverage = (0.5 - distance).clamp(0.0, 1.0);
            let in_rim = (distance + rim).clamp(0.0, 1.0);
            // Glass has an edge of its own.
            let lift = if color[3] == 0 { 0.0 } else { 0.14 * in_rim };
            for channel in 0..3 {
                let base = f32::from(color[channel]);
                pixels.push((base + (255.0 - base) * lift) as u8);
            }
            let alpha = f32::from(color[3]) / 255.0;
            let alpha = alpha + (1.0 - alpha) * lift;
            pixels.push((alpha * coverage * 255.0) as u8);
        }
    }
    pixels
}

/// White, at this coverage, over a picture in straight alpha.
fn blend(pixels: &mut [u8], width: u32, height: u32, x: i32, y: i32, coverage: u8) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 || coverage == 0 {
        return;
    }
    let at = (y as usize * width as usize + x as usize) * 4;
    let top = f32::from(coverage) / 255.0;
    let under = f32::from(pixels[at + 3]) / 255.0;
    let alpha = top + under * (1.0 - top);
    for channel in 0..3 {
        let base = f32::from(pixels[at + channel]);
        pixels[at + channel] = ((255.0 * top + base * under * (1.0 - top)) / alpha) as u8;
    }
    pixels[at + 3] = (alpha * 255.0) as u8;
}

/// As wide as the screen, up to the dock, so that a click anywhere else closes it and the dock still
/// works.
pub fn settings(screen_height: f32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Left | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((0, surface_height(screen_height) as u32)),
        exclusive_zone: Some(-1),
        keyboard_interactivity: KeyboardInteractivity::None,
        namespace: Some(String::from("dock")),
        ..Default::default()
    }
}

pub fn surface_height(screen_height: f32) -> f32 {
    screen_height - dock::reserved_height()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fan(rows: usize) -> Fan {
        let rows = (0..rows)
            .map(|index| {
                (
                    PathBuf::from(format!("/{index}")),
                    format!("file {index}"),
                    None,
                )
            })
            .collect();
        Fan::new(IcedId::unique(), rows, 0, 500.0, 800.0, true)
    }

    #[test]
    fn it_arcs_right_and_leans_more_as_it_rises() {
        let fan = fan(10);
        let (low, high) = (fan.place_of(0), fan.place_of(11));
        assert!(high.center.y < low.center.y);
        assert!(high.center.x > low.center.x);
        assert!(high.lean > low.lean);
        assert!((high.lean.to_degrees() - 14.0).abs() < 0.01);
    }

    #[test]
    fn a_labels_picture_keeps_its_capsule_in_the_middle_whatever_its_lean() {
        // Both are even, which is what keeps the middle on a whole pixel.
        let long = "a very long name of a file, the kind that gets downloaded all the time.png";
        for content in ["a name", long, "→"] {
            for lean in [0.0_f32, 0.02, 0.1, 0.25] {
                let (_, width, holds) = draw_label(content, LABEL_COLOR, LABEL_HEIGHT, lean);
                assert_eq!(holds.0 % 2.0, 0.0);
                assert_eq!(holds.1 % 2.0, 0.0);
                let (sin, cos) = lean.sin_cos();
                assert!(holds.0 >= width && holds.0 >= width * cos + LABEL_HEIGHT * sin);
                assert!(holds.1 >= LABEL_HEIGHT && holds.1 >= width * sin + LABEL_HEIGHT * cos);
            }
        }
    }

    #[test]
    fn rows_start_small_and_end_in_place() {
        let mut fan = fan(5);
        // It waits for its glass.
        assert_eq!(fan.place_now(2).out, 0.0);
        fan.arrive();
        fan.out = fan.out.started_ago(3.0);
        let done = fan.place_now(2);
        assert!((done.out - 1.0).abs() < 0.001);
        assert_eq!(done.center, fan.place_of(2).center);
        assert!(
            fan.material_opacities()
                .iter()
                .all(|opacity| *opacity == 1.0)
        );
        fan.close();
        fan.out = fan.out.started_ago(3.0);
        assert!(fan.has_closed());
        assert_eq!(fan.place_now(2).out, 0.0);
        assert!(
            fan.material_opacities()
                .iter()
                .all(|opacity| *opacity == 0.0)
        );
    }

    #[test]
    fn finds_a_row_along_its_lean() {
        let fan = fan(10);
        let top = fan.place_of(9);
        assert_eq!(fan.row_at(top.center), Some(9));
        // Out along the label, which rises to the left: straight left of the picture would be
        // under it, in another row's way.
        let (sin, cos) = top.lean.sin_cos();
        let along_label = Point::new(top.center.x - cos * 40.0, top.center.y - sin * 40.0);
        assert_eq!(fan.row_at(along_label), Some(9));
        // The last row, past the files, is the way to the folder.
        let last = fan.place_of(10);
        assert_eq!(fan.row_at(last.center), Some(10));
        assert!(!fan.is_file(10));
        assert!(fan.is_file(9));
    }

    #[test]
    fn its_glass_holds_each_capsule_and_stays_one_size_on_the_way_out() {
        let mut fan = fan(10);
        fan.arrive();
        fan.out = fan.out.started_ago(3.0);
        let shapes = fan.material_shapes();
        assert_eq!(shapes.len(), 12);
        for (index, shape) in shapes.iter().take(11).enumerate() {
            let material::Style::RotatedCard([width, height, millidegrees]) = shape.style else {
                panic!("not at an angle");
            };
            let lean = (millidegrees as f32 / 1000.0).to_radians();
            let (sin, cos) = lean.sin_cos();
            let holds_width = width as f32 * cos + height as f32 * sin;
            let holds_height = width as f32 * sin + height as f32 * cos;
            assert!(shape.rect.2 as f32 >= holds_width, "row {index} is cut off");
            assert!(
                shape.rect.3 as f32 >= holds_height,
                "row {index} is cut off"
            );
            // Halfway out, it's the same size.
            let (.., held_width, held_height, _) = fan.capsule_rect(
                index,
                Place {
                    center: Point::new(3.3, 7.7),
                    ..fan.place_of(index)
                },
            );
            assert_eq!((held_width, held_height), (shape.rect.2, shape.rect.3));
        }
    }

    #[test]
    fn nothing_is_hit_away_from_the_rows() {
        let fan = fan(3);
        assert_eq!(fan.row_at(Point::new(1400.0, 300.0)), None);
        assert_eq!(fan.row_at(Point::new(500.0, 10.0)), None);
    }

    #[test]
    fn a_press_only_becomes_a_drag_on_a_file_after_moving() {
        let mut fan = fan(2);
        let at = fan.place_of(0).center;
        fan.pressed = Some((0, at));
        assert!(!fan.should_drag(at));
        assert!(fan.should_drag(Point::new(at.x + 10.0, at.y)));
        fan.pressed = Some((2, at));
        assert!(!fan.should_drag(Point::new(at.x + 10.0, at.y)));
    }

    #[test]
    fn fits_rows_above_the_dock() {
        assert_eq!(Fan::rows_that_fit(1200.0), MAX_ROWS);
        assert_eq!(Fan::rows_that_fit(300.0), 2);
        assert_eq!(Fan::rows_that_fit(50.0), 1);
    }

    #[test]
    fn capsules_are_round_and_filled() {
        let (width, height) = (60, 20);
        let pixels = capsule(width, height, [10, 20, 30, 200]);
        let alpha = |x: u32, y: u32| pixels[((y * width + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert!(alpha(30, 10) >= 200);
    }
}
