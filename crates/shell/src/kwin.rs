//! Talks to KWin: application windows through KDE's window management protocol, on a Wayland
//! connection of our own, and keyboard layouts through KWin's D-Bus interface.
//!
//! KWin only offers window management to clients whose desktop entry asks for it with
//! `X-KDE-Wayland-Interfaces=org_kde_plasma_window_management` and whose `Exec` matches their
//! executable; see `contrib/makshell.desktop`.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read as _};
use std::os::fd::AsFd as _;
use std::sync::{Mutex, OnceLock};
use std::thread;

use iced_futures::Subscription;
use iced_futures::futures::channel::{mpsc, oneshot};
use iced_futures::futures::{SinkExt, Stream, StreamExt, stream};
use iced_widget::image;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use zbus::zvariant::{Fd, OwnedValue, Value};

use crate::dbus;
use crate::dbusmenu::Address;
use crate::plasma_protocol::org_kde_plasma_window::{self, OrgKdePlasmaWindow};
use crate::plasma_protocol::org_kde_plasma_window_management::{
    self, OrgKdePlasmaWindowManagement,
};

/// The parts of a window the shell cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Ours, not KWin's; stable while the window lives.
    pub id: u64,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub is_focused: bool,
    /// The window is asking for attention.
    pub is_urgent: bool,
    /// When it last got focus, as an ever-increasing count, for most-recently-used ordering.
    pub focused_at: Option<u64>,
    /// Where its app exports its menus, if it does.
    pub menu: Option<Address>,
    /// What KWin's own interfaces know it by.
    pub uuid: String,
    pub is_minimized: bool,
    pub is_full_screen: bool,
    /// The process it belongs to, once KWin has said.
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardLayouts {
    /// Display names, like "English (US)".
    pub names: Vec<String>,
    /// Index of the active one in `names`.
    pub current: usize,
}

/// The full window list, ordered by when windows opened, every time it changes.
pub fn windows() -> Subscription<Vec<WindowInfo>> {
    Subscription::run(window_stream)
}

/// The keyboard layouts, every time they or the active one change.
pub fn keyboard_layouts() -> Subscription<KeyboardLayouts> {
    Subscription::run(layout_stream)
}

/// Brings the window forward, unminimizing it if needed.
pub fn focus_window(id: u64) {
    with_window(id, |window| {
        window.set_state(STATE_ACTIVE | STATE_MINIMIZED, STATE_ACTIVE);
    });
}

/// Minimizes the window.
pub fn minimize_window(id: u64) {
    with_window(id, |window| {
        window.set_state(STATE_MINIMIZED, STATE_MINIMIZED);
    });
}

/// Takes the window out of full screen.
pub fn leave_full_screen(id: u64) {
    with_window(id, |window| {
        window.set_state(STATE_FULL_SCREEN, 0);
    });
}

/// Asks the window to close, the same as its close button would.
pub fn close_window(id: u64) {
    with_window(id, OrgKdePlasmaWindow::close);
}

#[zbus::proxy(
    interface = "org.kde.KWin.ScreenShot2",
    default_service = "org.kde.KWin",
    default_path = "/org/kde/KWin/ScreenShot2"
)]
trait ScreenShot {
    fn capture_window(
        &self,
        handle: &str,
        options: HashMap<&str, Value<'_>>,
        pipe: Fd<'_>,
    ) -> zbus::Result<HashMap<String, OwnedValue>>;
}

