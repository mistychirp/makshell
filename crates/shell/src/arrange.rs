//! The menu of the green traffic light, which arranges a window: Move & Resize, Fill & Arrange and
//! Full Screen, measured from Tahoe's. Hovering the light tells the shell, from the window decoration
//! in KWin's process, and the choice goes back to our KWin effect, which is what can move windows.

use iced_futures::Subscription;
use iced_futures::futures::channel::mpsc;
use iced_futures::futures::{SinkExt as _, Stream, StreamExt as _, future};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings};
use iced_widget::core::{Alignment, Background, Border, Color, Length, Padding};
use iced_widget::{Space, button, column, container, mouse_area, pin, row, stack, svg, text};

use crate::app::{Element, Message};
use crate::{dbus, fonts, material};

const SERVICE: &str = "org.makshell.Shell";
const PATH: &str = "/org/makshell/Windows";

const WIDTH: f32 = 239.0;
const HEIGHT: f32 = 193.0;
const TAIL_HEIGHT: f32 = 10.0;
const TAIL_WIDTH: f32 = 22.0;
/// From the panel's left edge to the light its tail points at. Its base has to clear the corner, or
/// the two make a bump in the outline.
const TAIL_CENTER: f32 = 36.0;
const RADIUS: f32 = 18.0;
const PADDING_X: f32 = 15.0;
const TILE_WIDTH: f32 = 25.0;
const TILE_HEIGHT: f32 = 19.0;
const TILE_PADDING: f32 = 5.0;
/// From one tile's left edge to the next's.
const TILE_STEP: f32 = 54.5;
const FIRST_TILE: f32 = 36.0 - TILE_WIDTH / 2.0;
// From the panel's top.
const MOVE_TITLE: f32 = 18.0;
const MOVE_TILES: f32 = 51.0;
const FIRST_LINE: f32 = 78.0;
const FILL_TITLE: f32 = 97.0;
const FILL_TILES: f32 = 130.0;
const SECOND_LINE: f32 = 157.0;
const FULL_ROW: f32 = 175.0;
const ROW_HEIGHT: f32 = 24.0;
const ROW_INSET: f32 = 6.0;
/// Where a row's picture starts, and how big its words are, measured from Tahoe's.
const ROW_PADDING: f32 = 12.5;
const ROW_TEXT: f32 = 14.0;
const SMALL_PICTURE: (f32, f32) = (14.0, 11.0);
const SUB_WIDTH: f32 = 152.0;
const SUB_HEIGHT: f32 = 80.0;
const SUB_RADIUS: f32 = 16.0;
const SUB_ROW: f32 = 24.0;
const SUB_OVERLAP: f32 = 4.0;
/// How far the submenu's top is above the middle of the row it comes from.
const SUB_ABOVE: f32 = 17.0;
const SIZE: (f32, f32) = (
    WIDTH + SUB_WIDTH - SUB_OVERLAP,
    TAIL_HEIGHT + HEIGHT + SUB_HEIGHT - SUB_ABOVE,
);

/// Where a window can go. These are the names our KWin effect knows them by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Left,
    Right,
    Top,
    Bottom,
    Fill,
    Two,
    Three,
    Four,
    FullScreen,
}

impl Layout {
    fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Fill => "fill",
            Self::Two => "two",
            Self::Three => "three",
            Self::Four => "four",
            Self::FullScreen => "fullscreen",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The pointer has rested on a green light: where its middle and bottom are in its window.
    Hovered(i32, i32),
    /// The pointer left the green light.
    Left,
    /// The Settings app changed the config file.
    ConfigChanged,
    /// The Settings app asked for the clipboard's history to be forgotten.
    ClearClipboard,
    /// Spotlight's shortcut was pressed.
    ToggleSpotlight,
    /// A volume or brightness key was pressed, by its name in KDE's shortcuts.
    MediaKey(String),
}

/// An open menu.
pub struct Open {
    pub surface: iced_layershell::reexport::IcedId,
    /// The window it's about.
    pub window: String,
    pub material: Option<material::MaterialSurface>,
    pub submenu: bool,
    pub over_menu: bool,
    pub over_light: bool,
    /// Counts the times the pointer left, so that only the latest wait closes the menu.
    pub leaves: u64,
}

struct Windows {
    events: mpsc::UnboundedSender<Event>,
}

#[zbus::interface(name = "org.makshell.Windows")]
impl Windows {
    #[zbus(name = "ZoomHovered")]
    fn zoom_hovered(&self, x: i32, y: i32) {
        let _ = self.events.unbounded_send(Event::Hovered(x, y));
    }

    #[zbus(name = "ZoomLeft")]
    fn zoom_left(&self) {
        let _ = self.events.unbounded_send(Event::Left);
    }

