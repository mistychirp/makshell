//! What the top of the screen does while the focused window is full screen, as on macOS: the menu
//! bar is out of the way until the pointer touches the top, and then slides down with a title bar
//! under it, whose traffic lights close the window or take it out of full screen.

use std::time::{Duration, Instant};

use iced_layershell::reexport::{
    Anchor, IcedId, KeyboardInteractivity, Layer, NewLayerShellSettings,
};
use iced_widget::core::{Alignment, Background, Border, Color, Length};
use iced_widget::{Space, button, container, mouse_area, row, stack, text};

use crate::anim::{Motion, Spring};
use crate::app::{Element, Message};
use crate::icons::{self, Glyph};
use crate::material::{self, Shape};
use crate::menubar;

/// The title bar under the menu bar.
pub const TITLE_HEIGHT: f32 = 32.0;
/// The strip along the top that the pointer touches to bring them down: as little of the window as
/// still catches a pointer pushed against the top.
const TRIGGER_HEIGHT: u32 = 2;
/// How long after the pointer leaves them they go back up.
pub const HIDE_AFTER: Duration = Duration::from_millis(400);
/// Down, and back up, without overshooting: past the top there'd be a gap above the bar.
const SPRING: Spring = Spring::new(0.28, 1.0);

const LIGHT_SIZE: f32 = 12.0;
const LIGHT_SPACING: f32 = 8.0;
const LIGHTS_LEFT: f32 = 12.0;

/// The full-screen window, and the bars that come down over it.
pub struct FullScreen {
    /// The window, by the shell's id for it.
    pub window: u64,
    pub trigger: IcedId,
    pub title_bar: IcedId,
    pub title_material: Option<material::MaterialSurface>,
    /// Width of the title bar; 0 until it's configured.
    pub title_width: f32,
    /// 0 out of sight, 1 all the way down.
    pub shown: Motion,
    /// The pointer is on the menu bar, the title bar or the strip along the top.
    pub hovered: bool,
    /// When the pointer last left them, to put them away a little after.
    pub left_at: Option<Instant>,
    pub lights_hovered: bool,
    /// How far down the bars were last put, in whole pixels, to only move them when that changes.
    pub placed: Option<i32>,
}

impl FullScreen {
    pub fn new(window: u64, trigger: IcedId, title_bar: IcedId) -> Self {
        Self {
            window,
            trigger,
            title_bar,
            title_material: None,
            title_width: 0.0,
            shown: Motion::at(0.0, SPRING, 0.002),
            hovered: false,
            left_at: None,
            lights_hovered: false,
            placed: None,
        }
    }

    pub fn is_moving(&self) -> bool {
        self.shown.is_moving()
    }

    /// Whether they're down, or on the way.
    pub fn is_shown(&self) -> bool {
        self.shown.target() > 0.5
    }

    pub fn show(&mut self) {
        self.left_at = None;
        self.shown.go_to(1.0);
    }

    pub fn hide(&mut self) {
        self.shown.go_to(0.0);
        self.lights_hovered = false;
    }

    /// Where the menu bar's top is: above the screen, by as much as is out of sight.
    pub fn offset(&self) -> i32 {
        let hidden = menubar::HEIGHT as f32 + TITLE_HEIGHT;
        -((1.0 - self.shown.value().clamp(0.0, 1.0)) * hidden).round() as i32
    }

    /// Whether it's time they went back up.
    pub fn should_hide(&self) -> bool {
        !self.hovered && self.left_at.is_some_and(|at| at.elapsed() >= HIDE_AFTER)
    }
}

/// The strip along the top of the screen, which only feels for the pointer.
pub fn trigger_settings() -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Left | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((0, TRIGGER_HEIGHT)),
        exclusive_zone: Some(-1),
        keyboard_interactivity: KeyboardInteractivity::None,
        namespace: Some(String::from("dock")),
        ..Default::default()
    }
}

/// The title bar, out of sight under where the menu bar is.
pub fn title_settings() -> NewLayerShellSettings {
    let hidden = (menubar::HEIGHT as f32 + TITLE_HEIGHT) as i32;
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Left | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((0, TITLE_HEIGHT as u32)),
        exclusive_zone: Some(-1),
        margin: Some((menubar::HEIGHT as i32 - hidden, 0, 0, 0)),
        keyboard_interactivity: KeyboardInteractivity::None,
        namespace: Some(String::from("dock")),
        ..Default::default()
    }
}

/// Frosted, like the menu bar over it, with a line between the title bar and the window.
pub fn title_shapes(width: f32) -> Vec<Shape> {
    vec![Shape {
        rect: (0, 0, width.ceil() as i32, TITLE_HEIGHT as i32),
        radius: 0,
        style: material::Style::Frosted,
    }]
}

/// The title bar: the traffic lights, and the window's title in the middle.
pub fn title_view<'a>(title: &'a str, lights_hovered: bool) -> Element<'a> {
    let title = container(
        text(title)
            .size(13)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.85)),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill);
    let lights = container(lights(lights_hovered))
        .padding([0.0, LIGHTS_LEFT])
        .center_y(Length::Fill);
    stack![title, lights]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Close, minimize, which full screen has no use for so it's off like macOS's, and leaving full
/// screen.
fn lights<'a>(hovered: bool) -> Element<'a> {
    let dot = |color: Color, mark: Option<Element<'a>>, on_press: Option<Message>| {
        button(
            container(mark.unwrap_or_else(|| Space::new().into()))
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
        .width(LIGHT_SIZE)
        .height(LIGHT_SIZE)
        .padding(0)
        .on_press_maybe(on_press)
        .style(move |_, _| button::Style {
            background: Some(Background::Color(color)),
            border: Border {
                radius: (LIGHT_SIZE / 2.0).into(),
                ..Default::default()
            },
            ..Default::default()
        })
    };
    let ink = Color::from_rgba(0.0, 0.0, 0.0, 0.6);
    let close_mark = hovered.then(|| icons::icon(Glyph::Close, 9.0, ink));
    let exit_mark = hovered.then(|| icons::icon(Glyph::ExitFullScreen, 10.0, ink));
    mouse_area(
        row![
            dot(
                Color::from_rgb8(255, 92, 93),
                close_mark,
                Some(Message::FullScreenClose)
            ),
            dot(Color::from_rgba(1.0, 1.0, 1.0, 0.17), None, None),
            dot(
                Color::from_rgb8(40, 200, 64),
                exit_mark,
                Some(Message::FullScreenExit)
            ),
        ]
        .spacing(LIGHT_SPACING)
        .align_y(Alignment::Center),
    )
    .on_enter(Message::FullScreenLightsHovered(true))
    .on_exit(Message::FullScreenLightsHovered(false))
    .into()
}