/// The window's picture, at most `max_size` pixels on its longer side. KWin sends it down a pipe.
pub async fn capture_window(uuid: String, max_size: u32) -> Option<image::Handle> {
    let result: zbus::Result<_> = async {
        let connection = dbus::session().await.ok_or(zbus::Error::Unsupported)?;
        let proxy = ScreenShotProxy::new(&connection).await?;
        let (reader, writer) = io::pipe().map_err(|err| zbus::Error::Failure(err.to_string()))?;
        let (sender, pixels) = oneshot::channel();
        thread::spawn(move || {
            let mut reader = reader;
            let mut bytes = Vec::new();
            let _ = sender.send(reader.read_to_end(&mut bytes).map(|_| bytes));
        });
        let options = HashMap::from([
            ("include-decoration", Value::Bool(true)),
            ("native-resolution", Value::Bool(true)),
        ]);
        let metadata = proxy
            .capture_window(&uuid, options, Fd::from(writer.as_fd()))
            .await?;
        // The reader only finishes once nothing holds the pipe's other end.
        drop(writer);
        let bytes = pixels
            .await
            .map_err(|err| zbus::Error::Failure(err.to_string()))?
            .map_err(|err| zbus::Error::Failure(err.to_string()))?;
        Ok((metadata, bytes))
    }
    .await;
    let (metadata, bytes) = result
        .inspect_err(|err| log::warn!("can't capture a window: {err}"))
        .ok()?;
    let number = |name: &str| u32::try_from(metadata.get(name)?).ok();
    let (width, height, stride, format) = (
        number("width")?,
        number("height")?,
        number("stride")?,
        number("format")?,
    );
    thumbnail(&bytes, width, height, stride, format, max_size)
}

/// The 8-bit QImage formats KWin sends, by QImage::Format's number (KWin 6.7's renderer sent
/// ARGB32_Premultiplied, 6.8's RGBA8888_Premultiplied): the order of the bytes in memory, and
/// what the alpha byte holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PixelFormat {
    /// Format_RGB32, ARGB32 and ARGB32_Premultiplied: 0xAARRGGBB words, so blue first.
    Bgra(Alpha),
    /// Format_RGBX8888, RGBA8888 and RGBA8888_Premultiplied: red first.
    Rgba(Alpha),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alpha {
    /// Not used: the picture is opaque.
    Opaque,
    Straight,
    Premultiplied,
}

impl PixelFormat {
    fn from_qimage(format: u32) -> Option<Self> {
        Some(match format {
            4 => Self::Bgra(Alpha::Opaque),
            5 => Self::Bgra(Alpha::Straight),
            6 => Self::Bgra(Alpha::Premultiplied),
            16 => Self::Rgba(Alpha::Opaque),
            17 => Self::Rgba(Alpha::Straight),
            18 => Self::Rgba(Alpha::Premultiplied),
            _ => return None,
        })
    }

    /// One pixel as straight RGBA, which iced wants.
    fn straight_rgba(self, pixel: &[u8]) -> [u8; 4] {
        let (rgba, alpha) = match self {
            Self::Bgra(alpha) => ([pixel[2], pixel[1], pixel[0], pixel[3]], alpha),
            Self::Rgba(alpha) => ([pixel[0], pixel[1], pixel[2], pixel[3]], alpha),
        };
        let [r, g, b, a] = rgba;
        match alpha {
            Alpha::Opaque => [r, g, b, 255],
            Alpha::Straight => rgba,
            Alpha::Premultiplied => {
                let straight = |c: u8| match a {
                    0 => 0,
                    a => (u16::from(c) * 255 / u16::from(a)).min(255) as u8,
                };
                [straight(r), straight(g), straight(b), a]
            }
        }
    }
}

fn thumbnail(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    max_size: u32,
) -> Option<image::Handle> {
    if width == 0 || height == 0 || bytes.len() < (stride * height) as usize {
        return None;
    }
    let Some(pixel_format) = PixelFormat::from_qimage(format) else {
        log::warn!("can't read a window's picture in QImage format {format}");
        return None;
    };
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for line in bytes.chunks_exact(stride as usize).take(height as usize) {
        for pixel in line[..(width * 4) as usize].chunks_exact(4) {
            rgba.extend_from_slice(&pixel_format.straight_rgba(pixel));
        }
    }
    let full = ::image::RgbaImage::from_raw(width, height, rgba)?;
    let scale = (max_size as f32 / width.max(height) as f32).min(1.0);
    let small = ::image::imageops::thumbnail(
        &full,
        ((width as f32 * scale) as u32).max(1),
        ((height as f32 * scale) as u32).max(1),
    );
    let (width, height) = small.dimensions();
    Some(image::Handle::from_rgba(
        width,
        height,
        on_dark_card(small).into_raw(),
    ))
}

