//! The material behind shell surfaces: liquid glass (makshell-glass-v1, from our KWin plugin) when
//! the compositor offers it, otherwise background blur (ext-background-effect-v1).
//!
//! The shell shares its Wayland connection with iced_layershell, so the `wl_surface` behind an
//! iced window (reached with `window::run`) works with the protocol objects bound here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::task::Poll;

use iced_futures::Subscription;
use iced_futures::futures::channel::mpsc;
use iced_futures::futures::{SinkExt as _, Stream, StreamExt as _};

use raw_window_handle::RawWindowHandle;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::{
    self, ExtBackgroundEffectManagerV1,
};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

use crate::glass_protocol::makshell_glass_manager_v1::MakshellGlassManagerV1;
use crate::glass_protocol::makshell_glass_v1::{self, MakshellGlassV1};
use crate::shadow_protocol::org_kde_kwin_shadow::OrgKdeKwinShadow;
use crate::shadow_protocol::org_kde_kwin_shadow_manager::OrgKdeKwinShadowManager;
use crate::window_shadow;

/// A rectangle in surface-local logical coordinates: `(x, y, width, height)`.
pub type Rect = (i32, i32, i32, i32);

/// Like macOS's Liquid Glass variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// A refracting rim over a sharp background.
    #[allow(dead_code)]
    Clear,
    /// A flat blurred background with crisp edge lines, macOS's "regular" glass. For menus,
    /// popovers and alerts, whose text needs a calm background.
    Frosted,
    /// A light blur through a lens, darkened a little. For controls floating over content, like
    /// the dock.
    Liquid,
    /// A dark tint over a lens, for small labels like the dock's.
    Bubble,
    /// A grey veil over a lens that mirrors the far side at the edge, inside a rim that catches the
    /// light, for Control Center's modules. It doesn't blur, so it needs a backdrop under it.
    Control,
    /// Frosted, with a tail pointing down from its bottom, like a dock menu's: where its tip is from
    /// the left, how wide it is and how tall. Plain frosted where that isn't supported.
    FrostedWithTail([i32; 3]),
    /// Like [`Style::FrostedWithTail`], with the tail on top, pointing up. Plain frosted where that
    /// isn't supported.
    FrostedWithTopTail([i32; 3]),
    /// Frosted for dialogs, and lighter when the dialog isn't active. Plain frosted where that
    /// isn't supported.
    Window { active: bool },
    /// A menu's tint over the dock's lens, for notifications.
    Card,
    /// A very wide blur under a light grey tint, inside an even rim, for small buttons.
    Chip,
    /// A blur that fades out instead of ending at an edge, to set things apart from a busy
    /// background. It fades in over this many logical pixels from the left, top, right and bottom.
    /// Dropped where that isn't supported.
    Backdrop([u32; 4]),
    /// Control glass in a rounded rectangle that can move around inside `rect`, which stays the same
    /// so that nothing's set up again every frame: where its middle is from the left and top of
    /// `rect`, its width and height, and its corner radius, all in thousandths of a logical pixel.
    /// Dropped where that isn't supported.
    ControlBox([i32; 5]),
    /// Card glass the same way, with circles melted into it like drops of liquid: where the box's
    /// middle is, its width, height and corner radius, and for each circle where it is, its radius
    /// (0 for none) and how far it reaches for the box, all in thousandths of a logical pixel. Dropped
    /// where that isn't supported.
    CardBlob([i32; 5], [[i32; 4]; 4]),
    /// Like [`Style::Card`], at an angle: a shape this wide and tall, turned clockwise by this many
    /// thousandths of a degree about the middle of `rect`, which has to hold it. Dropped where that
    /// isn't supported.
    RotatedCard([i32; 3]),
    /// Nothing at all, like macOS's menu bar; the shape only says where to measure brightness
    /// (see [`Material::track_brightness`]). Dropped where that isn't supported.
    Transparent,
}

/// How a surface comes and goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appear {
    /// Fades in almost at once, and fades out like [`Appear::Fade`].
    Quick,
    /// Fades in and out, like Control Center.
    Fade,
    /// There at once, and fades out quickly, like a menu.
    Pop,
    /// Grows out of a point as it fades in, and goes at once, like an alert.
    Grow(i32, i32),
    /// Slides in from the right, like Notification Center.
    Slide,
}

