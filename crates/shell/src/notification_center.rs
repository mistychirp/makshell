//! Notification Center, which hangs from the clock: past notifications with no panel behind
//! them, grouped by app. A group shows as one card with the edge of the next peeking out from
//! under it, until it's opened.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced_widget::core::mouse::ScrollDelta;
use iced_widget::core::text::{Shaping, Wrapping};
use iced_widget::core::{Alignment, Color, Length};
use iced_widget::graphics::text::cosmic_text::{self, Attrs, Buffer, Metrics, SwashCache, Weight};
use iced_widget::{Space, container, image, mouse_area, pin, row, stack, text};

use crate::group::group;

use crate::anim::{self, Motion, Spring};
use crate::app::{Element, Message};
use crate::notifications::{
    CARD_WIDTH, CHIP_INK, CLOSE_SIZE, HoverFade, Notification, OVERHANG, RADIUS, card, chip_shape,
    close_button, close_shape, closing_face, panel_style,
};
use crate::{dock, fonts, material, menu, menubar};

/// Wider than the cards, for the blur behind the message when there are none to fade out in.
pub const WIDTH: f32 = 540.0;
const RIGHT_PADDING: f32 = 8.0;
const CARD_X: f32 = WIDTH - RIGHT_PADDING - CARD_WIDTH;
const GAP: f32 = 8.0;
const HEADER_HEIGHT: f32 = 26.0;
const SHOW_LESS_WIDTH: f32 = 73.0;
/// How tall "Show less" is; its ✕ is a little shorter, centered beside it.
const SHOW_LESS_HEIGHT: f32 = 21.0;
const CONTROL_GAP: f32 = 8.0;
/// The edge of the group's next card, under its first.
const PEEKS: [f32; 2] = [10.0, 22.0];
const PEEK_HEIGHT: f32 = 8.0;
const EMPTY_TOP: f32 = 40.0;
/// "Edit Widgets", under everything, like Tahoe's: always there, notifications or not.
const EDIT_WIDGETS: &str = "Edit Widgets";
const EDIT_HEIGHT: f32 = 22.0;
const EDIT_PADDING: f32 = 11.0;
/// How far under "No recent notifications" it goes when there are none.
const EDIT_UNDER_EMPTY: f32 = 36.0;
const TITLE_SIZE: f32 = 15.0;
const PANEL_TITLE_SIZE: f32 = 17.0;
const PANEL_TITLE_HEIGHT: f32 = 24.0;
/// The height of a line of text, in text sizes: what the text widget uses unless told otherwise.
const LINE_HEIGHT: f32 = 1.3;
const SHADOW_REACH: f32 = 8.0;
/// Pixels per logical pixel of a shadow. It's soft, so this needn't follow the screen.
const SHADOW_SCALE: f32 = 2.0;
const SHADOW_BLUR_RADIUS: usize = 5;
const SHADOW_STRENGTH: f32 = 0.9;
/// How far down the blur goes, like Tahoe's, which only blurs behind this message.
const BACKDROP_HEIGHT: f32 = 231.0;
/// How far to scroll to move by a row: about a notch of a mouse wheel.
const PIXELS_PER_ROW: f32 = 25.0;

/// How long a closed card takes to go, measured from Tahoe's, and how far it rises as it does.
const CLOSING_TIME: Duration = Duration::from_millis(110);
const CLOSING_RISE: f32 = 40.0;
/// How small it gets by then.
const CLOSING_SCALE: f32 = 0.5;
/// How long the rest of a group takes to go under its first card when it closes. It's what opening
/// one takes, run backwards, with nothing to go by but how Tahoe opens one and closes a single card.
const FOLD_TIME: Duration = ARRIVAL_TIME;
const FOLD_SCALE: f32 = 0.9;
/// How long a row that's new here takes to show, when a group opens.
const ARRIVAL_TIME: Duration = Duration::from_millis(180);

/// How rows move as others come and go, measured from Tahoe's.
const ROW_SPRING: Spring = Spring::new(0.285, 0.92);

/// How rows get to where they should be, as others come and go, and the cards on their way out.
#[derive(Default)]
pub struct Motions {
    rows: HashMap<String, RowMotion>,
    closing: Vec<Closing>,
    /// The rest of a group on its way under its first card, which are drawn under everything.
    folding: Vec<Closing>,
    folding_headers: Vec<FoldingHeader>,
    going: bool,
}

