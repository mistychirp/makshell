//! MacOS-Like, the icon themes makshell gives apps: macOS Tahoe's app icons from MacTahoe, with
//! Tahoe tiles made for the apps it hasn't got. MacOS-Like's `setup.sh` builds both, light and
//! dark, for the apps this user has, into `~/.local/share/icons`. It comes with makshell as
//! `libexec/makshell-icons`, next to the shell's `bin`, and runs again when apps come and go.

use std::hash::{Hash as _, Hasher as _};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{env, fs};

use crate::appearance;
use crate::config::Config;

pub const LIGHT: &str = "MacOS-Like-Light";
pub const DARK: &str = "MacOS-Like-Dark";

/// The session restarts a shell that exits with this, which is how one that's just got the
/// themes for the first time gets to see them: icon lookups only know the themes there were when
/// it started.
pub const RESTART: i32 = 75;

/// One build at a time.
static BUILDING: AtomicBool = AtomicBool::new(false);

/// Whether the themes were installed when the shell started.
static THERE_AT_START: OnceLock<bool> = OnceLock::new();

/// Notes whether the themes are there, before anything looks up an icon.
pub fn note_start() {
    THERE_AT_START.get_or_init(installed);
}

pub fn is_ours(theme: &str) -> bool {
    theme == LIGHT || theme == DARK
}

/// The light or dark one.
pub fn theme(light: bool) -> &'static str {
    if light { LIGHT } else { DARK }
}

/// Whether both are installed, wherever they came from.
pub fn installed() -> bool {
    appearance::icon_theme_exists(LIGHT) && appearance::icon_theme_exists(DARK)
}

/// What builds them, if makshell came with it: `MAKSHELL_ICONS`, or `libexec/makshell-icons` next
/// to the shell's `bin`.
fn builder() -> Option<PathBuf> {
    if let Some(path) = env::var_os("MAKSHELL_ICONS") {
        return Some(PathBuf::from(path));
    }
    let exe = env::current_exe().ok()?;
    let path = exe.parent()?.parent()?.join("libexec/makshell-icons");
    path.is_file().then_some(path)
}

/// Whether something else puts them in place, like home-manager, whose links into the Nix store
/// aren't for the shell to replace.
fn provided_elsewhere() -> bool {
    let icons = appearance::data_home().join("icons");
    [LIGHT, DARK].iter().any(|theme| {
        fs::symlink_metadata(icons.join(theme)).is_ok_and(|meta| meta.file_type().is_symlink())
    })
}

/// What the themes are built for: the icons installed apps name, and the builder that made them,
/// whose path changes with a new version of makshell.
fn fingerprint(builder: &std::path::Path) -> String {
    let entries = freedesktop_desktop_entry::desktop_entries(&[] as &[String]);
    let mut icons: Vec<&str> = entries.iter().filter_map(|entry| entry.icon()).collect();
    icons.sort_unstable();
    icons.dedup();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    icons.hash(&mut hasher);
    fs::canonicalize(builder)
        .unwrap_or_else(|_| builder.to_owned())
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Why the icons couldn't be made, for Settings to say, from what the builder said: most often,
/// something it needs isn't installed.
fn problem_in(errors: &str) -> String {
    let missing: Vec<&str> = errors
        .lines()
        .filter_map(|line| line.strip_prefix("missing dependency: "))
        .filter_map(|what| what.split_whitespace().next())
        .collect();
    if missing.is_empty() {
        String::from("Couldn't make them; the log says why.")
    } else {
        format!("Needs {} to make them.", missing.join(" and "))
    }
}

fn problem_file() -> PathBuf {
    appearance::state_dir().join("icons-problem")
}

fn note_problem(problem: Option<&str>) {
    let file = problem_file();
    let result = match problem {
        Some(problem) => fs::write(&file, problem),
        None => match fs::remove_file(&file) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    };
    if let Err(err) = result {
        log::warn!("can't note how building the icons went: {err}");
    }
}

/// Why the icons couldn't be made last time, if they couldn't.
pub fn problem() -> Option<String> {
    fs::read_to_string(problem_file())
        .ok()
        .map(|problem| problem.trim().to_owned())
        .filter(|problem| !problem.is_empty())
}

/// What a build came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Built {
    /// Nothing to do, or it couldn't be done.
    Nothing,
    /// The themes are new or remade.
    Remade,
    /// The themes are there for the first time, which this shell can't see until it restarts.
    First,
}

/// Builds the themes, if they're wanted, makshell has the builder and they're missing or made for
/// other apps than there are now. It takes a few seconds, and more on a slow machine.
pub async fn build() -> Built {
    tokio::task::spawn_blocking(build_now)
        .await
        .unwrap_or(Built::Nothing)
}

fn build_now() -> Built {
    if !Config::load().appearance.macos_icons {
        return Built::Nothing;
    }
    let Some(builder) = builder() else {
        return Built::Nothing;
    };
    if provided_elsewhere() {
        return Built::Nothing;
    }
    let state = appearance::state_dir().join("icons-built");
    let fingerprint = fingerprint(&builder);
    let installed = installed();
    if installed && fs::read_to_string(&state).is_ok_and(|built| built.trim() == fingerprint) {
        return Built::Nothing;
    }
    if BUILDING.swap(true, Ordering::AcqRel) {
        return Built::Nothing;
    }
    log::info!("building the MacOS-Like icons with {}", builder.display());
    let mut command = Command::new(&builder);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // Out of the way of everything else.
    // SAFETY: setpriority is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
    let output = command.output();
    BUILDING.store(false, Ordering::Release);
    match output {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let errors = String::from_utf8_lossy(&output.stderr);
            let last: Vec<&str> = errors.lines().rev().take(5).collect();
            log::warn!(
                "couldn't build the MacOS-Like icons ({}): {}",
                output.status,
                last.into_iter().rev().collect::<Vec<_>>().join(" / ")
            );
            note_problem(Some(&problem_in(&errors)));
            return Built::Nothing;
        }
        Err(err) => {
            log::warn!("couldn't run {}: {err}", builder.display());
            note_problem(Some("Couldn't make them; the log says why."));
            return Built::Nothing;
        }
    }
    note_problem(None);
    if let Some(parent) = state.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(err) = fs::write(&state, &fingerprint) {
        log::warn!("can't remember the icons were built: {err}");
    }
    log::info!("built the MacOS-Like icons");
    if THERE_AT_START.get().copied().unwrap_or(installed) {
        Built::Remade
    } else {
        Built::First
    }
}

#[cfg(test)]
mod tests {
    use super::problem_in;

    #[test]
    fn a_missing_tool_is_named() {
        let errors = "missing dependency: Pillow        (Arch: python-pillow  Debian/Ubuntu: python3-pil)\n\
                      missing dependency: rsvg-convert  (Arch: librsvg)\n";
        assert_eq!(
            problem_in(errors),
            "Needs Pillow and rsvg-convert to make them."
        );
        assert_eq!(
            problem_in("Traceback ..."),
            "Couldn't make them; the log says why."
        );
    }
}