/// A rounded rectangle of material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub rect: Rect,
    pub radius: i32,
    pub style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Glass,
    Blur,
}

/// How opaque the dark fill a panel draws for itself should be, so its text stays legible. Glass
/// tints under its edge lines, so a fill on top would only dim them.
pub fn panel_tint(material: Option<Kind>) -> f32 {
    match material {
        Some(Kind::Glass) => 0.0,
        Some(Kind::Blur) => 0.45,
        None => 0.9,
    }
}

/// The border a panel draws for itself: glass draws its own edge lines, but blur or nothing leaves
/// the panel needing one.
pub fn panel_border(material: Option<Kind>, radius: f32) -> iced_widget::core::Border {
    let width = if material == Some(Kind::Glass) {
        0.0
    } else {
        1.0
    };
    iced_widget::core::Border {
        color: iced_widget::core::Color::from_rgba(1.0, 1.0, 1.0, 0.12),
        width,
        radius: radius.into(),
    }
}

pub struct Material {
    connection: Connection,
    compositor: WlCompositor,
    manager: Manager,
    queue: QueueHandle<Globals>,
    /// Where events for our objects land; iced_layershell reads the shared socket, and
    /// [`Material::brightness`] dispatches what it queued here.
    event_queue: RefCell<EventQueue<Globals>>,
    state: RefCell<Globals>,
    shadows: Option<(OrgKdeKwinShadowManager, WlShm)>,
    /// The shadow's pieces, once they've been drawn: for active windows and for the others.
    shadow_tiles: RefCell<[Option<window_shadow::Tiles>; 2]>,
}

enum Manager {
    Glass(MakshellGlassManagerV1),
    Blur(ExtBackgroundEffectManagerV1),
}

/// A surface with material attached.
pub struct MaterialSurface {
    surface: WlSurface,
    effect: Effect,
    shadow: RefCell<Option<OrgKdeKwinShadow>>,
    /// The parts of what the surface shows that are out of focus for now, which go out with its
    /// shapes.
    blurs: RefCell<Vec<ContentBlur>>,
}

/// Part of what a surface shows, drawn out of focus by a radius in logical pixels: how things come
/// into view on Tahoe, blurred at first and sharp a moment later.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentBlur {
    pub rect: Rect,
    pub radius: f32,
}

enum Effect {
    Glass(MakshellGlassV1),
    Blur(ExtBackgroundEffectSurfaceV1),
}

#[derive(Default)]
struct Globals {
    blur_supported: bool,
    /// The latest luminance (out of 1000) behind each glass object that tracks it.
    brightness: HashMap<ObjectId, u32>,
}

impl Material {
    /// Binds the best protocol available on `connection`. Returns `None` when there's neither.
    pub fn new(connection: &Connection) -> Option<Self> {
        let (globals, mut event_queue) = registry_queue_init::<Globals>(connection)
            .inspect_err(|err| log::warn!("can't list Wayland globals: {err}"))
            .ok()?;
        let queue = event_queue.handle();
        let compositor: WlCompositor = globals.bind(&queue, 1..=6, ()).ok()?;
        if let Ok(seat) = globals.bind::<WlSeat, _, _>(&queue, 1..=1, ()) {
            iced_layershell::toplevel::set_seat(seat);
        }

        let manager = if let Ok(glass) = globals.bind(&queue, 1..=12, ()) {
            log::info!("using liquid glass");
            Manager::Glass(glass)
        } else {
            let blur: ExtBackgroundEffectManagerV1 = globals
                .bind(&queue, 1..=1, ())
                .inspect_err(|err| log::info!("no glass or background effect support: {err}"))
                .ok()?;

            // Wait for the manager's capabilities event.
            let mut state = Globals::default();
            event_queue.roundtrip(&mut state).ok()?;
            if !state.blur_supported {
                log::info!("compositor has background effects, but not blur");
                return None;
            }
            log::info!("no liquid glass, using blur");
            Manager::Blur(blur)
        };

        let shadows = globals
            .bind(&queue, 1..=2, ())
            .ok()
            .zip(globals.bind(&queue, 1..=1, ()).ok());

        Some(Self {
            connection: connection.clone(),
            compositor,
            manager,
            queue,
            event_queue: RefCell::new(event_queue),
            state: RefCell::new(Globals::default()),
            shadows,
            shadow_tiles: RefCell::new([None, None]),
        })
    }

