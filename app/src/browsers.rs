//! Chromium-family browsers: finding the installed ones and making them load
//! the extension by themselves.
//!
//! A browser only takes instructions at start-up, so "automatic" means a
//! launcher override: a copy of the browser's own `.desktop` file in the
//! user's applications directory (which takes precedence), with
//! `--load-extension=<dir>` added to its command lines. The extension files
//! are written where that browser can read them. Removing the override
//! undoes everything; a launcher the user wrote themselves is never touched.

use std::path::{Path, PathBuf};

use crate::autostart::exec_quote;
use crate::state::{data_dir, data_home, home};

const MARKER: &str = "X-RTX-VSR-Managed=true";
const LOAD: &str = "--load-extension=";
const FEATURES: &str = "--disable-features=";
/// Chrome 137+ ignores `--load-extension` unless this feature is disabled.
const FEATURE: &str = "DisableLoadExtensionCommandLineSwitch";

macro_rules! extension_files {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../../extension/", $name)) as &[u8])),*]
    };
}
/// The extension, as built into the app.
static EXTENSION: &[(&str, &[u8])] = extension_files![
    "manifest.json",
    "background.js",
    "content.js",
    "inject.js",
    "popup.html",
    "popup.js",
    "options.html",
    "options.js",
    "icons/icon-16.png",
    "icons/icon-32.png",
    "icons/icon-48.png",
    "icons/icon-128.png",
    "icons/indicator.png",
];

/// Launcher names (without `.desktop`) of browsers that accept
/// `--load-extension`.
const KNOWN: &[&str] = &[
    "google-chrome",
    "google-chrome-beta",
    "google-chrome-unstable",
    "com.google.Chrome",
    "com.google.ChromeDev",
    "chromium",
    "chromium-browser",
    "org.chromium.Chromium",
    "chromium_chromium",
    "io.github.ungoogled_software.ungoogled_chromium",
    "brave-browser",
    "brave-browser-beta",
    "brave-browser-nightly",
    "com.brave.Browser",
    "brave_brave",
    "vivaldi-stable",
    "vivaldi-snapshot",
    "com.vivaldi.Vivaldi",
    "microsoft-edge",
    "microsoft-edge-beta",
    "microsoft-edge-dev",
    "com.microsoft.Edge",
    "opera",
    "com.opera.Opera",
    "thorium-browser",
];

#[derive(Clone, Debug, PartialEq)]
pub enum Packaging {
    Native,
    /// Sandboxed: sees only its own directory under `~/.var/app`.
    Flatpak(String),
    /// Sandboxed: sees only its own directory under `~/snap`.
    Snap(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Browser {
    /// Launcher file name, e.g. `com.vivaldi.Vivaldi.desktop`.
    pub file: String,
    pub name: String,
    /// The launcher the browser's package installed.
    source: PathBuf,
    pub packaging: Packaging,
    /// Our launcher override is in place.
    pub integrated: bool,
}

fn override_dir() -> PathBuf {
    data_home().join("applications")
}

/// Directories that hold launchers installed by packages.
fn system_dirs() -> Vec<PathBuf> {
    let user = data_home();
    let listed = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let listed = if listed.is_empty() {
        "/usr/local/share:/usr/share".to_owned()
    } else {
        listed
    };
    let mut dirs: Vec<PathBuf> = listed
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    dirs.push(user.join("flatpak/exports/share"));
    dirs.push("/var/lib/flatpak/exports/share".into());
    dirs.push("/var/lib/snapd/desktop".into());
    dirs.retain(|d| *d != user);
    dirs
}

fn key<'a>(entry: &'a str, name: &str) -> Option<&'a str> {
    entry
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
}

