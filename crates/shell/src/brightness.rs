//! The screen's brightness. A laptop's own screen has a backlight the kernel knows, read from
//! `/sys/class/backlight` and set through logind, which lets the session's user do it. A monitor
//! is asked over DDC/CI with `ddcutil` instead, which takes most of a second per call, so that
//! happens off the UI thread, and only the latest level set gets sent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::{fs, io};

use iced_futures::futures::channel::oneshot;

const BACKLIGHTS: &str = "/sys/class/backlight";

/// A laptop screen's backlight.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Backlight {
    /// Its name under `/sys/class/backlight`.
    name: String,
    max: u32,
}

impl Backlight {
    /// The one to use: the firmware's if there is one, which is what the kernel says to prefer, then
    /// the platform's, then the graphics driver's own.
    fn find(root: &Path) -> Option<Self> {
        let rank = |kind: &str| match kind {
            "firmware" => 0,
            "platform" => 1,
            _ => 2,
        };
        let mut found: Vec<(u8, Self)> = fs::read_dir(root)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let max = read_number(&entry.path().join("max_brightness")).ok()?;
                let kind = fs::read_to_string(entry.path().join("type")).unwrap_or_default();
                (max > 0).then(|| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    (rank(kind.trim()), Self { name, max })
                })
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        found.into_iter().next().map(|(_, backlight)| backlight)
    }

    fn level(&self, root: &Path) -> Option<f32> {
        let raw = read_number(&root.join(&self.name).join("brightness")).ok()?;
        Some((raw as f32 / self.max as f32).clamp(0.0, 1.0))
    }

    /// What to set for a level from 0 to 1. Never quite nothing: on many screens that turns the
    /// backlight off, and there'd be no seeing how to turn it back up.
    fn raw(&self, level: f32) -> u32 {
        let least = self.max.div_ceil(100).max(1);
        ((level.clamp(0.0, 1.0) * self.max as f32).round() as u32).clamp(least, self.max)
    }
}

fn read_number(path: &Path) -> io::Result<u32> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(io::Error::other)
}

fn backlight() -> Option<Backlight> {
    Backlight::find(&PathBuf::from(BACKLIGHTS))
}

/// Sets it through logind, on the session the shell's in.
fn set_backlight(connection: &zbus::blocking::Connection, backlight: &Backlight, raw: u32) {
    let result = connection.call_method(
        Some("org.freedesktop.login1"),
        "/org/freedesktop/login1/session/auto",
        Some("org.freedesktop.login1.Session"),
        "SetBrightness",
        &("backlight", backlight.name.as_str(), raw),
    );
    if let Err(err) = result {
        log::warn!("can't set the backlight {}: {err}", backlight.name);
    }
}

/// What's waiting to be sent.
enum Request {
    /// A percentage, to a monitor.
    Monitor(u8),
    Backlight(Backlight, u32),
}

/// The request waiting to be sent, and whether a thread is sending.
static PENDING: Mutex<(Option<Request>, bool)> = Mutex::new((None, false));

/// The brightness from 0 to 1, or `None` without a screen that can tell.
pub async fn level() -> Option<f32> {
    if let Some(backlight) = backlight() {
        return backlight.level(Path::new(BACKLIGHTS));
    }
    let (sender, receiver) = oneshot::channel();
    thread::spawn(move || {
        let _ = sender.send(read());
    });
    receiver.await.ok().flatten()
}

fn read() -> Option<f32> {
    let output = Command::new("ddcutil")
        .args(["getvcp", "10", "--brief"])
        .output()
        .ok()?;
    // Like "VCP 10 C 50 100": the feature, its type, the current value and the maximum.
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.lines().find(|line| line.starts_with("VCP 10 "))?;
    let mut fields = line.split_whitespace().skip(3);
    let current: f32 = fields.next()?.parse().ok()?;
    let max: f32 = fields.next()?.parse().ok()?;
    (max > 0.0).then_some(current / max)
}

/// Asks for a brightness from 0 to 1. While one is being sent, only the latest request is kept.
pub fn set_level(level: f32) {
    let request = match backlight() {
        Some(backlight) => {
            let raw = backlight.raw(level);
            Request::Backlight(backlight, raw)
        }
        None => Request::Monitor((level.clamp(0.0, 1.0) * 100.0).round() as u8),
    };
    let mut pending = PENDING.lock().unwrap();
    pending.0 = Some(request);
    if pending.1 {
        return;
    }
    pending.1 = true;
    thread::spawn(|| {
        // Opened for the first backlight request, and kept for the rest of a slider's drag.
        let mut system_bus = None;
        loop {
            let next = {
                let mut pending = PENDING.lock().unwrap();
                let next = pending.0.take();
                pending.1 = next.is_some();
                next
            };
            match next {
                None => return,
                Some(Request::Monitor(percent)) => {
                    if let Err(err) = Command::new("ddcutil")
                        .args(["setvcp", "10", &percent.to_string()])
                        .status()
                    {
                        log::warn!("can't run ddcutil: {err}");
                    }
                }
                Some(Request::Backlight(backlight, raw)) => {
                    if system_bus.is_none() {
                        system_bus = zbus::blocking::Connection::system()
                            .inspect_err(|err| log::warn!("can't reach logind: {err}"))
                            .ok();
                    }
                    if let Some(connection) = &system_bus {
                        set_backlight(connection, &backlight, raw);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backlight_in(root: &Path, name: &str, kind: &str, max: u32, now: u32) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
        fs::write(dir.join("brightness"), format!("{now}\n")).unwrap();
    }

    #[test]
    fn the_firmwares_backlight_is_the_one_and_never_goes_dark() {
        let root = std::env::temp_dir().join(format!("makshell-backlight-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert_eq!(Backlight::find(&root), None);

        backlight_in(&root, "intel_backlight", "raw", 852, 426);
        backlight_in(&root, "acpi_video0", "firmware", 15, 15);
        let found = Backlight::find(&root).unwrap();
        assert_eq!(found.name, "acpi_video0");
        assert_eq!(found.level(&root), Some(1.0));

        let raw = Backlight {
            name: String::from("intel_backlight"),
            max: 852,
        };
        assert_eq!(raw.level(&root), Some(0.5));
        assert_eq!(raw.raw(1.0), 852);
        assert_eq!(raw.raw(0.5), 426);
        // A hundredth at least, rounded up.
        assert_eq!(raw.raw(0.0), 9);
        fs::remove_dir_all(&root).unwrap();
    }
}
