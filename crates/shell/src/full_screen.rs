//! What the top of the screen does while the focused window is full screen, as on macOS: the menu
//! bar is out of the way until the pointer touches the top, and then slides down with the window's
//! traffic lights in it, after the logo, which close the window or take it out of full screen.

use std::time::{Duration, Instant};

use iced_layershell::reexport::{
    Anchor, IcedId, KeyboardInteractivity, Layer, NewLayerShellSettings,
};
use iced_widget::core::{Alignment, Background, Border, Color, Length};
use iced_widget::{Space, button, container, mouse_area, row};

use crate::anim::{Motion, Spring};
use crate::app::{Element, Message};
use crate::icons::{self, Glyph};
use crate::menubar;

/// The strip along the top that the pointer touches to bring the bar down: as little of the window
/// as still catches a pointer pushed against the top.
const TRIGGER_HEIGHT: u32 = 2;
/// How long after the pointer leaves the bar it goes back up.
pub const HIDE_AFTER: Duration = Duration::from_millis(400);
/// Down, and back up, without overshooting: past the top there'd be a gap above the bar.
const SPRING: Spring = Spring::new(0.28, 1.0);

const LIGHT_SIZE: f32 = 12.0;
const LIGHT_SPACING: f32 = 8.0;
/// Around the lights, split between the logo's side and the app name's.
const LIGHTS_GAP: f32 = 12.0;
/// How much room the lights take in the menu bar, between the logo and the app's name.
pub const LIGHTS_WIDTH: f32 = 3.0 * LIGHT_SIZE + 2.0 * LIGHT_SPACING + LIGHTS_GAP;

/// The full-screen window, and the menu bar that comes down over it.
pub struct FullScreen {
    /// The window, by the shell's id for it.
    pub window: u64,
    pub trigger: IcedId,
    /// 0 out of sight, 1 all the way down.
    pub shown: Motion,
    /// Which of the menu bar and the strip along the top the pointer's on. Going from one to the
    /// other, the shell can hear it arrive before it hears it leave.
    on: Vec<IcedId>,
    /// When the pointer last left the bar, to put it away a little after.
    pub left_at: Option<Instant>,
    pub lights_hovered: bool,
    /// How far down the bar was last put, in whole pixels, to only move it when that changes.
    pub placed: Option<i32>,
}

impl FullScreen {
    pub fn new(window: u64, trigger: IcedId) -> Self {
        Self {
            window,
            trigger,
            shown: Motion::at(0.0, SPRING, 0.002),
            on: Vec::new(),
            left_at: None,
            lights_hovered: false,
            placed: None,
        }
    }

    /// The pointer came onto the bar or the strip, or moved on it.
    pub fn pointer_on(&mut self, id: IcedId) {
        if !self.on.contains(&id) {
            self.on.push(id);
        }
        self.left_at = None;
    }

    /// The pointer left the bar or the strip.
    pub fn pointer_off(&mut self, id: IcedId) {
        self.on.retain(|on| *on != id);
        if self.on.is_empty() {
            self.left_at = Some(Instant::now());
        }
    }

    pub fn is_hovered(&self) -> bool {
        !self.on.is_empty()
    }

    pub fn is_moving(&self) -> bool {
        self.shown.is_moving()
    }

    /// Whether it's down, or on the way.
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
        let hidden = menubar::HEIGHT as f32;
        -((1.0 - self.shown.value().clamp(0.0, 1.0)) * hidden).round() as i32
    }

    /// Whether it's time it went back up.
    pub fn should_hide(&self) -> bool {
        !self.is_hovered() && self.left_at.is_some_and(|at| at.elapsed() >= HIDE_AFTER)
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

/// The traffic lights in the menu bar, after the logo: close, minimize, which full screen has no use
/// for so it's off like macOS's, and leaving full screen.
pub fn lights<'a>(hovered: bool) -> Element<'a> {
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
    container(
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
        .on_exit(Message::FullScreenLightsHovered(false)),
    )
    .center_x(LIGHTS_WIDTH)
    .center_y(Length::Fill)
    .into()
}
