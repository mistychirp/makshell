//! Monochrome glyphs drawn by the shell itself, in the spirit of SF Symbols, tinted to whatever
//! color they sit on.

use iced_widget::core::Color;
use iced_widget::svg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// Two switches: the Control Center's icon.
    ControlCenter,
    Wifi,
    WifiMedium,
    WifiWeak,
    WifiOff,
    Bluetooth,
    BluetoothOff,
    SunLow,
    SunHigh,
    Camera,
    Lock,
    SpeakerLow,
    SpeakerHigh,
    SpeakerMuted,
    Play,
    Pause,
    Previous,
    Next,
    Close,
    /// Two triangles pointing in to the middle, on the green light of a full-screen window.
    ExitFullScreen,
    /// Half a circle, for sleep.
    Sleep,
    Restart,
    Power,
    Person,
    /// A window with a title bar, for a menu's list of an app's windows.
    Window,
    /// Where sound goes: a screen with sound coming from it, like AirPlay's.
    Outputs,
    Cloud,
    Moon,
    Rain,
    Snow,
    Bolt,
    /// A picture of mountains.
    Photo,
    /// Piles of things, for stacks.
    Stacks,
    Gear,
    /// A folder with a plus, for making one.
    NewFolder,
    /// An i in a circle.
    Info,
    /// Arrows up and down, for sorting.
    Sort,
    /// Dots in rows, for tidying up.
    Grid,
    Trash,
    /// Angle brackets around dots: a wired network, like macOS's.
    Ethernet,
    /// For a ticked box.
    Check,
    /// A network that wants a password.
    LockSmall,
    /// What marks a menu item that opens a submenu.
    Chevron,
    /// A drop of something clear, for the glass settings.
    Drop,
    /// A circle half filled in, for the appearance settings.
    Appearance,
    /// Three dots, for more of what can be done.
    Ellipsis,
    Bell,
    /// A magnifying glass.
    Search,
    /// What the Downloads tile shows while its fan is open.
    ChevronDown,
    Folder,
    Clipboard,
    Keyboard,
}