    pub fn kind(&self) -> Kind {
        match self.manager {
            Manager::Glass(_) => Kind::Glass,
            Manager::Blur(_) => Kind::Blur,
        }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn attach(&self, surface: WlSurface) -> MaterialSurface {
        let effect = match &self.manager {
            Manager::Glass(manager) => Effect::Glass(manager.get_glass(&surface, &self.queue, ())),
            Manager::Blur(manager) => {
                Effect::Blur(manager.get_background_effect(&surface, &self.queue, ()))
            }
        };
        MaterialSurface {
            surface,
            effect,
            shadow: RefCell::new(None),
            blurs: RefCell::new(Vec::new()),
        }
    }

    /// Asks for an animation for a surface's coming and going. Call it before setting its shapes,
    /// which is what starts it.
    pub fn set_appear(&self, target: &MaterialSurface, appear: Appear) {
        if let Effect::Glass(glass) = &target.effect
            && glass.version() >= 8
        {
            let (kind, x, y) = match appear {
                Appear::Fade => (makshell_glass_v1::Appear::Fade, 0, 0),
                Appear::Quick if glass.version() >= 11 => (makshell_glass_v1::Appear::Quick, 0, 0),
                Appear::Quick => (makshell_glass_v1::Appear::Fade, 0, 0),
                Appear::Pop => (makshell_glass_v1::Appear::Pop, 0, 0),
                Appear::Grow(x, y) if glass.version() >= 10 => {
                    (makshell_glass_v1::Appear::Grow, x, y)
                }
                Appear::Grow(..) => (makshell_glass_v1::Appear::Fade, 0, 0),
                Appear::Slide => (makshell_glass_v1::Appear::Slide, 0, 0),
            };
            glass.set_appear(kind, x, y);
        }
    }

    /// Gives a window of ours the shadow a decoration would have brought, which takes effect with
    /// the surface's next commit. Set the shapes after this.
    pub fn set_window_shadow(&self, target: &MaterialSurface, radius: f32, active: bool) {
        let Some((manager, shm)) = &self.shadows else {
            return;
        };
        let mut tiles = self.shadow_tiles.borrow_mut();
        let tiles = &mut tiles[usize::from(active)];
        if tiles.is_none() {
            *tiles = window_shadow::Tiles::new(shm, &self.queue, radius, active);
        }
        let Some(tiles) = tiles else {
            return;
        };
        let shadow = manager.create(&target.surface, &self.queue, ());
        tiles.attach(&shadow);
        if let Some(old) = target.shadow.replace(Some(shadow)) {
            old.destroy();
        }
    }

    /// Which parts of what the surface shows are out of focus. They go out with its shapes, the next
    /// time those are set or queued, so set these first. Returns whether that's a change.
    pub fn set_content_blurs(&self, target: &MaterialSurface, blurs: Vec<ContentBlur>) -> bool {
        let mut current = target.blurs.borrow_mut();
        let changed = *current != blurs;
        *current = blurs;
        changed
    }

    /// Replaces the material shapes and commits them right away.
    pub fn set_shapes(&self, target: &MaterialSurface, shapes: &[Shape]) {
        self.set_faded_shapes(target, shapes, &[]);
    }

    /// Like [`Material::set_shapes`], each shape as opaque as the same place in `opacities`, from 0
    /// to 1, or fully if it's past the end of them.
    pub fn set_faded_shapes(&self, target: &MaterialSurface, shapes: &[Shape], opacities: &[f32]) {
        self.queue_faded_shapes(target, shapes, opacities);
        // The state is double-buffered. Committing without a new buffer only applies it, and
        // between iced's frames there's no other pending state we could flush by accident.
        target.surface.commit();
        if let Err(err) = self.connection.flush() {
            log::error!("can't flush Wayland connection: {err}");
        }
    }

    /// Like [`Material::set_faded_shapes`], except that it goes out with the surface's next frame,
    /// so that the glass and what's on it move together. That takes a frame coming.
    pub fn queue_faded_shapes(
        &self,
        target: &MaterialSurface,
        shapes: &[Shape],
        opacities: &[f32],
    ) {
        match &target.effect {
            Effect::Glass(glass) => {
                glass.clear();
                for (index, shape) in shapes.iter().enumerate() {
                    let (x, y, width, height) = shape.rect;
                    let opacity = opacities.get(index).copied().unwrap_or(1.0);
                    if opacity <= 0.0 {
                        continue;
                    }
                    if let Style::CardBlob(
                        [box_x, box_y, box_width, box_height, box_radius],
                        drops,
                    ) = shape.style
                    {
                        if glass.version() >= 10 {
                            let fixed = |milli: i32| f64::from(milli) / 1000.0;
                            glass.add_box(
                                x,
                                y,
                                width,
                                height,
                                makshell_glass_v1::Style::Card,
                                fixed(box_x),
                                fixed(box_y),
                                fixed(box_width),
                                fixed(box_height),
                                fixed(box_radius),
                            );
                            for [drop_x, drop_y, radius, blend] in drops {
                                if radius > 0 {
                                    glass.add_droplet(
                                        fixed(drop_x),
                                        fixed(drop_y),
                                        fixed(radius),
                                        fixed(blend),
                                    );
                                }
                            }
                            if opacity < 1.0 {
                                glass.set_last_opacity(f64::from(opacity));
                            }
                        }
                        continue;
                    }
                    if let Style::ControlBox([box_x, box_y, box_width, box_height, box_radius]) =
                        shape.style
                    {
                        if glass.version() >= 10 {
                            let fixed = |milli: i32| f64::from(milli) / 1000.0;
                            glass.add_box(
                                x,
                                y,
                                width,
                                height,
                                makshell_glass_v1::Style::Control,
                                fixed(box_x),
                                fixed(box_y),
                                fixed(box_width),
                                fixed(box_height),
                                fixed(box_radius),
                            );
                            if opacity < 1.0 {
                                glass.set_last_opacity(f64::from(opacity));
                            }
                        }
                        continue;
                    }
                    if let Style::Backdrop([left, top, right, bottom]) = shape.style {
                        if glass.version() >= 4 {
                            glass.add_backdrop(x, y, width, height, left, top, right, bottom);
                        }
                        continue;
                    }
                    if let Style::FrostedWithTail([center, width_, height_]) = shape.style
                        && glass.version() >= 5
                    {
                        glass.add_rounded_rect_with_tail(
                            x,
                            y,
                            width,
                            height,
                            shape.radius.max(0) as u32,
                            makshell_glass_v1::Style::Frosted,
                            center,
                            width_ as u32,
                            height_ as u32,
                        );
                        continue;
                    }
                    if let Style::FrostedWithTopTail([center, width_, height_]) = shape.style
                        && glass.version() >= 7
                    {
                        glass.add_rounded_rect_with_top_tail(
                            x,
                            y,
                            width,
                            height,
                            shape.radius.max(0) as u32,
                            makshell_glass_v1::Style::Frosted,
                            center,
                            width_ as u32,
                            height_ as u32,
                        );
                        continue;
                    }
                    if let Style::RotatedCard([box_width, box_height, millidegrees]) = shape.style {
                        if glass.version() >= 9 {
                            glass.add_rotated_rect(
                                x,
                                y,
                                width,
                                height,
                                shape.radius.max(0) as u32,
                                makshell_glass_v1::Style::Card,
                                box_width as u32,
                                box_height as u32,
                                millidegrees,
                            );
                            if opacity < 1.0 && glass.version() >= 10 {
                                glass.set_last_opacity(f64::from(opacity));
                            }
                        }
                        continue;
                    }
                    let style = match shape.style {
                        Style::Clear => makshell_glass_v1::Style::Clear,
                        Style::Frosted
                        | Style::FrostedWithTail(_)
                        | Style::FrostedWithTopTail(_) => makshell_glass_v1::Style::Frosted,
                        Style::Liquid if glass.version() >= 2 => makshell_glass_v1::Style::Liquid,
                        Style::Bubble if glass.version() >= 2 => makshell_glass_v1::Style::Bubble,
                        Style::Control if glass.version() >= 2 => makshell_glass_v1::Style::Control,
                        Style::Card if glass.version() >= 3 => makshell_glass_v1::Style::Card,
                        Style::Chip if glass.version() >= 3 => makshell_glass_v1::Style::Chip,
                        Style::Window { active: true } if glass.version() >= 6 => {
                            makshell_glass_v1::Style::Window
                        }
                        Style::Window { active: false } if glass.version() >= 6 => {
                            makshell_glass_v1::Style::WindowInactive
                        }
                        Style::Liquid
                        | Style::Bubble
                        | Style::Control
                        | Style::Card
                        | Style::Chip
                        | Style::Window { .. } => makshell_glass_v1::Style::Frosted,
                        Style::Transparent if glass.version() >= 2 => {
                            makshell_glass_v1::Style::Transparent
                        }
                        Style::Transparent
                        | Style::Backdrop(_)
                        | Style::RotatedCard(_)
                        | Style::ControlBox(_)
                        | Style::CardBlob(..) => continue,
                    };
                    glass.add_rounded_rect(x, y, width, height, shape.radius.max(0) as u32, style);
                    if opacity < 1.0 && glass.version() >= 10 {
                        glass.set_last_opacity(f64::from(opacity));
                    }
                }
                if glass.version() >= 12 {
                    for blur in target.blurs.borrow().iter() {
                        let (x, y, width, height) = blur.rect;
                        glass.add_content_blur(x, y, width, height, f64::from(blur.radius));
                    }
                }
            }
            // Blur has no notion of clear glass, so every shape is blurred.
            Effect::Blur(effect) => {
                let region = self.compositor.create_region(&self.queue, ());
                for shape in shapes.iter().filter(|shape| {
                    !matches!(
                        shape.style,
                        Style::Transparent | Style::Backdrop(_) | Style::RotatedCard(_)
                    )
                }) {
                    let (x, y, width, height) = shape.rect;
                    for (rx, ry, rw, rh) in rounded_rect(width, height, shape.radius) {
                        region.add(x + rx, y + ry, rw, rh);
                    }
                }
                effect.set_blur_region(Some(&region));
                region.destroy();
            }
        }
    }
}

impl Material {
    /// Asks to hear how bright the background behind the first shape is. Returns whether the
    /// compositor can tell.
    pub fn track_brightness(&self, target: &MaterialSurface) -> bool {
        match &target.effect {
            Effect::Glass(glass) if glass.version() >= 2 => {
                glass.track_brightness();
                true
            }
            _ => false,
        }
    }