pub fn detect() -> Vec<Browser> {
    let dirs = system_dirs();
    let mut found = Vec::new();
    for stem in KNOWN {
        let file = format!("{stem}.desktop");
        let Some(source) = dirs
            .iter()
            .map(|d| d.join("applications").join(&file))
            .find(|p| p.is_file())
        else {
            continue;
        };
        let Ok(entry) = std::fs::read_to_string(&source) else {
            continue;
        };
        let packaging = if let Some(id) = key(&entry, "X-Flatpak") {
            Packaging::Flatpak(id.trim().to_owned())
        } else if source.starts_with("/var/lib/snapd") {
            Packaging::Snap(stem.split('_').next().unwrap_or(stem).to_owned())
        } else {
            Packaging::Native
        };
        let integrated =
            std::fs::read_to_string(override_dir().join(&file)).is_ok_and(|o| o.contains(MARKER));
        found.push(Browser {
            name: key(&entry, "Name").unwrap_or(stem).trim().to_owned(),
            file,
            source,
            packaging,
            integrated,
        });
    }
    found
}

/// Where the extension goes so that this kind of browser can read it.
fn extension_dir(packaging: &Packaging) -> PathBuf {
    match packaging {
        Packaging::Native => data_dir().join("extension"),
        Packaging::Flatpak(id) => home()
            .join(".var/app")
            .join(id)
            .join("data/rtx-vsr/extension"),
        Packaging::Snap(name) => home()
            .join("snap")
            .join(name)
            .join("common/rtx-vsr/extension"),
    }
}

fn write_extension(dir: &Path) -> Result<(), String> {
    for (name, bytes) in EXTENSION {
        let path = dir.join(name);
        if std::fs::read(&path).is_ok_and(|old| old == *bytes) {
            continue;
        }
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Writes the extension to the app's data directory, for loading by hand
/// ("Load unpacked"), and returns where it is.
pub fn export_extension() -> Result<PathBuf, String> {
    let dir = extension_dir(&Packaging::Native);
    write_extension(&dir)?;
    Ok(dir)
}

/// Byte ranges of the arguments in an `Exec` value (quotes included).
fn arguments(exec: &str) -> Vec<(usize, usize)> {
    let bytes = exec.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            i += 1;
            continue;
        }
        let start = i;
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(bytes.len());
        } else {
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
        }
        spans.push((start, i));
    }
    spans
}

/// Adds the switches to one command line. `app_id` is the Flatpak
/// application the line runs, whose own arguments start after its name.
fn patch_exec(exec: &str, app_id: Option<&str>, extension: &str) -> String {
    let spans = arguments(exec);
    if spans.is_empty() {
        return exec.to_owned();
    }
    let arg = |i: usize| &exec[spans[i].0..spans[i].1];
    // Skip an `env NAME=value …` prefix to find the program.
    let mut program = 0;
    if arg(0).rsplit('/').next() == Some("env") {
        program = 1;
        while program < spans.len() - 1
            && !arg(program).starts_with('-')
            && arg(program).contains('=')
        {
            program += 1;
        }
    }
    let after = app_id
        .and_then(|id| (program..spans.len()).find(|i| arg(*i) == id))
        .unwrap_or(program.min(spans.len() - 1));

    // A switch given twice counts once (the last one), so join existing ones.
    let (mut have_load, mut have_feature) = (false, false);
    let mut out = String::with_capacity(exec.len() + extension.len() + 96);
    let mut cursor = 0;
    for (i, (start, end)) in spans.iter().enumerate() {
        out.push_str(&exec[cursor..*start]);
        let a = arg(i);
        if i > after && a.starts_with(LOAD) {
            have_load = true;
            out.push_str(a);
            if !a[LOAD.len()..].split(',').any(|d| d == extension) {
                out.push(',');
                out.push_str(extension);
            }
        } else if i > after && a.starts_with(FEATURES) {
            have_feature = true;
            out.push_str(a);
            if !a[FEATURES.len()..].split(',').any(|f| f == FEATURE) {
                out.push(',');
                out.push_str(FEATURE);
            }
        } else {
            out.push_str(a);
        }
        cursor = *end;
        if i == after {
            // placeholder position: the new switches go right here
            out.push('\u{0}');
        }
    }
    out.push_str(&exec[cursor..]);
    let mut added = String::new();
    if !have_load {
        added.push(' ');
        added.push_str(&exec_quote(&format!("{LOAD}{extension}")));
    }
    if !have_feature {
        added.push(' ');
        added.push_str(FEATURES);
        added.push_str(FEATURE);
    }
    out.replace('\u{0}', &added)
}