    /// Spotlight's shortcut was pressed, which our KWin plugin listens for.
    #[zbus(name = "ToggleSpotlight")]
    fn toggle_spotlight(&self) {
        let _ = self.events.unbounded_send(Event::ToggleSpotlight);
    }

    /// A volume or brightness key was pressed, which our KWin plugin listens for.
    #[zbus(name = "MediaKey")]
    fn media_key(&self, name: String) {
        let _ = self.events.unbounded_send(Event::MediaKey(name));
    }

    /// The Settings app changed the config file.
    #[zbus(name = "ConfigChanged")]
    fn config_changed(&self) {
        let _ = self.events.unbounded_send(Event::ConfigChanged);
    }

    /// The Settings app asked for the clipboard's history to be forgotten.
    #[zbus(name = "ClearClipboard")]
    fn clear_clipboard(&self) {
        let _ = self.events.unbounded_send(Event::ClearClipboard);
    }
}

/// Hears from window decorations for as long as the shell runs.
pub fn events() -> Subscription<Event> {
    Subscription::run(serve)
}

fn serve() -> impl Stream<Item = Event> {
    iced_futures::stream::channel(8, |mut output: mpsc::Sender<Event>| async move {
        let Some(connection) = dbus::session().await else {
            return future::pending().await;
        };
        let (events, mut incoming) = mpsc::unbounded();
        if let Err(err) = connection
            .object_server()
            .at(PATH, Windows { events })
            .await
        {
            log::warn!("can't hear from window decorations: {err}");
            return future::pending().await;
        }
        if let Err(err) = connection.request_name(SERVICE).await {
            log::warn!("can't hear from window decorations: {err}");
        }
        while let Some(event) = incoming.next().await {
            let _ = output.send(event).await;
        }
    })
}

#[zbus::proxy(
    interface = "org.makshell.Arranger",
    default_service = "org.kde.KWin",
    default_path = "/org/makshell/Arranger"
)]
trait Arranger {
    #[zbus(name = "windowUnderCursor")]
    fn window_under_cursor(&self) -> zbus::Result<(String, i32, i32)>;
    #[zbus(name = "arrange")]
    fn arrange(&self, id: &str, layout: &str) -> zbus::Result<()>;
}

/// The window under the pointer, and where its top left corner is on the screen.
pub async fn window_under_cursor() -> Option<(String, i32, i32)> {
    let connection = dbus::session().await?;
    let answer = async {
        ArrangerProxy::new(&connection)
            .await?
            .window_under_cursor()
            .await
    }
    .await;
    answer
        .inspect_err(|err| log::warn!("can't ask which window that is: {err}"))
        .ok()
        .filter(|(id, ..)| !id.is_empty())
}

pub async fn arrange(window: String, layout: Layout) {
    let Some(connection) = dbus::session().await else {
        return;
    };
    let result = async {
        ArrangerProxy::new(&connection)
            .await?
            .arrange(&window, layout.name())
            .await
    }
    .await;
    if let Err(err) = result {
        log::warn!("can't arrange a window: {err}");
    }
}

#[zbus::proxy(
    interface = "org.makshell.Genie",
    default_service = "org.kde.KWin",
    default_path = "/org/makshell/Genie"
)]
trait Genie {
    #[zbus(name = "setTarget")]
    fn set_target(&self, id: &str, x: i32, y: i32, width: i32, height: i32) -> zbus::Result<()>;
    #[zbus(name = "skip")]
    fn skip(&self, id: &str) -> zbus::Result<()>;
}

/// Tells the genie to leave these windows alone when they go and come back.
pub async fn skip_genie(ids: Vec<String>) {
    let Some(connection) = dbus::session().await else {
        return;
    };
    let result = async {
        let genie = GenieProxy::new(&connection).await?;
        for id in &ids {
            genie.skip(id).await?;
        }
        zbus::Result::Ok(())
    }
    .await;
    if let Err(err) = result {
        log::debug!("can't tell the genie to skip a window: {err}");
    }
}

/// Tells the genie where things go in the dock, on the screen: each window's id with its place, and
/// an empty id for where the next one will go.
pub async fn set_genie_targets(targets: Vec<(String, (i32, i32, i32, i32))>) {
    let Some(connection) = dbus::session().await else {
        return;
    };
    let result = async {
        let genie = GenieProxy::new(&connection).await?;
        for (id, (x, y, width, height)) in &targets {
            genie.set_target(id, *x, *y, *width, *height).await?;
        }
        zbus::Result::Ok(())
    }
    .await;
    // Not an error if the effect isn't there, which is only worth knowing when debugging.
    if let Err(err) = result {
        log::debug!("can't tell the genie where things go: {err}");
    }
}