    /// The latest relative luminance behind a surface that tracks brightness, from 0 to 1.
    pub fn brightness(&self, target: &MaterialSurface) -> Option<f32> {
        let Effect::Glass(glass) = &target.effect else {
            return None;
        };
        let mut state = self.state.borrow_mut();
        // Dispatches what's queued, and asks to be told when more comes: see [`events`].
        let waker = std::task::Waker::from(std::sync::Arc::new(Arrived));
        let mut context = std::task::Context::from_waker(&waker);
        if let Poll::Ready(Err(err)) = self
            .event_queue
            .borrow_mut()
            .poll_dispatch_pending(&mut context, &mut state)
        {
            log::error!("can't dispatch material events: {err}");
        }
        let luminance = *state.brightness.get(&glass.id())?;
        Some(luminance as f32 / 1000.0)
    }
}

impl MaterialSurface {
    /// Removes the material. Call it before the surface goes away, or the protocol objects leak.
    pub fn destroy(self) {
        if let Some(shadow) = self.shadow.into_inner() {
            shadow.destroy();
        }
        match self.effect {
            Effect::Glass(glass) => glass.destroy(),
            Effect::Blur(effect) => effect.destroy(),
        }
    }
}

/// Wraps the `wl_surface` of an iced window. Call it from inside `iced_runtime::window::run`.
pub fn surface_of(
    connection: &Connection,
    window: &dyn iced_runtime::window::Window,
) -> Option<WlSurface> {
    let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the pointer is a live `wl_surface` proxy from iced_layershell, on `connection`.
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }
        .ok()?;
    WlSurface::from_id(connection, id).ok()
}

