//! Launcher entries: the one that puts the app in the applications menu and
//! the one that starts it at login.

use std::path::{Path, PathBuf};

use crate::state::{config_home, data_home, APP_ID};

const ICON: &[u8] = include_bytes!("../assets/icon-256.png");

fn autostart_file() -> PathBuf {
    config_home()
        .join("autostart")
        .join(format!("{APP_ID}.desktop"))
}

/// Quotes one argument for a desktop entry's `Exec` key.
pub fn exec_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+".contains(c));
    if plain {
        return arg.to_owned();
    }
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    // `%` introduces a field code anywhere in the key.
    quoted.replace('%', "%%")
}

fn entry(exe: &Path, args: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=RTX VSR\n\
         GenericName=Video Upscaler\n\
         Comment=AI upscaling for browser video on NVIDIA RTX graphics cards\n\
         Exec={}{args}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=AudioVideo;Video;Utility;\n\
         Keywords=nvidia;rtx;vsr;upscale;video;super resolution;\n\
         StartupWMClass={APP_ID}\n",
        exec_quote(&exe.to_string_lossy()),
    )
}

fn write_if_changed(path: &Path, content: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn is_enabled() -> bool {
    autostart_file().is_file()
}

/// Starts (or stops starting) the app when the user logs in.
pub fn set(enabled: bool) -> Result<(), String> {
    let path = autostart_file();
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let text = entry(&exe, " --autostart") + "X-GNOME-Autostart-enabled=true\n";
    write_if_changed(&path, text.as_bytes())
}

/// Keeps the applications-menu entry, its icon and (if on) the login entry
/// pointing at this executable. Called at every start, so a moved or updated
/// binary fixes them by being run once.
pub fn refresh() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let data = data_home();
    let _ = write_if_changed(
        &data
            .join("icons/hicolor/256x256/apps")
            .join(format!("{APP_ID}.png")),
        ICON,
    );
    let _ = write_if_changed(
        &data.join("applications").join(format!("{APP_ID}.desktop")),
        entry(&exe, "").as_bytes(),
    );
    if is_enabled() {
        let _ = set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::exec_quote;

    #[test]
    fn exec_arguments_are_quoted_when_needed() {
        assert_eq!(exec_quote("/usr/bin/rtx-vsr"), "/usr/bin/rtx-vsr");
        assert_eq!(exec_quote("/home/a b/rtx-vsr"), "\"/home/a b/rtx-vsr\"");
        assert_eq!(exec_quote("/opt/$x/100%"), "\"/opt/\\$x/100%%\"");
    }
}