struct RowMotion {
    y: Motion,
    /// When it arrived, unless it was there from the start.
    arrived: Option<Instant>,
    /// How far the edges of the rest of its group are out from under it, from 0 to 1.
    peeks: Motion,
}

struct Closing {
    notification: Notification,
    time: String,
    y: f32,
    to: f32,
    started: Instant,
    takes: Duration,
    scale: f32,
}

impl Closing {
    fn t(&self) -> f32 {
        (anim::seconds_since(self.started) / self.takes.as_secs_f32()).min(1.0)
    }

    fn y(&self, t: f32) -> f32 {
        self.y + (self.to - self.y) * t
    }

    fn scale(&self, t: f32) -> f32 {
        1.0 - (1.0 - self.scale) * t
    }

    /// Its glass, `t` of the way through going.
    fn shape(&self, t: f32) -> material::Shape {
        let scale = self.scale(t);
        let whole = self.notification.height();
        let (width, height) = (CARD_WIDTH * scale, whole * scale);
        card_shape(
            CARD_X + (CARD_WIDTH - width) / 2.0,
            self.y(t) + (whole - height) / 2.0,
            width,
            height,
            RADIUS * scale,
        )
    }

    fn layer<'a>(&'a self, t: f32, material: Option<material::Kind>) -> Element<'a> {
        pin(closing_face(
            &self.notification,
            Some(self.time.clone()),
            t,
            self.scale(t),
            material,
        ))
        .x(CARD_X)
        .y(self.y(t))
        .into()
    }
}

/// The controls of a group that's closing, fading where they are.
struct FoldingHeader {
    app_key: String,
    app: String,
    y: f32,
    started: Instant,
}

impl FoldingHeader {
    fn t(&self) -> f32 {
        (anim::seconds_since(self.started) / FOLD_TIME.as_secs_f32()).min(1.0)
    }
}

impl Motions {
    /// Sends everything toward what's there now.
    pub fn sync(
        &mut self,
        notifications: &[Notification],
        open: &HashSet<String>,
        first: usize,
        height: f32,
    ) {
        let from_start = !std::mem::replace(&mut self.going, true);
        let targets: Vec<(String, f32, bool)> = placed(notifications, open, first, height)
            .into_iter()
            .map(|(row, y)| {
                let peeks = matches!(
                    row,
                    Row::Card {
                        closed_group: true,
                        ..
                    }
                );
                (row.key(), y, peeks)
            })
            .collect();
        let mut above: Option<f32> = None;
        for (key, target, peeks) in &targets {
            let peeks = if *peeks { 1.0 } else { 0.0 };
            let row = self.rows.entry(key.clone()).or_insert_with(|| {
                // A new one comes out from under the one above it.
                let start = if from_start {
                    *target
                } else {
                    above.unwrap_or(*target)
                };
                RowMotion {
                    y: Motion::at(start, ROW_SPRING, 0.25),
                    arrived: (!from_start).then(anim::now),
                    peeks: Motion::at(peeks, ROW_SPRING, 0.01),
                }
            });
            row.y.go_to(*target);
            row.peeks.go_to(peeks);
            if row
                .arrived
                .is_some_and(|arrived| anim::seconds_since(arrived) >= ARRIVAL_TIME.as_secs_f32())
            {
                row.arrived = None;
            }
            above = Some(row.y.value());
        }
        self.rows
            .retain(|key, _| targets.iter().any(|(target, _, _)| target == key));
        self.closing.retain(|closing| closing.t() < 1.0);
        self.folding.retain(|closing| closing.t() < 1.0);
        self.folding_headers.retain(|header| header.t() < 1.0);
    }

    /// A card was closed: it goes from where it is.
    pub fn close(&mut self, notification: &Notification) {
        if let Some(row) = self.rows.get(&card_key(notification.id)) {
            self.closing.push(Closing {
                notification: notification.clone(),
                time: notification.time_label(),
                y: row.y.value(),
                to: row.y.value() - CLOSING_RISE,
                started: anim::now(),
                takes: CLOSING_TIME,
                scale: CLOSING_SCALE,
            });
        }
    }