/// Approximates a rounded rectangle with rectangles, since a `wl_region` can't hold curves.
fn rounded_rect(width: i32, height: i32, radius: i32) -> Vec<Rect> {
    let radius = radius.min(width / 2).min(height / 2).max(0);
    let r = radius as f32;

    // How far each corner row is inset from the side, merged into runs of equal inset.
    let mut runs: Vec<(i32, i32, i32)> = Vec::new(); // (first row, rows, inset)
    for row in 0..radius {
        let dy = r - row as f32 - 0.5;
        let inset = (r - (r * r - dy * dy).sqrt()).round() as i32;
        match runs.last_mut() {
            Some((_, rows, last_inset)) if *last_inset == inset => *rows += 1,
            _ => runs.push((row, 1, inset)),
        }
    }

    let mut rects = Vec::with_capacity(runs.len() * 2 + 1);
    for &(row, rows, inset) in &runs {
        rects.push((inset, row, width - 2 * inset, rows));
        rects.push((inset, height - row - rows, width - 2 * inset, rows));
    }
    rects.push((0, radius, width, height - 2 * radius));
    rects
}

impl Dispatch<WlRegistry, GlobalListContents> for Globals {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Globals {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            state.blur_supported = flags.into_result().is_ok_and(|flags| {
                flags.contains(ext_background_effect_manager_v1::Capability::Blur)
            });
        }
    }
}