/// A window's picture on a dark rounded card. Glass isn't in the picture, and neither is what's behind
/// a translucent window, which would otherwise show whatever's behind the dock through it.
fn on_dark_card(mut picture: ::image::RgbaImage) -> ::image::RgbaImage {
    const CARD: [f32; 3] = [34.0, 30.0, 40.0];
    let (width, height) = (picture.width() as f32, picture.height() as f32);
    let radius = width.max(height) * 0.03;
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        // How far outside the rounded corner the middle of this pixel is.
        let dx = (radius - (x as f32 + 0.5))
            .max(x as f32 + 0.5 - (width - radius))
            .max(0.0);
        let dy = (radius - (y as f32 + 0.5))
            .max(y as f32 + 0.5 - (height - radius))
            .max(0.0);
        let outside = (dx * dx + dy * dy).sqrt() - radius;
        let coverage = (0.5 - outside).clamp(0.0, 1.0);
        let alpha = f32::from(pixel[3]) / 255.0;
        for channel in 0..3 {
            pixel[channel] =
                (f32::from(pixel[channel]) * alpha + CARD[channel] * (1.0 - alpha)) as u8;
        }
        pixel[3] = (coverage * 255.0) as u8;
    }
    picture
}

#[zbus::proxy(
    interface = "org.kde.KWin.Effect.WindowView1",
    default_service = "org.kde.KWin.Effect.WindowView1",
    default_path = "/org/kde/KWin/Effect/WindowView1"
)]
trait WindowView {
    // KDE names its members in camelCase.
    #[zbus(name = "activate")]
    fn activate(&self, handles: &[&str]) -> zbus::Result<()>;
}

/// Shows these windows, by their uuids, side by side, like macOS's Show All Windows.
pub async fn show_windows(uuids: Vec<String>) {
    let result = async {
        let connection = dbus::session().await.ok_or(zbus::Error::Unsupported)?;
        let handles: Vec<&str> = uuids.iter().map(String::as_str).collect();
        WindowViewProxy::new(&connection)
            .await?
            .activate(&handles)
            .await
    }
    .await;
    if let Err(err) = result {
        log::warn!("can't show an app's windows: {err}");
    }
}

pub async fn switch_layout(index: usize) {
    let result = async {
        let connection = dbus::session().await.ok_or(zbus::Error::Unsupported)?;
        LayoutsProxy::new(&connection)
            .await?
            .set_layout(index as u32)
            .await
    }
    .await;
    if let Err(err) = result {
        log::warn!("can't switch keyboard layout: {err}");
    }
}

// Window state flags (`org_kde_plasma_window_management.state`).
const STATE_ACTIVE: u32 = 0x1;
const STATE_MINIMIZED: u32 = 0x2;
const STATE_FULL_SCREEN: u32 = 0x8;
const STATE_DEMANDS_ATTENTION: u32 = 0x80;
const STATE_SKIP_TASKBAR: u32 = 0x1000;

/// Version 16: the last before stacking order moved to its own object, which we don't need.
const VERSION: u32 = 16;

/// Window objects by our id, so requests can come from the UI thread.
static WINDOWS: Mutex<BTreeMap<u64, OrgKdePlasmaWindow>> = Mutex::new(BTreeMap::new());
static CONNECTION: OnceLock<Connection> = OnceLock::new();

fn with_window(id: u64, request: impl FnOnce(&OrgKdePlasmaWindow)) {
    let windows = WINDOWS.lock().unwrap();
    let Some(window) = windows.get(&id) else {
        return;
    };
    request(window);
    if let Some(Err(err)) = CONNECTION.get().map(Connection::flush) {
        log::error!("can't flush the window management connection: {err}");
    }
}

fn window_stream() -> impl Stream<Item = Vec<WindowInfo>> {
    iced_futures::stream::channel(16, |mut output: mpsc::Sender<Vec<WindowInfo>>| async move {
        let (tx, mut rx) = mpsc::unbounded();

        // Wayland dispatching blocks, so it gets its own thread.
        thread::spawn(move || {
            if let Err(err) = track_windows(tx) {
                log::error!("window tracking ended: {err}");
            }
        });

        while let Some(windows) = rx.next().await {
            if output.send(windows).await.is_err() {
                break;
            }
        }
    })
}

