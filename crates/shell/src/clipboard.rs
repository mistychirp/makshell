//! The clipboard's history, through ext-data-control-v1. It skips what a password manager marks as
//! secret, and `clip_store` keeps the rest across sessions. It has its own connection and thread,
//! since reading what an app copied means waiting for that app.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::{io, thread};

use iced_futures::Subscription;
use iced_futures::futures::channel::mpsc;
use iced_futures::futures::{SinkExt as _, Stream, StreamExt as _};
use iced_widget::image;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop, event_created_child};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{
    self, ExtDataControlOfferV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self, ExtDataControlSourceV1,
};

/// How much of one copied thing is kept.
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// How long an app gets to hand over what it copied.
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// What password managers set on what they copy.
const SECRET_MIME: &str = "x-kde-passwordManagerHint";
const OURS_MIME: &str = "application/x-makshell-clipboard";
const TEXT_MIMES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// Something that was copied.
#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    Files(Vec<PathBuf>),
    Image {
        /// The picture as it was copied, to copy again.
        bytes: Arc<Vec<u8>>,
        mime: String,
        picture: image::Handle,
    },
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Files(a), Self::Files(b)) => a == b,
            (Self::Image { bytes: a, .. }, Self::Image { bytes: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Content {
    /// The types and bytes to offer it as.
    fn offers(&self) -> (Vec<String>, Vec<u8>) {
        match self {
            Self::Text(text) => (
                TEXT_MIMES.iter().map(ToString::to_string).collect(),
                text.clone().into_bytes(),
            ),
            Self::Files(paths) => {
                let uris: String = paths
                    .iter()
                    .map(|path| format!("{}\r\n", crate::places::file_uri(path)))
                    .collect();
                (vec![String::from("text/uri-list")], uris.into_bytes())
            }
            Self::Image { bytes, mime, .. } => (vec![mime.clone()], bytes.as_ref().clone()),
        }
    }
}

struct Shared {
    connection: Connection,
    manager: ExtDataControlManagerV1,
    device: ExtDataControlDeviceV1,
    queue: QueueHandle<State>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// Hears what gets copied, for as long as the shell runs.
pub fn events() -> Subscription<Content> {
    Subscription::run(watch)
}

fn watch() -> impl Stream<Item = Content> {
    iced_futures::stream::channel(8, |mut output: mpsc::Sender<Content>| async move {
        let (sender, mut incoming) = mpsc::unbounded();
        thread::spawn(move || {
            if let Err(err) = run(sender) {
                log::warn!("no clipboard history: {err}");
            }
        });
        while let Some(content) = incoming.next().await {
            let _ = output.send(content).await;
        }
    })
}

/// Puts this on the clipboard.
pub fn copy(content: &Content) {
    let Some(shared) = SHARED.get() else {
        log::warn!("can't copy: no clipboard");
        return;
    };
    let (mimes, bytes) = content.offers();
    let source = shared
        .manager
        .create_data_source(&shared.queue, Arc::new(bytes));
    for mime in mimes {
        source.offer(mime);
    }
    source.offer(String::from(OURS_MIME));
    shared.device.set_selection(Some(&source));
    if let Err(err) = shared.connection.flush() {
        log::warn!("can't copy: {err}");
    }
}

fn run(sender: mpsc::UnboundedSender<Content>) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|err| err.to_string())?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&connection).map_err(|err| err.to_string())?;
    let handle = queue.handle();
    let manager: ExtDataControlManagerV1 = globals
        .bind(&handle, 1..=1, ())
        .map_err(|err| err.to_string())?;
    let seat: WlSeat = globals
        .bind(&handle, 1..=9, ())
        .map_err(|err| err.to_string())?;
    let device = manager.get_data_device(&seat, &handle, ());
    let mut state = State {
        connection: connection.clone(),
        sender,
        offered: Arc::new(Mutex::new(HashSet::new())),
    };
    let _ = SHARED.set(Shared {
        connection,
        manager,
        device,
        queue: handle,
    });
    loop {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|err| err.to_string())?;
    }
}