delegate_noop!(Globals: WlCompositor);
delegate_noop!(Globals: ignore WlSeat);
delegate_noop!(Globals: ignore WlShm);
delegate_noop!(Globals: WlShmPool);
delegate_noop!(Globals: ignore WlBuffer);
delegate_noop!(Globals: OrgKdeKwinShadowManager);
delegate_noop!(Globals: OrgKdeKwinShadow);
delegate_noop!(Globals: WlRegion);
delegate_noop!(Globals: ExtBackgroundEffectSurfaceV1);
delegate_noop!(Globals: MakshellGlassManagerV1);
/// Where [`events`] hears that something arrived in the material's queue.
static ARRIVED: Mutex<Option<mpsc::UnboundedSender<()>>> = Mutex::new(None);

/// Says something arrived in the material's queue. It's called while iced_layershell dispatches
/// the shared connection on the UI thread, which sorts each event into its queue.
struct Arrived;

impl std::task::Wake for Arrived {
    fn wake(self: std::sync::Arc<Self>) {
        if let Some(sender) = ARRIVED.lock().unwrap().as_ref() {
            let _ = sender.unbounded_send(());
        }
    }
}

/// Each time something arrives in the material's queue, like a new brightness behind glass, which
/// [`Material::brightness`] then dispatches. Nothing happens in between: KWin only says when that
/// changes. The queue is only ever dispatched on the UI thread, which reads the shared socket;
/// dispatching it anywhere else could sort the UI's own events into its queue where it doesn't look.
pub fn events() -> Subscription<()> {
    Subscription::run(event_stream)
}

fn event_stream() -> impl Stream<Item = ()> {
    iced_futures::stream::channel(4, |mut output: mpsc::Sender<()>| async move {
        let (sender, mut arrived) = mpsc::unbounded();
        *ARRIVED.lock().unwrap() = Some(sender);
        // Once to start with, which is when the UI thread first asks to be told.
        if output.send(()).await.is_err() {
            return;
        }
        while arrived.next().await.is_some() {
            if output.send(()).await.is_err() {
                return;
            }
        }
    })
}

impl Dispatch<MakshellGlassV1, ()> for Globals {
    fn event(
        state: &mut Self,
        glass: &MakshellGlassV1,
        event: makshell_glass_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let makshell_glass_v1::Event::Brightness { luminance } = event;
        state.brightness.insert(glass.id(), luminance);
    }
}