    /// An open group is closing: all but its first card go under that one, to where it's headed,
    /// which is where its header is now.
    pub fn fold(&mut self, notifications: &[Notification], app_key: &str) {
        let header_key = format!("header {app_key}");
        let Some(header) = self.rows.get(&header_key) else {
            return;
        };
        let (header_y, to) = (header.y.value(), header.y.target());
        let group: Vec<&Notification> = notifications
            .iter()
            .filter(|notification| notification.app_key == app_key)
            .collect();
        if let Some(first) = group.first() {
            self.folding_headers.push(FoldingHeader {
                app_key: app_key.to_owned(),
                app: first.app.clone(),
                y: header_y,
                started: anim::now(),
            });
        }
        for notification in group.into_iter().skip(1) {
            if let Some(row) = self.rows.get(&card_key(notification.id)) {
                self.folding.push(Closing {
                    notification: notification.clone(),
                    time: notification.time_label(),
                    y: row.y.value(),
                    to,
                    started: anim::now(),
                    takes: FOLD_TIME,
                    scale: FOLD_SCALE,
                });
            }
        }
    }

    pub fn is_animating(&self) -> bool {
        !self.closing.is_empty()
            || !self.folding.is_empty()
            || !self.folding_headers.is_empty()
            || self
                .rows
                .values()
                .any(|row| row.arrived.is_some() || row.y.is_moving() || row.peeks.is_moving())
    }

    fn y(&self, key: &str, target: f32) -> f32 {
        self.rows.get(key).map_or(target, |row| row.y.value())
    }

    /// How far the edges of the rest of a card's group are out from under it.
    fn peeks(&self, key: &str) -> f32 {
        self.rows
            .get(key)
            .map_or(0.0, |row| row.peeks.value().clamp(0.0, 1.0))
    }

    fn shows(&self, key: &str) -> f32 {
        self.rows
            .get(key)
            .and_then(|row| row.arrived)
            .map_or(1.0, |arrived| {
                (anim::seconds_since(arrived) / ARRIVAL_TIME.as_secs_f32()).min(1.0)
            })
    }
}

fn card_key(id: u32) -> String {
    format!("card {id}")
}

fn card_shape(x: f32, y: f32, width: f32, height: f32, radius: f32) -> material::Shape {
    material::Shape {
        rect: (x as i32, y as i32, width as i32, height as i32),
        radius: radius as i32,
        style: material::Style::Card,
    }
}

/// Where the edge of the group's next card is, for how far out it is: from behind the card above.
fn peek_y(index: usize, card_bottom: f32, peeks: f32) -> f32 {
    card_bottom + 1.0 + index as f32 * PEEK_HEIGHT
        - (1.0 - peeks) * (index + 1) as f32 * PEEK_HEIGHT
}

/// The glass of a group's "Show less" and ✕.
fn header_shapes(y: f32) -> [material::Shape; 2] {
    let close_x = CARD_X + CARD_WIDTH - CLOSE_SIZE;
    [
        chip_shape(
            close_x - CONTROL_GAP - SHOW_LESS_WIDTH,
            y,
            SHOW_LESS_WIDTH,
            SHOW_LESS_HEIGHT,
        ),
        close_shape(close_x, y + (SHOW_LESS_HEIGHT - CLOSE_SIZE) / 2.0),
    ]
}

/// The height of Notification Center's surface on a screen this tall: down to the dock. Any taller
/// and the compositor slides it up over the bar to keep it out of the dock's reserved area.
pub fn height(screen_height: f32) -> f32 {
    (screen_height - menubar::HEIGHT as f32 - dock::reserved_height() - 16.0).max(100.0)
}

/// What can be scrolled past.
#[derive(Debug, Clone, Copy)]
enum Row<'a> {
    /// The panel's own title, with the ✕ that clears everything.
    Title,
    Header {
        app_key: &'a str,
        app: &'a str,
    },
    Card {
        notification: &'a Notification,
        /// It stands for a closed group of more than one.
        closed_group: bool,
    },
    /// The button that opens the widget gallery, after everything else.
    EditWidgets,
}

