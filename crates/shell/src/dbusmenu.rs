//! Menus apps export over D-Bus, for tray icons and the menu bar.

use std::collections::HashMap;

use iced_widget::image;
use serde::Deserialize;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedValue, Type, Value};

use crate::app::Message;
use crate::apps::AppIndex;
use crate::{dbus, menu};

pub const INTERFACE: &str = "com.canonical.dbusmenu";

#[zbus::proxy(interface = "com.canonical.dbusmenu")]
trait Menu {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, RawItem)>;
    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;
    fn event(&self, id: i32, event_id: &str, data: &Value<'_>, timestamp: u32) -> zbus::Result<()>;
}

/// An item as it comes over the bus: its id, properties and children.
#[derive(Debug, Deserialize, Type)]
#[allow(dead_code)]
struct RawItem(i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// An item in an app's menu.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: i32,
    pub label: String,
    enabled: bool,
    separator: bool,
    checked: bool,
    /// The keys that do the same, in macOS's symbols.
    shortcut: Option<String>,
    icon_name: String,
    /// Its icon, once [`load_icons`] has found it.
    icon: Option<image::Handle>,
    /// It's meant to open a submenu, even if that's empty right now.
    opens_submenu: bool,
    pub children: Vec<Item>,
}

/// Where an app's menu is on the bus.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address {
    pub service: String,
    pub path: String,
}

impl Address {
    async fn proxy(&self, connection: &zbus::Connection) -> zbus::Result<MenuProxy<'static>> {
        MenuProxy::builder(connection)
            .destination(self.service.clone())?
            .path(self.path.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await
    }

    /// The whole menu. Without an app there, it's empty.
    pub async fn read(&self, connection: &zbus::Connection) -> Vec<Item> {
        let layout = async { self.proxy(connection).await?.get_layout(0, -1, &[]).await }.await;
        match layout {
            Ok((_, RawItem(_, _, children))) => {
                let children: Vec<&Value<'_>> = children.iter().map(|child| &**child).collect();
                parse_children(&children)
            }
            Err(err) => {
                log::debug!("no menu from {}: {err}", self.service);
                Vec::new()
            }
        }
    }

    /// Tells the app these menus are about to show, which is when some fill them.
    pub async fn about_to_show(&self, connection: &zbus::Connection, ids: &[i32]) {
        if let Ok(proxy) = self.proxy(connection).await {
            for id in ids {
                let _ = proxy.about_to_show(*id).await;
            }
        }
    }

    /// The entries for these items, which tell the app when they're chosen.
    pub fn entries(&self, items: &[Item]) -> Vec<menu::Entry> {
        tidy_separators(items)
            .into_iter()
            .map(|item| {
                if item.separator {
                    return menu::Entry::Separator;
                }
                let on_select = Message::DbusMenuItemChosen(self.clone(), item.id);
                let mut entry = menu::Entry::item(&item.label, on_select)
                    .checked(item.checked)
                    .enabled(item.enabled && (!item.opens_submenu || !item.children.is_empty()));
                if let Some(icon) = &item.icon {
                    entry = entry.icon(menu::Icon::Image(icon.clone()));
                }
                if let Some(shortcut) = &item.shortcut {
                    entry = entry.shortcut(shortcut);
                }
                if !item.children.is_empty() {
                    entry = entry.submenu(self.entries(&item.children));
                }
                entry
            })
            .collect()
    }

    /// Tells the app that an item in its menu was chosen.
    pub async fn item_chosen(self, id: i32) {
        let result = async {
            let connection = dbus::session().await.ok_or(zbus::Error::Unsupported)?;
            let proxy = self.proxy(&connection).await?;
            proxy.event(id, "clicked", &Value::I32(0), 0).await
        }
        .await;
        if let Err(err) = result {
            log::warn!("can't report a menu choice: {err}");
        }
    }

    /// Tells the app that this menu is opening, so that it's up to date the next time.
    pub async fn opening(self, id: i32) {
        if let Some(connection) = dbus::session().await {
            self.about_to_show(&connection, &[id]).await;
        }
    }
}

/// Finds the items' icons.
pub fn load_icons(items: &mut [Item], apps: &mut AppIndex) {
    for item in items {
        if !item.icon_name.is_empty() {
            item.icon = apps.menu_icon(&item.icon_name);
        }
        load_icons(&mut item.children, apps);
    }
}

/// `value` with any variants around it taken off.
fn peeled<'a>(value: &'a Value<'a>) -> &'a Value<'a> {
    match value {
        Value::Value(inner) => peeled(inner),
        value => value,
    }
}