struct State {
    connection: Connection,
    sender: mpsc::UnboundedSender<Content>,
    /// The types of the offer being described.
    offered: Arc<Mutex<HashSet<String>>>,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
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
delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore ExtDataControlManagerV1);

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::DataOffer { .. } => {
                state.offered.lock().unwrap().clear();
            }
            ext_data_control_device_v1::Event::Selection { id: Some(offer) } => {
                let mimes = std::mem::take(&mut *state.offered.lock().unwrap());
                state.take(&offer, &mimes);
                offer.destroy();
            }
            _ => {}
        }
    }

    event_created_child!(State, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            state.offered.lock().unwrap().insert(mime_type);
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, Arc<Vec<u8>>> for State {
    fn event(
        _: &mut Self,
        source: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        bytes: &Arc<Vec<u8>>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { fd, .. } => {
                let bytes = bytes.clone();
                thread::spawn(move || {
                    let _ = File::from(fd).write_all(&bytes);
                });
            }
            ext_data_control_source_v1::Event::Cancelled => source.destroy(),
            _ => {}
        }
    }
}

/// What to read an offer as, from what it offers.
fn best_type(mimes: &HashSet<String>) -> Option<String> {
    if mimes.contains(OURS_MIME) || mimes.contains(SECRET_MIME) {
        return None;
    }
    let preferred = ["text/uri-list", "image/png", "image/jpeg"]
        .into_iter()
        .chain(TEXT_MIMES);
    preferred
        .into_iter()
        .find(|mime| mimes.contains(*mime))
        .map(str::to_owned)
}

impl State {
    fn take(&self, offer: &ExtDataControlOfferV1, mimes: &HashSet<String>) {
        let Some(mime) = best_type(mimes) else {
            return;
        };
        let (reader, writer) = match io::pipe() {
            Ok(pipe) => pipe,
            Err(err) => {
                log::warn!("can't read the clipboard: {err}");
                return;
            }
        };
        offer.receive(mime.clone(), writer.as_fd());
        if self.connection.flush().is_err() {
            return;
        }
        drop(writer);
        let sender = self.sender.clone();
        // Not on this thread: an app that's slow to answer would hold everything else up.
        thread::spawn(move || {
            if let Some(content) = read_offer(reader, &mime) {
                let _ = sender.unbounded_send(content);
            }
        });
    }
}

fn read_offer(reader: io::PipeReader, mime: &str) -> Option<Content> {
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut limited = reader.take(MAX_BYTES as u64 + 1);
    limited.read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_BYTES {
        log::debug!("not keeping something too big from the clipboard");
        return None;
    }
    if started.elapsed() > READ_TIMEOUT {
        log::debug!("the clipboard took a while to read");
    }
    parse(&bytes, mime)
}

fn parse(bytes: &[u8], mime: &str) -> Option<Content> {
    match mime {
        "text/uri-list" => {
            let files: Vec<PathBuf> = std::str::from_utf8(bytes)
                .ok()?
                .lines()
                .filter(|line| !line.starts_with('#'))
                .filter_map(crate::portal::path_of)
                .collect();
            (!files.is_empty()).then_some(Content::Files(files))
        }
        "image/png" | "image/jpeg" => {
            let decoded = ::image::load_from_memory(bytes).ok()?;
            let small = decoded.thumbnail(96, 96).to_rgba8();
            let (width, height) = small.dimensions();
            Some(Content::Image {
                bytes: Arc::new(bytes.to_vec()),
                mime: mime.to_owned(),
                picture: image::Handle::from_rgba(width, height, small.into_raw()),
            })
        }
        _ => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            (!text.trim().is_empty()).then_some(Content::Text(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(mimes: &[&str]) -> HashSet<String> {
        mimes.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn leaves_out_secrets_and_what_it_copied_itself() {
        assert_eq!(best_type(&types(&["text/plain", SECRET_MIME])), None);
        assert_eq!(best_type(&types(&["text/plain", OURS_MIME])), None);
    }

    #[test]
    fn prefers_files_and_pictures_over_their_text() {
        assert_eq!(
            best_type(&types(&["text/plain", "text/uri-list"])).as_deref(),
            Some("text/uri-list")
        );
        assert_eq!(
            best_type(&types(&["UTF8_STRING", "image/png"])).as_deref(),
            Some("image/png")
        );
        assert_eq!(best_type(&types(&["application/x-other"])), None);
    }

    #[test]
    fn reads_files_and_skips_blank_text() {
        let list = parse(b"# a comment\r\nfile:///tmp/a%20b\r\n", "text/uri-list").unwrap();
        assert_eq!(list, Content::Files(vec![PathBuf::from("/tmp/a b")]));
        assert_eq!(parse(b"  \n", "text/plain"), None);
    }

    #[test]
    fn offers_what_it_read() {
        let (mimes, bytes) = Content::Text(String::from("hi")).offers();
        assert!(mimes.contains(&String::from("UTF8_STRING")));
        assert_eq!(bytes, b"hi");
    }
}