impl Row<'_> {
    /// What tells it from the others, wherever it moves to.
    fn key(&self) -> String {
        match self {
            Row::Title => String::from("title"),
            Row::Header { app_key, .. } => format!("header {app_key}"),
            Row::Card { notification, .. } => card_key(notification.id),
            Row::EditWidgets => String::from("edit widgets"),
        }
    }

    fn height(&self) -> f32 {
        match self {
            Row::Title => PANEL_TITLE_HEIGHT,
            Row::Header { .. } => HEADER_HEIGHT,
            Row::EditWidgets => EDIT_HEIGHT,
            Row::Card {
                notification,
                closed_group,
            } => {
                notification.target_height()
                    + if *closed_group {
                        PEEKS.len() as f32 * PEEK_HEIGHT
                    } else {
                        0.0
                    }
            }
        }
    }
}

/// The rows to show, most recent app first, with each group's most recent notification first.
fn rows<'a>(notifications: &'a [Notification], open: &HashSet<String>) -> Vec<Row<'a>> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    if !notifications.is_empty() {
        rows.push(Row::Title);
    }
    for first in notifications {
        if !seen.insert(first.app_key.as_str()) {
            continue;
        }
        let group: Vec<&Notification> = notifications
            .iter()
            .filter(|notification| notification.app_key == first.app_key)
            .collect();
        if group.len() > 1 && open.contains(&first.app_key) {
            rows.push(Row::Header {
                app_key: &first.app_key,
                app: &first.app,
            });
            rows.extend(group.into_iter().map(|notification| Row::Card {
                notification,
                closed_group: false,
            }));
        } else {
            rows.push(Row::Card {
                notification: first,
                closed_group: group.len() > 1,
            });
        }
    }
    rows.push(Row::EditWidgets);
    rows
}

fn edit_width() -> f32 {
    (menu::text_width(EDIT_WIDGETS, 12.0, fonts::REGULAR) + 2.0 * EDIT_PADDING).ceil()
}

/// Where "Edit Widgets" starts: in the middle of the cards' column.
fn edit_x() -> f32 {
    (CARD_X + (CARD_WIDTH - edit_width()) / 2.0).round()
}

/// The rows that fit, whole, from row `first` down, with where each goes. Scrolling moves by whole
/// rows: glass can't be clipped at the surface's edge, so a row is either shown or not.
fn placed<'a>(
    notifications: &'a [Notification],
    open: &HashSet<String>,
    first: usize,
    height: f32,
) -> Vec<(Row<'a>, f32)> {
    // With no notifications, there's a line saying so first.
    let mut y = if notifications.is_empty() {
        EMPTY_TOP + EDIT_UNDER_EMPTY
    } else {
        OVERHANG
    };
    let mut placed = Vec::new();
    for row in rows(notifications, open).into_iter().skip(first) {
        if y + row.height() > height {
            break;
        }
        placed.push((row, y));
        y += row.height() + GAP;
    }
    placed
}

/// How many rows there are to scroll through.
pub fn row_count(notifications: &[Notification], open: &HashSet<String>) -> usize {
    rows(notifications, open).len()
}