impl Glyph {
    fn source(self) -> &'static str {
        match self {
            Glyph::ControlCenter => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.7">
                <rect x="2.5" y="3.5" width="19" height="7.5" rx="3.75"/>
                <circle cx="17.75" cy="7.25" r="2.3" fill="#000" stroke="none"/>
                <rect x="2.5" y="13" width="19" height="7.5" rx="3.75"/>
                <circle cx="6.25" cy="16.75" r="2.3" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::Wifi => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.4" stroke-linecap="round">
                <path d="M2 9.2a14.5 14.5 0 0 1 20 0"/>
                <path d="M5.4 12.7a9.6 9.6 0 0 1 13.2 0"/>
                <path d="M8.8 16.2a4.7 4.7 0 0 1 6.4 0"/>
                <circle cx="12" cy="19.6" r="1.7" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::WifiMedium => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.4" stroke-linecap="round">
                <path d="M5.4 12.7a9.6 9.6 0 0 1 13.2 0"/>
                <path d="M8.8 16.2a4.7 4.7 0 0 1 6.4 0"/>
                <circle cx="12" cy="19.6" r="1.7" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::WifiWeak => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.4" stroke-linecap="round">
                <path d="M8.8 16.2a4.7 4.7 0 0 1 6.4 0"/>
                <circle cx="12" cy="19.6" r="1.7" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::Sleep => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.2">
                <circle cx="12" cy="12" r="9"/>
                <path d="M3 12h18" />
                <path d="M3.5 12a8.5 8.5 0 0 0 17 0z" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::Restart => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.2" stroke-linejoin="round">
                <path d="M19 4v16L5 12z"/>
                </svg>"##
            }
            Glyph::Power => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.2" stroke-linecap="round">
                <path d="M12 3v9"/>
                <path d="M7 6.2a8.5 8.5 0 1 0 10 0"/>
                </svg>"##
            }
            Glyph::Person => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2">
                <circle cx="12" cy="12" r="9.5"/>
                <circle cx="12" cy="9.5" r="3.5" fill="#000" stroke="none"/>
                <path d="M5.5 18.5a7.5 7.5 0 0 1 13 0" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::Window => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2">
                <rect x="3" y="5" width="18" height="14" rx="3"/>
                <path d="M6 9h1M9 9h1M12 9h1" stroke-linecap="round"/>
                </svg>"##
            }
            Glyph::Outputs => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
                <path d="M6.6 17.2a8.4 8.4 0 1 1 10.8 0"/>
                <path d="M8.9 14.2a4.6 4.6 0 1 1 6.2 0"/>
                <path d="M12 15.2l5 6H7z" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::LockSmall => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <rect x="6" y="11" width="12" height="9" rx="2" fill="#000"/>
                <path d="M8.5 11V8a3.5 3.5 0 0 1 7 0v3" fill="none" stroke="#000" stroke-width="2"/>
                </svg>"##
            }
            Glyph::WifiOff => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.4" stroke-linecap="round">
                <path d="M2 9.2a14.5 14.5 0 0 1 20 0"/>
                <path d="M5.4 12.7a9.6 9.6 0 0 1 13.2 0"/>
                <path d="M8.8 16.2a4.7 4.7 0 0 1 6.4 0"/>
                <circle cx="12" cy="19.6" r="1.7" fill="#000" stroke="none"/>
                <path d="M4 3l16 18"/>
                </svg>"##
            }
            Glyph::BluetoothOff => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M6.5 7l11 10-5.5 5V2l5.5 5-11 10"/>
                <path d="M4 3l16 18"/>
                </svg>"##
            }
            Glyph::SunLow => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round">
                <circle cx="12" cy="12" r="4.5" fill="#000" stroke="none"/>
                <path d="M12 4v1M12 19v1M4 12h1M19 12h1M6.3 6.3l.7.7M17 17l.7.7M6.3 17.7l.7-.7M17 7l.7-.7"/>
                </svg>"##
            }
            Glyph::SunHigh => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.2" stroke-linecap="round">
                <circle cx="12" cy="12" r="4.5" fill="#000" stroke="none"/>
                <path d="M12 1.5v3M12 19.5v3M1.5 12h3M19.5 12h3M4.6 4.6l2.1 2.1M17.3 17.3l2.1 2.1M4.6 19.4l2.1-2.1M17.3 6.7l2.1-2.1"/>
                </svg>"##
            }
            Glyph::Camera => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                <path d="M3 8V5a2 2 0 0 1 2-2h3M16 3h3a2 2 0 0 1 2 2v3M21 16v3a2 2 0 0 1-2 2h-3M8 21H5a2 2 0 0 1-2-2v-3"/>
                <path d="M7 10.5a1 1 0 0 1 1-1h1.5l1-1.5h3l1 1.5H16a1 1 0 0 1 1 1V16a1 1 0 0 1-1 1H8a1 1 0 0 1-1-1z" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::Cloud => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M6.6 19.5a4.6 4.6 0 0 1-.7-9.15 6.3 6.3 0 0 1 12-1.25 5.3 5.3 0 0 1-.4 10.4z"/>
                </svg>"##
            }
            Glyph::Moon => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M20.5 14.6A8.9 8.9 0 0 1 9.4 3.5a9 9 0 1 0 11.1 11.1z"/>
                </svg>"##
            }
            Glyph::Rain => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M6.6 15a4.1 4.1 0 0 1-.6-8.15 5.8 5.8 0 0 1 11-1.15A4.8 4.8 0 0 1 17 15z"/>
                <path d="M8 17.5l-1.2 3M12.6 17.5l-1.2 3M17.2 17.5l-1.2 3" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round"/>
                </svg>"##
            }
            Glyph::Snow => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M6.6 15a4.1 4.1 0 0 1-.6-8.15 5.8 5.8 0 0 1 11-1.15A4.8 4.8 0 0 1 17 15z"/>
                <circle cx="8" cy="18.2" r="1.3"/><circle cx="12" cy="20.3" r="1.3"/><circle cx="16" cy="18.2" r="1.3"/>
                </svg>"##
            }
            Glyph::Bolt => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M6.6 14a4.1 4.1 0 0 1-.6-8.15 5.8 5.8 0 0 1 11-1.15A4.8 4.8 0 0 1 17 14h-2.7l-2-1.2L9 14z"/>
                <path d="M12.8 13l-3.4 4.8h2.7l-1.3 4.2 4.4-5.8h-2.7l1.6-3.2z"/>
                </svg>"##
            }
            Glyph::Photo => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round">
                <rect x="3" y="4.5" width="18" height="15" rx="2.6"/>
                <circle cx="8.3" cy="9.5" r="1.7" fill="#000" stroke="none"/>
                <path d="M3.5 17.5l5.2-4.6 3.3 2.7 3.7-4 5 5"/>
                </svg>"##
            }
            Glyph::Stacks => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round">
                <rect x="3.5" y="3.5" width="17" height="6" rx="1.6"/>
                <rect x="3.5" y="13" width="7" height="7.5" rx="1.6"/>
                <rect x="13.5" y="13" width="7" height="7.5" rx="1.6"/>
                </svg>"##
            }
            Glyph::ChevronDown => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round">
                <path d="M5 9l7 7 7-7"/>
                </svg>"##
            }
            Glyph::Search => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.2" stroke-linecap="round">
                <circle cx="10.5" cy="10.5" r="6.5"/>
                <path d="M15.5 15.5 21 21"/>
                </svg>"##
            }
            Glyph::Folder => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round">
                <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h3.7l2.3 2.5h7A2.5 2.5 0 0 1 21 10v7.5a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 17.5z"/>
                </svg>"##
            }
            Glyph::Clipboard => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round">
                <rect x="5" y="4.5" width="14" height="17" rx="2.5"/>
                <rect x="9" y="2.5" width="6" height="4" rx="1.2" fill="#000"/>
                </svg>"##
            }
            Glyph::Drop => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <path d="M12 2.5c3.6 4.6 7 8.3 7 12.2a7 7 0 0 1-14 0c0-3.9 3.4-7.6 7-12.2z"/>
                </svg>"##
            }
            Glyph::Ellipsis => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <circle cx="5" cy="12" r="2.1"/><circle cx="12" cy="12" r="2.1"/><circle cx="19" cy="12" r="2.1"/>
                </svg>"##
            }
            Glyph::Bell => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <path d="M12 2.5a1.6 1.6 0 0 1 1.6 1.6v.5c3 .7 4.9 3.3 4.9 6.4 0 3.2.6 4.7 1.9 6 .5.6.1 1.5-.7 1.5H4.3c-.8 0-1.2-.9-.7-1.5 1.3-1.3 1.9-2.8 1.9-6 0-3.1 1.9-5.7 4.9-6.4v-.5A1.6 1.6 0 0 1 12 2.5z"/>
                <path d="M9.4 20h5.2a2.6 2.6 0 0 1-5.2 0z"/>
                </svg>"##
            }
            Glyph::Appearance => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2">
                <circle cx="12" cy="12" r="8.5"/>
                <path d="M12 3.5a8.5 8.5 0 0 1 0 17z" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::Keyboard => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round">
                <rect x="2.5" y="6" width="19" height="12.5" rx="2.5"/>
                <path d="M6.5 10h.01M10.2 10h.01M13.8 10h.01M17.5 10h.01M8 14.5h8"/>
                </svg>"##
            }
            Glyph::Gear => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round">
                <circle cx="12" cy="12" r="3"/>
                <path d="M12 3v2.6M12 18.4V21M3 12h2.6M18.4 12H21M5.6 5.6l1.9 1.9M16.5 16.5l1.9 1.9M18.4 5.6l-1.9 1.9M7.5 16.5l-1.9 1.9" stroke-linecap="round" stroke-width="2.2"/>
                <circle cx="12" cy="12" r="6.6"/>
                </svg>"##
            }
            Glyph::NewFolder => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round">
                <path d="M3 18V7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>
                <path d="M12 11v5.5M9.2 13.75h5.6"/>
                </svg>"##
            }
            Glyph::Info => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round">
                <circle cx="12" cy="12" r="9"/>
                <path d="M12 11v5.6" stroke-width="2.1"/>
                <circle cx="12" cy="7.8" r="1.25" fill="#000" stroke="none"/>
                </svg>"##
            }
            Glyph::Sort => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
                <path d="M8 19.5V4.5M3.5 9 8 4.5 12.5 9"/>
                <path d="M16 4.5v15M11.5 15l4.5 4.5 4.5-4.5"/>
                </svg>"##
            }
            Glyph::Grid => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <rect x="3.5" y="4" width="4.6" height="4.6" rx="1"/><rect x="9.7" y="4" width="4.6" height="4.6" rx="1"/><rect x="15.9" y="4" width="4.6" height="4.6" rx="1"/>
                <rect x="3.5" y="9.7" width="4.6" height="4.6" rx="1"/><rect x="9.7" y="9.7" width="4.6" height="4.6" rx="1"/><rect x="15.9" y="9.7" width="4.6" height="4.6" rx="1"/>
                <rect x="3.5" y="15.4" width="4.6" height="4.6" rx="1"/><rect x="9.7" y="15.4" width="4.6" height="4.6" rx="1"/><rect x="15.9" y="15.4" width="4.6" height="4.6" rx="1"/>
                </svg>"##
            }
            Glyph::Trash => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                <path d="M4 6.5h16M9 6.5V4.8a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v1.7"/>
                <path d="M6 6.5l1 13a1.5 1.5 0 0 0 1.5 1.4h7a1.5 1.5 0 0 0 1.5-1.4l1-13"/>
                <path d="M10 10.5v6.5M14 10.5v6.5"/>
                </svg>"##
            }
            Glyph::Ethernet => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <path d="M6.6 6.2 1.6 12l5 5.8" fill="none" stroke="#000" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"/>
                <path d="M17.4 6.2l5 5.8-5 5.8" fill="none" stroke="#000" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"/>
                <circle cx="7.9" cy="12" r="1.5"/>
                <circle cx="12" cy="12" r="1.5"/>
                <circle cx="16.1" cy="12" r="1.5"/>
                </svg>"##
            }
            Glyph::Check => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="3.4" stroke-linecap="round" stroke-linejoin="round">
                <path d="M4.5 12.8l5 5L19.5 6.5"/>
                </svg>"##
            }
            Glyph::Lock => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <rect x="5" y="10.5" width="14" height="10.5" rx="2.2" fill="#000"/>
                <path d="M8 10.5V7.5a4 4 0 0 1 8 0v3" fill="none" stroke="#000" stroke-width="2"/>
                </svg>"##
            }
            Glyph::Bluetooth => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M6.5 7l11 10-5.5 5V2l5.5 5-11 10"/>
                </svg>"##
            }
            Glyph::SpeakerLow => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M3 9h4l5-4.5v15L7 15H3z" fill="#000"/>
                <path d="M15.5 8.8a4.6 4.6 0 0 1 0 6.4" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round"/>
                </svg>"##
            }
            Glyph::SpeakerHigh => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M2 9h4l5-4.5v15L6 15H2z" fill="#000"/>
                <g fill="none" stroke="#000" stroke-width="2" stroke-linecap="round">
                <path d="M14.5 8.8a4.6 4.6 0 0 1 0 6.4"/>
                <path d="M17.6 5.8a9 9 0 0 1 0 12.4"/>
                <path d="M20.6 2.9a13.2 13.2 0 0 1 0 18.2"/>
                </g>
                </svg>"##
            }
            Glyph::SpeakerMuted => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M2 9h4l5-4.5v15L6 15H2z" fill="#000"/>
                <path d="M15 9l6 6M21 9l-6 6" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round"/>
                </svg>"##
            }
            Glyph::Play => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                <path d="M7 4.5v15a1 1 0 0 0 1.5.9l12-7.5a1 1 0 0 0 0-1.8l-12-7.5A1 1 0 0 0 7 4.5z" fill="#000"/>
                </svg>"##
            }
            Glyph::Pause => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <rect x="5.5" y="4" width="4.5" height="16" rx="1.2"/>
                <rect x="14" y="4" width="4.5" height="16" rx="1.2"/>
                </svg>"##
            }
            Glyph::Previous => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <path d="M11.5 6.2v11.6a.8.8 0 0 1-1.2.7L1.6 12.7a.8.8 0 0 1 0-1.4l8.7-5.8a.8.8 0 0 1 1.2.7z"/>
                <path d="M22 6.2v11.6a.8.8 0 0 1-1.2.7l-8.7-5.8a.8.8 0 0 1 0-1.4l8.7-5.8a.8.8 0 0 1 1.2.7z"/>
                </svg>"##
            }
            Glyph::Chevron => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
                <path d="M8.5 4l8 8-8 8"/>
                </svg>"##
            }
            Glyph::Close => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2.6" stroke-linecap="round">
                <path d="M4 4l16 16M20 4L4 20"/>
                </svg>"##
            }
            Glyph::ExitFullScreen => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <path d="M11 3v8H3z M13 21v-8h8z"/>
                </svg>"##
            }
            Glyph::Next => {
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000">
                <path d="M12.5 6.2v11.6a.8.8 0 0 0 1.2.7l8.7-5.8a.8.8 0 0 0 0-1.4l-8.7-5.8a.8.8 0 0 0-1.2.7z"/>
                <path d="M2 6.2v11.6a.8.8 0 0 0 1.2.7l8.7-5.8a.8.8 0 0 0 0-1.4L3.2 5.5a.8.8 0 0 0-1.2.7z"/>
                </svg>"##
            }
        }
    }
}

/// `glyph` at `size` logical pixels, in `color`.
pub fn icon<'a, Message: 'a>(
    glyph: Glyph,
    size: f32,
    color: Color,
) -> iced_widget::core::Element<
    'a,
    Message,
    iced_widget::core::Theme,
    iced_widget::renderer::Renderer,
> {
    svg(svg::Handle::from_memory(glyph.source().as_bytes()))
        .width(size)
        .height(size)
        // The tint only takes the color's hue; its alpha has to be applied separately.
        .opacity(color.a)
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}
