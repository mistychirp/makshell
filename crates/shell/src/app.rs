//! The shell process: owns every layer surface and the state they share.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use iced_futures::Subscription;
use iced_futures::backend::native::tokio::time;
use iced_layershell::actions::{ActionCallback, IcedNewPopupSettings};
use iced_layershell::daemon;
use iced_layershell::reexport::{
    IcedId, KeyboardInteractivity, Layer, PopupAnchor, PopupConstraintAdjustment, PopupGravity,
    Task, WithConnection,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use iced_widget::core::{
    Color, Event, Length, Point, Rectangle, Size, Theme, event, keyboard, mouse, theme, window,
};
use iced_widget::renderer::Renderer;
use iced_widget::{Space, image};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy as _};

use crate::alert::{self, Alert};
use crate::anim;
use crate::apps::{self, App, AppIndex};
use crate::banners::{self, Banners};
use crate::config::{self, Config};
use crate::icon_themes;
use crate::icons::Glyph;
use crate::kwin::{self, KeyboardLayouts, WindowInfo};
use crate::material::{self, Material, MaterialSurface};
use crate::notifications::{self, Notification};
use crate::places::{self, Place};
use crate::{
    appearance, appmenu, arrange, audio, autostart, battery, bluetooth, brightness, clip_store,
    clipboard, control, dbusmenu, desktop, dnd, dock, fan, fonts, full_screen, hud, icons,
    launcher, media, menu, menubar, network, notification_center, portal, running, spotlight, tray,
    wallpaper, widgets,
};

pub type Element<'a> = iced_widget::core::Element<'a, Message, Theme, Renderer>;