/// The browser's launcher with every command line patched and our marker.
fn patched_entry(original: &str, app_id: Option<&str>, extension: &str) -> String {
    let mut out = String::with_capacity(original.len() + 512);
    for line in original.lines() {
        if line == MARKER {
            continue;
        }
        if let Some(exec) = line.strip_prefix("Exec=") {
            out.push_str("Exec=");
            out.push_str(&patch_exec(exec, app_id, extension));
        } else if line.trim() == "DBusActivatable=true" {
            out.push_str("DBusActivatable=false"); // or Exec would be bypassed
        } else {
            out.push_str(line);
        }
        out.push('\n');
        if line.trim() == "[Desktop Entry]" {
            out.push_str(MARKER);
            out.push('\n');
        }
    }
    out
}

impl Browser {
    fn override_path(&self) -> PathBuf {
        override_dir().join(&self.file)
    }

    /// Installs (or refreshes) the extension and the launcher override.
    pub fn integrate(&mut self) -> Result<(), String> {
        let target = self.override_path();
        if let Ok(existing) = std::fs::read_to_string(&target) {
            if !existing.contains(MARKER) {
                return Err(format!(
                    "{} is a launcher you made yourself; it was left alone. Add {LOAD}… to it by hand, or remove it.",
                    target.display()
                ));
            }
        }
        let extension = extension_dir(&self.packaging);
        write_extension(&extension)?;
        let original = std::fs::read_to_string(&self.source)
            .map_err(|e| format!("{}: {e}", self.source.display()))?;
        let app_id = match &self.packaging {
            Packaging::Flatpak(id) => Some(id.as_str()),
            _ => None,
        };
        let entry = patched_entry(&original, app_id, &extension.to_string_lossy());
        if std::fs::read_to_string(&target).ok().as_deref() != Some(entry.as_str()) {
            let dir = override_dir();
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            std::fs::write(&target, entry).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        self.integrated = true;
        Ok(())
    }

    /// Removes the launcher override and the sandboxed copy of the extension.
    pub fn disconnect(&mut self) -> Result<(), String> {
        let target = self.override_path();
        match std::fs::read_to_string(&target) {
            Ok(existing) if existing.contains(MARKER) => {
                std::fs::remove_file(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            }
            _ => {}
        }
        if self.packaging != Packaging::Native {
            let dir = extension_dir(&self.packaging);
            if let Some(ours) = dir.parent().filter(|p| p.ends_with("rtx-vsr")) {
                let _ = std::fs::remove_dir_all(ours);
            }
        }
        self.integrated = false;
        Ok(())
    }

    /// Whether a running instance of this browser was started with the
    /// extension (otherwise it needs a restart to pick the override up).
    pub fn loaded(&self) -> bool {
        let wanted = extension_dir(&self.packaging);
        let wanted = wanted.to_string_lossy();
        let Ok(processes) = std::fs::read_dir("/proc") else {
            return false;
        };
        processes.flatten().any(|p| {
            let name = p.file_name();
            if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            std::fs::read(p.path().join("cmdline")).is_ok_and(|cmdline| {
                cmdline.split(|b| *b == 0).any(|arg| {
                    let arg = String::from_utf8_lossy(arg);
                    arg.strip_prefix(LOAD)
                        .is_some_and(|dirs| dirs.split(',').any(|d| d == wanted))
                })
            })
        })
    }
}

/// Brings the overrides of connected browsers up to date: with this build's
/// extension, and with launchers a browser update may have changed.
pub fn refresh() {
    for mut browser in detect().into_iter().filter(|b| b.integrated) {
        let _ = browser.integrate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXT: &str = "/home/u/.var/app/com.vivaldi.Vivaldi/data/rtx-vsr/extension";
    const SWITCHES: &str = "--load-extension=/home/u/.var/app/com.vivaldi.Vivaldi/data/rtx-vsr/extension --disable-features=DisableLoadExtensionCommandLineSwitch";

    #[test]
    fn switches_go_after_the_program() {
        assert_eq!(
            patch_exec("/usr/bin/google-chrome-stable %U", None, EXT),
            format!("/usr/bin/google-chrome-stable {SWITCHES} %U")
        );
        assert_eq!(
            patch_exec("/usr/bin/google-chrome-stable --incognito", None, EXT),
            format!("/usr/bin/google-chrome-stable {SWITCHES} --incognito")
        );
        assert_eq!(
            patch_exec("brave-browser", None, EXT),
            format!("brave-browser {SWITCHES}")
        );
        assert_eq!(
            patch_exec(
                "env BAMF_DESKTOP_FILE_HINT=/x/chromium.desktop /snap/bin/chromium %U",
                None,
                EXT
            ),
            format!(
                "env BAMF_DESKTOP_FILE_HINT=/x/chromium.desktop /snap/bin/chromium {SWITCHES} %U"
            )
        );
        assert_eq!(
            patch_exec("\"/opt/My Browser/chrome\" %U", None, EXT),
            format!("\"/opt/My Browser/chrome\" {SWITCHES} %U")
        );
    }

    #[test]
    fn flatpak_switches_go_after_the_application() {
        let exec = "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=vivaldi --file-forwarding com.vivaldi.Vivaldi @@u %U @@";
        assert_eq!(
            patch_exec(exec, Some("com.vivaldi.Vivaldi"), EXT),
            format!("/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=vivaldi --file-forwarding com.vivaldi.Vivaldi {SWITCHES} @@u %U @@")
        );
    }

    #[test]
    fn existing_switches_are_extended_not_shadowed() {
        let patched = patch_exec(
            "chrome --disable-features=Foo --load-extension=/a %U",
            None,
            "/b",
        );
        assert_eq!(
            patched,
            "chrome --disable-features=Foo,DisableLoadExtensionCommandLineSwitch --load-extension=/a,/b %U"
        );
        // and patching is idempotent
        assert_eq!(patch_exec(&patched, None, "/b"), patched);
    }

    #[test]
    fn a_path_with_spaces_is_quoted() {
        assert_eq!(
            patch_exec("chrome %U", None, "/home/a b/ext"),
            "chrome \"--load-extension=/home/a b/ext\" --disable-features=DisableLoadExtensionCommandLineSwitch %U"
        );
    }

    #[test]
    fn every_command_line_of_a_launcher_is_patched() {
        let original = "[Desktop Entry]\nName=Chromium\nExec=chromium %U\nDBusActivatable=true\n\n[Desktop Action new-window]\nName=New Window\nExec=chromium --new-window\n";
        let patched = patched_entry(original, None, "/e");
        assert!(patched.starts_with("[Desktop Entry]\nX-RTX-VSR-Managed=true\nName=Chromium\n"));
        assert_eq!(patched.matches("--load-extension=/e").count(), 2);
        assert!(patched.contains("Exec=chromium --load-extension=/e --disable-features=DisableLoadExtensionCommandLineSwitch --new-window\n"));
        assert!(patched.contains("DBusActivatable=false\n"));
        // applying it to its own output changes nothing
        assert_eq!(patched_entry(&patched, None, "/e"), patched);
    }

    #[test]
    fn the_embedded_extension_is_complete() {
        let manifest = EXTENSION
            .iter()
            .find(|(n, _)| *n == "manifest.json")
            .unwrap()
            .1;
        let manifest: serde_json::Value = serde_json::from_slice(manifest).unwrap();
        let mut referenced = vec![
            manifest["background"]["service_worker"]
                .as_str()
                .unwrap()
                .to_owned(),
            manifest["options_page"].as_str().unwrap().to_owned(),
            manifest["action"]["default_popup"]
                .as_str()
                .unwrap()
                .to_owned(),
        ];
        referenced.extend(
            manifest["icons"]
                .as_object()
                .unwrap()
                .values()
                .map(|v| v.as_str().unwrap().to_owned()),
        );
        for group in manifest["web_accessible_resources"].as_array().unwrap() {
            referenced.extend(
                group["resources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned()),
            );
        }
        for file in referenced {
            assert!(
                EXTENSION.iter().any(|(n, _)| *n == file),
                "{file} is not embedded"
            );
        }
    }
}
