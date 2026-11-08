//! The clipboard's history on disk, so that it outlasts the session, in
//! `$XDG_STATE_HOME/makshell/clipboard`: an index of what was copied, with copied pictures in files
//! beside it. Only the user can read any of it, since what's copied can be private; what password
//! managers mark as secret never gets this far.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::hash::{Hash as _, Hasher as _};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::appearance;
use crate::clipboard::Content;

const INDEX: &str = "history.toml";

/// One at a time, the last one written last.
static WRITING: Mutex<()> = Mutex::new(());

/// Something that was copied, as it's kept.
pub struct Kept {
    pub content: Content,
    pub copied: chrono::DateTime<chrono::Local>,
    /// The desktop entry id of the app it was most likely copied from.
    pub source: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Index {
    #[serde(default)]
    clips: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    copied: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<Vec<PathBuf>>,
    /// The picture's file, beside the index, and its type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
}

fn folder() -> PathBuf {
    appearance::state_dir().join("clipboard")
}

/// What was kept last time, newest first.
pub fn load() -> Vec<Kept> {
    load_from(&folder())
}

fn load_from(folder: &Path) -> Vec<Kept> {
    let Ok(text) = fs::read_to_string(folder.join(INDEX)) else {
        return Vec::new();
    };
    let index: Index = match toml::from_str(&text) {
        Ok(index) => index,
        Err(err) => {
            log::warn!("can't read the clipboard history: {err}");
            return Vec::new();
        }
    };
    index
        .clips
        .into_iter()
        .filter_map(|entry| {
            let copied = chrono::DateTime::parse_from_rfc3339(&entry.copied)
                .ok()?
                .with_timezone(&chrono::Local);
            let content = if let Some(text) = entry.text {
                Content::Text(text)
            } else if let Some(files) = entry.files {
                Content::Files(files)
            } else {
                let name = entry.image?;
                let bytes = fs::read(folder.join(&name)).ok()?;
                let picture = iced_widget::image::Handle::from_bytes(bytes.clone());
                Content::Image {
                    bytes: Arc::new(bytes),
                    mime: entry.mime.unwrap_or_else(|| String::from("image/png")),
                    picture,
                }
            };
            Some(Kept {
                content,
                copied,
                source: entry.source,
            })
        })
        .collect()
}

/// Keeps this history, newest first, in the background.
pub fn save(clips: Vec<Kept>) {
    std::thread::spawn(move || {
        let _one_at_a_time = WRITING.lock();
        if let Err(err) = save_to(&folder(), &clips) {
            log::warn!("can't keep the clipboard history: {err}");
        }
    });
}

fn save_to(folder: &Path, clips: &[Kept]) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
    let mut pictures = HashSet::new();
    let mut index = Index::default();
    for clip in clips {
        let mut entry = Entry {
            copied: clip.copied.to_rfc3339(),
            source: clip.source.clone(),
            text: None,
            files: None,
            image: None,
            mime: None,
        };
        match &clip.content {
            Content::Text(text) => entry.text = Some(text.clone()),
            Content::Files(files) => entry.files = Some(files.clone()),
            Content::Image { bytes, mime, .. } => {
                let name = picture_name(bytes, mime);
                let path = folder.join(&name);
                if !path.exists() {
                    write_private(&path, bytes)?;
                }
                pictures.insert(name.clone());
                entry.image = Some(name);
                entry.mime = Some(mime.clone());
            }
        }
        index.clips.push(entry);
    }
    let text = toml::to_string(&index).map_err(io::Error::other)?;
    let temporary = folder.join(format!("{INDEX}.new"));
    write_private(&temporary, text.as_bytes())?;
    fs::rename(&temporary, folder.join(INDEX))?;
    // Pictures nothing's kept for any more.
    for file in fs::read_dir(folder)?.flatten() {
        let name = file.file_name().to_string_lossy().into_owned();
        if name != INDEX && !pictures.contains(&name) {
            let _ = fs::remove_file(file.path());
        }
    }
    Ok(())
}

/// A picture's file name, from what's in it, so the same picture copied again is one file.
fn picture_name(bytes: &[u8], mime: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let extension = mime
        .strip_prefix("image/")
        .map(|kind| kind.split(['+', ';']).next().unwrap_or(kind))
        .filter(|kind| kind.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("bin");
    format!("{:016x}.{extension}", hasher.finish())
}

/// A file only its owner can read.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_history_comes_back_as_it_was_kept() {
        let folder = std::env::temp_dir().join(format!("makshell-clips-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        let copied = chrono::Local::now();
        let picture = vec![1u8, 2, 3];
        let clips = vec![
            Kept {
                content: Content::Text(String::from("hello \"there\"\nsecond line")),
                copied,
                source: Some(String::from("org.kde.kate")),
            },
            Kept {
                content: Content::Files(vec![PathBuf::from("/tmp/a b.txt")]),
                copied,
                source: None,
            },
            Kept {
                content: Content::Image {
                    bytes: Arc::new(picture.clone()),
                    mime: String::from("image/png"),
                    picture: iced_widget::image::Handle::from_bytes(picture.clone()),
                },
                copied,
                source: None,
            },
        ];
        save_to(&folder, &clips).unwrap();
        // Only its owner can look.
        let mode = fs::metadata(&folder).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let mode = fs::metadata(folder.join(INDEX))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let back = load_from(&folder);
        assert_eq!(back.len(), 3);
        assert!(back[0].content == clips[0].content);
        assert_eq!(back[0].source.as_deref(), Some("org.kde.kate"));
        assert!(back[1].content == clips[1].content);
        assert!(back[2].content == clips[2].content);
        assert_eq!(back[0].copied.timestamp(), copied.timestamp());

        // Forgetting the picture removes its file.
        save_to(&folder, &clips[..1]).unwrap();
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
        fs::remove_dir_all(&folder).unwrap();
    }
}