pub fn run() -> Result<(), iced_layershell::Error> {
    // Our own connection, shared with iced_layershell, so protocol objects we create can refer
    // to the surfaces it creates.
    let connection = Connection::connect_to_env()
        .inspect_err(|err| log::error!("can't connect to Wayland: {err}"))
        .ok();
    let material = connection.as_ref().and_then(Material::new).map(Rc::new);
    if let Some(connection) = &connection {
        dnd::start(connection);
    }

    daemon(
        move || Shell::new(material.clone()),
        namespace,
        Shell::update,
        Shell::view,
    )
    .style(style)
    .subscription(Shell::subscription)
    .settings(Settings {
        // No implicit surface; `Shell::new` opens each one explicitly.
        layer_settings: LayerShellSettings {
            start_mode: StartMode::Background,
            ..Default::default()
        },
        default_font: fonts::REGULAR,
        default_text_size: menubar::TEXT_SIZE.into(),
        with_connection: connection.map(WithConnection::Value),
        ..Default::default()
    })
    .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    Wallpaper,
    /// The icons and widgets above the wallpaper.
    Desktop,
    /// Where to choose widgets from, while editing them.
    WidgetGallery,
    Spotlight,
    /// The Downloads stack, open.
    Fan,
    /// The volume or brightness, for a moment.
    Hud,
    /// The green traffic light's menu.
    ArrangeMenu,
    MenuBar,
    Dock,
    Popup,
    Alert,
    Banners,
    /// The strip along the top that brings the menu bar down over a full-screen window.
    FullScreenTrigger,
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// Other apps are in line with the config file, which comes to this accent.
    AppearanceApplied(Option<appearance::Applied>),
    /// Time to see whether it's become light or dark, for Auto.
    AppearanceTick,
    /// Time to see whether the MacOS-Like icons need building, for apps that came since.
    IconThemesCheck,
    IconThemesBuilt(icon_themes::Built),
    WindowsChanged(Vec<WindowInfo>),
    KeyboardLayoutsChanged(KeyboardLayouts),
    /// The battery, or none, every time it changes.
    BatteryChanged(Option<battery::Battery>),
    LowPowerMode(bool),
    /// A layout was chosen in the keyboard layout menu; carries its index.
    SwitchLayout(usize),
    /// The Control Center's keyboard module was pressed.
    NextLayout,
    Screenshot,
    LockScreen,
    /// The Control Center's display slider moved.
    BrightnessChanged(f32),
    BrightnessReleased,
    /// The monitor's brightness was read, if it could be.
    BrightnessRead(Option<f32>),
    /// The Control Center's volume slider moved.
    VolumeChanged(f32),
    VolumeReleased,
    ToggleMute,
    ToggleWifi,
    ToggleBluetooth,
    Media(media::Control),
    /// Time to reread what the Control Center shows.
    ControlRefresh,
    /// A module's detail was opened, or left.
    ControlDetail(Option<control::Detail>),
    /// An animation frame for a module turning into its detail, or back.
    ControlFrame,
    /// Something that takes presses was pressed where they do nothing.
    Nothing,
    /// A detail panel's settings row was pressed; carries the settings page.
    ControlSettings(&'static str),
    /// A keyboard layout was chosen in the Keyboard detail; carries its index.
    ControlLayout(usize),
    /// A sound output was chosen; carries its place in the list.
    ControlOutput(usize),
    /// A Wi-Fi network was chosen; carries its name.
    WifiConnect(String),
    /// A Bluetooth device was chosen; carries its path, and whether to connect.
    BluetoothConnect(zbus::zvariant::OwnedObjectPath, bool),
    ControlStatus(control::Status),
    /// Time to look at what the bar's status items show.
    BarStatusRefresh,
    BarStatus(
        Option<network::Wifi>,
        Option<bluetooth::Bluetooth>,
        Option<audio::Volume>,
    ),
    /// What a status item's menu shows has been read, so it opens at its size.
    StatusMenuReady(menubar::Title, control::Status),
    Tick,
    /// A dock icon was pressed; carries the app's id. It becomes a click or a drag.
    DockPressed(String),
    /// Raw pointer motion on a surface, which keeps coming during a drag even outside it.
    PointerMoved(IcedId, Point),
    /// The left button was released over (or while grabbed by) a surface.
    PointerReleased(IcedId),
    /// A button was pressed on a surface, and no widget there took it.
    SurfacePressed(IcedId),
    /// The pointer came onto a surface, which may be all it does: pushed against the top of the
    /// screen, it doesn't move any more.
    PointerEntered(IcedId),
    /// The pointer left a surface.
    PointerLeft(IcedId),
    /// Time to see whether the bars over a full-screen window should go back up.
    FullScreenCheck,
    FullScreenFrame,
    FullScreenLightsHovered(bool),
    FullScreenClose,
    FullScreenExit,
    DockPointerMoved(Point),
    DockPointerLeft,
    /// An animation frame for dock magnification.
    DockFrame(Instant),
    SurfaceOpened(IcedId, Size),
    SurfaceResized(IcedId, Size),
    /// An icon finished loading.
    IconLoaded(
        iced_widget::core::image::Id,
        Result<iced_runtime::image::Allocation, iced_runtime::image::Error>,
    ),
    /// The wallpaper finished uploading to the GPU.
    WallpaperLoaded(Result<iced_runtime::image::Allocation, iced_runtime::image::Error>),
    /// A surface's `wl_surface`, if it could be reached, to put material behind.
    SurfaceReached(IcedId, Option<WlSurface>),
    /// A menu bar title was pressed.
    BarPressed(menubar::Title),
    /// The pointer moved onto a menu bar title.
    BarEntered(menubar::Title),
    /// Something was chosen in the system menu.
    System(menubar::SystemAction),
    /// A system action was confirmed in its alert.
    SystemConfirmed(menubar::SystemAction),
    /// Time to check for news about the brightness behind the bar.
    BarBrightness,
    /// A dock icon was right-clicked; carries the app's id.
    /// A frame was just drawn, at this moment, which is when everything moving takes its next step.
    Frame(Instant),
    /// In case nothing's drawing, like when what's moving is out of sight.
    FrameKick,
    /// The pointer came onto a Control Center module, or left it.
    ControlHover(control::Module, bool),
    /// A menu's row was chosen, in a menu this many submenus deep, and what to do once it's blinked.
    MenuRowChosen(usize, usize, Box<Message>),
    MenuBlinkFrame,
    DockMenuRequested(String),
    /// A minimized window's tile in the dock was pressed, or right-clicked.
    DockThumbnailPressed(u64),
    DockThumbnailMenu(u64),
    /// A minimized window's picture arrived, if it could be taken.
    WindowCaptured(u64, Option<image::Handle>),
    /// An app's news about its dock badge.
    LauncherUpdate(launcher::Update),
    /// Something was chosen in a dock popup.
    DockMenu(dock::MenuAction),
    PlaceClicked(Place),
    PlaceMenuRequested(Place),
    /// The alert was answered: confirmed, or cancelled.
    AlertAnswered(bool),
    /// News from window decorations about their green lights.
    Arrange(arrange::Event),
    /// Which window a green light's menu is about, and where to open it.
    ArrangeFound(Point, Option<(String, i32, i32)>),
    ArrangeOverMenu(bool),
    ArrangeSubmenu(bool),
    ArrangeChosen(arrange::Layout),
    /// The pointer left a while ago: closes the menu if this was the last time it did.
    ArrangeLeft(u64),
    Desktop(desktop::Action),
    /// News about drags over the shell's surfaces, or from them.
    Dnd(dnd::Event),
    DesktopMetrics(desktop::Metrics),
    /// The press's serial is ready for dragging the selection.
    DesktopDragReady,
    /// Files were moved to the desktop: their names, and where they were dropped.
    DesktopReceived(Vec<String>, Point),
    Widget(widgets::Action),
    WidgetPhotoChosen(u64, Option<PathBuf>),
    WidgetPhotoLoaded(PathBuf, Option<widgets::Picture>),
    FetchWeather,
    WeatherFetched(Option<widgets::Weather>),
    DesktopPressed,
    DesktopRightPressed,
    DesktopRefresh,
    DesktopListed(Vec<desktop::Entry>),
    DesktopThumbnail(PathBuf, Option<image::Handle>),
    DesktopRenameChanged(String),
    /// Ctrl or Shift are held, or not.
    AddingModifiers(bool),
    /// A new wallpaper was picked, if one was.
    WallpaperChosen(Option<PathBuf>),
    /// "System Settings…" was chosen in the menu.
    OpenSettings,
    /// An animation frame for the volume and brightness capsule.
    HudFrame,
    /// The brightness, read to change it with a key, by this much.
    BrightnessStep(Option<f32>, f32),
    /// A picture of one of the newest downloads, for the Downloads tile.
    PileThumbnail(PathBuf, Option<image::Handle>),
    /// The pointer moved over the Downloads fan.
    FanMoved(Point),
    FanPressed,
    FanReleased,
    /// A right press in the fan: over a file, its menu.
    FanRightPressed,
    /// Something chosen from a file's menu in the fan.
    FanFile(fan::FileAction, PathBuf),
    /// An animation frame for the fan.
    FanFrame,
    /// A picture of a file in the fan.
    FanThumbnail(PathBuf, Option<image::Handle>),
    /// A drag out of the fan can start, now that the press's serial is known.
    FanDragReady,
    /// Super and a number, which go to Spotlight's panels.
    SpotlightHotkey(usize),
    /// Spotlight's shortcut, or its icon in the bar.
    SpotlightToggle,
    SpotlightClose,
    SpotlightTyped(String),
    SpotlightMode(spotlight::Mode),
    SpotlightCategory(Option<spotlight::Category>),
    SpotlightHovered(usize),
    SpotlightChosen(usize),
    /// An arrow key: how many rows down, and how many places to the right.
    SpotlightMoved(i32, i32),
    /// It's been a moment since this was typed.
    SpotlightSearchDue(String),
    SpotlightFilesFound(String, Vec<spotlight::FileHit>),
    SpotlightRecentFiles(Vec<spotlight::FileHit>),
    /// Time to read a few more apps, so that it has them all by the time it opens.
    SpotlightLoadApps,
    /// Something's been redrawn in the background, like an icon that took a moment to load.
    SpotlightTick,
    /// Something was copied.
    Copied(clipboard::Content),
    /// Hiding an app: its windows go, with no animation and no tile in the dock.
    HideWindows(Vec<u64>),
    /// It's been a moment since an app with this id was asked to close its windows.
    QuitGraceOver(String),
    /// Time to see which apps are still running.
    CheckRunning,
    RunningChecked(BTreeMap<String, Vec<running::Process>>),
    /// The alert was pressed where there's no button.
    AlertDragged,
    AlertFieldChanged(String),
    AlertFieldShown(bool),
    /// News from NetworkManager's requests for passwords.
    WifiAgent(network::AgentEvent),
    /// Whether the machine's on a cable with no Wi-Fi.
    EthernetChecked(bool),
    CheckEthernet,
    /// Return or Escape was pressed: whether it was Return.
    ReturnOrEscape(bool),
    SurfaceClosed(IcedId),
    /// An app sent or took back a notification.
    Notification(notifications::Event),
    /// An animation frame for banners, or a check on whether one has expired.
    BannerFrame(Instant),
    NotificationEntered(u32),
    NotificationLeft(u32),
    /// A notification's card was pressed.
    NotificationPressed(u32),
    /// One of a notification's buttons was pressed; carries the action's key.
    NotificationAction(u32, String),
    /// A notification's ✕ was pressed.
    NotificationDismissed(u32),
    /// A card's capsule was pressed, for its menu, and where it is on its surface.
    NotificationOptions(u32, (i32, i32, i32, i32)),
    /// An open card's … was pressed, and where its menu goes on its surface.
    NotificationMore(u32, (i32, i32, i32, i32)),
    /// An app's notifications are to come without banners for this long.
    NotificationAppMuted(String, Duration),
    NotificationAppTurnedOff(String),
    OpenNotificationSettings,
    /// A card's arrow was pressed, which opens it up to show its picture, or closes it.
    NotificationExpand(u32),
    /// Something about notifications is moving, which has no more to it than time passing.
    NotificationFrame,
    /// A group in Notification Center was opened, closed or cleared; carries the app's key.
    NotificationGroupOpened(String),
    NotificationGroupClosed(String),
    NotificationGroupCleared(String),
    /// Notification Center's own ✕ was pressed.
    NotificationsCleared,
    /// The session's tray icons changed.
    TrayItems(Vec<tray::Item>),
    /// Something was chosen in a menu an app exports.
    DbusMenuItemChosen(dbusmenu::Address, i32),
    /// One of the focused app's menus changed.
    AppMenuChanged(dbusmenu::Address, Vec<dbusmenu::Item>),
    /// The pointer moved onto a menu's row: how many submenus deep the menu is, and which row.
    MenuRowEntered(usize, usize),
    /// A row that leads to a submenu was pressed.
    MenuRowPressed(usize, usize),
    /// Time to check whether the pointer has settled on a menu's row.
    MenuHoverCheck,
    /// Scrolled over Notification Center, in rows.
    NotificationCenterScrolled(f32),
}

/// A popup that's open, or opening: a menu, or the Downloads stack.
struct Popup {
    id: IcedId,
    /// The menu bar title it hangs from, if any; otherwise it belongs to the dock.
    title: Option<menubar::Title>,
    content: PopupContent,
    size: (u32, u32),
    material: Option<MaterialSurface>,
    /// Shapes last sent, for content that changes while it's open.
    shapes: Vec<material::Shape>,
    /// How opaque they are, where that's not fully.
    shape_opacities: Vec<f32>,
    /// Where its left edge is on the screen.
    x: f32,
    /// Where a menu's tail's tip is from its left edge, if it has one.
    tail: Option<f32>,
}

/// A menu open beside a row of another.
struct Submenu {
    id: IcedId,
    /// The row of the menu before it that it opened from.
    opener: usize,
    /// Where its left edge is on the screen.
    x: f32,
    entries: Vec<menu::Entry>,
    size: (u32, u32),
    material: Option<MaterialSurface>,
    /// It was asked to close, and hasn't yet.
    closing: bool,
}

/// A menu's row that was chosen, blinking before it does what it's for, like on macOS.
struct MenuBlink {
    level: usize,
    row: usize,
    started: Instant,
    chosen: Message,
}

struct MenuHover {
    level: usize,
    row: usize,
    since: Instant,
}

struct OpenAlert {
    id: IcedId,
    alert: Alert,
    material: Option<MaterialSurface>,
    /// Whether it has the keyboard. An alert looks lighter when it doesn't.
    active: bool,
}

enum PopupContent {
    Menu(Vec<menu::Entry>),
    /// How big things are on the desktop.
    ViewOptions,
    /// Shows [`Shell::control`].
    ControlCenter,
    /// Shows [`Shell::notifications`].
    NotificationCenter,
    /// A status item's menu: what Control Center's module for it opens to.
    Status(control::Detail),
}

struct Shell {
    surfaces: HashMap<IcedId, Surface>,
    /// A description of what each surface last showed, to tell when it needs a redraw.
    surface_contents: HashMap<IcedId, String>,
    wallpaper: Option<image::Handle>,
    /// Keeps the wallpaper on the GPU. Big images upload in the background and draw as nothing
    /// until they're ready, and the wallpaper doesn't otherwise redraw to catch up.
    wallpaper_loaded: Option<iced_runtime::image::Allocation>,
    arrange_menu: Option<arrange::Open>,
    /// What the genie was last told.
    genie_targets: Vec<(String, (i32, i32, i32, i32))>,
    desktop_id: IcedId,
    desktop: desktop::Desktop,
    desktop_surface: Option<WlSurface>,
    desktop_material: Option<MaterialSurface>,
    desktop_shapes: Vec<material::Shape>,
    /// View Options are waiting for the menu they were chosen from to go.
    next_view_options: Option<IcedNewPopupSettings>,
    dock_surface: Option<ObjectId>,
    /// Which of the shell's surfaces a drag is over.
    drag_over: Option<ObjectId>,
    widgets: widgets::Widgets,
    gallery_id: Option<IcedId>,
    gallery_material: Option<MaterialSurface>,
    /// Keeps icons on the GPU once they've loaded.
    loaded_icons: HashMap<iced_widget::core::image::Id, Option<iced_runtime::image::Allocation>>,
    apps: AppIndex,
    pinned: Vec<String>,
    /// The app of the focused window.
    focused_app: Option<Rc<App>>,
    clock: String,
    menubar_id: IcedId,
    /// Width of the menu bar surface; 0 until the compositor has configured it.
    menubar_width: f32,
    menubar_material: Option<MaterialSurface>,
    /// The distro logo as silhouettes, for dark and light backdrops.
    logo: Option<image::Handle>,
    logo_dark: Option<image::Handle>,
    /// How bright the background behind the bar is, if the compositor can tell.
    bar_backdrop: Option<menubar::Backdrop>,
    /// Whether the compositor reports the brightness behind the bar.
    bar_tracks_brightness: bool,
    /// The focused window is full screen, and the menu bar comes down over it when it's wanted.
    full_screen: Option<full_screen::FullScreen>,
    keyboard_layouts: Option<KeyboardLayouts>,
    /// What the Control Center shows, refreshed when it opens.
    control: control::Panel,
    /// When the volume slider last set the volume, to throttle `wpctl` while dragging.
    volume_set_at: Option<Instant>,
    /// Short name of the active keyboard layout, when there's more than one.
    input_label: Option<String>,
    /// The distro logo in color, for alerts about the whole system.
    system_icon: Option<image::Handle>,
    /// The latest window list from KWin.
    windows: Vec<WindowInfo>,
    dock: Vec<dock::Item>,
    dock_id: IcedId,
    /// Width of the dock surface; 0 until the compositor has configured it.
    dock_width: f32,
    dock_pointer: dock::Pointer,
    /// What `dock_pointer.amount` is easing toward.
    dock_target: f32,
    dock_last_frame: Option<Instant>,
    dock_room: dock::RoomTracker,
    dock_was_dragged: bool,
    /// The apps that aren't kept in the dock, oldest first, and whether they show.
    recents: Vec<String>,
    show_recents: bool,
    /// How much room the line before the recent apps has.
    dock_section: anim::Motion,
    dock_was_pinned: bool,
    /// The pointer left the dock while it was pinned.
    dock_pointer_left: bool,
    /// When the last frame was drawn, of any surface, while things are moving.
    last_frame: Instant,
    /// Size of a fully magnified icon.
    dock_max_icon: f32,
    /// The wallpaper as it's written in the config file.
    wallpaper_path: Option<String>,
    always_glassy_widgets: bool,
    dock_layout: dock::Layout,
    material: Option<Rc<Material>>,
    dock_material: Option<MaterialSurface>,
    /// Shapes last sent for the dock, to skip redundant updates while icons animate.
    dock_shapes: Vec<material::Shape>,
    /// Latest pointer position over the dock surface.
    dock_cursor: Point,
    /// A press on a dock icon that hasn't become a click or a drag yet.
    dock_press: Option<DockPress>,
    dock_drag: Option<dock::Drag>,
    /// Apps launched from the dock that haven't opened a window yet, and when.
    launching: HashMap<String, Instant>,
    fan: Option<fan::Fan>,
    hud: Option<hud::Hud>,
    /// The newest downloads, newest first, which the Downloads tile shows.
    pile: Vec<PathBuf>,
    pile_pictures: Vec<image::Handle>,
    spotlight: Option<spotlight::Spotlight>,
    /// The apps it lists, as far as they've been read.
    spotlight_apps: Vec<Rc<App>>,
    /// The ids of those that haven't been yet.
    spotlight_unread: Vec<String>,
    spotlight_file_icons: spotlight::FileIcons,
    /// What's been copied, newest first.
    clips: std::collections::VecDeque<spotlight::Clip>,
    /// How many copied things the history keeps.
    clips_max: usize,
    /// Where the pointer was last over the fan, which a press doesn't say.
    fan_pointer: Point,
    /// What's running of each app, by the id its windows give, or a pinned app's id.
    running: BTreeMap<String, Vec<running::Process>>,
    /// Shared start time for attention bounces, so they stay in step.
    started: Instant,
    popup: Option<Popup>,
    /// A menu bar title whose menu opens once the open popup is gone.
    next_bar_menu: Option<menubar::Title>,
    /// Submenus open under the popup's menu, outermost first.
    submenus: Vec<Submenu>,
    /// A menu's row the pointer is over, and since when, that may change which submenus are open.
    menu_hover: Option<MenuHover>,
    menu_blink: Option<MenuBlink>,
    widget_tint: widgets::Tint,
    /// A submenu to open once those being closed are gone, since only the topmost popup can have
    /// one: the level and row of the menu it's opening from.
    next_submenu: Option<(usize, usize)>,
    tray: Vec<tray::Icon>,
    /// Where the focused app's menus are, if it exports any.
    app_menu_address: Option<dbusmenu::Address>,
    /// The focused app's menus, with their names on their own for the bar.
    app_menu: Vec<dbusmenu::Item>,
    app_menu_titles: Vec<String>,
    alert: Option<OpenAlert>,
    /// A network's password that NetworkManager wants, waiting for the alert that's up to go.
    waiting_secret: Option<network::SecretRequest>,
    /// The machine's on a cable, with no Wi-Fi.
    ethernet: bool,
    /// Desktop entry id of the app that opens folders and the trash.
    file_manager: String,
    downloads_dir: PathBuf,
    trash_dir: PathBuf,
    /// How many items were in the trash at the last check.
    trash_count: usize,
    /// The current icon of each of [`Place::ALL`].
    place_icons: Vec<Option<image::Handle>>,
    /// Windows hidden with their app, which are minimized but don't show up in the dock as such.
    hidden_windows: HashSet<u64>,
    /// Minimized windows, which the dock shows, in the order they were minimized.
    minimized: Vec<dock::Minimized>,
    /// Count badges, by desktop entry id in lowercase.
    dock_badges: HashMap<String, u32>,
    /// What each app last said about its count, and whether to show it.
    badge_counts: HashMap<String, (u32, bool)>,
    /// Height of the screen, once the wallpaper's surface has said.
    screen_height: f32,
    /// What Notification Center holds, most recent first.
    notifications: Vec<Notification>,
    /// The notification the pointer is over.
    notification_hovered: Option<u32>,
    /// The card whose menu of options is open, which counts as being pointed at.
    notification_pinned: Option<u32>,
    notification_fade: notifications::HoverFade,
    center_motion: notification_center::Motions,
    banners_opacities: Vec<f32>,
    /// Groups opened in Notification Center, by app.
    open_groups: HashSet<String>,
    /// The appearance goes by the sun, which takes looking now and then.
    appearance_auto: bool,
    /// Which status items the bar shows.
    show_wifi: bool,
    show_bluetooth: bool,
    show_sound: bool,
    show_battery: bool,
    show_battery_percent: bool,
    /// A laptop's battery.
    battery: Option<battery::Battery>,
    /// When each app that's muted gets its banners back. It isn't kept for the next session.
    muted_apps: HashMap<String, Instant>,
    notifications_off: Vec<String>,
    /// How far Notification Center is scrolled, in rows.
    center_scroll: f32,
    banners: Banners,
    banners_surface: Option<IcedId>,
    /// The surface was asked to close, and hasn't yet.
    banners_closing: bool,
    banners_material: Option<MaterialSurface>,
    banners_shapes: Vec<material::Shape>,
    banners_region: Vec<(i32, i32, i32, i32)>,
}

/// A dock icon a popup can be anchored to.
enum DockSlot {
    App(usize),
    /// An offset into [`Place::ALL`].
    Place(usize),
    /// A place in the list of minimized windows.
    Minimized(usize),
}

fn place_slot(place: Place) -> DockSlot {
    DockSlot::Place(Place::ALL.iter().position(|&p| p == place).unwrap_or(0))
}

struct DockPress {
    app_id: String,
    at: Point,
}

/// How far the pointer has to move with the button down before a press becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;

/// How long a launch bounces before the dock gives up waiting for a window.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(8);

/// The screen's height until the wallpaper's surface says, if there is one.
const FALLBACK_SCREEN_HEIGHT: f32 = 700.0;
/// How long the pointer has to stay on a menu's row to change which submenus are open, so that
/// heading for an open submenu across other rows doesn't close it.
const MENU_HOVER_DELAY: Duration = Duration::from_millis(130);
/// How the line before the dock's recent apps makes room.
const DOCK_SECTION_SPRING: anim::Spring = anim::Spring::new(0.3, 0.85);
/// How long things can go without a frame while they're moving, before they move anyway.
const FRAME_KICK: Duration = Duration::from_millis(32);
/// How long the dock fills up for as the session starts, which isn't worth animating.
const DOCK_SETTLE: Duration = Duration::from_secs(3);
/// How long a chosen menu row stays off, and then back on, measured from Tahoe's.
const MENU_BLINK_PHASE: Duration = Duration::from_millis(70);
/// How big minimized windows' pictures are kept, on their longer side: enough for a magnified dock.
const THUMBNAIL_SIZE: u32 = 512;
/// How long the green light's menu waits for the pointer to come back.
/// How many files the Downloads tile shows.
const PILE_SIZE: usize = 3;
const VOLUME_STEP: f32 = 0.05;
const VOLUME_STEP_SMALL: f32 = 0.01;
const BRIGHTNESS_STEP: f32 = 0.05;
const BRIGHTNESS_STEP_SMALL: f32 = 0.01;
/// How many downloads to look at, for saying how many more there are.
const FAN_MOST_DOWNLOADS: usize = 5000;
/// How long Spotlight waits after a keystroke before looking through the files.
const SPOTLIGHT_TYPING: Duration = Duration::from_millis(180);
/// How many apps it reads per tick, in the background, since reading one means decoding its icon.
const SPOTLIGHT_APPS_AT_ONCE: usize = 4;
/// How long an app has to close its windows when asked to quit, before what's left is told to.
const QUIT_GRACE: Duration = Duration::from_secs(3);
const ARRANGE_MENU_GRACE: Duration = Duration::from_millis(350);
/// How soon to look up the weather again when that didn't work.
const WEATHER_RETRY: Duration = Duration::from_secs(60);
/// How many past notifications Notification Center keeps.
const MAX_NOTIFICATIONS: usize = 100;

impl Shell {
    fn new(material: Option<Rc<Material>>) -> (Self, Task<Message>) {
        autostart::run_once();
        let config = Config::load();
        let mut apps = AppIndex::load();
        // What was copied before, which outlasts the session.
        let clips = clip_store::load()
            .into_iter()
            .take(config.clipboard.max_items)
            .map(|kept| spotlight::Clip {
                content: kept.content,
                copied: kept.copied,
                source: kept.source.map(|id| apps.resolve(&id)),
            })
            .collect();
        let recents = dock::load_recents();
        let dock = dock::items(
            &config.dock.pinned,
            if config.dock.show_recents {
                &recents
            } else {
                &[]
            },
            &[],
            &BTreeMap::new(),
            &mut apps,
        );

        let (menubar_id, open_menubar) = Message::layershell_open(menubar::settings());
        dock::set_base_icon(config.dock.icon_size);
        let dock_max_icon = dock_max_icon(&config.dock);
        let (dock_id, open_dock) = Message::layershell_open(dock::settings(dock_max_icon));
        let (desktop_id, open_desktop) = Message::layershell_open(desktop::settings());

        let file_manager = config
            .file_manager
            .clone()
            .or_else(|| apps::default_app("inode/directory"))
            .unwrap_or_default();
        let trash_dir = places::trash_dir();
        let trash_count = places::trash_count(&trash_dir);
        let configured_logo = config.menubar.logo.as_deref();
        let logo_names = menubar::logo_names(configured_logo, true);
        let logo = apps.silhouette(&logo_names, [255, 255, 255, 255]);
        let logo_dark = apps.silhouette(&logo_names, [0, 0, 0, 217]);
        let system_icon = menubar::logo_names(configured_logo, false)
            .iter()
            .find_map(|name| apps.themed_icon(name));

        let spotlight_unread = apps.listed_ids();

        let shell = Self {
            surfaces: HashMap::from([(menubar_id, Surface::MenuBar), (dock_id, Surface::Dock)]),
            surface_contents: HashMap::new(),
            wallpaper_loaded: None,
            arrange_menu: None,
            genie_targets: Vec::new(),
            desktop_id,
            desktop: desktop::Desktop::load(),
            desktop_surface: None,
            desktop_material: None,
            desktop_shapes: Vec::new(),
            next_view_options: None,
            dock_surface: None,
            drag_over: None,
            widgets: widgets::Widgets::load(),
            gallery_id: None,
            gallery_material: None,
            loaded_icons: HashMap::new(),
            wallpaper: config
                .desktop
                .wallpaper
                .as_deref()
                .and_then(wallpaper::load),
            wallpaper_path: {
                wallpaper::link_for_lock_screen(config.desktop.wallpaper.as_deref());
                config.desktop.wallpaper.clone()
            },
            always_glassy_widgets: config.desktop.always_glassy_widgets,
            apps,
            pinned: config.dock.pinned,
            focused_app: None,
            clock: menubar::format_clock(),
            menubar_id,
            menubar_width: 0.0,
            menubar_material: None,
            logo,
            logo_dark,
            bar_backdrop: None,
            bar_tracks_brightness: false,
            full_screen: None,
            keyboard_layouts: None,
            control: control::Panel::default(),
            volume_set_at: None,
            input_label: None,
            system_icon,
            windows: Vec::new(),
            dock,
            dock_id,
            dock_width: 0.0,
            dock_pointer: dock::Pointer::default(),
            dock_target: 0.0,
            dock_last_frame: None,
            dock_room: dock::RoomTracker::default(),
            dock_was_dragged: false,
            recents,
            show_recents: config.dock.show_recents,
            dock_section: anim::Motion::at(0.0, DOCK_SECTION_SPRING, 0.005),
            dock_was_pinned: false,
            dock_pointer_left: false,
            last_frame: Instant::now(),
            dock_max_icon,
            dock_layout: dock::Layout::default(),
            material,
            dock_material: None,
            dock_shapes: Vec::new(),
            dock_cursor: Point::ORIGIN,
            dock_press: None,
            dock_drag: None,
            launching: HashMap::new(),
            running: BTreeMap::new(),
            fan_pointer: Point::ORIGIN,
            fan: None,
            hud: None,
            pile: Vec::new(),
            pile_pictures: Vec::new(),
            spotlight: None,
            spotlight_apps: Vec::new(),
            spotlight_unread,
            spotlight_file_icons: spotlight::FileIcons::default(),
            clips,
            clips_max: config.clipboard.max_items,
            started: Instant::now(),
            popup: None,
            next_bar_menu: None,
            submenus: Vec::new(),
            menu_hover: None,
            menu_blink: None,
            widget_tint: widgets::Tint::default(),
            next_submenu: None,
            tray: Vec::new(),
            app_menu_address: None,
            app_menu: Vec::new(),
            app_menu_titles: Vec::new(),
            alert: None,
            waiting_secret: None,
            ethernet: false,
            file_manager,
            downloads_dir: places::downloads_dir(),
            trash_dir,
            trash_count,
            place_icons: Vec::new(),
            hidden_windows: HashSet::new(),
            minimized: Vec::new(),
            dock_badges: HashMap::new(),
            badge_counts: HashMap::new(),
            screen_height: FALLBACK_SCREEN_HEIGHT,
            notifications: Vec::new(),
            notification_hovered: None,
            notification_pinned: None,
            notification_fade: notifications::HoverFade::default(),
            center_motion: notification_center::Motions::default(),
            banners_opacities: Vec::new(),
            open_groups: HashSet::new(),
            appearance_auto: config.appearance.mode == "auto",
            show_wifi: config.menubar.show_wifi,
            show_bluetooth: config.menubar.show_bluetooth,
            show_sound: config.menubar.show_sound,
            show_battery: config.menubar.show_battery,
            show_battery_percent: config.menubar.show_battery_percentage,
            battery: None,
            muted_apps: HashMap::new(),
            notifications_off: config.notifications.off.clone(),
            center_scroll: 0.0,
            banners: Banners::default(),
            banners_surface: None,
            banners_closing: false,
            banners_material: None,
            banners_shapes: Vec::new(),
            banners_region: Vec::new(),
        };
        let mut shell = shell;
        shell.update_place_icons();

        // Read now, so Control Center knows its size when it opens; the read takes a while.
        let mut opening = vec![
            open_menubar,
            open_dock,
            Task::perform(brightness::level(), Message::BrightnessRead),
            Task::perform(appearance::refresh(), Message::AppearanceApplied),
            Task::perform(icon_themes::build(), Message::IconThemesBuilt),
        ];
        if shell.wallpaper.is_some() {
            let (id, open) = Message::layershell_open(wallpaper::settings());
            shell.surfaces.insert(id, Surface::Wallpaper);
            opening.push(open);
        }
        shell.surfaces.insert(desktop_id, Surface::Desktop);
        opening.push(open_desktop);

        (shell, Task::batch(opening))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        // While things move, time goes by frames, which keeps it steady. Otherwise, it's now.
        if !self.is_moving() {
            anim::set_now(Instant::now());
        }
        let task = self.apply(message);
        self.update_dock_pin();
        self.update_notification_motion();
        self.update_panel_shapes();
        self.update_desktop_shapes();
        self.update_spotlight_shapes();
        self.update_fan_shapes();
        self.mark_changed_surfaces();
        let targets = self.update_genie_targets();
        match self.load_new_icons() {
            Some(loading) => Task::batch([task, targets, loading]),
            None => Task::batch([task, targets]),
        }
    }

    /// Tells the genie where things are in the dock, when that's changed. Not while the dock's
    /// magnified, when they're moving all the time and the pointer isn't minimizing anything.
    fn update_genie_targets(&mut self) -> Task<Message> {
        if self.dock_target != 0.0 || self.dock_pointer.amount != 0.0 {
            return Task::none();
        }
        let top = self.screen_height - dock::surface_height(self.dock_max_icon);
        let on_screen = |rect: &Rectangle| {
            (
                rect.x as i32,
                (top + rect.y) as i32,
                rect.width as i32,
                rect.height as i32,
            )
        };
        let layout = &self.dock_layout;
        let mut targets = Vec::new();
        if let Some(next) = dock::next_minimized_tile(layout) {
            targets.push((String::new(), on_screen(&next)));
        }
        for (index, minimized) in self.minimized.iter().enumerate() {
            if let Some(window) = self.windows.iter().find(|window| window.id == minimized.id)
                && let Some(rect) = layout.icons.get(dock::minimized_slot(layout, index))
            {
                targets.push((window.uuid.clone(), on_screen(rect)));
            }
        }
        if targets == self.genie_targets {
            return Task::none();
        }
        self.genie_targets = targets.clone();
        Task::future(arrange::set_genie_targets(targets)).discard()
    }

    /// Images load in the background and draw as nothing until they're ready, and the surfaces
    /// that show icons only redraw when something changes. So icons are loaded here first, and those
    /// surfaces redraw when one is ready. Without this, an icon can stay blank until it's hovered.
    fn load_new_icons(&mut self) -> Option<Task<Message>> {
        let new: Vec<image::Handle> = self
            .dock
            .iter()
            .filter_map(|item| item.app.icon.as_ref())
            .chain(self.place_icons.iter().flatten())
            .chain(self.pile_pictures.iter())
            .chain(self.desktop.icons())
            .chain(self.widgets.photo_handles())
            .filter(|handle| !self.loaded_icons.contains_key(&handle.id()))
            .cloned()
            .collect();
        if new.is_empty() {
            return None;
        }
        Some(Task::batch(new.into_iter().map(|handle| {
            let id = handle.id();
            self.loaded_icons.insert(id, None);
            iced_runtime::image::allocate(handle).map(move |loaded| Message::IconLoaded(id, loaded))
        })))
    }

    /// Asks for a redraw of the surfaces whose content changed, so that a clock tick doesn't
    /// redraw the wallpaper.
    fn mark_changed_surfaces(&mut self) {
        let surfaces = &self.surfaces;
        self.surface_contents
            .retain(|id, _| surfaces.contains_key(id));

        for (&id, &surface) in &self.surfaces {
            let content = match surface {
                Surface::Wallpaper => self.wallpaper_loaded.is_some().to_string(),
                Surface::Desktop => format!(
                    "{} {}",
                    self.desktop.content_key(self.focused_app.is_none()),
                    self.widgets.content_key(self.widget_tint.value())
                ),
                Surface::WidgetGallery => self.widgets.content_key(0.0),
                // Only there for a moment, with its hovers its own.
                Surface::ArrangeMenu => {
                    iced_layershell::redraw::mark_changed(id);
                    continue;
                }
                Surface::MenuBar => format!("{:?}", self.bar()),
                Surface::FullScreenTrigger => continue,
                Surface::Dock => {
                    format!(
                        "{:?} {:?} {:?} {:?} {:?} {} {:?}",
                        self.dock,
                        self.place_icons,
                        self.dock_layout,
                        self.minimized,
                        self.dock_badges,
                        self.fan_is_open(),
                        self.pile_pictures
                    )
                }
                // Short-lived, with live content.
                Surface::Popup
                | Surface::Alert
                | Surface::Banners
                | Surface::Spotlight
                | Surface::Hud
                | Surface::Fan => {
                    iced_layershell::redraw::mark_changed(id);
                    continue;
                }
            };
            if self.surface_contents.get(&id) != Some(&content) {
                iced_layershell::redraw::mark_changed(id);
                self.surface_contents.insert(id, content);
            }
        }
    }

    fn apply(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AppearanceApplied(applied) => {
                if let Some(applied) = applied {
                    appearance::set_accent(applied.accent);
                    // Whatever's on screen in the old color is redrawn.
                    self.surface_contents.clear();
                    if applied.icons_changed {
                        return self.reload_icons();
                    }
                }
            }
            Message::AppearanceTick => {
                return Task::perform(appearance::refresh(), Message::AppearanceApplied);
            }
            Message::IconThemesCheck => {
                return Task::perform(icon_themes::build(), Message::IconThemesBuilt);
            }
            Message::IconThemesBuilt(built) => match built {
                icon_themes::Built::Nothing => {}
                // Icon lookups only know the themes there were when the shell started, so it
                // starts again, and the session brings it back.
                icon_themes::Built::First => {
                    log::info!("restarting to use the new icons");
                    std::process::exit(icon_themes::RESTART);
                }
                icon_themes::Built::Remade => {
                    return Task::batch([
                        self.reload_icons(),
                        Task::perform(appearance::icons_remade(), Message::AppearanceApplied),
                    ]);
                }
            },
            Message::WindowsChanged(mut windows) => {
                // An alert is a window to the compositor, but not to the rest of the shell.
                let alert_active = windows
                    .iter()
                    .any(|w| w.is_focused && w.app_id.as_deref() == Some(alert::APP_ID));
                windows.retain(|w| w.app_id.as_deref() != Some(alert::APP_ID));
                if let Some(open) = &mut self.alert
                    && open.active != alert_active
                {
                    open.active = alert_active;
                    if let (Some(material), Some(target)) = (&self.material, &open.material) {
                        material.set_window_shadow(target, alert::RADIUS, alert_active);
                        material
                            .set_shapes(target, &alert::material_shapes(&open.alert, alert_active));
                    }
                }
                self.focused_app = windows
                    .iter()
                    .find(|w| w.is_focused)
                    .and_then(|w| w.app_id.as_deref())
                    .map(|app_id| self.apps.resolve(app_id));
                let focused_menu = windows
                    .iter()
                    .find(|w| w.is_focused)
                    .and_then(|w| w.menu.clone());
                if focused_menu != self.app_menu_address {
                    self.app_menu_address = focused_menu;
                    self.app_menu.clear();
                    self.app_menu_titles.clear();
                }
                self.windows = windows;
                let full_screen = self.follow_full_screen();
                self.note_window_processes();
                self.note_recents();
                self.dock = self.dock_items();
                let captures = self.sync_minimized();
                // A window showing up is what ends a launch bounce.
                let dock = &self.dock;
                self.launching.retain(|app_id, _| {
                    !dock
                        .iter()
                        .any(|item| item.app.id == *app_id && item.has_windows())
                });
                return Task::batch([self.relayout_dock(), captures, full_screen]);
            }
            Message::BatteryChanged(battery) => {
                // An open battery menu shows what it was opened with; it's quick to open again.
                self.battery = battery;
            }
            Message::LowPowerMode(on) => {
                if let Some(battery) = &mut self.battery {
                    battery.low_power = Some(on);
                }
                return Task::batch([
                    Task::future(battery::set_low_power(on)).discard(),
                    self.close_popup(),
                ]);
            }
            Message::KeyboardLayoutsChanged(layouts) => {
                self.input_label = (layouts.names.len() > 1)
                    .then(|| layouts.names.get(layouts.current))
                    .flatten()
                    .map(|name| menubar::input_label(name));
                self.control.keyboard = Some(layouts.clone());
                self.keyboard_layouts = Some(layouts);
            }
            // A popup's grab only closes it for clicks on other apps. The wallpaper, the bar and the
            // dock are ours, so they have to do it themselves.
            Message::SurfacePressed(id) => {
                let panel_open = matches!(
                    self.popup.as_ref().map(|popup| &popup.content),
                    Some(PopupContent::NotificationCenter | PopupContent::ControlCenter)
                );
                // Pressing outside a Control Center module's detail goes back to the modules, the way
                // a press beside it does. That one's the detail's own, and only takes the left button.
                if self.control.detail.is_some()
                    && matches!(
                        self.popup.as_ref().map(|popup| &popup.content),
                        Some(PopupContent::ControlCenter)
                    )
                    && self.surfaces.get(&id) == Some(&Surface::Popup)
                {
                    let going_back = self.control.morph.is_some_and(|morph| !morph.is_opening());
                    if going_back {
                        return Task::none();
                    }
                    return self.apply(Message::ControlDetail(None));
                }
                // The surfaces of these panels are mostly empty.
                if matches!(
                    self.surfaces.get(&id),
                    Some(Surface::Wallpaper | Surface::Desktop | Surface::MenuBar | Surface::Dock)
                ) || (panel_open && self.surfaces.get(&id) == Some(&Surface::Popup))
                {
                    return self.close_popup();
                }
            }
            Message::NextLayout => {
                if let Some(layouts) = &self.keyboard_layouts {
                    let next = (layouts.current + 1) % layouts.names.len().max(1);
                    return Task::future(kwin::switch_layout(next)).discard();
                }
            }
            Message::Screenshot => {
                apps::spawn(vec![String::from("spectacle")]);
                return self.close_popup();
            }
            Message::LockScreen => {
                menubar::SystemAction::Lock.run();
                return self.close_popup();
            }
            Message::BrightnessChanged(level) => {
                self.control.brightness = Some(level);
                brightness::set_level(level);
            }
            Message::BrightnessReleased => {
                if let Some(level) = self.control.brightness {
                    brightness::set_level(level);
                }
            }
            Message::BrightnessRead(level) => self.control.brightness = level,
            Message::VolumeChanged(level) => {
                if let Some(volume) = &mut self.control.volume {
                    volume.level = level;
                    // Turning the volume up unmutes, like on macOS.
                    if std::mem::replace(&mut volume.muted, false) {
                        audio::set_muted(false);
                    }
                }
                // Dragging changes the level many times a second; wpctl needn't keep up.
                if self
                    .volume_set_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_millis(40))
                {
                    self.volume_set_at = Some(Instant::now());
                    audio::set_level(level);
                }
            }
            Message::VolumeReleased => {
                if let Some(volume) = self.control.volume {
                    audio::set_level(volume.level);
                }
            }
            Message::ToggleMute => {
                if let Some(volume) = &mut self.control.volume {
                    volume.muted = !volume.muted;
                    audio::set_muted(volume.muted);
                }
            }
            Message::ToggleWifi => {
                if let Some(wifi) = &mut self.control.wifi {
                    wifi.enabled = !wifi.enabled;
                    let task = network::set_wifi_enabled(wifi.enabled);
                    return Task::perform(task, |_| Message::ControlRefresh);
                }
            }
            Message::ToggleBluetooth => {
                if let Some(bluetooth) = &mut self.control.bluetooth {
                    bluetooth.powered = !bluetooth.powered;
                    let task = bluetooth::set_powered(bluetooth.clone(), bluetooth.powered);
                    return Task::perform(task, |_| Message::ControlRefresh);
                }
            }
            Message::Media(control) => {
                if let Some(track) = &self.control.track {
                    let task = media::control(track.clone(), control);
                    return Task::perform(task, |_| Message::ControlRefresh);
                }
            }
            Message::ControlRefresh => {
                return Task::perform(control::status(), Message::ControlStatus);
            }
            Message::BarStatusRefresh => {
                return Task::perform(control::bar_status(), |(wifi, bluetooth, volume)| {
                    Message::BarStatus(wifi, bluetooth, volume)
                });
            }
            Message::BarStatus(wifi, bluetooth, volume) => {
                self.control.wifi = wifi;
                self.control.bluetooth = bluetooth;
                self.control.volume = volume;
            }
            Message::StatusMenuReady(title, status) => {
                let detail = match title {
                    menubar::Title::Wifi => control::Detail::Wifi,
                    menubar::Title::Bluetooth => control::Detail::Bluetooth,
                    _ => control::Detail::Sound,
                };
                let shown = self.apply(Message::ControlStatus(status));
                let size = control::status_size(&self.control, detail);
                let open = self.open_bar_popup(title, size, PopupContent::Status(detail));
                return Task::batch([shown, open]);
            }
            Message::ControlFrame => {
                if let Some(morph) = self.control.morph
                    && morph.is_done()
                {
                    self.control.morph = None;
                    if !morph.is_opening() {
                        self.control.detail = None;
                    }
                }
            }
            Message::ControlDetail(None) => {
                if self.control.detail.is_some() {
                    self.control.morph = Some(control::Morph::start(&self.control, false));
                }
                return Task::done(Message::ControlRefresh);
            }
            Message::ControlDetail(detail) => {
                self.control.morph = Some(control::Morph::start(&self.control, true));
                self.control.detail = detail;
                let refresh = Task::done(Message::ControlRefresh);
                if detail == Some(control::Detail::Wifi) {
                    let scan = Task::future(network::scan()).discard();
                    return Task::batch([refresh, scan]);
                }
                return refresh;
            }
            Message::ControlSettings(page) => {
                apps::spawn(vec![String::from("kcmshell6"), page.to_owned()]);
                return self.close_popup();
            }
            Message::ControlLayout(index) => {
                return Task::future(kwin::switch_layout(index)).discard();
            }
            Message::ControlOutput(index) => {
                if let Some(output) = self.control.outputs.get(index) {
                    output.select();
                }
                return Task::done(Message::ControlRefresh);
            }
            Message::WifiConnect(ssid) => {
                return Task::perform(network::connect(ssid), |_| Message::ControlRefresh);
            }
            Message::BluetoothConnect(device, connect) => {
                let task = bluetooth::set_connected(device, connect);
                return Task::perform(task, |_| Message::ControlRefresh);
            }
            Message::ControlStatus(status) => {
                let art = status.track.as_ref().and_then(|track| track.art.clone());
                if self
                    .control
                    .track
                    .as_ref()
                    .and_then(|track| track.art.as_ref())
                    != art.as_ref()
                {
                    self.control.art = art.map(image::Handle::from_path);
                }
                self.control.wifi = status.wifi;
                self.control.bluetooth = status.bluetooth;
                self.control.track = status.track;
                self.control.networks = status.networks;
                self.control.devices = status.devices;
                self.control.outputs = status.outputs;
            }
            Message::SwitchLayout(index) => {
                let switch = Task::future(kwin::switch_layout(index)).discard();
                return Task::batch([self.close_popup(), switch]);
            }
            Message::Tick => {
                self.clock = menubar::format_clock();
                let trash_count = places::trash_count(&self.trash_dir);
                if trash_count != self.trash_count {
                    self.trash_count = trash_count;
                    self.update_place_icons();
                }
            }
            Message::DockPressed(app_id) => {
                self.dock_press = Some(DockPress {
                    app_id,
                    at: self.dock_cursor,
                });
            }
            Message::PointerMoved(id, _) | Message::PointerEntered(id)
                if self.is_full_screen_bar(id) =>
            {
                self.full_screen_pointer_on(id);
            }
            // The dock's own pointer handling goes on as well, below.
            Message::PointerEntered(id) if id == self.dock_id && self.full_screen.is_some() => {
                self.full_screen_pointer_on(id);
            }
            Message::PointerLeft(id)
                if self.is_full_screen_bar(id)
                    || (id == self.dock_id && self.full_screen.is_some()) =>
            {
                return self.full_screen_pointer_off(id);
            }
            Message::PointerLeft(_) | Message::PointerEntered(_) => {}
            Message::FullScreenCheck => {
                // A menu, or the fan out of a folder in the dock, which the pointer's on instead.
                let menu_open = self.popup.is_some() || self.fan.is_some();
                if let Some(open) = &mut self.full_screen {
                    let mut later = false;
                    for reveal in [&mut open.top, &mut open.bottom] {
                        if reveal.is_shown() && !reveal.is_hovered() {
                            if menu_open {
                                later = true;
                            } else if reveal.should_hide() {
                                reveal.hide();
                            }
                        }
                    }
                    if !open.top.is_shown() {
                        open.lights_hovered = false;
                    }
                    if later {
                        return full_screen_check_later();
                    }
                }
            }
            Message::FullScreenFrame => return self.place_full_screen_bars(),
            Message::FullScreenLightsHovered(over) => {
                if let Some(open) = &mut self.full_screen {
                    open.lights_hovered = over;
                }
            }
            Message::FullScreenClose => {
                if let Some(open) = &self.full_screen {
                    kwin::close_window(open.window);
                }
            }
            Message::FullScreenExit => {
                if let Some(open) = &self.full_screen {
                    kwin::leave_full_screen(open.window);
                }
            }
            Message::PointerMoved(id, point) if id == self.dock_id => {
                self.full_screen_pointer_on(id);
                self.dock_cursor = point;
                if let Some(drag) = &mut self.dock_drag {
                    drag.pointer = point;
                    return self.relayout_dock();
                }
                if let Some(press) = &self.dock_press
                    && press.at.distance(point) > DRAG_THRESHOLD
                {
                    return self.start_drag();
                }
            }
            Message::PointerReleased(id) if id == self.dock_id => {
                let Some(press) = self.dock_press.take() else {
                    return Task::none();
                };
                match self.dock_drag.take() {
                    Some(drag) => {
                        self.drop_dragged(&drag);
                        return self.relayout_dock();
                    }
                    None => return self.activate(&press.app_id),
                }
            }
            Message::DockPointerMoved(_) if self.dock_drag.is_some() => {}
            Message::DockPointerMoved(_) if self.dock_pinned() => {
                self.dock_pointer_left = false;
            }
            Message::DockPointerLeft if self.dock_pinned() => {
                self.dock_pointer_left = true;
            }
            Message::DockPointerMoved(point) => {
                self.dock_pointer.base_x = Some(dock::base_x(&self.dock_layout, point.x));
                self.dock_pointer.hovered = dock::item_at(&self.dock_layout, point);
                if self.dock_max_icon > dock::base_icon() {
                    self.dock_target = 1.0;
                }
                return self.relayout_dock();
            }
            Message::DockPointerLeft => {
                self.dock_pointer.hovered = None;
                self.dock_target = 0.0;
                return self.relayout_dock();
            }
            Message::DockFrame(now) => {
                self.launching
                    .retain(|_, started| now.duration_since(*started) < LAUNCH_TIMEOUT);

                let pointer = &mut self.dock_pointer;
                if pointer.amount == self.dock_target {
                    self.dock_last_frame = None;
                } else {
                    // Exponential easing, independent of how regularly frames arrive.
                    let dt = self
                        .dock_last_frame
                        .map_or(0.016, |last| (now - last).as_secs_f32())
                        .min(0.05);
                    self.dock_last_frame = Some(now);

                    pointer.amount +=
                        (self.dock_target - pointer.amount) * (1.0 - (-dt / 0.06).exp());
                    if (self.dock_target - pointer.amount).abs() < 0.005 {
                        pointer.amount = self.dock_target;
                    }
                    if pointer.amount == 0.0 {
                        pointer.base_x = None;
                    }
                }
                return self.relayout_dock();
            }
            Message::SurfaceOpened(id, _)
                if self.surfaces.get(&id) == Some(&Surface::Spotlight) =>
            {
                let focus =
                    iced_runtime::widget::operation::focus(spotlight::Spotlight::field_id());
                return Task::batch([self.reach_surface(id), focus]);
            }
            Message::SurfaceOpened(id, _) if self.surfaces.get(&id) == Some(&Surface::Fan) => {
                return self.reach_surface(id);
            }
            Message::SurfaceOpened(id, _) if self.surfaces.get(&id) == Some(&Surface::Hud) => {
                // It's only to look at.
                let input = Task::done(Message::SetInputRegion {
                    id,
                    callback: ActionCallback::new(|_| {}),
                });
                return Task::batch([self.reach_surface(id), input]);
            }
            Message::SurfaceOpened(id, size) if id == self.dock_id => {
                self.dock_width = size.width;
                let relayout = self.relayout_dock();

                // Attaching twice is a protocol error, so only ever do it once.
                if self.dock_material.is_none() {
                    return Task::batch([relayout, self.reach_surface(id)]);
                }
                return relayout;
            }
            Message::SurfaceOpened(id, size) | Message::SurfaceResized(id, size)
                if id == self.desktop_id =>
            {
                self.screen_height = size.height;
                self.desktop.set_size(size);
                let reach = if self.desktop_surface.is_none() {
                    Task::batch([
                        self.reach_surface(id),
                        Task::perform(tokio::time::sleep(desktop::KEYBOARD_AFTER), move |()| {
                            Message::KeyboardInteractivityChange {
                                id,
                                keyboard_interactivity: KeyboardInteractivity::OnDemand,
                            }
                        }),
                    ])
                } else {
                    Task::none()
                };
                return Task::batch([
                    reach,
                    Task::done(Message::DesktopRefresh),
                    Task::done(Message::FetchWeather),
                    self.load_widget_photos(),
                ]);
            }
            Message::DesktopRefresh => {
                let pile = self.refresh_pile();
                let listed =
                    Task::perform(desktop::list(self.desktop.folder()), Message::DesktopListed);
                return Task::batch([pile, listed]);
            }
            Message::PileThumbnail(path, Some(picture)) => {
                if let Some(index) = self.pile.iter().position(|known| *known == path)
                    && let Some(slot) = self.pile_pictures.get_mut(index)
                {
                    *slot = picture;
                }
            }
            Message::PileThumbnail(_, None) => {}
            Message::DesktopListed(entries) => {
                let apps = &mut self.apps;
                let wanted = self.desktop.merge(entries, |entry| {
                    if entry.drive.is_some() {
                        return apps
                            .themed_icon("drive-removable-media")
                            .or_else(|| apps.themed_icon("drive-harddisk"));
                    }
                    apps.themed_icon(places::icon_name(&places::Download {
                        path: entry.path.clone(),
                        name: entry.name.clone(),
                        is_dir: entry.is_dir,
                    }))
                });
                let thumbnails = Task::batch(wanted.into_iter().map(|path| {
                    let reading = path.clone();
                    Task::perform(desktop::thumbnail(reading), move |thumbnail| {
                        Message::DesktopThumbnail(path.clone(), thumbnail)
                    })
                }));
                if self.desktop.take_rename_focus() {
                    let field = desktop::rename_field();
                    let focus = iced_runtime::widget::operation::focus(field.clone());
                    let select = iced_runtime::widget::operation::select_all(field);
                    return Task::batch([thumbnails, focus.chain(select)]);
                }
                return thumbnails;
            }
            Message::DesktopThumbnail(path, Some(thumbnail)) => {
                self.desktop.set_thumbnail(&path, thumbnail);
            }
            Message::DesktopThumbnail(_, None) => {}
            Message::PointerMoved(id, point) if id == self.desktop_id => {
                self.widgets.pointer_moved(point);
                if !self.widgets.is_dragging() && self.desktop.pointer_moved(point) {
                    // A drag and drop needs the press's serial, which the event loop has.
                    return iced_runtime::window::drag(self.desktop_id)
                        .chain(Task::done(Message::DesktopDragReady));
                }
            }
            Message::DesktopDragReady => {
                let serial = iced_layershell::toplevel::take_drag_serial();
                match (serial, &self.desktop_surface) {
                    (Some(serial), Some(surface)) => {
                        let icon = dnd::drag_icon(&self.desktop.selected_icons());
                        let uris = dnd::uris_of(&self.desktop.selection());
                        dnd::start_drag(surface, serial, &uris, icon);
                    }
                    _ => self.desktop.drag_ended(),
                }
            }
            Message::Dnd(event) => return self.handle_drag(event),
            Message::Arrange(arrange::Event::Hovered(x, y)) => {
                if let Some(open) = &mut self.arrange_menu {
                    open.over_light = true;
                    return Task::none();
                }
                let at = Point::new(x as f32, y as f32);
                return Task::perform(arrange::window_under_cursor(), move |found| {
                    Message::ArrangeFound(at, found)
                });
            }
            Message::Arrange(arrange::Event::Left) => {
                if let Some(open) = &mut self.arrange_menu {
                    open.over_light = false;
                }
                return self.arrange_pointer_left();
            }
            Message::ArrangeFound(_, None) => {}
            Message::ArrangeFound(at, Some((window, x, y))) => {
                if self.arrange_menu.is_some() {
                    return Task::none();
                }
                let settings = arrange::settings(x + at.x as i32, y + at.y as i32);
                let (id, open) = Message::layershell_open(settings);
                self.surfaces.insert(id, Surface::ArrangeMenu);
                self.arrange_menu = Some(arrange::Open {
                    surface: id,
                    window,
                    material: None,
                    submenu: false,
                    over_menu: false,
                    over_light: true,
                    leaves: 0,
                });
                return open;
            }
            Message::SurfaceOpened(id, _)
                if self.surfaces.get(&id) == Some(&Surface::ArrangeMenu) =>
            {
                return Task::batch([self.reach_surface(id), self.update_arrange_input()]);
            }
            Message::ArrangeOverMenu(over) => {
                if let Some(open) = &mut self.arrange_menu {
                    open.over_menu = over;
                }
                if !over {
                    return self.arrange_pointer_left();
                }
            }
            Message::ArrangeSubmenu(submenu) => {
                if let Some(open) = &mut self.arrange_menu
                    && open.submenu != submenu
                {
                    open.submenu = submenu;
                    if let (Some(material), Some(target)) = (&self.material, &open.material) {
                        material.set_shapes(target, &arrange::material_shapes(submenu));
                    }
                    return self.update_arrange_input();
                }
            }
            Message::ArrangeChosen(layout) => {
                let Some(open) = &self.arrange_menu else {
                    return Task::none();
                };
                let close = iced_runtime::window::close(open.surface);
                let choice = arrange::arrange(open.window.clone(), layout);
                return Task::batch([close, Task::future(choice).discard()]);
            }
            Message::ArrangeLeft(leaves) => {
                if let Some(open) = &self.arrange_menu
                    && open.leaves == leaves
                    && !open.over_menu
                    && !open.over_light
                {
                    return iced_runtime::window::close(open.surface);
                }
            }
            Message::DesktopMetrics(metrics) => self.desktop.set_metrics(metrics),
            Message::DesktopReceived(names, at) => {
                self.desktop.place_dropped(&names, at);
                return Task::done(Message::DesktopRefresh);
            }
            Message::PointerReleased(id) if id == self.desktop_id => {
                self.widgets.released();
                self.desktop.released();
            }
            Message::Widget(action) => {
                let close = self.close_popup();
                return Task::batch([close, self.run_widget_action(action)]);
            }
            Message::WidgetPhotoChosen(id, Some(path)) => {
                self.widgets.set_photo(id, path);
                return self.load_widget_photos();
            }
            Message::WidgetPhotoChosen(_, None) => {}
            Message::WidgetPhotoLoaded(path, Some(picture)) => {
                self.widgets.set_photo_picture(path, picture);
            }
            Message::WidgetPhotoLoaded(_, None) => {}
            Message::FetchWeather => {
                if self.widgets.has_weather()
                    && let Some(city) = &self.widgets.city
                {
                    return Task::perform(
                        widgets::fetch_weather(city.clone()),
                        Message::WeatherFetched,
                    );
                }
            }
            Message::WeatherFetched(Some(weather)) => self.widgets.weather = Some(weather),
            // Whatever it last said is still the best guess, but ask again soon.
            Message::WeatherFetched(None) => {
                return Task::perform(tokio::time::sleep(WEATHER_RETRY), |()| {
                    Message::FetchWeather
                });
            }
            Message::AddingModifiers(adding) => self.desktop.adding = adding,
            Message::DesktopPressed => {
                // A click that closes a menu doesn't do anything else.
                if self.popup.is_some() {
                    return self.close_popup();
                }
                let finished_rename = self.finish_desktop_rename();
                let (action, on_widget) = self.widgets.pressed();
                if let Some(action) = action {
                    return Task::batch([finished_rename, self.run_widget_action(action)]);
                }
                if on_widget {
                    return finished_rename;
                }
                return match self.desktop.pressed() {
                    desktop::Pressed::Open => {
                        self.open_desktop_selection();
                        finished_rename
                    }
                    desktop::Pressed::Nothing => finished_rename,
                };
            }
            Message::DesktopRightPressed => {
                let finished_rename = self.finish_desktop_rename();
                let entries = self
                    .widgets
                    .context_menu()
                    .unwrap_or_else(|| self.desktop.context_menu());
                let size = menu::size(&entries);
                let (x, y) = self.desktop.pointer();
                let settings = IcedNewPopupSettings::new(self.desktop_id, size, (x, y, 1, 1))
                    .anchor(PopupAnchor::BottomRight)
                    .gravity(PopupGravity::BottomRight);
                let open = self.open_popup(None, settings, PopupContent::Menu(entries), 0.0, None);
                return Task::batch([finished_rename, open]);
            }
            Message::Desktop(action) => {
                let close = self.close_popup();
                return Task::batch([close, self.run_desktop_action(action)]);
            }
            Message::DesktopRenameChanged(value) => {
                if let Some(rename) = &mut self.desktop.rename {
                    rename.value = value;
                }
            }
            Message::WallpaperChosen(None) => {}
            Message::WallpaperChosen(Some(path)) => {
                if let Err(err) = config::save_wallpaper(&path) {
                    log::warn!("can't save the wallpaper: {err}");
                }
                self.wallpaper_path = Some(path.to_string_lossy().into_owned());
                wallpaper::link_for_lock_screen(self.wallpaper_path.as_deref());
                return self.show_wallpaper(image::Handle::from_path(path));
            }
            Message::QuitGraceOver(app_id) => {
                // A window still open is the app asking something, like whether to save.
                if let Some(item) = self
                    .dock
                    .iter()
                    .find(|item| item.app.id == app_id && !item.has_windows())
                {
                    item.processes()
                        .iter()
                        .for_each(|process| process.terminate());
                }
            }
            Message::CheckRunning => {
                let known = self.running.clone();
                let programs = self
                    .pinned
                    .iter()
                    .filter_map(|id| {
                        let app = self.apps.resolve(id);
                        let program = running::program_of(app.command.as_deref()?)?;
                        Some((id.clone(), program))
                    })
                    .collect();
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || running::check(known, programs))
                            .await
                            .unwrap_or_default()
                    },
                    Message::RunningChecked,
                );
            }
            Message::RunningChecked(running) => {
                let before = std::mem::replace(&mut self.running, running);
                // Windows that came up while that was being worked out.
                self.note_window_processes();
                if before != self.running {
                    self.dock = self.dock_items();
                    return self.relayout_dock();
                }
            }
            Message::HideWindows(ids) => ids.into_iter().for_each(kwin::minimize_window),
            Message::OpenSettings => {
                let close = self.close_popup();
                match self
                    .windows
                    .iter()
                    .find(|window| window.app_id.as_deref() == Some(crate::settings::APP_ID))
                {
                    Some(window) => kwin::focus_window(window.id),
                    None => match std::env::current_exe() {
                        Ok(exe) => apps::spawn(vec![
                            exe.to_string_lossy().into_owned(),
                            String::from("settings"),
                        ]),
                        Err(err) => log::warn!("can't find the Settings app: {err}"),
                    },
                }
                return close;
            }
            Message::Arrange(arrange::Event::ConfigChanged) => return self.reload_config(),
            Message::Arrange(arrange::Event::ClearClipboard) => {
                self.clips.clear();
                self.keep_clips();
            }
            Message::Arrange(arrange::Event::MediaKey(key)) => return self.media_key(&key),
            Message::BrightnessStep(level, step) => {
                self.control.brightness = level;
                return self.step_brightness(step);
            }
            Message::HudFrame => {
                if let Some(open) = &self.hud
                    && open.frame()
                {
                    return iced_runtime::window::close(open.id);
                }
            }
            Message::Arrange(arrange::Event::ToggleSpotlight) | Message::SpotlightToggle => {
                return self.toggle_spotlight();
            }
            Message::SpotlightClose => return self.close_spotlight(),
            Message::SpotlightHotkey(number) => {
                let mode = match number {
                    1 => spotlight::Mode::Apps,
                    2 => spotlight::Mode::Files,
                    3 => spotlight::Mode::Actions,
                    _ => spotlight::Mode::Clipboard,
                };
                if self.spotlight.is_some() {
                    return Task::done(Message::SpotlightMode(mode));
                }
            }
            Message::FanMoved(point) => {
                self.fan_pointer = point;
                if let Some(open) = &mut self.fan {
                    if open.is_closing() {
                        return Task::none();
                    }
                    open.hovered = open.row_at(point);
                    if let Some(index) = open.hovered {
                        open.prepare_hovered(index);
                    }
                    if open.should_drag(point) {
                        open.dragging = true;
                        return iced_runtime::window::drag(open.id)
                            .chain(Task::done(Message::FanDragReady));
                    }
                }
            }
            Message::FanPressed => {
                let point = self.fan_pointer;
                if let Some(open) = &mut self.fan {
                    match open.row_at(point) {
                        Some(index) => open.pressed = Some((index, point)),
                        None => open.close(),
                    }
                }
            }
            Message::FanReleased => {
                let Some(open) = &mut self.fan else {
                    return Task::none();
                };
                let pressed = open.pressed.take();
                if open.dragging {
                    return Task::none();
                }
                let Some((index, _)) = pressed else {
                    return Task::none();
                };
                let path = open.rows.get(index).map(|row| row.path.clone());
                open.close();
                match path {
                    Some(path) => {
                        apps::spawn(vec![String::from("xdg-open"), path.display().to_string()]);
                    }
                    None => self.open_place(Place::Downloads),
                }
            }
            Message::FanRightPressed => {
                let point = self.fan_pointer;
                let Some(open) = &mut self.fan else {
                    return Task::none();
                };
                if open.is_closing() {
                    return Task::none();
                }
                let Some(entries) = open.row_at(point).and_then(|index| open.file_menu(index))
                else {
                    return Task::none();
                };
                // It isn't the start of a press or a drag.
                open.pressed = None;
                let size = menu::size(&entries);
                let anchor = (point.x as i32, point.y as i32, 1, 1);
                let settings = IcedNewPopupSettings::new(open.id, size, anchor)
                    .anchor(PopupAnchor::BottomRight)
                    .gravity(PopupGravity::BottomRight);
                return self.open_popup(None, settings, PopupContent::Menu(entries), 0.0, None);
            }
            Message::FanFile(action, path) => {
                let close = self.close_popup();
                if let Some(open) = &mut self.fan {
                    open.close();
                }
                let done = match action {
                    fan::FileAction::Open => {
                        apps::spawn(vec![String::from("xdg-open"), path.display().to_string()]);
                        Task::none()
                    }
                    fan::FileAction::ShowInFiles => {
                        Task::perform(fan::show_in_files(path), |shown| {
                            // Its folder, at least.
                            if shown {
                                Message::Nothing
                            } else {
                                Message::DockMenu(dock::MenuAction::OpenPlace(Place::Downloads))
                            }
                        })
                    }
                    fan::FileAction::GetInfo => {
                        Task::future(desktop::get_info(vec![path])).discard()
                    }
                    fan::FileAction::MoveToTrash => {
                        if let Err(err) = places::move_to_trash(&path) {
                            log::warn!("can't move {} to the trash: {err}", path.display());
                        }
                        // The trash can fills right away, rather than at the next tick.
                        Task::done(Message::Tick)
                    }
                };
                return Task::batch([close, done]);
            }
            Message::FanFrame => {
                if let Some(open) = &self.fan
                    && open.has_closed()
                {
                    return iced_runtime::window::close(open.id);
                }
            }
            Message::FanThumbnail(path, picture) => {
                if let (Some(open), Some(picture)) = (&mut self.fan, picture)
                    && let Some(row) = open.rows.iter_mut().find(|row| row.path == path)
                {
                    row.picture = Some(picture);
                }
            }
            Message::FanDragReady => return self.start_fan_drag(),
            Message::SpotlightTyped(query) => {
                if let Some(open) = &mut self.spotlight {
                    let was_expanded = open.expanded();
                    open.query = query;
                    open.selected = 0;
                    let refocus = if open.expanded() == was_expanded {
                        Task::none()
                    } else {
                        spotlight::Spotlight::refocus()
                    };
                    if let Some(due) = open.wants_file_search() {
                        let search =
                            Task::perform(tokio::time::sleep(SPOTLIGHT_TYPING), move |()| {
                                Message::SpotlightSearchDue(due.clone())
                            });
                        return Task::batch([refocus, search]);
                    }
                    return refocus;
                }
            }
            Message::SpotlightSearchDue(query) => {
                let Some(open) = &self.spotlight else {
                    return Task::none();
                };
                if open.query.trim() == query {
                    return Task::perform(
                        async move {
                            let searched = query.clone();
                            let found = tokio::task::spawn_blocking(move || {
                                spotlight::search_files(&searched)
                            })
                            .await
                            .unwrap_or_default();
                            (query, found)
                        },
                        |(query, found)| Message::SpotlightFilesFound(query, found),
                    );
                }
            }
            Message::SpotlightFilesFound(query, found) => {
                if let Some(open) = &mut self.spotlight
                    && open.query.trim() == query
                {
                    open.files = found;
                    open.files_query = query;
                }
            }
            Message::SpotlightRecentFiles(recent) => {
                if let Some(open) = &mut self.spotlight {
                    open.recent_files = recent;
                }
            }
            Message::SpotlightMode(mode) => {
                if let Some(open) = &mut self.spotlight {
                    open.content_changed();
                    open.mode = mode;
                    open.query.clear();
                    open.category = None;
                    open.selected = 0;
                }
                return spotlight::Spotlight::refocus();
            }
            Message::SpotlightCategory(category) => {
                if let Some(open) = &mut self.spotlight {
                    open.category = category;
                    open.selected = 0;
                }
                return iced_runtime::widget::operation::focus(spotlight::Spotlight::field_id());
            }
            Message::SpotlightHovered(index) => {
                if let Some(open) = &mut self.spotlight {
                    open.selected = index;
                }
            }
            Message::SpotlightMoved(rows, across) => {
                if let Some(open) = &self.spotlight {
                    let count = open.hits(&self.spotlight_apps, &self.clips).len();
                    if let Some(open) = &mut self.spotlight {
                        open.select_by(rows, across, count);
                        return iced_runtime::widget::operation::snap_to(
                            spotlight::Spotlight::results_id(),
                            iced_widget::scrollable::RelativeOffset {
                                x: None,
                                y: Some(open.scroll_share(count)),
                            },
                        );
                    }
                }
            }
            Message::SpotlightChosen(index) => return self.choose_in_spotlight(index),
            Message::SpotlightLoadApps => {
                for _ in 0..SPOTLIGHT_APPS_AT_ONCE {
                    let Some(id) = self.spotlight_unread.pop() else {
                        break;
                    };
                    let app = self.apps.resolve(&id);
                    if app.command.is_some()
                        && !app.unlisted
                        && !self.spotlight_apps.iter().any(|known| known.id == app.id)
                    {
                        self.spotlight_apps.push(app);
                    }
                }
            }
            Message::SpotlightTick => {}
            Message::Copied(content) => {
                spotlight::remember_clip(
                    &mut self.clips,
                    spotlight::Clip {
                        content,
                        copied: chrono::Local::now(),
                        source: self.focused_app.clone(),
                    },
                    self.clips_max,
                );
                self.keep_clips();
            }
            Message::SurfaceOpened(id, size)
                if self.surfaces.get(&id) == Some(&Surface::Wallpaper) =>
            {
                self.screen_height = size.height;
                if let Some(handle) = &self.wallpaper {
                    return iced_runtime::image::allocate(handle.clone())
                        .map(Message::WallpaperLoaded);
                }
            }
            Message::IconLoaded(id, loaded) => {
                match loaded {
                    Ok(allocation) => {
                        self.loaded_icons.insert(id, Some(allocation));
                    }
                    Err(err) => log::warn!("can't load an icon: {err}"),
                }
                iced_layershell::redraw::mark_changed(self.dock_id);
                iced_layershell::redraw::mark_changed(self.desktop_id);
            }
            Message::WallpaperLoaded(Ok(allocation)) => self.wallpaper_loaded = Some(allocation),
            Message::WallpaperLoaded(Err(err)) => log::error!("can't load the wallpaper: {err}"),
            Message::SurfaceOpened(id, size) if id == self.menubar_id => {
                self.menubar_width = size.width;
                if self.menubar_material.is_none() {
                    return self.reach_surface(id);
                }
            }
            Message::SurfaceResized(id, size) if id == self.menubar_id => {
                self.menubar_width = size.width;
                self.set_menubar_shapes();
            }
            Message::SurfaceResized(id, size) if id == self.dock_id => {
                if size.width != self.dock_width {
                    self.dock_width = size.width;
                    return self.relayout_dock();
                }
            }
            Message::SurfaceReached(id, Some(surface))
                if self.fan.as_ref().is_some_and(|open| open.id == id) =>
            {
                if let Some(open) = &mut self.fan {
                    open.surface = Some(surface.clone());
                }
                self.attach_material(id, surface);
            }
            Message::SurfaceReached(id, Some(surface)) if id == self.desktop_id => {
                self.desktop_surface = Some(surface.clone());
                self.attach_material(id, surface);
            }
            Message::SurfaceReached(id, Some(surface)) => {
                if id == self.dock_id {
                    self.dock_surface = Some(surface.id());
                }
                self.attach_material(id, surface);
            }
            Message::SurfaceReached(_, None) => log::warn!("can't reach a wl_surface; no material"),
            // A new popup can only take the grab once the old one is destroyed, so another title's
            // menu opens when the open one reports closed.
            Message::BarPressed(title) => {
                // Like on macOS, Control Center's own icon goes back from a module's detail first, and
                // closes it after that.
                let going_back = self.control.morph.is_some_and(|morph| !morph.is_opening());
                if title == menubar::Title::ControlCenter
                    && matches!(
                        self.popup.as_ref().map(|popup| &popup.content),
                        Some(PopupContent::ControlCenter)
                    )
                    && self.control.detail.is_some()
                    && !going_back
                {
                    return self.apply(Message::ControlDetail(None));
                }
                if let Some(open) = self.popup.as_ref().and_then(|popup| popup.title)
                    && open != title
                {
                    self.next_bar_menu = Some(title);
                    return self.close_popup();
                }
                return self.open_bar_menu(title);
            }
            // Like on macOS, pointing at another app menu while one's open switches to it. The
            // items on the right take a click.
            Message::BarEntered(title) => {
                let is_menu = |title| {
                    matches!(
                        title,
                        menubar::Title::System | menubar::Title::App | menubar::Title::AppMenu(_)
                    )
                };
                if is_menu(title)
                    && let Some(open) = self.popup.as_ref().and_then(|popup| popup.title)
                    && open != title
                    && is_menu(open)
                {
                    self.next_bar_menu = Some(title);
                    return self.close_popup();
                }
            }
            Message::System(action) => {
                let close = self.close_popup();
                let Some(confirmation) = action.confirmation() else {
                    action.run();
                    return close;
                };
                let alert = Alert {
                    title: confirmation.title.to_owned(),
                    detail: String::from("Unsaved changes in open apps may be lost."),
                    confirm: confirmation.confirm.to_owned(),
                    on_confirm: Message::SystemConfirmed(action),
                    icon: self
                        .system_icon
                        .clone()
                        .map_or(alert::Icon::None, alert::Icon::Image),
                    field: None,
                };
                return Task::batch([close, self.open_alert(alert)]);
            }
            Message::SystemConfirmed(action) => action.run(),
            Message::BarBrightness => {
                if let (Some(material), Some(target)) = (&self.material, &self.menubar_material)
                    && let Some(luminance) = material.brightness(target)
                {
                    let backdrop = menubar::Backdrop::from_luminance(self.bar_backdrop, luminance);
                    if self.bar_backdrop != Some(backdrop) {
                        log::debug!("menu bar backdrop {backdrop:?} at luminance {luminance}");
                        self.bar_backdrop = Some(backdrop);
                    }
                }
            }
            Message::DockMenuRequested(app_id) => return self.open_dock_menu(&app_id),
            Message::DockMenu(action) => {
                let close = self.close_popup();
                return Task::batch([close, self.run_dock_action(action)]);
            }
            Message::AlertFieldChanged(value) => {
                if let Some(field) = self.alert_field() {
                    field.value = value;
                }
            }
            Message::AlertFieldShown(visible) => {
                if let Some(field) = self.alert_field() {
                    field.visible = visible;
                }
            }
            Message::WifiAgent(network::AgentEvent::Asks(request)) => {
                // After a wrong password it asks again right away, while the last alert is still
                // closing. That one's turn comes when the alert is gone.
                if self.alert.is_some() {
                    if let Some(older) = self.waiting_secret.replace(request) {
                        network::answer(older.id, None);
                    }
                    return Task::none();
                }
                return self.ask_for_password(request);
            }
            Message::WifiAgent(network::AgentEvent::Withdrawn(request)) => {
                if self
                    .waiting_secret
                    .as_ref()
                    .is_some_and(|waiting| waiting.id == request)
                {
                    self.waiting_secret = None;
                }
                if let Some(open) = &mut self.alert
                    && open
                        .alert
                        .field
                        .as_ref()
                        .is_some_and(|field| field.purpose == alert::Purpose::Password(request))
                {
                    open.alert.field = None;
                    return iced_runtime::window::close(open.id);
                }
            }
            Message::CheckEthernet => {
                return Task::perform(network::on_ethernet_only(), Message::EthernetChecked);
            }
            Message::EthernetChecked(ethernet) => self.ethernet = ethernet,
            Message::AlertAnswered(confirmed) => {
                let Some(open) = &mut self.alert else {
                    return Task::none();
                };
                if confirmed && !open.alert.can_confirm() {
                    return Task::none();
                }
                let field = open.alert.field.take();
                let close = iced_runtime::window::close(open.id);
                match field {
                    Some(alert::Field {
                        purpose: alert::Purpose::Password(request),
                        value,
                        ..
                    }) => network::answer(request, confirmed.then_some(value)),
                    Some(alert::Field {
                        purpose: alert::Purpose::City,
                        value,
                        ..
                    }) if confirmed => {
                        self.widgets.set_city(value.trim().to_owned());
                        return Task::batch([close, Task::done(Message::FetchWeather)]);
                    }
                    _ => {}
                }
                let Some(open) = &self.alert else {
                    return close;
                };
                if confirmed {
                    return Task::batch([close, Task::done(open.alert.on_confirm.clone())]);
                }
                return close;
            }
            Message::AlertDragged => {
                return match &self.alert {
                    Some(open) => iced_runtime::window::drag(open.id),
                    None => Task::none(),
                };
            }
            Message::ReturnOrEscape(confirmed) => {
                if let Some(open) = &mut self.spotlight {
                    if confirmed {
                        let selected = open.selected;
                        return self.choose_in_spotlight(selected);
                    }
                    if !open.query.is_empty() {
                        open.query.clear();
                        open.selected = 0;
                    } else if open.mode != spotlight::Mode::Search {
                        open.mode = spotlight::Mode::Search;
                        open.category = None;
                        open.selected = 0;
                    } else {
                        return self.close_spotlight();
                    }
                    return spotlight::Spotlight::refocus();
                }
                if self.alert.as_ref().is_some_and(|open| open.active) {
                    return Task::done(Message::AlertAnswered(confirmed));
                }
                if self.desktop.rename.is_some() {
                    if !confirmed {
                        self.desktop.rename = None;
                    }
                    return self.finish_desktop_rename();
                }
                // Return renames what's selected, like on macOS.
                if confirmed
                    && self.focused_app.is_none()
                    && let [path] = self.desktop.selection().as_slice()
                {
                    return self.start_desktop_rename(path.clone());
                }
            }
            Message::SurfaceOpened(id, _) if Some(id) == self.gallery_id => {
                return self.reach_surface(id);
            }
            Message::SurfaceOpened(id, _) if self.alert.as_ref().is_some_and(|a| a.id == id) => {
                let reach = self.reach_surface(id);
                let has_field = self
                    .alert
                    .as_ref()
                    .is_some_and(|open| open.alert.field.is_some());
                return if has_field {
                    let focus = iced_runtime::widget::operation::focus(Alert::field_id());
                    Task::batch([reach, focus])
                } else {
                    reach
                };
            }
            Message::DockThumbnailPressed(id) => kwin::focus_window(id),
            Message::DockThumbnailMenu(id) => {
                let Some(index) = self.minimized.iter().position(|window| window.id == id) else {
                    return Task::none();
                };
                let app_id = self.minimized[index].app.id.clone();
                let Some(item) = self.dock.iter().find(|item| item.app.id == app_id) else {
                    return Task::none();
                };
                let pinned = self.pinned.iter().any(|id| *id == app_id);
                let entries = dock::menu_entries(item, pinned);
                let size = menu::size(&entries);
                return self.open_dock_popup(
                    DockSlot::Minimized(index),
                    PopupContent::Menu(entries),
                    size,
                );
            }
            Message::WindowCaptured(id, image) => {
                if let Some(window) = self.minimized.iter_mut().find(|window| window.id == id) {
                    window.image = image;
                }
            }
            Message::LauncherUpdate(update) => {
                let (count, visible) = self.badge_counts.entry(update.app.clone()).or_default();
                *count = update.count.unwrap_or(*count);
                *visible = update.visible.unwrap_or(*visible);
                if *visible && *count > 0 {
                    self.dock_badges.insert(update.app, *count);
                } else {
                    self.dock_badges.remove(&update.app);
                }
            }
            Message::PlaceClicked(place) => match place {
                Place::Downloads => return self.toggle_fan(),
                Place::Trash => self.open_place(Place::Trash),
            },
            Message::PlaceMenuRequested(place) => {
                let entries = dock::place_menu_entries(place, self.trash_count);
                let size = menu::size(&entries);
                return self.open_dock_popup(place_slot(place), PopupContent::Menu(entries), size);
            }
            Message::SurfaceOpened(id, _) if self.popup.as_ref().is_some_and(|p| p.id == id) => {
                return self.reach_surface(id);
            }
            Message::SurfaceOpened(id, _) if self.submenus.iter().any(|menu| menu.id == id) => {
                return self.reach_surface(id);
            }
            Message::SurfaceOpened(id, _) if self.banners_surface == Some(id) => {
                self.banners_region.clear();
                return Task::batch([self.reach_surface(id), self.sync_banners()]);
            }
            Message::Notification(event) => return self.handle_notification(event),
            Message::BannerFrame(now) => {
                let over = self.banners.frame(now, self.card_pointed_at());
                let mut tasks = vec![self.sync_banners()];
                tasks.extend(over.into_iter().map(|id| {
                    Task::future(notifications::report_closed(
                        id,
                        notifications::CLOSED_EXPIRED,
                    ))
                    .discard()
                }));
                return Task::batch(tasks);
            }
            Message::NotificationEntered(id) => {
                self.notification_hovered = Some(id);
                return self.sync_banners();
            }
            Message::NotificationLeft(id) => {
                if self.notification_hovered == Some(id) {
                    self.notification_hovered = None;
                }
                self.banners.pointer_left(id, Instant::now());
                return self.sync_banners();
            }
            Message::NotificationPressed(id) => {
                let Some(notification) = self.find_notification(id) else {
                    return Task::none();
                };
                let app_key = notification.app_key.clone();
                let has_default_action = notification.has_default_action;
                self.focus_app(&app_key);
                let action = has_default_action.then(|| {
                    Task::future(notifications::invoke_action(
                        id,
                        notifications::DEFAULT_ACTION.to_owned(),
                    ))
                    .discard()
                });
                let dismiss = self.dismiss_notification(id);
                let close = self.close_popup();
                return Task::batch(action.into_iter().chain([dismiss, close]));
            }
            Message::NotificationAction(id, key) => {
                let invoke = Task::future(notifications::invoke_action(id, key)).discard();
                let close = if matches!(
                    self.popup.as_ref().map(|popup| &popup.content),
                    Some(PopupContent::NotificationCenter)
                ) {
                    self.close_submenus(0)
                } else {
                    self.close_popup()
                };
                return Task::batch([close, invoke, self.dismiss_notification(id)]);
            }
            Message::NotificationDismissed(id) => return self.dismiss_notification(id),
            Message::NotificationOptions(id, anchor) => {
                let entries = self.find_notification(id).map(Notification::options);
                return self.open_notification_menu(id, anchor, entries);
            }
            Message::NotificationMore(id, anchor) => {
                let entries = self.find_notification(id).map(Notification::more_menu);
                return self.open_notification_menu(id, anchor, entries);
            }
            Message::NotificationAppMuted(app_key, time) => {
                self.muted_apps.insert(app_key, Instant::now() + time);
                return self.close_notification_menu();
            }
            Message::NotificationAppTurnedOff(app_key) => {
                if !self.notifications_off.contains(&app_key) {
                    self.notifications_off.push(app_key);
                    if let Err(err) = config::save_notifications_off(&self.notifications_off) {
                        log::warn!("can't save which apps' notifications are off: {err}");
                    }
                }
                return self.close_notification_menu();
            }
            Message::OpenNotificationSettings => {
                let close = self.close_popup();
                if let Some(window) = self
                    .windows
                    .iter()
                    .find(|window| window.app_id.as_deref() == Some(crate::settings::APP_ID))
                {
                    kwin::focus_window(window.id);
                }
                // If it's running already, this only tells it which page.
                match std::env::current_exe() {
                    Ok(exe) => apps::spawn(vec![
                        exe.to_string_lossy().into_owned(),
                        String::from("settings"),
                        String::from(crate::settings::NOTIFICATIONS_PAGE),
                    ]),
                    Err(err) => log::warn!("can't find the Settings app: {err}"),
                }
                return close;
            }
            Message::NotificationFrame => return self.sync_banners(),
            Message::NotificationExpand(id) => {
                if let Some(known) = self.notifications.iter_mut().find(|known| known.id == id) {
                    known.toggle_expanded();
                }
                self.banners.toggle_expanded(id);
                return self.sync_banners();
            }
            Message::TrayItems(items) => {
                self.tray = items
                    .into_iter()
                    .map(|item| tray::Icon::new(item, &mut self.apps))
                    .collect();
            }
            Message::DbusMenuItemChosen(address, id) => {
                let chosen = Task::future(address.item_chosen(id)).discard();
                return Task::batch([self.close_popup(), chosen]);
            }
            Message::AppMenuChanged(address, mut items) => {
                dbusmenu::load_icons(&mut items, &mut self.apps);
                if self.app_menu_address.as_ref() == Some(&address) {
                    self.app_menu_titles = items
                        .iter()
                        .filter(|item| !item.label.is_empty())
                        .map(|item| item.label.clone())
                        .collect();
                    self.app_menu = items
                        .into_iter()
                        .filter(|item| !item.label.is_empty())
                        .collect();
                }
            }
            Message::MenuRowEntered(level, row) => {
                self.menu_hover = Some(MenuHover {
                    level,
                    row,
                    since: Instant::now(),
                });
            }
            Message::MenuRowPressed(level, row) => {
                self.menu_hover = None;
                return self.show_submenu(level, row);
            }
            Message::MenuRowChosen(level, row, chosen) => {
                if self.menu_blink.is_none() {
                    self.menu_hover = None;
                    self.menu_blink = Some(MenuBlink {
                        level,
                        row,
                        started: anim::now(),
                        chosen: *chosen,
                    });
                }
            }
            Message::Frame(at) => return self.next_frame(at),
            Message::FrameKick => {
                if self.is_moving() && self.last_frame.elapsed() >= FRAME_KICK {
                    return self.next_frame(Instant::now());
                }
            }
            Message::ControlHover(module, over) => self.control.set_hovered(module, over),
            Message::MenuBlinkFrame => {
                if self.menu_blink.as_ref().is_some_and(|blink| {
                    anim::seconds_since(blink.started) >= 2.0 * MENU_BLINK_PHASE.as_secs_f32()
                }) && let Some(blink) = self.menu_blink.take()
                {
                    return Task::done(blink.chosen);
                }
            }
            Message::MenuHoverCheck => {
                if let Some(hover) = &self.menu_hover
                    && hover.since.elapsed() >= MENU_HOVER_DELAY
                {
                    let (level, row) = (hover.level, hover.row);
                    self.menu_hover = None;
                    return self.show_submenu(level, row);
                }
            }
            Message::NotificationGroupOpened(app_key) => {
                self.open_groups.insert(app_key);
            }
            Message::NotificationGroupClosed(app_key) => {
                self.center_motion.fold(&self.notifications, &app_key);
                self.open_groups.remove(&app_key);
            }
            Message::NotificationsCleared => {
                let ids: Vec<u32> = self.notifications.iter().map(|known| known.id).collect();
                self.open_groups.clear();
                return Task::batch(ids.into_iter().map(|id| self.dismiss_notification(id)));
            }
            Message::NotificationGroupCleared(app_key) => {
                let ids: Vec<u32> = self
                    .notifications
                    .iter()
                    .filter(|notification| notification.app_key == app_key)
                    .map(|notification| notification.id)
                    .collect();
                self.open_groups.remove(&app_key);
                return Task::batch(ids.into_iter().map(|id| self.dismiss_notification(id)));
            }
            Message::NotificationCenterScrolled(amount) => {
                let rows = notification_center::row_count(&self.notifications, &self.open_groups);
                self.center_scroll =
                    (self.center_scroll - amount).clamp(0.0, rows.saturating_sub(1) as f32);
            }
            Message::SurfaceClosed(id) => {
                self.surfaces.remove(&id);
                if self.hud.as_ref().is_some_and(|open| open.id == id) {
                    if let Some(material) = self.hud.take().and_then(|open| open.material) {
                        material.destroy();
                    }
                }
                if self.fan.as_ref().is_some_and(|open| open.id == id) {
                    if let Some(material) = self.fan.take().and_then(|open| open.material) {
                        material.destroy();
                    }
                }
                if self.spotlight.as_ref().is_some_and(|open| open.id == id) {
                    if let Some(material) = self.spotlight.take().and_then(|open| open.material) {
                        material.destroy();
                    }
                    iced_layershell::toplevel::set_keyboard_target(None);
                }
                if let Some(index) = self.submenus.iter().position(|menu| menu.id == id) {
                    let submenu = self.submenus.remove(index);
                    if let Some(material) = submenu.material {
                        material.destroy();
                    }
                    if submenu.opener == usize::MAX {
                        return self.unpin_notification();
                    }
                    if !self.submenus.iter().any(|menu| menu.closing)
                        && let Some((level, row)) = self.next_submenu.take()
                    {
                        return self.show_submenu(level, row);
                    }
                    return Task::none();
                }
                if self.popup.as_ref().is_some_and(|popup| popup.id == id)
                    && let Some(popup) = self.popup.take()
                {
                    if let Some(material) = popup.material {
                        material.destroy();
                    }
                    self.menu_hover = None;
                    self.next_submenu = None;
                    let unpinned = self.unpin_notification();
                    let relayout = Task::batch([self.relayout_dock(), unpinned]);
                    if let Some(settings) = self.next_view_options.take() {
                        let open =
                            self.open_popup(None, settings, PopupContent::ViewOptions, 0.0, None);
                        return Task::batch([relayout, open]);
                    }
                    return match self.next_bar_menu.take() {
                        Some(title) => Task::batch([relayout, self.open_bar_menu(title)]),
                        None => relayout,
                    };
                }
                if self
                    .arrange_menu
                    .as_ref()
                    .is_some_and(|open| open.surface == id)
                    && let Some(open) = self.arrange_menu.take()
                    && let Some(material) = open.material
                {
                    material.destroy();
                }
                if self.gallery_id == Some(id) {
                    self.gallery_id = None;
                    if let Some(material) = self.gallery_material.take() {
                        material.destroy();
                    }
                }
                if self.alert.as_ref().is_some_and(|open| open.id == id)
                    && let Some(open) = self.alert.take()
                {
                    iced_layershell::toplevel::set_keyboard_target(None);
                    // Closed some other way than by answering it, like with Alt+F4.
                    if let Some(alert::Field {
                        purpose: alert::Purpose::Password(request),
                        ..
                    }) = open.alert.field
                    {
                        network::answer(request, None);
                    }
                    if let Some(material) = open.material {
                        material.destroy();
                    }
                    if let Some(request) = self.waiting_secret.take() {
                        return self.ask_for_password(request);
                    }
                }
                if self.banners_surface == Some(id) {
                    self.banners_surface = None;
                    self.banners_closing = false;
                    if let Some(material) = self.banners_material.take() {
                        material.destroy();
                    }
                    // Something new came in while it was closing.
                    return self.sync_banners();
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn open_dock_menu(&mut self, app_id: &str) -> Task<Message> {
        let Some(index) = self.dock.iter().position(|item| item.app.id == app_id) else {
            return Task::none();
        };
        let pinned = self.pinned.iter().any(|id| id == app_id);
        let entries = dock::menu_entries(&self.dock[index], pinned);
        let size = menu::size(&entries);
        self.open_dock_popup(DockSlot::App(index), PopupContent::Menu(entries), size)
    }

    /// Opens a popup above a dock icon.
    fn open_dock_popup(
        &mut self,
        slot: DockSlot,
        content: PopupContent,
        size: (u32, u32),
    ) -> Task<Message> {
        let index = match slot {
            DockSlot::App(index) => index,
            DockSlot::Place(offset) => dock::place_slot(&self.dock_layout, offset),
            DockSlot::Minimized(index) => dock::minimized_slot(&self.dock_layout, index),
        };
        let Some(anchor) = dock::menu_anchor(&self.dock_layout, index) else {
            return Task::none();
        };
        // A menu's tail is part of its surface, and points at the icon. The compositor keeps the
        // whole thing on the screen, which can move it from being centered over the icon.
        let is_menu = matches!(content, PopupContent::Menu(_));
        let size = if is_menu {
            (size.0, size.1 + menu::TAIL_HEIGHT as u32)
        } else {
            size
        };
        let icon_center = anchor.0 as f32 + anchor.2 as f32 / 2.0;
        let x = (icon_center - size.0 as f32 / 2.0)
            .min(self.dock_width - size.0 as f32)
            .max(0.0);
        let tail = is_menu.then(|| menu::tail_center(size.0, x, icon_center));
        let settings = IcedNewPopupSettings::new(self.dock_id, size, anchor)
            .anchor(PopupAnchor::Top)
            .gravity(PopupGravity::Top);
        self.open_popup(None, settings, content, x, tail)
    }

    /// Opens a menu bar title's menu under it.
    fn open_bar_menu(&mut self, title: menubar::Title) -> Task<Message> {
        let entries = match title {
            menubar::Title::System => menubar::system_menu(),
            menubar::Title::App => {
                let Some(app) = &self.focused_app else {
                    return Task::none();
                };
                menubar::app_menu(app)
            }
            menubar::Title::Spotlight => return self.toggle_spotlight(),
            menubar::Title::Ethernet => menubar::ethernet_menu(),
            menubar::Title::Battery => match &self.battery {
                Some(battery) => menubar::battery_menu(battery),
                None => return Task::none(),
            },
            // Read first, so that it opens at the size of what it lists.
            menubar::Title::Wifi | menubar::Title::Bluetooth | menubar::Title::Sound => {
                if self.popup.is_some() {
                    return self.close_popup();
                }
                let read = Task::perform(control::status(), move |status| {
                    Message::StatusMenuReady(title, status)
                });
                if title == menubar::Title::Wifi {
                    return Task::batch([read, Task::future(network::scan()).discard()]);
                }
                return read;
            }
            menubar::Title::Input => match &self.keyboard_layouts {
                Some(layouts) => menubar::input_menu(layouts),
                None => return Task::none(),
            },
            menubar::Title::ControlCenter => {
                self.control.detail = None;
                self.control.morph = None;
                self.control.opened = Some(anim::now());
                self.control.clear_hover();
                self.control.volume = audio::volume();
                let reread = Task::perform(brightness::level(), Message::BrightnessRead);
                let anchor = self.bar().screen_edge_anchor();
                let settings = IcedNewPopupSettings::new(
                    self.menubar_id,
                    control::size(&self.control),
                    anchor,
                )
                .anchor(PopupAnchor::BottomRight)
                .gravity(PopupGravity::BottomLeft);
                let open = self.open_popup(
                    Some(title),
                    settings,
                    PopupContent::ControlCenter,
                    0.0,
                    None,
                );
                return Task::batch([open, reread, Task::done(Message::ControlRefresh)]);
            }
            menubar::Title::AppMenu(index) => {
                let (Some(address), Some(item)) =
                    (&self.app_menu_address, self.app_menu.get(index))
                else {
                    return Task::none();
                };
                let entries = address.entries(&item.children);
                let opening = Task::future(address.clone().opening(item.id)).discard();
                // An app can hide everything in a menu for now.
                if entries.is_empty() {
                    return opening;
                }
                let size = menu::size(&entries);
                let open = self.open_bar_popup(title, size, PopupContent::Menu(entries));
                return Task::batch([open, opening]);
            }
            menubar::Title::Tray(index) => {
                let Some(icon) = self.tray.get(index) else {
                    return Task::none();
                };
                if !icon.item.has_menu() {
                    return Task::future(icon.item.activate(0, 0)).discard();
                }
                icon.item.entries()
            }
            menubar::Title::Clock => {
                self.center_scroll = 0.0;
                self.open_groups.clear();
                let size = (
                    notification_center::WIDTH as u32,
                    notification_center::height(self.screen_height) as u32,
                );
                let settings = IcedNewPopupSettings::new(
                    self.menubar_id,
                    size,
                    self.bar().right_edge_anchor(),
                )
                .anchor(PopupAnchor::BottomRight)
                .gravity(PopupGravity::BottomLeft);
                let open = self.open_popup(
                    Some(title),
                    settings,
                    PopupContent::NotificationCenter,
                    0.0,
                    None,
                );
                // Everything's in the panel now, and a banner would sit on top of it.
                self.banners.dismiss_all();
                return Task::batch([open, self.sync_banners()]);
            }
        };
        let size = menu::size(&entries);
        self.open_bar_popup(title, size, PopupContent::Menu(entries))
    }

    /// The entries of the menu `level` submenus deep.
    fn menu_at(&self, level: usize) -> Option<&[menu::Entry]> {
        match level {
            0 => match &self.popup.as_ref()?.content {
                PopupContent::Menu(entries) => Some(entries),
                _ => None,
            },
            level => Some(&self.submenus.get(level - 1)?.entries),
        }
    }

    /// Makes the row's submenu the innermost one open, if it has one. If it doesn't, this closes
    /// whatever was open from that menu.
    fn show_submenu(&mut self, level: usize, row: usize) -> Task<Message> {
        let wanted = self
            .menu_at(level)
            .and_then(|entries| entries.get(row))
            .and_then(menu::Entry::submenu_entries)
            .map(<[menu::Entry]>::to_vec);

        let keep = match self.submenus.get(level) {
            Some(open) if open.opener == row && !open.closing && wanted.is_some() => level + 1,
            _ => level,
        };
        if keep == level + 1 {
            self.next_submenu = None;
            return self.close_submenus(keep);
        }
        if self.submenus.len() > keep {
            self.next_submenu = wanted.is_some().then_some((level, row));
            return self.close_submenus(keep);
        }
        let Some(entries) = wanted else {
            return Task::none();
        };
        self.open_submenu(level, row, entries)
    }

    /// Where the left edge of the menu `level` submenus deep is on the screen.
    fn menu_x(&self, level: usize) -> f32 {
        match level.checked_sub(1) {
            Some(index) => self.submenus.get(index).map_or(0.0, |menu| menu.x),
            None => self.popup.as_ref().map_or(0.0, |popup| popup.x),
        }
    }

    /// Closes the submenus from this depth in. The surface closes its own children.
    fn close_submenus(&mut self, depth: usize) -> Task<Message> {
        let Some(first) = self.submenus.get(depth) else {
            return Task::none();
        };
        let close = iced_runtime::window::close(first.id);
        for submenu in &mut self.submenus[depth..] {
            submenu.closing = true;
        }
        close
    }

    fn open_submenu(
        &mut self,
        level: usize,
        row: usize,
        entries: Vec<menu::Entry>,
    ) -> Task<Message> {
        let (parent, parent_width) = match level {
            0 => match &self.popup {
                Some(popup) => (popup.id, popup.size.0),
                None => return Task::none(),
            },
            level => match self.submenus.get(level - 1) {
                Some(menu) => (menu.id, menu.size.0),
                None => return Task::none(),
            },
        };
        let parent_x = self.menu_x(level);
        let Some(siblings) = self.menu_at(level) else {
            return Task::none();
        };
        let anchor = menu::submenu_anchor(siblings, row, parent_width);
        let size = menu::size(&entries);
        // To the right if there's room. Otherwise to the left, and then on to the left from a menu
        // that went that way, since its parent would otherwise be in the way.
        let right_x = parent_x + parent_width as f32 - menu::SUBMENU_OVERLAP as f32;
        let went_left = level > 0 && parent_x < self.menu_x(level - 1);
        let goes_right = !went_left && right_x + size.0 as f32 <= self.menubar_width;
        let x = if goes_right {
            right_x
        } else {
            parent_x + menu::SUBMENU_OVERLAP as f32 - size.0 as f32
        };
        let (anchor_point, gravity) = if goes_right {
            (PopupAnchor::TopRight, PopupGravity::BottomRight)
        } else {
            (PopupAnchor::TopLeft, PopupGravity::BottomLeft)
        };
        let id = IcedId::unique();
        self.surfaces.insert(id, Surface::Popup);
        self.submenus.push(Submenu {
            id,
            opener: row,
            x,
            entries,
            size,
            material: None,
            closing: false,
        });
        let settings = IcedNewPopupSettings::new(parent, size, anchor)
            .anchor(anchor_point)
            .gravity(gravity)
            .constraint_adjustment(
                PopupConstraintAdjustment::FlipX | PopupConstraintAdjustment::SlideY,
            );
        Task::done(Message::NewPopUp { settings, id })
    }

    /// Opens a popup under a menu bar title.
    fn open_bar_popup(
        &mut self,
        title: menubar::Title,
        size: (u32, u32),
        content: PopupContent,
    ) -> Task<Message> {
        let Some(anchor) = self.bar().menu_anchor(title) else {
            return Task::none();
        };
        // A menu near the end of the bar is slid back onto the screen.
        let x = (anchor.0 as f32).min(self.menubar_width - size.0 as f32);
        let settings = IcedNewPopupSettings::new(self.menubar_id, size, anchor)
            .anchor(PopupAnchor::BottomLeft)
            .gravity(PopupGravity::BottomRight);
        self.open_popup(Some(title), settings, content, x, None)
    }

    /// Opens a popup, unless one is open: then it closes that one instead.
    fn open_popup(
        &mut self,
        title: Option<menubar::Title>,
        settings: IcedNewPopupSettings,
        content: PopupContent,
        x: f32,
        tail: Option<f32>,
    ) -> Task<Message> {
        // xdg-shell only lets the topmost popup grab, so a new popup can't open until the old
        // one is gone. Close it, and let the next click open the new one.
        if self.popup.is_some() {
            return self.close_popup();
        }

        let id = IcedId::unique();
        self.surfaces.insert(id, Surface::Popup);
        self.popup = Some(Popup {
            id,
            title,
            content,
            size: settings.size,
            material: None,
            shapes: Vec::new(),
            shape_opacities: Vec::new(),
            x,
            tail,
        });
        Task::batch([
            self.relayout_dock(),
            Task::done(Message::NewPopUp { settings, id }),
        ])
    }

    /// Reaches the `wl_surface` behind an iced surface, to put material behind it.
    fn reach_surface(&self, id: IcedId) -> Task<Message> {
        let Some(material) = &self.material else {
            return Task::none();
        };
        let connection = material.connection().clone();
        iced_runtime::window::run(id, move |window| material::surface_of(&connection, window))
            .map(move |surface| Message::SurfaceReached(id, surface))
    }

    fn attach_material(&mut self, id: IcedId, surface: WlSurface) {
        let Some(material) = &self.material else {
            return;
        };
        let target = material.attach(surface);
        match self.surfaces.get(&id) {
            Some(Surface::MenuBar) => {
                self.bar_tracks_brightness = material.track_brightness(&target);
                material.set_shapes(
                    &target,
                    &menubar::material_shapes(self.menubar_width, self.full_screen.is_some()),
                );
                self.menubar_material = Some(target);
            }
            Some(Surface::Dock) => {
                self.dock_shapes = dock::material_shapes(&self.dock_layout);
                material.set_shapes(&target, &self.dock_shapes);
                self.dock_material = Some(target);
            }
            Some(Surface::Popup) if self.submenus.iter().any(|menu| menu.id == id) => {
                let Some(submenu) = self.submenus.iter_mut().find(|menu| menu.id == id) else {
                    return;
                };
                material.set_shapes(&target, &menu::material_shapes(submenu.size, None));
                submenu.material = Some(target);
            }
            Some(Surface::Popup) => match &mut self.popup {
                Some(popup) if popup.id == id => {
                    let shapes = match popup.content {
                        PopupContent::Menu(_)
                        | PopupContent::ViewOptions
                        | PopupContent::Status(_) => menu::material_shapes(popup.size, popup.tail),
                        PopupContent::ControlCenter => control::material_shapes(&self.control),
                        PopupContent::NotificationCenter => {
                            notification_center::material_shapes(
                                &self.notifications,
                                &self.open_groups,
                                self.center_scroll as usize,
                                self.notification_hovered,
                                popup.size.1 as f32,
                                &self.center_motion,
                                &self.notification_fade,
                            )
                            .0
                        }
                    };
                    let appear = match popup.content {
                        PopupContent::ControlCenter => Some(material::Appear::Fade),
                        PopupContent::NotificationCenter => Some(material::Appear::Slide),
                        PopupContent::Menu(_)
                        | PopupContent::ViewOptions
                        | PopupContent::Status(_) => Some(material::Appear::Pop),
                    };
                    if let Some(appear) = appear {
                        material.set_appear(&target, appear);
                    }
                    material.set_shapes(&target, &shapes);
                    popup.shapes = shapes;
                    popup.material = Some(target);
                }
                _ => target.destroy(),
            },
            Some(Surface::FullScreenTrigger) => target.destroy(),
            Some(Surface::Hud) => match &mut self.hud {
                Some(open) if open.id == id => {
                    material.set_appear(&target, material::Appear::Fade);
                    material.set_shapes(&target, &hud::material_shapes());
                    open.material = Some(target);
                }
                _ => target.destroy(),
            },
            Some(Surface::Fan) => match &mut self.fan {
                Some(open) if open.id == id => {
                    open.shapes = open.material_shapes();
                    open.arrive();
                    open.shape_opacities = open.material_opacities();
                    material.set_faded_shapes(&target, &open.shapes, &open.shape_opacities);
                    open.material = Some(target);
                }
                _ => target.destroy(),
            },
            Some(Surface::Spotlight) => {
                let (width, height) = (self.menubar_width, self.screen_height);
                match &mut self.spotlight {
                    Some(open) if open.id == id => {
                        material.set_appear(&target, material::Appear::Quick);
                        open.glass_arrived();
                        open.shapes = open.material_shapes(width, height);
                        open.opacities = open.material_opacities();
                        material.set_faded_shapes(&target, &open.shapes, &open.opacities);
                        open.material = Some(target);
                    }
                    _ => target.destroy(),
                }
            }
            Some(Surface::Alert) => match &mut self.alert {
                Some(open) if open.id == id => {
                    let (x, y) = alert::middle(&open.alert);
                    material.set_appear(&target, material::Appear::Grow(x, y));
                    material.set_window_shadow(&target, alert::RADIUS, open.active);
                    material.set_shapes(&target, &alert::material_shapes(&open.alert, open.active));
                    open.material = Some(target);
                }
                _ => target.destroy(),
            },
            Some(Surface::Banners) => {
                (self.banners_shapes, self.banners_opacities) = self
                    .banners
                    .material_shapes(self.notification_hovered, &self.notification_fade);
                material.set_faded_shapes(&target, &self.banners_shapes, &self.banners_opacities);
                self.banners_material = Some(target);
            }
            // Closed before it could be reached, or not a surface with material.
            Some(Surface::ArrangeMenu) => match &mut self.arrange_menu {
                Some(open) if open.surface == id => {
                    material.set_appear(&target, material::Appear::Pop);
                    material.set_shapes(&target, &arrange::material_shapes(open.submenu));
                    open.material = Some(target);
                }
                _ => target.destroy(),
            },
            Some(Surface::WidgetGallery) => {
                material.set_appear(&target, material::Appear::Fade);
                material.set_shapes(&target, &widgets::gallery_shapes());
                self.gallery_material = Some(target);
            }
            Some(Surface::Desktop) => {
                let tinted = self.widgets_tinted() || self.widget_tint.value() > 0.0;
                self.desktop_shapes = self.widgets.material_shapes(tinted);
                material.set_shapes(&target, &self.desktop_shapes);
                self.desktop_material = Some(target);
            }
            Some(Surface::Wallpaper) | None => target.destroy(),
        }
    }

    /// The row blinking in a menu this deep, if any, and whether it's lit right now.
    fn menu_blink_at(&self, level: usize) -> Option<(usize, bool)> {
        self.menu_blink
            .as_ref()
            .filter(|blink| blink.level == level)
            .map(|blink| {
                (
                    blink.row,
                    anim::seconds_since(blink.started) >= MENU_BLINK_PHASE.as_secs_f32(),
                )
            })
    }

    /// Whether this is one of the strips that bring the bars over a full-screen window, or the menu
    /// bar while it does. The dock is followed along with its own pointer handling.
    fn is_full_screen_bar(&self, id: IcedId) -> bool {
        self.full_screen.as_ref().is_some_and(|open| {
            id == open.top.trigger || id == open.bottom.trigger || id == self.menubar_id
        })
    }

    /// Which bar over a full-screen window this surface is, or brings.
    fn full_screen_reveal(&mut self, id: IcedId) -> Option<&mut full_screen::Reveal> {
        let (menubar, dock) = (self.menubar_id, self.dock_id);
        let open = self.full_screen.as_mut()?;
        if id == open.top.trigger || id == menubar {
            Some(&mut open.top)
        } else if id == open.bottom.trigger || id == dock {
            Some(&mut open.bottom)
        } else {
            None
        }
    }

    /// The pointer's on a bar over a full-screen window, or at the edge that brings it.
    fn full_screen_pointer_on(&mut self, id: IcedId) {
        let Some(reveal) = self.full_screen_reveal(id) else {
            return;
        };
        reveal.pointer_on(id);
        if id == reveal.trigger && !reveal.is_shown() {
            log::debug!("the pointer's at the edge: in comes the bar");
            reveal.show();
        }
    }

    fn full_screen_pointer_off(&mut self, id: IcedId) -> Task<Message> {
        let Some(reveal) = self.full_screen_reveal(id) else {
            return Task::none();
        };
        reveal.pointer_off(id);
        if reveal.is_hovered() {
            Task::none()
        } else {
            full_screen_check_later()
        }
    }

    /// Puts the menu bar and the dock over the focused window while it's full screen, out of sight
    /// until the pointer touches the top or the bottom, and back where they were after.
    fn follow_full_screen(&mut self) -> Task<Message> {
        let window = self
            .windows
            .iter()
            .find(|w| w.is_focused && w.is_full_screen && !w.is_minimized)
            .map(|w| w.id);
        match (window, &mut self.full_screen) {
            (Some(window), Some(open)) => {
                open.window = window;
                Task::none()
            }
            (Some(window), None) => {
                log::debug!("window {window} is full screen; the menu bar and the dock wait");
                let (top, open_top) = Message::layershell_open(full_screen::trigger_settings(true));
                let (bottom, open_bottom) =
                    Message::layershell_open(full_screen::trigger_settings(false));
                self.surfaces.insert(top, Surface::FullScreenTrigger);
                self.surfaces.insert(bottom, Surface::FullScreenTrigger);
                let open = full_screen::FullScreen::new(window, top, bottom);
                let menubar_offset = open.menubar_offset();
                let dock_offset = open.bottom.offset(dock_hidden_height());
                self.full_screen = Some(open);
                self.set_menubar_shapes();
                Task::batch([
                    Task::done(Message::LayerChange {
                        id: self.menubar_id,
                        layer: Layer::Overlay,
                    }),
                    Task::done(Message::MarginChange {
                        id: self.menubar_id,
                        margin: (menubar_offset, 0, 0, 0),
                    }),
                    Task::done(Message::LayerChange {
                        id: self.dock_id,
                        layer: Layer::Overlay,
                    }),
                    Task::done(Message::MarginChange {
                        id: self.dock_id,
                        margin: (0, 0, dock_offset, 0),
                    }),
                    open_top,
                    open_bottom,
                ])
            }
            (None, Some(_)) => {
                log::debug!("no focused window is full screen; the menu bar and the dock are back");
                let Some(open) = self.full_screen.take() else {
                    return Task::none();
                };
                self.set_menubar_shapes();
                Task::batch([
                    Task::done(Message::RemoveWindow(open.top.trigger)),
                    Task::done(Message::RemoveWindow(open.bottom.trigger)),
                    Task::done(Message::LayerChange {
                        id: self.menubar_id,
                        layer: Layer::Top,
                    }),
                    Task::done(Message::MarginChange {
                        id: self.menubar_id,
                        margin: (0, 0, 0, 0),
                    }),
                    Task::done(Message::LayerChange {
                        id: self.dock_id,
                        layer: Layer::Top,
                    }),
                    Task::done(Message::MarginChange {
                        id: self.dock_id,
                        margin: (0, 0, 0, 0),
                    }),
                ])
            }
            (None, None) => Task::none(),
        }
    }

    /// Moves the menu bar and the dock to where they are on their way in or out.
    fn place_full_screen_bars(&mut self) -> Task<Message> {
        let (menubar_id, dock_id) = (self.menubar_id, self.dock_id);
        let Some(open) = &mut self.full_screen else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        let bars = [
            (&mut open.top, menubar_id, menubar::HEIGHT as f32, true),
            (&mut open.bottom, dock_id, dock_hidden_height(), false),
        ];
        for (reveal, id, hidden, top) in bars {
            // Moving a bar doesn't draw it, and frames only come at the display's pace from
            // drawing: without this, they'd come from the fallback timer, at about 30 a second.
            if reveal.is_moving() {
                iced_layershell::redraw::mark_changed(id);
            }
            let offset = reveal.offset(hidden);
            if reveal.placed == Some(offset) {
                continue;
            }
            reveal.placed = Some(offset);
            tasks.push(Task::done(Message::MarginChange {
                id,
                margin: if top {
                    (offset, 0, 0, 0)
                } else {
                    (0, 0, offset, 0)
                },
            }));
        }
        Task::batch(tasks)
    }

    /// The menu bar's glass: none, normally, so the wallpaper shows; frosted over a full-screen
    /// window.
    fn set_menubar_shapes(&self) {
        if let (Some(material), Some(target)) = (&self.material, &self.menubar_material) {
            material.set_shapes(
                target,
                &menubar::material_shapes(self.menubar_width, self.full_screen.is_some()),
            );
        }
    }

    fn bar(&self) -> menubar::Bar<'_> {
        menubar::Bar {
            width: self.menubar_width,
            app_name: self.app_name(),
            clock: &self.clock,
            logo: match self.bar_backdrop {
                Some(menubar::Backdrop::Light) if self.full_screen.is_none() => {
                    self.logo_dark.as_ref()
                }
                _ => self.logo.as_ref(),
            },
            open: self.popup.as_ref().and_then(|popup| popup.title),
            // Over a full-screen window, it's on frosted glass, dark like menus.
            backdrop: if self.full_screen.is_some() {
                Some(menubar::Backdrop::Dark)
            } else {
                self.bar_backdrop
            },
            input: self.input_label.as_deref(),
            ethernet: self.ethernet,
            wifi: self
                .show_wifi
                .then(|| self.control.wifi.as_ref())
                .flatten()
                .map(|wifi| {
                    if wifi.enabled && wifi.network.is_some() {
                        Glyph::Wifi
                    } else {
                        Glyph::WifiOff
                    }
                }),
            bluetooth: self
                .show_bluetooth
                .then(|| self.control.bluetooth.as_ref())
                .flatten()
                .map(|bluetooth| {
                    if bluetooth.powered {
                        Glyph::Bluetooth
                    } else {
                        Glyph::BluetoothOff
                    }
                }),
            sound: self
                .show_sound
                .then_some(self.control.volume)
                .flatten()
                .map(|volume| {
                    if volume.muted || volume.level <= 0.0 {
                        Glyph::SpeakerMuted
                    } else if volume.level < 0.5 {
                        Glyph::SpeakerLow
                    } else {
                        Glyph::SpeakerHigh
                    }
                }),
            battery: self.battery.as_ref().filter(|_| self.show_battery),
            battery_percent: self.show_battery_percent,
            tray: &self.tray,
            app_menus: &self.app_menu_titles,
            lights: self.full_screen.as_ref().map(|open| open.lights_hovered),
        }
    }

    fn find_notification(&self, id: u32) -> Option<&Notification> {
        self.notifications
            .iter()
            .find(|notification| notification.id == id)
            .or_else(|| self.banners.find(id))
    }

    fn handle_notification(&mut self, event: notifications::Event) -> Task<Message> {
        let now = Instant::now();
        match event {
            notifications::Event::Notified(incoming) => {
                let replaces = incoming.replaces;
                let critical = incoming.critical;
                let notification = Notification::new(incoming, &mut self.apps);
                self.muted_apps.retain(|_, until| *until > now);
                let muted = !critical && self.muted_apps.contains_key(&notification.app_key);
                let off = !critical && self.notifications_off.contains(&notification.app_key);
                // With nowhere to show it, it's over, and its app hears that.
                if off || (muted && notification.transient) {
                    return Task::future(notifications::report_closed(
                        notification.id,
                        notifications::CLOSED_DISMISSED,
                    ))
                    .discard();
                }
                let known = replaces
                    && self
                        .notifications
                        .iter_mut()
                        .find(|known| known.id == notification.id)
                        .map(|known| *known = notification.clone())
                        .is_some();
                if !known && !notification.transient {
                    self.notifications.insert(0, notification.clone());
                    self.notifications.truncate(MAX_NOTIFICATIONS);
                }
                let center_open = matches!(
                    self.popup.as_ref().map(|popup| &popup.content),
                    Some(PopupContent::NotificationCenter)
                );
                if !self.banners.replace(&notification, now)
                    && (!center_open || notification.transient)
                    && !muted
                {
                    self.banners.show(notification, now);
                }
            }
            notifications::Event::Closed(id) => {
                self.notifications.retain(|known| known.id != id);
                self.banners.dismiss(id);
            }
        }
        self.sync_banners()
    }

    /// What's in the dock, from what's kept there, what's running, and the recent apps.
    fn dock_items(&mut self) -> Vec<dock::Item> {
        let recents: &[String] = if self.show_recents {
            &self.recents
        } else {
            &[]
        };
        dock::items(
            &self.pinned,
            recents,
            &self.windows,
            &self.running,
            &mut self.apps,
        )
    }

    /// Notes the apps with windows that aren't kept in the dock as recent, and which of those to keep.
    fn note_recents(&mut self) {
        let came_along: Vec<String> = self
            .windows
            .iter()
            .filter_map(|window| window.app_id.as_deref())
            .map(|app_id| self.apps.resolve(app_id))
            .filter(|app| app.command.is_some() && !app.unlisted && !self.pinned.contains(&app.id))
            .map(|app| app.id.clone())
            .collect();
        let running: HashSet<String> = self
            .dock
            .iter()
            .filter(|item| item.is_running())
            .map(|item| item.app.id.clone())
            .collect();
        if dock::note_recents(&mut self.recents, came_along, |id| running.contains(id)) {
            dock::save_recents(&self.recents);
        }
    }

    /// Where the recent apps start in the dock, if they're showing and there are any.
    fn recents_start(&self) -> Option<usize> {
        (self.show_recents && self.dock.len() > self.pinned.len()).then_some(self.pinned.len())
    }

    /// Whether anything's moving, which is what takes frames.
    fn is_moving(&self) -> bool {
        self.dock_is_moving()
            || self.banners.animating()
            || self
                .fan
                .as_ref()
                .is_some_and(|open| open.is_animating() || open.is_closing())
            || self.control.morph.is_some()
            || self.control.hover_is_moving()
            || control::is_popping(&self.control)
            || self.hud.as_ref().is_some_and(hud::Hud::is_animating)
            || self
                .spotlight
                .as_ref()
                .is_some_and(spotlight::Spotlight::is_animating)
            || self.notification_fade.is_animating()
            || self.center_motion.is_animating()
            || self.notifications.iter().any(Notification::is_expanding)
            || self.menu_blink.is_some()
            || self.widget_tint.is_animating()
            || self
                .full_screen
                .as_ref()
                .is_some_and(full_screen::FullScreen::is_moving)
    }

    fn dock_is_moving(&self) -> bool {
        self.dock_pointer.amount != self.dock_target
            || !self.launching.is_empty()
            || self.dock_room.is_animating()
            || self.dock_section.is_moving()
            || self.dock.iter().any(dock::Item::wants_attention)
    }

    /// Moves everything on to the frame for this moment.
    fn next_frame(&mut self, at: Instant) -> Task<Message> {
        let now = anim::next_frame(at);
        self.last_frame = at;
        let mut steps = Vec::new();
        if self.dock_is_moving() {
            steps.push(self.apply(Message::DockFrame(now)));
        }
        if self.banners.animating() {
            steps.push(self.apply(Message::BannerFrame(at)));
        }
        if self.fan.is_some() {
            steps.push(self.apply(Message::FanFrame));
        }
        if self.control.morph.is_some() {
            steps.push(self.apply(Message::ControlFrame));
        }
        if self.menu_blink.is_some() {
            steps.push(self.apply(Message::MenuBlinkFrame));
        }
        if self.full_screen.is_some() {
            steps.push(self.apply(Message::FullScreenFrame));
        }
        if self.notification_fade.is_animating()
            || self.center_motion.is_animating()
            || self.notifications.iter().any(Notification::is_expanding)
        {
            steps.push(self.apply(Message::NotificationFrame));
        }
        Task::batch(steps)
    }

    /// While a dock menu or the fan is open, the dock stays as it was when it opened, magnified or
    /// not, like on macOS, so that what they point at doesn't move out from under them.
    fn dock_pinned(&self) -> bool {
        self.fan_is_open()
            || self
                .popup
                .as_ref()
                .is_some_and(|popup| popup.tail.is_some())
    }

    /// Catches up with where the pointer went while the dock was pinned.
    fn update_dock_pin(&mut self) {
        let pinned = self.dock_pinned();
        if self.dock_was_pinned && !pinned && self.dock_pointer_left {
            self.dock_pointer.hovered = None;
            self.dock_target = 0.0;
            self.dock_pointer_left = false;
        }
        self.dock_was_pinned = pinned;
    }

    /// Which card counts as being pointed at.
    fn card_pointed_at(&self) -> Option<u32> {
        self.notification_hovered.or(self.notification_pinned)
    }

    /// Closes a menu opened from a card, leaving Notification Center there, if that's what it's on.
    fn close_notification_menu(&mut self) -> Task<Message> {
        if matches!(
            self.popup.as_ref().map(|popup| &popup.content),
            Some(PopupContent::NotificationCenter)
        ) {
            self.close_submenus(0)
        } else {
            self.close_popup()
        }
    }

    /// Opens a menu from a card, which stays as it is for as long as that's open.
    fn open_notification_menu(
        &mut self,
        id: u32,
        anchor: (i32, i32, i32, i32),
        entries: Option<Vec<menu::Entry>>,
    ) -> Task<Message> {
        let Some(entries) = entries else {
            return Task::none();
        };
        let size = menu::size(&entries);
        self.notification_pinned = Some(id);
        // Notification Center's a popup itself, which only one that's its child can open from.
        if let Some(popup) = &self.popup {
            if !matches!(popup.content, PopupContent::NotificationCenter) {
                return Task::none();
            }
            let parent = popup.id;
            let id = IcedId::unique();
            self.surfaces.insert(id, Surface::Popup);
            self.submenus.push(Submenu {
                id,
                // No row of what it opened from is a menu's.
                opener: usize::MAX,
                x: 0.0,
                entries,
                size,
                material: None,
                closing: false,
            });
            let settings = IcedNewPopupSettings::new(parent, size, anchor)
                .anchor(PopupAnchor::BottomRight)
                .gravity(PopupGravity::BottomLeft)
                .constraint_adjustment(
                    PopupConstraintAdjustment::FlipY | PopupConstraintAdjustment::SlideX,
                );
            return Task::done(Message::NewPopUp { settings, id });
        }
        let Some(parent) = self.banners_surface else {
            return Task::none();
        };
        let settings = IcedNewPopupSettings::new(parent, size, anchor)
            .anchor(PopupAnchor::BottomRight)
            .gravity(PopupGravity::BottomLeft)
            .constraint_adjustment(
                PopupConstraintAdjustment::FlipY | PopupConstraintAdjustment::SlideX,
            );
        self.open_popup(None, settings, PopupContent::Menu(entries), 0.0, None)
    }

    /// A menu of options closed: its card goes back to being like the others.
    fn unpin_notification(&mut self) -> Task<Message> {
        match self.notification_pinned.take() {
            Some(id) => {
                self.banners.pointer_left(id, Instant::now());
                self.sync_banners()
            }
            None => Task::none(),
        }
    }

    /// Keeps what moves by itself in notifications going.
    fn update_notification_motion(&mut self) {
        self.notification_fade.aim(self.card_pointed_at());
        match &self.popup {
            Some(popup) if matches!(popup.content, PopupContent::NotificationCenter) => {
                self.center_motion.sync(
                    &self.notifications,
                    &self.open_groups,
                    self.center_scroll as usize,
                    popup.size.1 as f32,
                );
            }
            _ => self.center_motion = notification_center::Motions::default(),
        }
    }

    /// Takes a notification away, and tells its app. It goes at once from a banner, like on macOS,
    /// and with a little more to it from Notification Center.
    fn dismiss_notification(&mut self, id: u32) -> Task<Message> {
        if let Some(known) = self.notifications.iter().find(|known| known.id == id) {
            self.center_motion.close(known);
        }
        self.notifications.retain(|known| known.id != id);
        self.banners.remove(id);
        if self.notification_hovered == Some(id) {
            self.notification_hovered = None;
        }
        Task::batch([
            self.sync_banners(),
            Task::future(notifications::report_closed(
                id,
                notifications::CLOSED_DISMISSED,
            ))
            .discard(),
        ])
    }

    /// Brings the app's most recently used window forward, if it has one.
    fn focus_app(&mut self, app_key: &str) {
        let window = self
            .windows
            .iter()
            .filter(|window| {
                window.app_id.as_deref().is_some_and(|app_id| {
                    app_id.eq_ignore_ascii_case(app_key) || self.apps.resolve(app_id).id == app_key
                })
            })
            .max_by_key(|window| window.focused_at);
        if let Some(window) = window {
            kwin::focus_window(window.id);
        }
    }

    /// Opens or closes the banners' surface as needed, and keeps its glass and input region with
    /// the banners.
    fn sync_banners(&mut self) -> Task<Message> {
        let Some(id) = self.banners_surface else {
            if self.banners.is_empty() {
                return Task::none();
            }
            let (id, open) = Message::layershell_open(banners::settings());
            self.surfaces.insert(id, Surface::Banners);
            self.banners_surface = Some(id);
            return open;
        };
        if self.banners.is_empty() {
            // A menu of options could still hang from it, which has to go first.
            if self.notification_pinned.is_some()
                || std::mem::replace(&mut self.banners_closing, true)
            {
                return Task::none();
            }
            return iced_runtime::window::close(id);
        }

        let (shapes, opacities) = self
            .banners
            .material_shapes(self.card_pointed_at(), &self.notification_fade);
        if (shapes != self.banners_shapes || opacities != self.banners_opacities)
            && let (Some(material), Some(target)) = (&self.material, &self.banners_material)
        {
            material.queue_faded_shapes(target, &shapes, &opacities);
            self.banners_shapes = shapes;
            self.banners_opacities = opacities;
        }

        let region = self.banners.input_region();
        if region == self.banners_region {
            return Task::none();
        }
        self.banners_region.clone_from(&region);
        Task::done(Message::SetInputRegion {
            id,
            callback: ActionCallback::new(move |wl_region| {
                for &(x, y, width, height) in &region {
                    wl_region.add(x, y, width, height);
                }
            }),
        })
    }

    /// Keeps the glass of the panels whose content changes while they're open under that content.
    fn update_panel_shapes(&mut self) {
        let pointed_at = self.card_pointed_at();
        let Some(popup) = &mut self.popup else {
            return;
        };
        let (shapes, opacities) = match popup.content {
            PopupContent::NotificationCenter => notification_center::material_shapes(
                &self.notifications,
                &self.open_groups,
                self.center_scroll as usize,
                pointed_at,
                popup.size.1 as f32,
                &self.center_motion,
                &self.notification_fade,
            ),
            PopupContent::ControlCenter => (
                control::material_shapes(&self.control),
                control::material_opacities(&self.control),
            ),
            _ => return,
        };
        if (shapes != popup.shapes || opacities != popup.shape_opacities)
            && let (Some(material), Some(target)) = (&self.material, &popup.material)
        {
            material.queue_faded_shapes(target, &shapes, &opacities);
            popup.shapes = shapes;
            popup.shape_opacities = opacities;
        }
    }

    /// What the menu bar calls the focused app; "Desktop" when nothing is focused.
    fn app_name(&self) -> &str {
        self.focused_app.as_ref().map_or("Desktop", |app| &app.name)
    }

    /// Opens an alert, unless one is already up.
    fn open_alert(&mut self, alert: Alert) -> Task<Message> {
        if self.alert.is_some() {
            return Task::none();
        }
        let (id, open) = Message::base_window_open(alert::settings(&alert));
        self.surfaces.insert(id, Surface::Alert);
        iced_layershell::toplevel::set_keyboard_target(Some(id));
        self.alert = Some(OpenAlert {
            id,
            alert,
            material: None,
            active: true,
        });
        open
    }

    fn ask_for_password(&mut self, request: network::SecretRequest) -> Task<Message> {
        let detail = if request.retry {
            "That password didn't work. Check it and try again."
        } else {
            "Enter the network's password to join it."
        };
        self.open_alert(Alert {
            title: format!("The Wi-Fi network “{}” requires a password.", request.ssid),
            detail: detail.to_owned(),
            confirm: String::from("Join"),
            on_confirm: Message::Nothing,
            icon: alert::Icon::Glyph(icons::Glyph::Wifi),
            field: Some(alert::Field {
                purpose: alert::Purpose::Password(request.id),
                value: String::new(),
                visible: false,
                min_length: request.min_length,
            }),
        })
    }

    /// Keeps up with the newest downloads, for the Downloads tile.
    fn refresh_pile(&mut self) -> Task<Message> {
        let newest = places::recent_downloads(&self.downloads_dir, PILE_SIZE);
        let paths: Vec<PathBuf> = newest
            .iter()
            .map(|download| download.path.clone())
            .collect();
        if paths == self.pile {
            return Task::none();
        }
        self.pile.clear();
        self.pile_pictures.clear();
        let mut thumbnails = Vec::new();
        for download in &newest {
            // A blank in the pile would throw off which picture is whose.
            let Some(icon) = self.apps.themed_icon(places::icon_name(download)) else {
                continue;
            };
            self.pile.push(download.path.clone());
            self.pile_pictures.push(icon);
            if desktop::has_thumbnail(&download.path) {
                let path = download.path.clone();
                thumbnails.push(Task::perform(
                    desktop::thumbnail(path.clone()),
                    move |picture| Message::PileThumbnail(path.clone(), picture),
                ));
            }
        }
        Task::batch(thumbnails)
    }

    /// Does what a volume or brightness key is for.
    fn media_key(&mut self, key: &str) -> Task<Message> {
        match key {
            "increase_volume" => self.step_volume(VOLUME_STEP),
            "decrease_volume" => self.step_volume(-VOLUME_STEP),
            "increase_volume_small" => self.step_volume(VOLUME_STEP_SMALL),
            "decrease_volume_small" => self.step_volume(-VOLUME_STEP_SMALL),
            "mute" => {
                let Some(volume) = audio::volume() else {
                    return Task::none();
                };
                audio::set_muted(!volume.muted);
                self.control.volume = Some(audio::Volume {
                    muted: !volume.muted,
                    ..volume
                });
                self.show_hud(hud::Kind::Sound, volume.level, !volume.muted)
            }
            "Increase Screen Brightness" => self.step_brightness(BRIGHTNESS_STEP),
            "Decrease Screen Brightness" => self.step_brightness(-BRIGHTNESS_STEP),
            "Increase Screen Brightness Small" => self.step_brightness(BRIGHTNESS_STEP_SMALL),
            "Decrease Screen Brightness Small" => self.step_brightness(-BRIGHTNESS_STEP_SMALL),
            other => {
                log::warn!("don't know the key {other}");
                Task::none()
            }
        }
    }

    fn step_volume(&mut self, step: f32) -> Task<Message> {
        let Some(volume) = audio::volume() else {
            return Task::none();
        };
        let level = (volume.level + step).clamp(0.0, 1.0);
        audio::set_level(level);
        // Changing the volume is what you'd want after muting, like on macOS.
        if volume.muted {
            audio::set_muted(false);
        }
        self.control.volume = Some(audio::Volume {
            level,
            muted: false,
        });
        self.show_hud(hud::Kind::Sound, level, false)
    }

    fn step_brightness(&mut self, step: f32) -> Task<Message> {
        // It takes most of a second to read, so it's kept from then on.
        let Some(current) = self.control.brightness else {
            return Task::perform(brightness::level(), move |level| match level {
                Some(level) => Message::BrightnessStep(Some(level), step),
                None => Message::Nothing,
            });
        };
        let level = (current + step).clamp(0.0, 1.0);
        self.control.brightness = Some(level);
        brightness::set_level(level);
        self.show_hud(hud::Kind::Display, level, false)
    }

    fn show_hud(&mut self, kind: hud::Kind, level: f32, muted: bool) -> Task<Message> {
        if let Some(open) = &mut self.hud {
            open.update(kind, level, muted);
            return Task::none();
        }
        let (id, open) = Message::layershell_open(hud::settings());
        self.surfaces.insert(id, Surface::Hud);
        self.hud = Some(hud::Hud::new(id, kind, level, muted));
        open
    }

    fn fan_is_open(&self) -> bool {
        self.fan.as_ref().is_some_and(|open| !open.is_closing())
    }

    fn toggle_fan(&mut self) -> Task<Message> {
        if let Some(open) = &mut self.fan {
            open.close();
            return self.relayout_dock();
        }
        // Right over the tile, wherever the dock's magnification has it now; it stays there.
        let tile_x = self
            .dock_layout
            .icons
            .get(dock::place_slot(&self.dock_layout, 0))
            .map_or(self.dock_width / 2.0, |tile| tile.center_x());
        let height = fan::surface_height(self.screen_height);
        let room = fan::Fan::rows_that_fit(height);
        let mut downloads = places::recent_downloads(&self.downloads_dir, FAN_MOST_DOWNLOADS);
        let more = downloads.len().saturating_sub(room);
        downloads.truncate(room);

        let mut tasks = Vec::new();
        let rows = downloads
            .into_iter()
            .map(|download| {
                if desktop::has_thumbnail(&download.path) {
                    let path = download.path.clone();
                    tasks.push(Task::perform(
                        desktop::thumbnail(path.clone()),
                        move |picture| Message::FanThumbnail(path.clone(), picture),
                    ));
                }
                let picture = self.apps.themed_icon(places::icon_name(&download));
                (download.path, download.name, picture)
            })
            .collect();

        let (id, open) = Message::layershell_open(fan::settings(self.screen_height));
        self.surfaces.insert(id, Surface::Fan);
        let glass = self
            .material
            .as_ref()
            .is_some_and(|material| material.kind() == material::Kind::Glass);
        self.fan = Some(fan::Fan::new(id, rows, more, tile_x, height, glass));
        tasks.push(open);
        tasks.push(self.close_popup());
        // Which hides the name over the tile.
        tasks.push(self.relayout_dock());
        Task::batch(tasks)
    }

    /// Starts dragging the pressed file out of the fan, and lets the pointer through its surface, so
    /// that it can be dropped on whatever's under it.
    fn start_fan_drag(&mut self) -> Task<Message> {
        let serial = iced_layershell::toplevel::take_drag_serial();
        let Some(open) = &mut self.fan else {
            return Task::none();
        };
        let row = open
            .pressed
            .take()
            .and_then(|(index, _)| open.rows.get(index));
        let (Some(serial), Some(surface), Some(row)) = (serial, &open.surface, row) else {
            open.dragging = false;
            return Task::none();
        };
        let paths = [row.path.clone()];
        let icon = dnd::drag_icon(row.picture.as_slice());
        dnd::start_drag(surface, serial, &dnd::uris_of(&paths), icon);
        Task::done(Message::SetInputRegion {
            id: open.id,
            callback: ActionCallback::new(|_| {}),
        })
    }

    fn toggle_spotlight(&mut self) -> Task<Message> {
        if self.spotlight.is_some() {
            return self.close_spotlight();
        }
        let (id, open) = Message::layershell_open(spotlight::settings());
        self.surfaces.insert(id, Surface::Spotlight);
        iced_layershell::toplevel::set_keyboard_target(Some(id));
        self.spotlight = Some(spotlight::Spotlight::new(id));
        if self.spotlight_file_icons.folder.is_none() {
            self.spotlight_file_icons = spotlight::FileIcons {
                folder: self.apps.themed_icon("folder"),
                image: self.apps.themed_icon("image-x-generic"),
                video: self.apps.themed_icon("video-x-generic"),
                audio: self.apps.themed_icon("audio-x-generic"),
                pdf: self.apps.themed_icon("application-pdf"),
                text: self.apps.themed_icon("text-x-generic"),
            };
        }
        let recent = Task::perform(
            async {
                tokio::task::spawn_blocking(spotlight::recent_files)
                    .await
                    .unwrap_or_default()
            },
            Message::SpotlightRecentFiles,
        );
        Task::batch([self.close_popup(), open, recent])
    }

    fn close_spotlight(&mut self) -> Task<Message> {
        match &self.spotlight {
            Some(open) => iced_runtime::window::close(open.id),
            None => Task::none(),
        }
    }

    /// Keeps the glass behind the fan's capsules in line with them.
    fn update_fan_shapes(&mut self) {
        if let (Some(material), Some(open)) = (&self.material, &mut self.fan) {
            let shapes = open.material_shapes();
            let opacities = open.material_opacities();
            if (shapes != open.shapes || opacities != open.shape_opacities)
                && let Some(target) = &open.material
            {
                material.queue_faded_shapes(target, &shapes, &opacities);
                open.shapes = shapes;
                open.shape_opacities = opacities;
            }
        }
    }

    /// Keeps the glass behind Spotlight in line with what it's showing.
    fn update_spotlight_shapes(&mut self) {
        let (width, height) = (self.menubar_width, self.screen_height);
        if let (Some(material), Some(open)) = (&self.material, &mut self.spotlight) {
            open.sync_expansion();
            let shapes = open.material_shapes(width, height);
            let opacities = open.material_opacities();
            if (shapes != open.shapes || opacities != open.opacities)
                && let Some(target) = &open.material
            {
                material.queue_faded_shapes(target, &shapes, &opacities);
                open.shapes = shapes;
                open.opacities = opacities;
            }
        }
    }

    /// Does what the thing at this place in Spotlight is for, and closes it.
    fn choose_in_spotlight(&mut self, index: usize) -> Task<Message> {
        let Some(open) = &self.spotlight else {
            return Task::none();
        };
        let Some(hit) = open
            .hits(&self.spotlight_apps, &self.clips)
            .into_iter()
            .nth(index)
        else {
            return Task::none();
        };
        let close = self.close_spotlight();
        let done = match hit {
            spotlight::Hit::App(app) => self.open_app(&app),
            spotlight::Hit::AppAction(app, action) => {
                if let Some(action) = app.actions.get(action) {
                    apps::spawn(action.command.clone());
                }
                Task::none()
            }
            spotlight::Hit::File(file) => {
                apps::spawn(vec![
                    String::from("xdg-open"),
                    file.path.to_string_lossy().into_owned(),
                ]);
                Task::none()
            }
            spotlight::Hit::Action(action) => Task::done(action.message()),
            spotlight::Hit::Clip(clip) => {
                if let Some(clip) = self.clips.get(clip) {
                    clipboard::copy(&clip.content);
                    let clip = clip.clone();
                    spotlight::remember_clip(&mut self.clips, clip, self.clips_max);
                    self.keep_clips();
                }
                Task::none()
            }
        };
        Task::batch([close, done])
    }

    /// Brings an app up, or starts it.
    fn open_app(&mut self, app: &App) -> Task<Message> {
        if self.dock.iter().any(|item| item.app.id == app.id) {
            return self.activate(&app.id);
        }
        match &app.command {
            Some(command) => {
                apps::spawn(command.clone());
                self.launching.insert(app.id.clone(), anim::now());
            }
            None => log::warn!("don't know how to launch {}", app.id),
        }
        Task::none()
    }

    /// Remembers the processes behind windows, which an app stays running for after they close.
    fn note_window_processes(&mut self) {
        for window in &self.windows {
            let (Some(app_id), Some(pid)) = (&window.app_id, window.pid) else {
                continue;
            };
            let Some(process) = running::Process::of(pid) else {
                continue;
            };
            let processes = self.running.entry(app_id.clone()).or_default();
            if !processes.contains(&process) {
                processes.push(process);
            }
        }
    }

    /// Whether widgets are glass: like on macOS, they are while a window is active, and in color
    /// when the desktop is, unless the Settings app says they always are.
    fn widgets_tinted(&self) -> bool {
        self.always_glassy_widgets
            || (self.focused_app.is_some() && !self.widgets.editing && !self.widgets.is_dragging())
    }

    fn show_wallpaper(&mut self, handle: image::Handle) -> Task<Message> {
        self.wallpaper = Some(handle.clone());
        self.wallpaper_loaded = None;
        if self
            .surfaces
            .values()
            .any(|surface| *surface == Surface::Wallpaper)
        {
            return iced_runtime::image::allocate(handle).map(Message::WallpaperLoaded);
        }
        let (id, open) = Message::layershell_open(wallpaper::settings());
        self.surfaces.insert(id, Surface::Wallpaper);
        open
    }

    /// Writes the clipboard's history down, so it outlasts the session.
    fn keep_clips(&self) {
        clip_store::save(
            self.clips
                .iter()
                .map(|clip| clip_store::Kept {
                    content: clip.content.clone(),
                    copied: clip.copied,
                    source: clip.source.as_ref().map(|app| app.id.clone()),
                })
                .collect(),
        );
    }

    /// Takes in what the Settings app just wrote to the config file.
    fn reload_config(&mut self) -> Task<Message> {
        let config = Config::load();
        if config.clipboard.max_items != self.clips_max {
            self.clips_max = config.clipboard.max_items;
            if self.clips.len() > self.clips_max {
                self.clips.truncate(self.clips_max);
                self.keep_clips();
            }
        }
        self.always_glassy_widgets = config.desktop.always_glassy_widgets;
        self.notifications_off.clone_from(&config.notifications.off);
        self.appearance_auto = config.appearance.mode == "auto";
        self.show_wifi = config.menubar.show_wifi;
        self.show_bluetooth = config.menubar.show_bluetooth;
        self.show_sound = config.menubar.show_sound;
        self.show_battery = config.menubar.show_battery;
        self.show_battery_percent = config.menubar.show_battery_percentage;
        let mut tasks = vec![
            Task::perform(appearance::refresh(), Message::AppearanceApplied),
            // In case macOS icons were just turned on.
            Task::perform(icon_themes::build(), Message::IconThemesBuilt),
        ];
        if config.desktop.wallpaper != self.wallpaper_path {
            self.wallpaper_path.clone_from(&config.desktop.wallpaper);
            wallpaper::link_for_lock_screen(config.desktop.wallpaper.as_deref());
            if let Some(handle) = config
                .desktop
                .wallpaper
                .as_deref()
                .and_then(wallpaper::load)
            {
                tasks.push(self.show_wallpaper(handle));
            }
        }
        if config.dock.pinned != self.pinned || config.dock.show_recents != self.show_recents {
            self.pinned = config.dock.pinned.clone();
            self.show_recents = config.dock.show_recents;
            self.dock = self.dock_items();
            tasks.push(self.relayout_dock());
        }
        let icon_was = dock::base_icon();
        dock::set_base_icon(config.dock.icon_size);
        let icon_changed = dock::base_icon() != icon_was;
        if icon_changed {
            tasks.push(Task::done(Message::ExclusiveZoneChange {
                id: self.dock_id,
                zone_size: dock::reserved_height() as i32,
            }));
        }
        let max_icon = dock_max_icon(&config.dock);
        if max_icon != self.dock_max_icon || icon_changed {
            self.dock_max_icon = max_icon;
            self.dock_target = 0.0;
            tasks.push(Task::done(Message::SizeChange {
                id: self.dock_id,
                size: (0, dock::surface_height(max_icon) as u32),
            }));
            tasks.push(self.relayout_dock());
        }
        Task::batch(tasks)
    }

    /// Keeps the glass behind widgets in line with them. It's there for as long as any of their
    /// glass look is showing, and under their color on the way there and back.
    fn update_desktop_shapes(&mut self) {
        let target = self.widgets_tinted();
        self.widget_tint.aim(target);
        let shapes = self
            .widgets
            .material_shapes(target || self.widget_tint.value() > 0.0);
        if shapes != self.desktop_shapes
            && let (Some(material), Some(target)) = (&self.material, &self.desktop_material)
        {
            material.set_shapes(target, &shapes);
            self.desktop_shapes = shapes;
        }
    }

    /// A little time for the pointer to cross between the light and the menu.
    fn arrange_pointer_left(&mut self) -> Task<Message> {
        let Some(open) = &mut self.arrange_menu else {
            return Task::none();
        };
        open.leaves += 1;
        let leaves = open.leaves;
        Task::perform(tokio::time::sleep(ARRANGE_MENU_GRACE), move |()| {
            Message::ArrangeLeft(leaves)
        })
    }

    fn update_arrange_input(&self) -> Task<Message> {
        let Some(open) = &self.arrange_menu else {
            return Task::none();
        };
        let region = arrange::input_region(open.submenu);
        Task::done(Message::SetInputRegion {
            id: open.surface,
            callback: ActionCallback::new(move |wl_region| {
                for &(x, y, width, height) in &region {
                    wl_region.add(x, y, width, height);
                }
            }),
        })
    }

    fn is_desktop(&self, surface: &ObjectId) -> bool {
        self.desktop_surface
            .as_ref()
            .is_some_and(|desktop| desktop.id() == *surface)
    }

    fn handle_drag(&mut self, event: dnd::Event) -> Task<Message> {
        match event {
            dnd::Event::Entered { surface, x, y } => {
                self.drag_over = Some(surface);
                self.drag_moved(Point::new(x, y))
            }
            dnd::Event::Moved { x, y } => self.drag_moved(Point::new(x, y)),
            dnd::Event::Left => self.drag_left(),
            dnd::Event::Dropped {
                surface,
                x,
                y,
                uris,
                own,
            } => {
                let left = self.drag_left();
                let at = Point::new(x, y);
                // Out of the fan, it's a new arrival on the desktop, not one of its icons moving.
                let own = own && !self.fan.as_ref().is_some_and(|open| open.dragging);
                let dropped = if self.is_desktop(&surface) {
                    self.drop_on_desktop(uris, own, at)
                } else if self.dock_surface.as_ref() == Some(&surface) {
                    self.drop_on_dock(uris, at)
                } else {
                    Task::none()
                };
                Task::batch([left, dropped])
            }
            dnd::Event::DragEnded => {
                self.desktop.drag_ended();
                if let Some(open) = &mut self.fan
                    && open.dragging
                {
                    open.close();
                }
                Task::done(Message::DesktopRefresh)
            }
        }
    }

    fn drag_moved(&mut self, at: Point) -> Task<Message> {
        let Some(surface) = self.drag_over.clone() else {
            return Task::none();
        };
        if self.is_desktop(&surface) {
            self.desktop.drag_over(Some(at));
            return Task::none();
        }
        if self.dock_surface.as_ref() == Some(&surface) {
            return self.apply(Message::DockPointerMoved(at));
        }
        Task::none()
    }

    fn drag_left(&mut self) -> Task<Message> {
        self.desktop.drag_over(None);
        let was_on_dock = self
            .drag_over
            .take()
            .is_some_and(|surface| self.dock_surface.as_ref() == Some(&surface));
        if was_on_dock {
            return self.apply(Message::DockPointerLeft);
        }
        Task::none()
    }

    fn drop_on_desktop(&mut self, uris: Vec<String>, own: bool, at: Point) -> Task<Message> {
        let paths = dnd::paths_of(&uris);
        if let Some(folder) = self.desktop.folder_at(at) {
            return Task::perform(desktop::move_into(paths, folder), |_| {
                Message::DesktopRefresh
            });
        }
        if own {
            self.desktop.moved_selection_to(at);
            return Task::none();
        }
        Task::perform(
            desktop::move_into(paths, self.desktop.folder()),
            move |names| Message::DesktopReceived(names, at),
        )
    }

    /// Something dropped on the dock: an app opens it, and the places take it in.
    fn drop_on_dock(&mut self, uris: Vec<String>, at: Point) -> Task<Message> {
        let Some(index) = dock::item_at(&self.dock_layout, at) else {
            return Task::none();
        };
        let paths = dnd::paths_of(&uris);
        if let Some(item) = self
            .dock
            .get(index)
            .filter(|_| index < self.dock_layout.apps)
        {
            let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
            if let Some(command) = self.apps.command_with_uris(&item.app.id, &uris) {
                apps::spawn(command);
            }
            return Task::none();
        }
        let place = Place::ALL
            .iter()
            .enumerate()
            .find(|(offset, _)| dock::place_slot(&self.dock_layout, *offset) == index)
            .map(|(_, place)| *place);
        match place {
            Some(Place::Trash) => {
                for path in &paths {
                    // A drive in the trash means eject it, as it's long meant on macOS.
                    if let Some(device) = self.desktop.drive_device(path) {
                        desktop::eject(&device);
                    } else if let Err(err) = places::move_to_trash(path) {
                        log::warn!("can't move {} to the trash: {err}", path.display());
                    }
                }
                Task::done(Message::DesktopRefresh)
            }
            Some(Place::Downloads) => {
                Task::perform(desktop::move_into(paths, places::downloads_dir()), |_| {
                    Message::DesktopRefresh
                })
            }
            None => Task::none(),
        }
    }

    fn start_desktop_rename(&mut self, path: PathBuf) -> Task<Message> {
        self.desktop.start_rename(path);
        iced_layershell::toplevel::set_keyboard_target(Some(self.desktop_id));
        // Its field is there once this update has been drawn.
        Task::done(Message::DesktopRefresh)
    }

    /// Gives the keyboard back, keeping the new name if there's one.
    fn finish_desktop_rename(&mut self) -> Task<Message> {
        self.desktop.commit_rename();
        if self.alert.is_none() {
            iced_layershell::toplevel::set_keyboard_target(None);
        }
        Task::done(Message::DesktopRefresh)
    }

    fn open_desktop_selection(&self) {
        for path in self.desktop.selection() {
            apps::spawn(vec![
                String::from("xdg-open"),
                path.to_string_lossy().into_owned(),
            ]);
        }
    }

    fn run_desktop_action(&mut self, action: desktop::Action) -> Task<Message> {
        use desktop::Action;
        match action {
            Action::NewFolder => match desktop::new_folder(&self.desktop.folder()) {
                Ok(path) => {
                    self.desktop.select_only(path.clone());
                    return self.start_desktop_rename(path);
                }
                Err(err) => log::warn!("can't make a new folder: {err}"),
            },
            Action::Open => self.open_desktop_selection(),
            Action::Rename => {
                if let [path] = self.desktop.selection().as_slice() {
                    return self.start_desktop_rename(path.clone());
                }
            }
            Action::MoveToTrash => {
                for path in self.desktop.selection() {
                    if let Err(err) = places::move_to_trash(&path) {
                        log::warn!("can't move {} to the trash: {err}", path.display());
                    }
                }
                return Task::done(Message::DesktopRefresh);
            }
            Action::GetInfo => {
                let mut paths = self.desktop.selection();
                if paths.is_empty() {
                    paths.push(self.desktop.folder());
                }
                return Task::future(desktop::get_info(paths)).discard();
            }
            Action::ChangeWallpaper => {
                return Task::perform(portal::choose_image(), Message::WallpaperChosen);
            }
            Action::EditWidgets => return self.run_widget_action(widgets::Action::Edit),
            Action::UseStacks => self.desktop.toggle_stacks(),
            Action::Eject => {
                for path in self.desktop.selection() {
                    if let Some(device) = self.desktop.drive_device(&path) {
                        desktop::eject(&device);
                    }
                }
            }
            Action::ViewOptions => {
                let (x, y) = self.desktop.pointer();
                let settings = IcedNewPopupSettings::new(
                    self.desktop_id,
                    desktop::VIEW_OPTIONS_SIZE,
                    (x, y, 1, 1),
                )
                .anchor(PopupAnchor::BottomRight)
                .gravity(PopupGravity::BottomRight);
                self.next_view_options = Some(settings);
                return Task::none();
            }
            Action::SortBy(sort) => self.desktop.set_sort(sort),
            Action::CleanUp => self.desktop.clean_up_action(None),
            Action::CleanUpBy(sort) => self.desktop.clean_up_action(Some(sort)),
        }
        Task::none()
    }

    fn alert_field(&mut self) -> Option<&mut alert::Field> {
        self.alert.as_mut()?.alert.field.as_mut()
    }

    fn ask_for_city(&mut self) -> Task<Message> {
        let glyph = alert::Icon::Glyph(icons::Glyph::Cloud);
        self.open_alert(Alert {
            title: String::from("Which city's weather should this show?"),
            detail: String::from("Its name goes to Open-Meteo, to look up the forecast."),
            confirm: String::from("Show"),
            on_confirm: Message::Nothing,
            icon: glyph,
            field: Some(alert::Field {
                purpose: alert::Purpose::City,
                value: self.widgets.city.clone().unwrap_or_default(),
                visible: true,
                min_length: 1,
            }),
        })
    }

    fn run_widget_action(&mut self, action: widgets::Action) -> Task<Message> {
        use widgets::{Action, Kind};
        match action {
            Action::Add(kind) => {
                let id = self.widgets.add(kind.clone());
                match kind {
                    Kind::Weather if self.widgets.city.is_none() => return self.ask_for_city(),
                    Kind::Weather => return Task::done(Message::FetchWeather),
                    Kind::Photo { .. } => return self.choose_widget_photo(id),
                    _ => {}
                }
            }
            Action::Remove(id) => self.widgets.remove(id),
            Action::SetSize(id, size) => self.widgets.set_size(id, size),
            Action::ChoosePhoto(id) => return self.choose_widget_photo(id),
            Action::ChangeCity => return self.ask_for_city(),
            Action::Edit => {
                if !self.widgets.editing {
                    self.widgets.editing = true;
                    let (id, open) = Message::layershell_open(widgets::gallery_settings());
                    self.surfaces.insert(id, Surface::WidgetGallery);
                    self.gallery_id = Some(id);
                    return open;
                }
            }
            Action::Done => {
                self.widgets.editing = false;
                if let Some(id) = self.gallery_id {
                    return iced_runtime::window::close(id);
                }
            }
        }
        Task::none()
    }

    fn choose_widget_photo(&self, id: u64) -> Task<Message> {
        Task::perform(portal::choose_image(), move |path| {
            Message::WidgetPhotoChosen(id, path)
        })
    }

    /// Reads the photos that widgets show and haven't been read yet.
    fn load_widget_photos(&self) -> Task<Message> {
        Task::batch(self.widgets.photos_to_load().into_iter().map(|path| {
            let reading = path.clone();
            Task::perform(widgets::load_photo(reading), move |picture| {
                Message::WidgetPhotoLoaded(path.clone(), picture)
            })
        }))
    }

    /// Closes the open popup, if any. Its state is cleaned up once the surface is gone.
    fn close_popup(&mut self) -> Task<Message> {
        self.menu_hover = None;
        self.next_submenu = None;
        match &self.popup {
            Some(popup) => iced_runtime::window::close(popup.id),
            None => Task::none(),
        }
    }

    /// Opens a place in the file manager.
    fn open_place(&self, place: Place) {
        let uri = match place {
            Place::Downloads => places::file_uri(&self.downloads_dir),
            Place::Trash => String::from("trash:///"),
        };
        match self.apps.command_with_uris(&self.file_manager, &[&uri]) {
            Some(command) => apps::spawn(command),
            None => apps::spawn(vec![String::from("xdg-open"), uri]),
        }
    }

    fn update_place_icons(&mut self) {
        let trash = if self.trash_count == 0 {
            "user-trash"
        } else {
            "user-trash-full"
        };
        self.place_icons = Place::ALL
            .iter()
            .map(|place| match place {
                Place::Downloads => self.apps.themed_icon("folder-download"),
                Place::Trash => self.apps.themed_icon(trash),
            })
            .collect();
    }

    fn start_drag(&mut self) -> Task<Message> {
        let Some(press) = &self.dock_press else {
            return Task::none();
        };
        let Some(index) = self
            .dock
            .iter()
            .position(|item| item.app.id == press.app_id)
        else {
            return Task::none();
        };
        let Some(grab) = dock::grab_offset(&self.dock_layout, index, press.at) else {
            return Task::none();
        };

        self.dock_drag = Some(dock::Drag {
            index,
            pointer: self.dock_cursor,
            grab,
            removable: self.pinned.contains(&press.app_id)
                || self.dock.get(index).is_some_and(|item| !item.is_running()),
        });
        // Icons are dragged at their plain size, so magnification snaps off.
        self.dock_pointer = dock::Pointer::default();
        self.dock_target = 0.0;
        self.relayout_dock()
    }

    /// Applies what the layout says releasing `drag` does, and saves the pinned apps.
    fn drop_dragged(&mut self, drag: &dock::Drag) {
        let Some(item) = self.dock.get(drag.index) else {
            return;
        };
        let app_id = item.app.id.clone();

        let kept = self.pinned.contains(&app_id);
        match self.dock_layout.drop {
            Some(dock::Drop::Remove) => {
                self.pinned.retain(|id| *id != app_id);
                self.recents.retain(|id| *id != app_id);
                dock::save_recents(&self.recents);
            }
            // Moved around among the recent apps, it's still one of them.
            Some(dock::Drop::Slot(slot))
                if !kept
                    && self
                        .dock_layout
                        .recents
                        .is_some_and(|recents| slot >= recents.start) => {}
            // Dropping an icon somewhere keeps it there, like on macOS: the dock's new order,
            // filtered down to pinned apps (now including this one), becomes the pinned list.
            Some(dock::Drop::Slot(slot)) => {
                let mut order: Vec<String> = self
                    .dock
                    .iter()
                    .enumerate()
                    .filter(|&(index, _)| index != drag.index)
                    .map(|(_, item)| item.app.id.clone())
                    .collect();
                order.insert(slot.min(order.len()), app_id.clone());

                let mut pinned: HashSet<String> = self.pinned.drain(..).collect();
                self.recents.retain(|id| *id != app_id);
                dock::save_recents(&self.recents);
                pinned.insert(app_id);
                self.pinned = order.into_iter().filter(|id| pinned.contains(id)).collect();
            }
            None => return,
        }

        if let Err(err) = config::save_pinned(&self.pinned) {
            log::error!("can't save pinned apps: {err}");
        }
        self.dock = self.dock_items();
    }

    /// Keeps the list of minimized windows up to date, and asks for pictures of new ones.
    fn sync_minimized(&mut self) -> Task<Message> {
        let windows = &self.windows;
        // A window that was hidden and is back is an ordinary one again.
        self.hidden_windows.retain(|id| {
            windows
                .iter()
                .any(|window| window.id == *id && window.is_minimized)
        });
        let hidden = &self.hidden_windows;
        self.minimized.retain(|known| {
            !hidden.contains(&known.id)
                && windows
                    .iter()
                    .any(|window| window.id == known.id && window.is_minimized)
        });
        let mut captures = Vec::new();
        for window in windows.iter().filter(|window| window.is_minimized) {
            if hidden.contains(&window.id)
                || self.minimized.iter().any(|known| known.id == window.id)
            {
                continue;
            }
            let Some(app_id) = &window.app_id else {
                continue;
            };
            self.minimized.push(dock::Minimized {
                id: window.id,
                title: window.title.clone().unwrap_or_default(),
                image: None,
                app: self.apps.resolve(app_id),
            });
            let id = window.id;
            captures.push(Task::perform(
                kwin::capture_window(window.uuid.clone(), THUMBNAIL_SIZE),
                move |image| Message::WindowCaptured(id, image),
            ));
        }
        Task::batch(captures)
    }

    /// Focuses the app, or launches it with a bounce if it isn't running.
    fn activate(&mut self, app_id: &str) -> Task<Message> {
        let Some(item) = self.dock.iter().find(|item| item.app.id == app_id) else {
            return Task::none();
        };
        if !item.has_windows() && item.app.command.is_some() {
            self.launching.insert(app_id.to_owned(), anim::now());
        }
        // An app that was hidden comes back with all its windows.
        let hidden: Vec<u64> = item
            .window_ids()
            .into_iter()
            .filter(|id| self.hidden_windows.contains(id))
            .collect();
        if hidden.is_empty() {
            dock::activate(item);
        } else {
            hidden.into_iter().rev().for_each(kwin::focus_window);
        }
        self.relayout_dock()
    }

    fn run_dock_action(&mut self, action: dock::MenuAction) -> Task<Message> {
        match action {
            dock::MenuAction::FocusWindow(id) => kwin::focus_window(id),
            dock::MenuAction::Open(app_id) => return self.activate(&app_id),
            dock::MenuAction::Launch(command) => apps::spawn(command),
            dock::MenuAction::ToggleLogin(app_id) => {
                autostart::set_enabled(
                    &app_id,
                    self.apps.entry_path(&app_id),
                    !autostart::is_enabled(&app_id),
                );
            }
            dock::MenuAction::ShowAll(uuids) => {
                return Task::future(kwin::show_windows(uuids)).discard();
            }
            dock::MenuAction::Hide(ids) => {
                self.hidden_windows.extend(ids.iter().copied());
                let uuids = self
                    .windows
                    .iter()
                    .filter(|window| ids.contains(&window.id))
                    .map(|window| window.uuid.clone())
                    .collect();
                // The genie has to hear before the windows go.
                return Task::perform(arrange::skip_genie(uuids), move |()| {
                    Message::HideWindows(ids.clone())
                });
            }
            dock::MenuAction::Quit(app_id) => {
                let Some(item) = self.dock.iter().find(|item| item.app.id == app_id) else {
                    return Task::none();
                };
                if !item.has_windows() {
                    item.processes()
                        .iter()
                        .for_each(|process| process.terminate());
                    return Task::none();
                }
                // Asked nicely first, since an app may want to save something, and only what's
                // left after that is told to quit.
                item.window_ids().into_iter().for_each(kwin::close_window);
                return Task::perform(tokio::time::sleep(QUIT_GRACE), move |()| {
                    Message::QuitGraceOver(app_id.clone())
                });
            }
            dock::MenuAction::ForgetRecent(app_id) => {
                self.recents.retain(|id| *id != app_id);
                dock::save_recents(&self.recents);
                self.dock = self.dock_items();
            }
            dock::MenuAction::TogglePin(app_id) => {
                if let Some(index) = self.pinned.iter().position(|id| *id == app_id) {
                    self.pinned.remove(index);
                } else {
                    self.recents.retain(|id| *id != app_id);
                    dock::save_recents(&self.recents);
                    self.pinned.push(app_id);
                }
                if let Err(err) = config::save_pinned(&self.pinned) {
                    log::error!("can't save pinned apps: {err}");
                }
                self.dock = self.dock_items();
            }
            dock::MenuAction::OpenPlace(place) => self.open_place(place),
            dock::MenuAction::ConfirmEmptyTrash => {
                let items = if self.trash_count == 1 {
                    "item"
                } else {
                    "items"
                };
                let alert = Alert {
                    title: format!(
                        "Are you sure you want to permanently erase the {} {items} in the Trash?",
                        self.trash_count
                    ),
                    detail: String::from("You can't undo this action."),
                    confirm: String::from("Empty Trash"),
                    on_confirm: Message::DockMenu(dock::MenuAction::EmptyTrash),
                    icon: self
                        .apps
                        .themed_icon("user-trash-full")
                        .map_or(alert::Icon::None, alert::Icon::Image),
                    field: None,
                };
                return self.open_alert(alert);
            }
            dock::MenuAction::EmptyTrash => {
                if let Err(err) = places::empty_trash(&self.trash_dir) {
                    log::error!("can't empty the trash: {err}");
                }
                self.trash_count = places::trash_count(&self.trash_dir);
                self.update_place_icons();
            }
        }
        Task::none()
    }

    /// Recomputes the dock layout, and pushes whatever changed to the material and the input
    /// region.
    /// Reads the apps' icons again, from an icon theme that's just changed.
    fn reload_icons(&mut self) -> Task<Message> {
        self.apps = AppIndex::load();
        self.dock = self.dock_items();
        self.update_place_icons();
        self.spotlight_apps.clear();
        self.spotlight_unread = self.apps.listed_ids();
        self.relayout_dock()
    }

    fn relayout_dock(&mut self) -> Task<Message> {
        // Nothing to place until the compositor has given the surface a size.
        if self.dock_width == 0.0 {
            return Task::none();
        }

        let now = anim::now();
        // Not for what a drag already made room for, nor for the dock filling up as the session starts.
        let dragging = self.dock_drag.is_some();
        self.dock_room.update(
            dock::slot_keys(&self.dock, &self.minimized),
            !dragging && !self.dock_was_dragged && self.started.elapsed() > DOCK_SETTLE,
            now,
        );
        self.dock_was_dragged = dragging;
        let room = self.dock_room.room(now);
        let start = self.recents_start();
        self.dock_section
            .go_to(if start.is_some() { 1.0 } else { 0.0 });
        // It stays where the kept apps end while the line goes.
        let recents = (start.is_some() || self.dock_section.is_moving())
            .then(|| (self.pinned.len(), self.dock_section.value()));
        let bounce = self
            .dock
            .iter()
            .map(|item| match self.launching.get(&item.app.id) {
                Some(started) => dock::launch_bounce(now.duration_since(*started).as_secs_f32()),
                None if item.wants_attention() => {
                    dock::attention_bounce(now.duration_since(self.started).as_secs_f32())
                }
                None => 0.0,
            })
            .collect();

        let mut layout = dock::layout(
            self.dock_width,
            &self.dock,
            &self.minimized,
            &self.dock_pointer,
            self.dock_max_icon,
            bounce,
            self.dock_drag.as_ref(),
            &room,
            recents,
        );
        // Like macOS, an open menu or stack hides the name over the icon. It would show
        // through the popup's clear glass anyway.
        if self.popup.is_some() || self.fan_is_open() {
            layout.label = None;
        }
        if layout == self.dock_layout {
            return Task::none();
        }
        if let Some(open) = &mut self.fan
            && let Some(tile) = layout.icons.get(dock::place_slot(&layout, 0))
        {
            open.set_tile(tile.center_x());
        }
        let input_region = dock::input_region(&layout);
        let input_changed = input_region != dock::input_region(&self.dock_layout);
        self.dock_layout = layout;

        let shapes = dock::material_shapes(&self.dock_layout);
        if shapes != self.dock_shapes
            && let (Some(material), Some(target)) = (&self.material, &self.dock_material)
        {
            material.queue_faded_shapes(target, &shapes, &[]);
            self.dock_shapes = shapes;
        }

        if !input_changed {
            return Task::none();
        }
        Task::done(Message::SetInputRegion {
            id: self.dock_id,
            callback: ActionCallback::new(move |region| {
                for &(x, y, width, height) in &input_region {
                    region.add(x, y, width, height);
                }
            }),
        })
    }

    fn view(&self, id: IcedId) -> Element<'_> {
        match self.surfaces.get(&id) {
            Some(Surface::Wallpaper) => match &self.wallpaper {
                Some(handle) => wallpaper::view(handle),
                None => Space::new().into(),
            },
            Some(Surface::Desktop) => desktop::view(
                &self.desktop,
                self.focused_app.is_none(),
                self.widgets
                    .layers(self.widget_tint.value(), self.screen_height),
            ),
            Some(Surface::WidgetGallery) => {
                widgets::gallery(&self.widgets, self.material.as_ref().map(|m| m.kind()))
            }
            Some(Surface::ArrangeMenu) => {
                arrange::view(self.arrange_menu.as_ref().is_some_and(|open| open.submenu))
            }
            Some(Surface::MenuBar) => menubar::view(&self.bar()),
            Some(Surface::Dock) => dock::view(
                &self.dock,
                &self.place_icons,
                &self.minimized,
                &self.dock_badges,
                &self.dock_layout,
                self.material.as_ref().map(|m| m.kind()),
                self.fan_is_open(),
                &self.pile_pictures,
            ),
            Some(Surface::Hud) => match &self.hud {
                Some(open) => open.view(),
                None => Space::new().into(),
            },
            Some(Surface::FullScreenTrigger) => {
                Space::new().width(Length::Fill).height(Length::Fill).into()
            }
            Some(Surface::Fan) => match &self.fan {
                Some(open) => open.view(),
                None => Space::new().into(),
            },
            Some(Surface::Popup) => {
                let material = self.material.as_ref().map(|m| m.kind());
                if let Some(index) = self.submenus.iter().position(|menu| menu.id == id) {
                    let open_row = self.submenus.get(index + 1).map(|menu| menu.opener);
                    return menu::view(
                        &self.submenus[index].entries,
                        material,
                        index + 1,
                        open_row,
                        false,
                        self.menu_blink_at(index + 1),
                    );
                }
                match self.popup.as_ref().map(|popup| &popup.content) {
                    Some(PopupContent::Menu(entries)) => {
                        let open_row = self.submenus.first().map(|menu| menu.opener);
                        let has_tail = self
                            .popup
                            .as_ref()
                            .is_some_and(|popup| popup.tail.is_some());
                        menu::view(
                            entries,
                            material,
                            0,
                            open_row,
                            has_tail,
                            self.menu_blink_at(0),
                        )
                    }
                    Some(PopupContent::ViewOptions) => {
                        desktop::view_options(self.desktop.metrics())
                    }
                    Some(PopupContent::ControlCenter) => control::view(&self.control, material),
                    Some(PopupContent::Status(detail)) => {
                        control::status_view(*detail, &self.control, material)
                    }
                    Some(PopupContent::NotificationCenter) => notification_center::view(
                        &self.notifications,
                        &self.open_groups,
                        self.center_scroll as usize,
                        self.card_pointed_at(),
                        self.popup.as_ref().map_or(0.0, |popup| popup.size.1 as f32),
                        &self.center_motion,
                        &self.notification_fade,
                        material,
                    ),
                    None => Space::new().into(),
                }
            }
            Some(Surface::Spotlight) => match &self.spotlight {
                Some(open) => open.view(
                    &self.spotlight_apps,
                    &self.clips,
                    &self.spotlight_file_icons,
                    self.menubar_width,
                    self.screen_height,
                ),
                None => Space::new().into(),
            },
            Some(Surface::Alert) => match &self.alert {
                Some(open) => alert::view(
                    &open.alert,
                    open.active,
                    self.material.as_ref().map(|m| m.kind()),
                ),
                None => Space::new().into(),
            },
            Some(Surface::Banners) => self.banners.view(
                self.card_pointed_at(),
                &self.notification_fade,
                self.material.as_ref().map(|m| m.kind()),
            ),
            None => Space::new().into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        // What moves goes by the frames, right after each one's drawn, so that it's sampled at a
        // steady distance from when it's shown.
        let frames = if self.is_moving() {
            Subscription::batch([
                iced_futures::event::listen_raw(|event, _, _| match event {
                    Event::Window(window::Event::RedrawRequested(at)) => Some(Message::Frame(at)),
                    _ => None,
                }),
                time::every(FRAME_KICK).map(|_| Message::FrameKick),
            ])
        } else {
            Subscription::none()
        };

        // What the Control Center shows can change under it, like a track ending.
        let control_open = matches!(
            self.popup.as_ref().map(|popup| &popup.content),
            Some(PopupContent::ControlCenter | PopupContent::Status(_))
        );
        // Light turns to dark at sunset, with nothing else to say so.
        let appearance_tick = if self.appearance_auto {
            time::every(Duration::from_secs(60)).map(|_| Message::AppearanceTick)
        } else {
            Subscription::none()
        };
        // Apps come and go, and each wants its own tile.
        let icon_check = time::every(Duration::from_secs(600)).map(|_| Message::IconThemesCheck);
        let control_refresh = if control_open {
            time::every(Duration::from_secs(2)).map(|_| Message::ControlRefresh)
        } else if self.show_wifi || self.show_bluetooth || self.show_sound {
            time::every(Duration::from_secs(3)).map(|_| Message::BarStatusRefresh)
        } else {
            Subscription::none()
        };

        // Brightness events arrive on the material's queue, which nothing else dispatches.
        let bar_brightness = if self.bar_tracks_brightness {
            time::every(Duration::from_millis(250)).map(|_| Message::BarBrightness)
        } else {
            Subscription::none()
        };

        // Banners need watching to expire.
        let banner_expiry = if self.banners.counting_down() {
            time::every(Duration::from_millis(250)).map(Message::BannerFrame)
        } else {
            Subscription::none()
        };

        let app_menu = match &self.app_menu_address {
            Some(address) => appmenu::follow(address.clone())
                .map(|(address, items)| Message::AppMenuChanged(address, items)),
            None => Subscription::none(),
        };

        let spotlight_load = if self.spotlight_unread.is_empty() {
            Subscription::none()
        } else {
            time::every(Duration::from_millis(30)).map(|_| Message::SpotlightLoadApps)
        };
        let hud_expiry = if self.hud.is_some() {
            time::every(Duration::from_millis(100)).map(|_| Message::HudFrame)
        } else {
            Subscription::none()
        };
        let spotlight_tick = if self.spotlight.is_some() {
            time::every(Duration::from_millis(250)).map(|_| Message::SpotlightTick)
        } else {
            Subscription::none()
        };
        let menu_hover = if self.menu_hover.is_some() {
            time::every(Duration::from_millis(30)).map(|_| Message::MenuHoverCheck)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            frames,
            banner_expiry,
            menu_hover,
            notifications::events().map(Message::Notification),
            network::secret_agent().map(Message::WifiAgent),
            dnd::events().map(Message::Dnd),
            arrange::events().map(Message::Arrange),
            clipboard::events().map(Message::Copied),
            hud_expiry,
            spotlight_load,
            spotlight_tick,
            time::every(Duration::from_secs(3)).map(|_| Message::CheckEthernet),
            time::every(Duration::from_secs(2)).map(|_| Message::DesktopRefresh),
            time::every(Duration::from_secs(2)).map(|_| Message::CheckRunning),
            time::every(widgets::WEATHER_INTERVAL).map(|_| Message::FetchWeather),
            tray::items().map(Message::TrayItems),
            launcher::updates().map(Message::LauncherUpdate),
            app_menu,
            appmenu::registrar(),
            bar_brightness,
            control_refresh,
            appearance_tick,
            icon_check,
            kwin::windows().map(Message::WindowsChanged),
            kwin::keyboard_layouts().map(Message::KeyboardLayoutsChanged),
            battery::watch().map(Message::BatteryChanged),
            time::every(Duration::from_secs(1)).map(|_| Message::Tick),
            iced_futures::event::listen_with(|event, status, id| match event {
                Event::Window(window::Event::Opened { size, .. }) => {
                    Some(Message::SurfaceOpened(id, size))
                }
                Event::Window(window::Event::Resized(size)) => {
                    Some(Message::SurfaceResized(id, size))
                }
                Event::Window(window::Event::Closed) => Some(Message::SurfaceClosed(id)),
                Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => Some(
                    Message::AddingModifiers(modifiers.control() || modifiers.shift()),
                ),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(key),
                    ..
                }) if matches!(
                    key,
                    keyboard::key::Named::Enter | keyboard::key::Named::Escape
                ) =>
                {
                    Some(Message::ReturnOrEscape(key == keyboard::key::Named::Enter))
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(key),
                    modifiers,
                    ..
                }) if modifiers.logo() && matches!(key.as_str(), "1" | "2" | "3" | "4") => {
                    key.parse().ok().map(Message::SpotlightHotkey)
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(key),
                    ..
                }) if matches!(
                    key,
                    keyboard::key::Named::ArrowUp
                        | keyboard::key::Named::ArrowDown
                        | keyboard::key::Named::ArrowLeft
                        | keyboard::key::Named::ArrowRight
                ) =>
                {
                    Some(match key {
                        keyboard::key::Named::ArrowUp => Message::SpotlightMoved(-1, 0),
                        keyboard::key::Named::ArrowDown => Message::SpotlightMoved(1, 0),
                        keyboard::key::Named::ArrowLeft => Message::SpotlightMoved(0, -1),
                        _ => Message::SpotlightMoved(0, 1),
                    })
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    Some(Message::PointerMoved(id, position))
                }
                Event::Mouse(mouse::Event::CursorEntered) => Some(Message::PointerEntered(id)),
                Event::Mouse(mouse::Event::CursorLeft) => Some(Message::PointerLeft(id)),
                Event::Mouse(mouse::Event::ButtonPressed(_))
                    if status == event::Status::Ignored =>
                {
                    Some(Message::SurfacePressed(id))
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::PointerReleased(id))
                }
                _ => None,
            }),
        ])
    }
}

/// Looks again a little after the pointer's left the bars over a full-screen window.
/// How far below the screen the dock goes over a full-screen window: the pill, the gap under it,
/// and room for its shadow.
fn dock_hidden_height() -> f32 {
    dock::reserved_height() + 24.0
}

fn full_screen_check_later() -> Task<Message> {
    Task::future(async {
        tokio::time::sleep(full_screen::HIDE_AFTER + Duration::from_millis(20)).await;
        Message::FullScreenCheck
    })
}

/// The size of a fully magnified icon, which is the plain size without magnification.
fn dock_max_icon(config: &config::DockConfig) -> f32 {
    if config.magnification {
        config.magnified_size.clamp(dock::base_icon(), 128.0)
    } else {
        dock::base_icon()
    }
}

fn namespace() -> String {
    String::from("makshell")
}

/// Surfaces are transparent; each view draws its own background.
fn style(_: &Shell, _: &Theme) -> theme::Style {
    theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: Color::WHITE,
    }
}