/// The glass, with how opaque each shape is.
#[allow(clippy::too_many_arguments)]
pub fn material_shapes(
    notifications: &[Notification],
    open: &HashSet<String>,
    first: usize,
    hovered: Option<u32>,
    height: f32,
    motion: &Motions,
    fade: &HoverFade,
) -> (Vec<material::Shape>, Vec<f32>) {
    let shape = card_shape;
    let mut shapes = Vec::new();
    let mut opacities = Vec::new();
    if notifications.is_empty() {
        shapes.push(material::Shape {
            rect: (0, 0, WIDTH as i32, BACKDROP_HEIGHT as i32),
            radius: 0,
            style: material::Style::Backdrop([190, 30, 100, 110]),
        });
        opacities.push(1.0);
    }
    for closing in &motion.folding {
        let t = closing.t();
        shapes.push(closing.shape(t));
        opacities.push(1.0 - t);
    }
    for header in &motion.folding_headers {
        let t = header.t();
        for chip in header_shapes(header.y) {
            shapes.push(chip);
            opacities.push(1.0 - t);
        }
    }
    for (row, y) in placed(notifications, open, first, height) {
        let key = row.key();
        let shows = motion.shows(&key);
        let y = motion.y(&key, y);
        let mut add = |new: material::Shape, opacity: f32| {
            shapes.push(new);
            opacities.push(opacity);
        };
        match row {
            Row::Title => {
                add(close_shape(CARD_X + CARD_WIDTH - CLOSE_SIZE, y), shows);
            }
            Row::Header { .. } => {
                for chip in header_shapes(y) {
                    add(chip, shows);
                }
            }
            Row::EditWidgets => {
                add(chip_shape(edit_x(), y, edit_width(), EDIT_HEIGHT), shows);
            }
            Row::Card {
                notification,
                closed_group,
            } => {
                let card_height = notification.height();
                // Under the card, which they come out from.
                let peeks = motion.peeks(&key);
                if peeks > 0.0 {
                    for (index, inset) in PEEKS.into_iter().enumerate() {
                        add(
                            shape(
                                CARD_X + inset,
                                peek_y(index, y + card_height, peeks),
                                CARD_WIDTH - 2.0 * inset,
                                PEEK_HEIGHT - 1.0,
                                (PEEK_HEIGHT - 1.0) / 2.0,
                            ),
                            shows * peeks,
                        );
                    }
                }
                add(shape(CARD_X, y, CARD_WIDTH, card_height, RADIUS), shows);
                // On top of the card's, so after it.
                let capsule = notification.capsule_shows(fade.amount(notification.id));
                if !closed_group
                    && capsule > 0.0
                    && let Some(rect) = notification.capsule_rect()
                {
                    add(
                        chip_shape(CARD_X + rect.x, y + rect.y, rect.width, rect.height),
                        capsule * shows,
                    );
                }
                if hovered == Some(notification.id) {
                    add(close_shape(CARD_X - OVERHANG, y - OVERHANG), shows);
                }
            }
        }
    }
    for closing in &motion.closing {
        let t = closing.t();
        shapes.push(closing.shape(t));
        opacities.push(1.0 - t);
    }
    (shapes, opacities)
}

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    notifications: &'a [Notification],
    open: &HashSet<String>,
    first: usize,
    hovered: Option<u32>,
    height: f32,
    motion: &'a Motions,
    fade: &HoverFade,
    material: Option<material::Kind>,
) -> Element<'a> {
    let mut layers: Vec<Element<'a>> = Vec::new();
    if notifications.is_empty() {
        let message = "No recent notifications";
        let x = CARD_X + (CARD_WIDTH - menu::text_width(message, TITLE_SIZE, fonts::BOLD)) / 2.0;
        layers.push(
            pin(title(message, TITLE_SIZE))
                .x(x - SHADOW_REACH)
                .y(EMPTY_TOP - SHADOW_REACH)
                .into(),
        );
    }

    for closing in &motion.folding {
        layers.push(closing.layer(closing.t(), material));
    }
    for folding in &motion.folding_headers {
        let shows = 1.0 - folding.t();
        layers.push(
            pin(group(header(&folding.app_key, material)).opacity(shows))
                .x(CARD_X)
                .y(folding.y)
                .into(),
        );
        layers.push(
            pin(group(title(&folding.app, TITLE_SIZE)).opacity(shows))
                .x(CARD_X - SHADOW_REACH)
                .y(folding.y - SHADOW_REACH)
                .into(),
        );
    }

    for (row, y) in placed(notifications, open, first, height) {
        let key = row.key();
        let shows = motion.shows(&key);
        let y = motion.y(&key, y);
        match row {
            Row::Title => {
                layers.push(
                    pin(title("Notification Center", PANEL_TITLE_SIZE))
                        .x(CARD_X - SHADOW_REACH)
                        .y(y - SHADOW_REACH - 2.0)
                        .into(),
                );
                layers.push(
                    pin(close_button(Message::NotificationsCleared, material))
                        .x(CARD_X + CARD_WIDTH - CLOSE_SIZE)
                        .y(y)
                        .into(),
                );
            }
            Row::Header { app_key, app } => {
                layers.push(
                    pin(group(header(app_key, material)).opacity(shows))
                        .x(CARD_X)
                        .y(y)
                        .into(),
                );
                layers.push(
                    pin(group(title(app, TITLE_SIZE)).opacity(shows))
                        .x(CARD_X - SHADOW_REACH)
                        .y(y - SHADOW_REACH)
                        .into(),
                );
            }
            Row::EditWidgets => {
                let button = mouse_area(
                    container(text(EDIT_WIDGETS).size(12).color(CHIP_INK))
                        .center_x(edit_width())
                        .center_y(EDIT_HEIGHT)
                        .style(move |_| panel_style(material, EDIT_HEIGHT / 2.0)),
                )
                .on_press(Message::NotificationCenterEditWidgets);
                layers.push(pin(group(button).opacity(shows)).x(edit_x()).y(y).into());
            }
            Row::Card {
                notification,
                closed_group,
            } => {
                let id = notification.id;
                let (on_press, on_close) = if closed_group {
                    (
                        Message::NotificationGroupOpened(notification.app_key.clone()),
                        Message::NotificationGroupCleared(notification.app_key.clone()),
                    )
                } else {
                    (
                        Message::NotificationPressed(id),
                        Message::NotificationDismissed(id),
                    )
                };
                let peeks = motion.peeks(&key);
                if peeks > 0.0 {
                    for (index, inset) in PEEKS.into_iter().enumerate() {
                        layers.push(
                            pin(group(
                                container(Space::new())
                                    .width(CARD_WIDTH - 2.0 * inset)
                                    .height(PEEK_HEIGHT - 1.0)
                                    .style(move |_| {
                                        panel_style(material, (PEEK_HEIGHT - 1.0) / 2.0)
                                    }),
                            )
                            .opacity(shows * peeks))
                            .x(CARD_X + inset)
                            .y(peek_y(index, y + notification.height(), peeks))
                            .into(),
                        );
                    }
                }
                layers.push(
                    pin(group(card(
                        notification,
                        Some(notification.time_label()),
                        hovered == Some(id),
                        if closed_group { 0.0 } else { fade.amount(id) },
                        (CARD_X, y),
                        material,
                        on_press,
                        on_close,
                    ))
                    .opacity(shows))
                    .x(CARD_X - OVERHANG)
                    .y(y - OVERHANG)
                    .into(),
                );
            }
        }
    }
    for closing in &motion.closing {
        layers.push(closing.layer(closing.t(), material));
    }

    // Scrolling anywhere works; presses on the empty part fall through and close the panel.
    mouse_area(
        container(stack(layers))
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_scroll(|delta| {
        Message::NotificationCenterScrolled(match delta {
            // A wheel notch comes as both; touchpads only send pixels.
            ScrollDelta::Lines { .. } => 0.0,
            ScrollDelta::Pixels { y, .. } => y / PIXELS_PER_ROW,
        })
    })
    .into()
}

