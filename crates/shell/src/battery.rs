//! The battery, from UPower, and Low Power Mode, from power-profiles-daemon: what a laptop's menu
//! bar shows, like a Mac's.

use iced_futures::Subscription;
use iced_futures::futures::channel::mpsc;
use iced_futures::futures::{SinkExt as _, Stream, StreamExt as _, stream};

use crate::dbus;

/// What the battery's doing, as far as the menu bar cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    /// Plugged in and full.
    Full,
    /// Plugged in but not charging, like a laptop holding its charge below full to spare the
    /// battery.
    OnHold,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    /// From 0 to 100.
    pub percent: f32,
    pub charge: Charge,
    /// Until it's empty while discharging, or full while charging, when UPower has worked it out.
    pub seconds_left: Option<u64>,
    /// Whether Low Power Mode is on, or `None` without power-profiles-daemon to turn it on.
    pub low_power: Option<bool>,
}

impl Battery {
    /// Low enough to show in red, like macOS does.
    pub fn is_low(&self) -> bool {
        self.charge == Charge::Discharging && self.percent <= LOW_PERCENT
    }

    pub fn is_plugged_in(&self) -> bool {
        self.charge != Charge::Discharging
    }
}

/// Where macOS turns the battery red.
pub const LOW_PERCENT: f32 = 10.0;

const LOW_POWER_PROFILE: &str = "power-saver";
const DEFAULT_PROFILE: &str = "balanced";

#[zbus::proxy(
    interface = "org.freedesktop.UPower",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower"
)]
trait UPower {
    #[zbus(property)]
    fn on_battery(&self) -> zbus::Result<bool>;
}

/// UPower's summary of all the batteries, as one.
#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait Device {
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;
    #[zbus(property, name = "Type")]
    fn kind(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn time_to_full(&self) -> zbus::Result<i64>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.PowerProfiles",
    default_service = "org.freedesktop.UPower.PowerProfiles",
    default_path = "/org/freedesktop/UPower/PowerProfiles"
)]
trait PowerProfiles {
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_active_profile(&self, profile: &str) -> zbus::Result<()>;
}

// UPower's device types and states.
const TYPE_BATTERY: u32 = 2;
const STATE_CHARGING: u32 = 1;
const STATE_DISCHARGING: u32 = 2;
const STATE_EMPTY: u32 = 3;
const STATE_FULL: u32 = 4;
const STATE_PENDING_CHARGE: u32 = 5;
const STATE_PENDING_DISCHARGE: u32 = 6;

/// The battery every time it changes, or `None` when there isn't one.
pub fn watch() -> Subscription<Option<Battery>> {
    Subscription::run(battery_stream)
}

fn battery_stream() -> impl Stream<Item = Option<Battery>> {
    iced_futures::stream::channel(4, |mut output: mpsc::Sender<Option<Battery>>| async move {
        let Some(connection) = dbus::system().await else {
            return;
        };
        let (Ok(upower), Ok(device)) = (
            UPowerProxy::new(&connection).await,
            DeviceProxy::new(&connection).await,
        ) else {
            log::info!("no UPower, so no battery");
            return;
        };
        // Without it, there's no Low Power Mode.
        let profiles = PowerProfilesProxy::new(&connection).await.ok();

        let mut changes = stream::select_all([
            device
                .receive_percentage_changed()
                .await
                .map(|_| ())
                .boxed(),
            device.receive_state_changed().await.map(|_| ()).boxed(),
            device
                .receive_time_to_empty_changed()
                .await
                .map(|_| ())
                .boxed(),
            device
                .receive_time_to_full_changed()
                .await
                .map(|_| ())
                .boxed(),
            device
                .receive_is_present_changed()
                .await
                .map(|_| ())
                .boxed(),
            upower
                .receive_on_battery_changed()
                .await
                .map(|_| ())
                .boxed(),
        ]);
        if let Some(profiles) = &profiles {
            changes.push(
                profiles
                    .receive_active_profile_changed()
                    .await
                    .map(|_| ())
                    .boxed(),
            );
        }

        let mut last = None;
        loop {
            let battery = read(&upower, &device, profiles.as_ref()).await;
            if last.as_ref() != Some(&battery) {
                if output.send(battery.clone()).await.is_err() {
                    return;
                }
                last = Some(battery);
            }
            if changes.next().await.is_none() {
                return;
            }
        }
    })
}