pub fn settings(x: i32, y: i32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Left,
        layer: Layer::Overlay,
        size: Some((SIZE.0 as u32, SIZE.1 as u32)),
        exclusive_zone: Some(-1),
        // Its tail's tip is at the light.
        margin: Some((y, 0, 0, (x - TAIL_CENTER as i32).max(0))),
        keyboard_interactivity: KeyboardInteractivity::None,
        namespace: Some(String::from("dock")),
        ..Default::default()
    }
}

fn sub_rect() -> (i32, i32, i32, i32) {
    (
        (WIDTH - SUB_OVERLAP) as i32,
        (TAIL_HEIGHT + FULL_ROW - SUB_ABOVE) as i32,
        SUB_WIDTH as i32,
        SUB_HEIGHT as i32,
    )
}

pub fn material_shapes(submenu: bool) -> Vec<material::Shape> {
    let mut shapes = vec![material::Shape {
        rect: (0, 0, WIDTH as i32, (TAIL_HEIGHT + HEIGHT) as i32),
        radius: RADIUS as i32,
        style: material::Style::FrostedWithTopTail([
            TAIL_CENTER as i32,
            TAIL_WIDTH as i32,
            TAIL_HEIGHT as i32,
        ]),
    }];
    if submenu {
        shapes.push(material::Shape {
            rect: sub_rect(),
            radius: SUB_RADIUS as i32,
            style: material::Style::Frosted,
        });
    }
    shapes
}

/// Where it takes the pointer: on what's showing, so that the rest of the window can be used.
pub fn input_region(submenu: bool) -> Vec<(i32, i32, i32, i32)> {
    let mut region = vec![(0, 0, WIDTH as i32, (TAIL_HEIGHT + HEIGHT) as i32)];
    if submenu {
        region.push(sub_rect());
    }
    region
}

/// A little picture of a screen, as big as a tile. `parts` are rectangles out of 100 by 100, with how
/// bright they are.
fn tile_picture<'a>(parts: &[(f32, f32, f32, f32, f32)]) -> Element<'a> {
    screen_picture(parts, (TILE_WIDTH, TILE_HEIGHT))
}

/// The picture for a row: an empty screen split down the middle, for full screen, or one part of it.
fn row_picture<'a>(parts: &[(f32, f32, f32, f32, f32)]) -> Element<'a> {
    screen_picture(parts, SMALL_PICTURE)
}

fn screen_picture<'a>(
    parts: &[(f32, f32, f32, f32, f32)],
    (shown_width, shown_height): (f32, f32),
) -> Element<'a> {
    let (width, height) = (shown_width * 2.0, shown_height * 2.0);
    let (inner_x, inner_y) = (5.0, 5.0);
    let (inner_width, inner_height) = (width - 2.0 * inner_x, height - 2.0 * inner_y);
    let mut rects = String::new();
    for &(x, y, w, h, brightness) in parts {
        rects.push_str(&format!(
            r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="1.5" fill="#fff" fill-opacity="{brightness}"/>"##,
            inner_x + x / 100.0 * inner_width,
            inner_y + y / 100.0 * inner_height,
            w / 100.0 * inner_width,
            h / 100.0 * inner_height,
        ));
    }
    let source = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}">{rects}<rect x="1.5" y="1.5" width="{}" height="{}" rx="6" fill="none" stroke="#fff" stroke-opacity="0.9" stroke-width="3"/></svg>"##,
        width - 3.0,
        height - 3.0,
    );
    svg(svg::Handle::from_memory(source.into_bytes()))
        .width(shown_width)
        .height(shown_height)
        .into()
}

fn tile<'a>(layout: Layout) -> Element<'a> {
    const ON: f32 = 0.85;
    const OFF: f32 = 0.28;
    let parts: &[(f32, f32, f32, f32, f32)] = match layout {
        Layout::Left => &[(0.0, 0.0, 47.0, 100.0, ON)],
        Layout::Right => &[(53.0, 0.0, 47.0, 100.0, ON)],
        Layout::Top => &[(0.0, 0.0, 100.0, 45.0, ON)],
        Layout::Bottom => &[(0.0, 55.0, 100.0, 45.0, ON)],
        Layout::Fill => &[(0.0, 0.0, 100.0, 100.0, ON)],
        Layout::Two => &[(0.0, 0.0, 47.0, 100.0, ON), (53.0, 0.0, 47.0, 100.0, OFF)],
        Layout::Three => &[
            (0.0, 0.0, 47.0, 100.0, ON),
            (53.0, 0.0, 47.0, 45.0, OFF),
            (53.0, 55.0, 47.0, 45.0, OFF),
        ],
        _ => &[
            (0.0, 0.0, 47.0, 45.0, ON),
            (53.0, 0.0, 47.0, 45.0, ON),
            (0.0, 55.0, 47.0, 45.0, ON),
            (53.0, 55.0, 47.0, 45.0, ON),
        ],
    };
    button(tile_picture(parts))
        .on_press(Message::ArrangeChosen(layout))
        .padding(TILE_PADDING)
        .style(|_, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                .then_some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.15))),
            border: Border {
                radius: 7.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn tile_row<'a>(layouts: [Layout; 4]) -> Element<'a> {
    let step = TILE_STEP - (TILE_WIDTH + 2.0 * TILE_PADDING);
    row(layouts.into_iter().map(tile)).spacing(step).into()
}

fn heading<'a>(words: &'a str) -> Element<'a> {
    text(words)
        .size(12.0)
        .font(fonts::BOLD)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.42))
        .into()
}