/// A title directly on the desktop, with Tahoe's soft shadow, which keeps it readable on a bright
/// wallpaper. It starts [`SHADOW_REACH`] up and to the left of the text.
fn title(content: &str, size: f32) -> Element<'_> {
    let width = menu::text_width(content, size, fonts::BOLD) + 2.0 * SHADOW_REACH;
    let height = LINE_HEIGHT * size + 2.0 * SHADOW_REACH;
    let shadow = SHADOWS.with_borrow_mut(|shadows| {
        shadows
            .entry(content.to_owned())
            .or_insert_with(|| text_shadow(content, size, width, height))
            .clone()
    });
    stack![
        image(shadow).width(width).height(height),
        pin(text(content)
            .size(size)
            .font(fonts::BOLD)
            .color(Color::WHITE)
            .shaping(Shaping::Advanced)
            .wrapping(Wrapping::None))
        .x(SHADOW_REACH)
        .y(SHADOW_REACH),
    ]
    .width(width)
    .height(height)
    .into()
}

thread_local! {
    /// Titles' shadows, which are slow to make and slower to upload again each frame.
    static SHADOWS: RefCell<HashMap<String, image::Handle>> = RefCell::default();
}

/// The shadow of a title, as an image of this size in logical pixels: its glyphs, drawn by the same
/// text engine that draws the text, and blurred, since a renderer can't blur text.
fn text_shadow(content: &str, size: f32, width: f32, height: f32) -> image::Handle {
    let (pixel_width, pixel_height) = (
        (width * SHADOW_SCALE).ceil() as usize,
        (height * SHADOW_SCALE).ceil() as usize,
    );
    let mut coverage = vec![0.0_f32; pixel_width * pixel_height];

    let mut system = iced_widget::graphics::text::font_system()
        .write()
        .expect("text system");
    let fonts = system.raw();
    let metrics = Metrics::new(size * SHADOW_SCALE, LINE_HEIGHT * size * SHADOW_SCALE);
    let mut buffer = Buffer::new(fonts, metrics);
    buffer.set_size(fonts, None, None);
    buffer.set_text(
        fonts,
        content,
        &Attrs::new()
            .family(cosmic_text::Family::Name("Inter"))
            .weight(Weight::SEMIBOLD),
        cosmic_text::Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(fonts, false);
    let offset = (SHADOW_REACH * SHADOW_SCALE) as i32;
    buffer.draw(
        fonts,
        &mut SwashCache::new(),
        cosmic_text::Color::rgb(255, 255, 255),
        |x, y, w, h, color| {
            for py in (y + offset)..(y + offset + h as i32) {
                for px in (x + offset)..(x + offset + w as i32) {
                    if px >= 0
                        && py >= 0
                        && (px as usize) < pixel_width
                        && let Some(value) =
                            coverage.get_mut(py as usize * pixel_width + px as usize)
                    {
                        *value = value.max(f32::from(color.a()) / 255.0);
                    }
                }
            }
        },
    );

    // Three box blurs come close to a Gaussian.
    for _ in 0..3 {
        box_blur(&mut coverage, pixel_width, pixel_height);
    }
    let pixels: Vec<u8> = coverage
        .iter()
        .flat_map(|value| [0, 0, 0, ((value * SHADOW_STRENGTH).min(1.0) * 255.0) as u8])
        .collect();
    image::Handle::from_rgba(pixel_width as u32, pixel_height as u32, pixels)
}

/// Blurs along rows, then along columns.
fn box_blur(values: &mut [f32], width: usize, height: usize) {
    let radius = SHADOW_BLUR_RADIUS;
    let blur_line = |get: &dyn Fn(usize) -> f32, length: usize, out: &mut Vec<f32>| {
        out.clear();
        out.extend((0..length).map(|center| {
            let first = center.saturating_sub(radius);
            let last = (center + radius).min(length - 1);
            (first..=last).map(get).sum::<f32>() / (2 * radius + 1) as f32
        }));
    };
    let mut line = Vec::new();
    for y in 0..height {
        let row: Vec<f32> = values[y * width..(y + 1) * width].to_vec();
        blur_line(&|x| row[x], width, &mut line);
        values[y * width..(y + 1) * width].copy_from_slice(&line);
    }
    for x in 0..width {
        let column: Vec<f32> = (0..height).map(|y| values[y * width + x]).collect();
        blur_line(&|y| column[y], height, &mut line);
        for (y, value) in line.iter().enumerate() {
            values[y * width + x] = *value;
        }
    }
}

/// An open group's "Show less", and the ✕ that clears it.
fn header<'a>(app_key: &str, material: Option<material::Kind>) -> Element<'a> {
    let show_less = mouse_area(
        container(text("Show less").size(12).color(CHIP_INK))
            .center_x(SHOW_LESS_WIDTH)
            .center_y(SHOW_LESS_HEIGHT)
            .style(move |_| panel_style(material, SHOW_LESS_HEIGHT / 2.0)),
    )
    .on_press(Message::NotificationGroupClosed(app_key.to_owned()));

    row![
        Space::new().width(Length::Fill),
        show_less,
        close_button(
            Message::NotificationGroupCleared(app_key.to_owned()),
            material
        ),
    ]
    .spacing(CONTROL_GAP)
    .width(CARD_WIDTH)
    .height(SHOW_LESS_HEIGHT)
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod motion_tests {
    use super::*;

    #[test]
    fn the_edges_of_a_group_come_out_from_behind_what_they_peek_from() {
        // All the way in, each is behind the card, or the edge above it.
        assert!(peek_y(0, 100.0, 0.0) < 100.0);
        assert!(peek_y(1, 100.0, 0.0) < 100.0);
        // All the way out, they're where they stay, in order.
        assert_eq!(peek_y(0, 100.0, 1.0), 101.0);
        assert_eq!(peek_y(1, 100.0, 1.0), 101.0 + PEEK_HEIGHT);
    }

    #[test]
    fn nothing_moves_when_nothing_is_there() {
        let mut motion = Motions::default();
        motion.sync(&[], &HashSet::new(), 0, 500.0);
        assert!(!motion.is_animating());
    }
}