fn parse_item(id: i32, properties: &[(String, &Value<'_>)], children: Vec<Item>) -> Item {
    let property = |name: &str| {
        properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| peeled(value))
    };
    let text = |name: &str| match property(name) {
        Some(Value::Str(text)) => text.as_str(),
        _ => "",
    };
    Item {
        id,
        label: plain_label(text("label")),
        enabled: !matches!(property("enabled"), Some(Value::Bool(false))),
        separator: text("type") == "separator",
        checked: !text("toggle-type").is_empty()
            && matches!(property("toggle-state"), Some(Value::I32(1))),
        shortcut: property("shortcut").and_then(parse_shortcut),
        icon_name: text("icon-name").to_owned(),
        icon: None,
        opens_submenu: text("children-display") == "submenu",
        children,
    }
}

fn is_visible(properties: &[(String, &Value<'_>)]) -> bool {
    !properties
        .iter()
        .any(|(key, value)| key == "visible" && matches!(peeled(value), Value::Bool(false)))
}

/// An item's children, which come as variants holding `(id, properties, children)`.
fn parse_children(children: &[&Value<'_>]) -> Vec<Item> {
    children
        .iter()
        .filter_map(|child| {
            let Value::Structure(structure) = peeled(child) else {
                return None;
            };
            let [
                Value::I32(id),
                Value::Dict(properties),
                Value::Array(children),
            ] = structure.fields()
            else {
                return None;
            };
            let properties: Vec<(String, &Value<'_>)> = properties
                .iter()
                .filter_map(|(key, value)| match peeled(key) {
                    Value::Str(key) => Some((key.to_string(), value)),
                    _ => None,
                })
                .collect();
            if !is_visible(&properties) {
                return None;
            }
            let children: Vec<&Value<'_>> = children.iter().collect();
            Some(parse_item(*id, &properties, parse_children(&children)))
        })
        .collect()
}

/// The items as a menu shows them, the way Qt's do: a separator only between two items, so none
/// at either end and never two in a row, which hidden items between separators would leave.
fn tidy_separators(items: &[Item]) -> Vec<&Item> {
    let mut shown: Vec<&Item> = Vec::with_capacity(items.len());
    for item in items {
        if item.separator && shown.last().is_none_or(|last| last.separator) {
            continue;
        }
        shown.push(item);
    }
    if shown.last().is_some_and(|last| last.separator) {
        shown.pop();
    }
    shown
}

/// A label without its mnemonic: `_Quit` is "Quit", and `__` is a literal underscore.
fn plain_label(label: &str) -> String {
    let mut plain = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                plain.push('_');
                chars.next();
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

/// The first of an item's shortcuts, which come as lists of keys, modifiers first.
fn parse_shortcut(value: &Value<'_>) -> Option<String> {
    let Value::Array(shortcuts) = value else {
        return None;
    };
    let Value::Array(keys) = peeled(shortcuts.iter().next()?) else {
        return None;
    };
    let mut shortcut = String::new();
    for key in keys.iter() {
        let Value::Str(key) = peeled(key) else {
            return None;
        };
        shortcut.push_str(match key.as_str() {
            "Control" => "⌃",
            "Shift" => "⇧",
            "Alt" => "⌥",
            "Super" => "⌘",
            "Return" | "Enter" => "↩",
            "Delete" => "⌦",
            "BackSpace" => "⌫",
            "Tab" => "⇥",
            "Escape" => "⎋",
            "Left" => "←",
            "Right" => "→",
            "Up" => "↑",
            "Down" => "↓",
            "space" => "Space",
            key => key,
        });
    }
    (!shortcut.is_empty()).then_some(shortcut)
}

#[cfg(test)]
mod tests {
    use super::{Item, tidy_separators};

    fn item(id: i32, separator: bool) -> Item {
        Item {
            id,
            label: String::new(),
            enabled: true,
            separator,
            checked: false,
            shortcut: None,
            icon_name: String::new(),
            icon: None,
            opens_submenu: false,
            children: Vec::new(),
        }
    }

    #[test]
    fn separators_only_go_between_items() {
        // Like Dolphin's Edit menu with its hidden items gone: a run of separators, and one at
        // either end.
        let items = [
            item(0, true),
            item(1, false),
            item(2, true),
            item(3, true),
            item(4, true),
            item(5, false),
            item(6, true),
        ];
        let ids: Vec<(i32, bool)> = tidy_separators(&items)
            .iter()
            .map(|item| (item.id, item.separator))
            .collect();
        assert_eq!(ids, [(1, false), (2, true), (5, false)]);
        assert!(tidy_separators(&[item(0, true)]).is_empty());
    }
}