fn track_windows(tx: mpsc::UnboundedSender<Vec<WindowInfo>>) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|err| err.to_string())?;
    let (globals, mut queue) =
        registry_queue_init::<Tracker>(&connection).map_err(|err| err.to_string())?;
    let _manager: OrgKdePlasmaWindowManagement = globals
        .bind(&queue.handle(), 1..=VERSION, ())
        .map_err(|_| {
            "KWin doesn't offer window management; is makshell's desktop entry installed?"
                .to_owned()
        })?;
    let _ = CONNECTION.set(connection);

    let mut tracker = Tracker {
        windows: HashMap::new(),
        next_id: 1,
        focus_clock: 0,
    };
    let mut last_sent = None;
    loop {
        queue
            .blocking_dispatch(&mut tracker)
            .map_err(|err| err.to_string())?;
        let snapshot = tracker.snapshot();
        if last_sent.as_ref() != Some(&snapshot) {
            if tx.unbounded_send(snapshot.clone()).is_err() {
                return Ok(());
            }
            last_sent = Some(snapshot);
        }
    }
}

struct Tracker {
    windows: HashMap<u64, Tracked>,
    next_id: u64,
    focus_clock: u64,
}

#[derive(Default)]
struct Tracked {
    menu: Option<Address>,
    uuid: String,
    app_id: Option<String>,
    title: Option<String>,
    state: u32,
    focused_at: Option<u64>,
    pid: Option<u32>,
    /// KWin has sent everything it knows; until then the window is half described.
    ready: bool,
}

impl Tracker {
    fn snapshot(&self) -> Vec<WindowInfo> {
        let mut windows: Vec<WindowInfo> = self
            .windows
            .iter()
            .filter(|(_, w)| {
                // Dialogs are hidden from the taskbar, but the shell wants to know if they're active.
                w.ready
                    && (w.state & STATE_SKIP_TASKBAR == 0
                        || w.app_id.as_deref() == Some(crate::alert::APP_ID))
            })
            .map(|(&id, w)| WindowInfo {
                id,
                app_id: w.app_id.clone(),
                title: w.title.clone(),
                is_focused: w.state & STATE_ACTIVE != 0,
                is_urgent: w.state & STATE_DEMANDS_ATTENTION != 0,
                focused_at: w.focused_at,
                menu: w.menu.clone(),
                uuid: w.uuid.clone(),
                is_minimized: w.state & STATE_MINIMIZED != 0,
                is_full_screen: w.state & STATE_FULL_SCREEN != 0,
                pid: w.pid,
            })
            .collect();
        windows.sort_by_key(|w| w.id);
        windows
    }
}

impl Dispatch<OrgKdePlasmaWindowManagement, ()> for Tracker {
    fn event(
        state: &mut Self,
        manager: &OrgKdePlasmaWindowManagement,
        event: org_kde_plasma_window_management::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let org_kde_plasma_window_management::Event::WindowWithUuid { uuid, .. } = event {
            let id = state.next_id;
            state.next_id += 1;
            let window = manager.get_window_by_uuid(uuid.clone(), queue, id);
            WINDOWS.lock().unwrap().insert(id, window);
            state.windows.insert(
                id,
                Tracked {
                    uuid,
                    ..Tracked::default()
                },
            );
        }
    }
}