fn separator<'a>() -> Element<'a> {
    container(Space::new())
        .width(WIDTH - 2.0 * PADDING_X)
        .height(1.0)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.13))),
            ..Default::default()
        })
        .into()
}

fn sub_row<'a>(parts: &[(f32, f32, f32, f32, f32)], words: &'a str, layout: Layout) -> Element<'a> {
    button(
        row![row_picture(parts), text(words).size(ROW_TEXT)]
            .spacing(9)
            .align_y(Alignment::Center),
    )
    .on_press(Message::ArrangeChosen(layout))
    .width(Length::Fill)
    .height(SUB_ROW)
    .padding(Padding {
        top: 3.0,
        bottom: 3.0,
        left: 10.0,
        right: 6.0,
    })
    .style(|_, status| button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(Background::Color(crate::appearance::accent())),
        text_color: Color::WHITE,
        border: Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

pub fn view<'a>(submenu: bool) -> Element<'a> {
    let at = |x: f32, y: f32, content: Element<'a>| -> Element<'a> {
        pin(content).x(x).y(TAIL_HEIGHT + y).into()
    };
    let tile_x = FIRST_TILE - TILE_PADDING;
    let tile_y = |center: f32| center - TILE_HEIGHT / 2.0 - TILE_PADDING;

    let full_row = mouse_area(
        container(
            row![
                row_picture(&[(44.0, 0.0, 12.0, 100.0, 0.9)]),
                text("Full Screen").size(ROW_TEXT),
                Space::new().width(Length::Fill),
                text("›").size(19.0).font(fonts::BOLD),
            ]
            .spacing(9)
            .align_y(Alignment::Center),
        )
        .padding([0.0, ROW_PADDING - ROW_INSET])
        .width(WIDTH - 2.0 * ROW_INSET)
        .height(ROW_HEIGHT)
        .center_y(ROW_HEIGHT)
        .style(move |_| container::Style {
            background: submenu.then_some(Background::Color(crate::appearance::accent())),
            border: Border {
                radius: 10.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .on_enter(Message::ArrangeSubmenu(true))
    .on_press(Message::ArrangeChosen(Layout::FullScreen));

    let mut layers = vec![
        at(PADDING_X, MOVE_TITLE - 8.0, heading("Move & Resize")),
        at(
            tile_x,
            tile_y(MOVE_TILES),
            tile_row([Layout::Left, Layout::Right, Layout::Top, Layout::Bottom]),
        ),
        // Pointing at these rather than Full Screen puts the submenu away.
        at(
            PADDING_X,
            FIRST_LINE,
            mouse_area(separator())
                .on_enter(Message::ArrangeSubmenu(false))
                .into(),
        ),
        at(PADDING_X, FILL_TITLE - 8.0, heading("Fill & Arrange")),
        at(
            tile_x,
            tile_y(FILL_TILES),
            tile_row([Layout::Fill, Layout::Two, Layout::Three, Layout::Four]),
        ),
        at(
            PADDING_X,
            SECOND_LINE,
            mouse_area(separator())
                .on_enter(Message::ArrangeSubmenu(false))
                .into(),
        ),
        at(ROW_INSET, FULL_ROW - ROW_HEIGHT / 2.0, full_row.into()),
    ];
    if submenu {
        let (x, y, ..) = sub_rect();
        layers.push(
            pin(container(column![
                sub_row(
                    &[(0.0, 0.0, 100.0, 100.0, 0.85)],
                    "Entire Screen",
                    Layout::FullScreen
                ),
                sub_row(
                    &[(0.0, 0.0, 47.0, 100.0, 0.85)],
                    "Left of Screen",
                    Layout::Left
                ),
                sub_row(
                    &[(53.0, 0.0, 47.0, 100.0, 0.85)],
                    "Right of Screen",
                    Layout::Right
                ),
            ])
            .padding(4)
            .width(SUB_WIDTH)
            .height(SUB_HEIGHT))
            .x(x as f32)
            .y(y as f32)
            .into(),
        );
    }
    mouse_area(
        container(stack(layers))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                text_color: Some(Color::WHITE),
                ..Default::default()
            }),
    )
    .on_enter(Message::ArrangeOverMenu(true))
    .on_exit(Message::ArrangeOverMenu(false))
    .into()
}