async fn read(
    upower: &UPowerProxy<'_>,
    device: &DeviceProxy<'_>,
    profiles: Option<&PowerProfilesProxy<'_>>,
) -> Option<Battery> {
    if !device.is_present().await.ok()? || device.kind().await.ok()? != TYPE_BATTERY {
        return None;
    }
    let percent = device.percentage().await.ok()? as f32;
    let on_battery = upower.on_battery().await.unwrap_or(false);
    let charge = charge(device.state().await.unwrap_or(0), on_battery);
    let seconds = match charge {
        Charge::Discharging => device.time_to_empty().await.unwrap_or(0),
        Charge::Charging => device.time_to_full().await.unwrap_or(0),
        Charge::Full | Charge::OnHold => 0,
    };
    let low_power = match profiles {
        Some(profiles) => profiles
            .active_profile()
            .await
            .ok()
            .map(|profile| profile == LOW_POWER_PROFILE),
        None => None,
    };
    Some(Battery {
        percent,
        charge,
        seconds_left: u64::try_from(seconds).ok().filter(|&seconds| seconds > 0),
        low_power,
    })
}

/// What UPower's state comes to. Whether it's on battery settles what an unknown or pending
/// state means.
fn charge(state: u32, on_battery: bool) -> Charge {
    match state {
        STATE_CHARGING => Charge::Charging,
        STATE_DISCHARGING | STATE_EMPTY => Charge::Discharging,
        STATE_FULL => Charge::Full,
        STATE_PENDING_CHARGE => Charge::OnHold,
        STATE_PENDING_DISCHARGE => Charge::Discharging,
        _ if on_battery => Charge::Discharging,
        _ => Charge::OnHold,
    }
}

/// Turns Low Power Mode on or off.
pub async fn set_low_power(on: bool) {
    let result = async {
        let connection = dbus::system()
            .await
            .ok_or_else(|| zbus::Error::Failure("no system bus".into()))?;
        let profiles = PowerProfilesProxy::new(&connection).await?;
        profiles
            .set_active_profile(if on {
                LOW_POWER_PROFILE
            } else {
                DEFAULT_PROFILE
            })
            .await
    }
    .await;
    if let Err(err) = result {
        log::warn!(
            "can't turn Low Power Mode {}: {err}",
            if on { "on" } else { "off" }
        );
    }
}

/// Whether this computer has a battery of its own, rather than only a mouse's or the like: what
/// Settings goes by to offer the battery's switches.
pub fn is_present() -> bool {
    let Ok(supplies) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    supplies.flatten().any(|supply| {
        let read =
            |name: &str| std::fs::read_to_string(supply.path().join(name)).unwrap_or_default();
        read("type").trim() == "Battery" && read("scope").trim() != "Device"
    })
}

/// How long is left, the way macOS puts it: "2:05".
pub fn duration(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60);
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_come_to_what_the_menu_bar_shows() {
        assert_eq!(charge(STATE_CHARGING, false), Charge::Charging);
        assert_eq!(charge(STATE_DISCHARGING, true), Charge::Discharging);
        assert_eq!(charge(STATE_FULL, false), Charge::Full);
        assert_eq!(charge(STATE_PENDING_CHARGE, false), Charge::OnHold);
        // Unknown: it's whichever the power source says.
        assert_eq!(charge(0, true), Charge::Discharging);
        assert_eq!(charge(0, false), Charge::OnHold);
    }

    #[test]
    fn time_left_reads_like_a_clock() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(59), "0:01");
        assert_eq!(duration(7500), "2:05");
    }
}