impl Dispatch<OrgKdePlasmaWindow, u64> for Tracker {
    fn event(
        state: &mut Self,
        window: &OrgKdePlasmaWindow,
        event: org_kde_plasma_window::Event,
        &id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(tracked) = state.windows.get_mut(&id) else {
            return;
        };
        match event {
            org_kde_plasma_window::Event::TitleChanged { title } => tracked.title = Some(title),
            org_kde_plasma_window::Event::ApplicationMenu {
                service_name,
                object_path,
            } => {
                tracked.menu = (!service_name.is_empty()).then_some(Address {
                    service: service_name,
                    path: object_path,
                });
            }
            org_kde_plasma_window::Event::AppIdChanged { app_id } => {
                tracked.app_id = (!app_id.is_empty()).then_some(app_id);
            }
            org_kde_plasma_window::Event::StateChanged { flags } => {
                if flags & STATE_ACTIVE != 0 && tracked.state & STATE_ACTIVE == 0 {
                    state.focus_clock += 1;
                    tracked.focused_at = Some(state.focus_clock);
                }
                tracked.state = flags;
            }
            org_kde_plasma_window::Event::PidChanged { pid } => {
                tracked.pid = (pid != 0).then_some(pid);
            }
            org_kde_plasma_window::Event::InitialState => tracked.ready = true,
            org_kde_plasma_window::Event::Unmapped => {
                state.windows.remove(&id);
                WINDOWS.lock().unwrap().remove(&id);
                window.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Tracker {
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

#[zbus::proxy(
    interface = "org.kde.KeyboardLayouts",
    default_service = "org.kde.keyboard",
    default_path = "/Layouts"
)]
trait Layouts {
    // KDE names its members in camelCase.
    #[zbus(name = "getLayout")]
    fn get_layout(&self) -> zbus::Result<u32>;
    #[zbus(name = "setLayout")]
    fn set_layout(&self, index: u32) -> zbus::Result<bool>;
    /// Short name, variant and display name of each layout.
    #[zbus(name = "getLayoutsList")]
    fn get_layouts_list(&self) -> zbus::Result<Vec<(String, String, String)>>;

    #[zbus(signal, name = "layoutChanged")]
    fn layout_changed(&self, index: u32) -> zbus::Result<()>;
    #[zbus(signal, name = "layoutListChanged")]
    fn layout_list_changed(&self) -> zbus::Result<()>;
}

fn layout_stream() -> impl Stream<Item = KeyboardLayouts> {
    iced_futures::stream::channel(4, |mut output: mpsc::Sender<KeyboardLayouts>| async move {
        let Some(connection) = dbus::session().await else {
            return;
        };
        let proxy = match LayoutsProxy::new(&connection).await {
            Ok(proxy) => proxy,
            Err(err) => {
                log::warn!("no keyboard layouts from KWin: {err}");
                return;
            }
        };
        let (Ok(changed), Ok(list_changed)) = (
            proxy.receive_layout_changed().await,
            proxy.receive_layout_list_changed().await,
        ) else {
            log::warn!("can't follow KWin's keyboard layouts");
            return;
        };

        // Reread everything on either signal; it's two small calls.
        let mut changes = stream::select(changed.map(|_| ()), list_changed.map(|_| ()));
        loop {
            let layouts = async {
                let list = proxy.get_layouts_list().await.ok()?;
                let current = proxy.get_layout().await.ok()? as usize;
                Some(KeyboardLayouts {
                    names: list.into_iter().map(|(_, _, name)| name).collect(),
                    current,
                })
            }
            .await;
            if let Some(layouts) = layouts
                && output.send(layouts).await.is_err()
            {
                return;
            }
            if changes.next().await.is_none() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{Alpha, PixelFormat, thumbnail};

    #[test]
    fn pixels_come_out_straight_whichever_format_kwin_sends() {
        // Half-transparent red, premultiplied, as 6.7 (ARGB32, blue first) and 6.8 (RGBA8888) send it.
        let argb = PixelFormat::from_qimage(6).unwrap();
        let rgba = PixelFormat::from_qimage(18).unwrap();
        assert_eq!(argb, PixelFormat::Bgra(Alpha::Premultiplied));
        assert_eq!(argb.straight_rgba(&[0, 0, 128, 128]), [255, 0, 0, 128]);
        assert_eq!(rgba.straight_rgba(&[128, 0, 0, 128]), [255, 0, 0, 128]);
        // Straight alpha stays as it is; an unused alpha byte means opaque.
        assert_eq!(
            PixelFormat::from_qimage(17)
                .unwrap()
                .straight_rgba(&[200, 0, 0, 128]),
            [200, 0, 0, 128]
        );
        assert_eq!(
            PixelFormat::from_qimage(4)
                .unwrap()
                .straight_rgba(&[0, 0, 200, 0]),
            [200, 0, 0, 255]
        );
        assert_eq!(PixelFormat::from_qimage(19), None);
    }

    #[test]
    fn a_window_in_kwin_68s_format_makes_a_thumbnail() {
        let (width, height, stride) = (4, 2, 16);
        let bytes = vec![255; (stride * height) as usize];
        assert!(thumbnail(&bytes, width, height, stride, 18, 64).is_some());
        assert!(thumbnail(&bytes, width, height, stride, 19, 64).is_none());
    }
}
