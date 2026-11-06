//! Per-user XDG desktop autostart registration for the mail service.

use gtk::glib;
use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const NAME: &str = "io.github.augustin_laurent.Bang.Service.desktop";

fn config_home() -> io::Result<PathBuf> {
    let dirs = directories::BaseDirs::new().ok_or_else(|| io::Error::other("no home directory"))?;
    Ok(dirs.config_dir().to_path_buf())
}

fn candidates(home: &Path, system_dirs: &[PathBuf]) -> Vec<PathBuf> {
    std::iter::once(home.to_path_buf())
        .chain(system_dirs.iter().cloned())
        .map(|dir| dir.join("autostart").join(NAME))
        .collect()
}

fn system_dirs() -> Vec<PathBuf> {
    std::env::var_os("XDG_CONFIG_DIRS")
        .filter(|s| !s.is_empty())
        .map(|v| {
            std::env::split_paths(&v)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")])
}

fn effective(paths: &[PathBuf]) -> io::Result<bool> {
    for path in paths {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let key = glib::KeyFile::new();
                key.load_from_data(&contents, glib::KeyFileFlags::NONE)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                let hidden = key.boolean("Desktop Entry", "Hidden").unwrap_or(false);
                let disabled = key
                    .boolean("Desktop Entry", "X-GNOME-Autostart-enabled")
                    .ok()
                    == Some(false);
                if hidden
                    || disabled
                    || key.string("Desktop Entry", "Type").ok().as_deref() != Some("Application")
                {
                    return Ok(false);
                }
                let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
                let desktop_matches = |value: &str| {
                    value.split(';').filter(|s| !s.is_empty()).any(|entry| {
                        desktop
                            .split(':')
                            .any(|current| current.eq_ignore_ascii_case(entry))
                    })
                };
                if key
                    .string("Desktop Entry", "OnlyShowIn")
                    .ok()
                    .is_some_and(|s| !desktop_matches(&s))
                    || key
                        .string("Desktop Entry", "NotShowIn")
                        .ok()
                        .is_some_and(|s| desktop_matches(&s))
                {
                    return Ok(false);
                }
                if let Ok(try_exec) = key.string("Desktop Entry", "TryExec") {
                    if !try_exec.is_empty() {
                        let executable = Path::new(try_exec.as_str());
                        let executable_file = |path: &Path| {
                            std::fs::metadata(path)
                                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                        };
                        let found = if executable.is_absolute() {
                            executable_file(executable)
                        } else if executable.components().count() == 1 {
                            std::env::var_os("PATH").is_some_and(|path| {
                                std::env::split_paths(&path)
                                    .any(|dir| executable_file(&dir.join(executable)))
                            })
                        } else {
                            false
                        };
                        if !found {
                            return Ok(false);
                        }
                    }
                }
                return Ok(key.has_key("Desktop Entry", "Exec").unwrap_or(false));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

pub(crate) fn enabled() -> io::Result<bool> {
    effective(&candidates(&config_home()?, &system_dirs()))
}

fn write_entry(home: &Path, executable: &Path, enabled: bool) -> io::Result<()> {
    let path = home.join("autostart").join(NAME);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("invalid autostart path"))?;
    std::fs::create_dir_all(parent)?;
    let exe = executable
        .to_str()
        .ok_or_else(|| io::Error::other("invalid executable path"))?;
    if exe.chars().any(|c| c == '\n' || c == '\r') {
        return Err(io::Error::other("invalid executable path"));
    }
    let quoted = exe
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$");
    let try_exec = exe.replace('\\', "\\\\");
    let initial = format!("[Desktop Entry]\nType=Application\nName=Bang background service\nTryExec={try_exec}\nExec=\"{quoted}\" --autostart\nTerminal=false\n");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => initial,
        Err(error) => return Err(error),
    };
    let key = glib::KeyFile::new();
    key.load_from_data(
        &contents,
        glib::KeyFileFlags::KEEP_COMMENTS | glib::KeyFileFlags::KEEP_TRANSLATIONS,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    key.set_boolean("Desktop Entry", "Hidden", !enabled);
    if key
        .has_key("Desktop Entry", "X-GNOME-Autostart-enabled")
        .unwrap_or(false)
    {
        key.set_boolean("Desktop Entry", "X-GNOME-Autostart-enabled", enabled);
    }
    let contents = key.to_data();
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, contents.as_bytes())?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn set_enabled(value: bool) -> io::Result<()> {
    let executable = std::env::current_exe()?.with_file_name("bang-service");
    if value {
        let metadata = std::fs::metadata(&executable)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::other("Bang service is not executable"));
        }
    }
    write_entry(&config_home()?, &executable, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_home_honors_xdg_and_uses_home_fallback() {
        if let Some(expected) = std::env::var_os("BANG_TEST_EXPECTED_CONFIG") {
            let home = config_home().unwrap();
            assert_eq!(home, PathBuf::from(expected));
            write_entry(&home, &std::env::current_exe().unwrap(), true).unwrap();
            assert!(effective(&[home.join("autostart").join(NAME)]).unwrap());
            return;
        }
        let root = std::env::temp_dir().join(format!("bang-xdg-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let custom = root.join("custom config");
        let fallback = root.join(".config");
        for (value, expected) in [
            (Some(custom.as_os_str()), custom.as_path()),
            (None, fallback.as_path()),
            (Some(std::ffi::OsStr::new("")), fallback.as_path()),
            (Some(std::ffi::OsStr::new("relative")), fallback.as_path()),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "autostart::tests::config_home_honors_xdg_and_uses_home_fallback",
                ])
                .env("HOME", &root)
                .env("BANG_TEST_EXPECTED_CONFIG", expected);
            if let Some(value) = value {
                command.env("XDG_CONFIG_HOME", value);
            } else {
                command.env_remove("XDG_CONFIG_HOME");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn user_entry_overrides_system_and_reflects_external_changes() {
        let root = std::env::temp_dir().join(format!(
            "bang-autostart-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let user = root.join("user");
        let system = root.join("system");
        let executable = std::env::current_exe().unwrap();
        let paths = candidates(&user, std::slice::from_ref(&system));
        write_entry(&system, &executable, true).unwrap();
        assert!(effective(&paths).unwrap());
        write_entry(&user, &executable, false).unwrap();
        assert!(!effective(&paths).unwrap());
        write_entry(&user, &executable, true).unwrap();
        assert!(effective(&paths).unwrap());
        if let Ok(output) = std::process::Command::new("desktop-file-validate")
            .arg(&paths[0])
            .output()
        {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::write(&paths[0], "[Desktop Entry]\nType=Application\nExec=/bin/true\nHidden=false\nX-GNOME-Autostart-enabled=false\n").unwrap();
        assert!(!effective(&paths).unwrap());
        std::fs::write(&paths[0], "# desktop preference\n[Desktop Entry]\nType=Application\nExec=/bin/true\nX-GNOME-Autostart-enabled=false\nOnlyShowIn=KDE;\nX-Custom-Setting=keep\n").unwrap();
        write_entry(&user, &executable, true).unwrap();
        let modified = std::fs::read_to_string(&paths[0]).unwrap();
        assert!(modified.contains("# desktop preference"));
        assert!(modified.contains("OnlyShowIn=KDE;"));
        assert!(modified.contains("X-Custom-Setting=keep"));
        assert!(modified.contains("X-GNOME-Autostart-enabled=true"));
        write_entry(&user, &executable, false).unwrap();
        let modified = std::fs::read_to_string(&paths[0]).unwrap();
        assert!(modified.contains("OnlyShowIn=KDE;"));
        assert!(modified.contains("X-Custom-Setting=keep"));
        assert!(!effective(&paths).unwrap());
        std::fs::write(&paths[0], "[Desktop Entry]\nType=Application\nExec=/bin/true\nTryExec=/nonexistent/bang-service\n").unwrap();
        assert!(!effective(&paths).unwrap());
        let invalid = "not a desktop entry\n";
        std::fs::write(&paths[0], invalid).unwrap();
        assert!(write_entry(&user, &executable, true).is_err());
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), invalid);
        let inaccessible = root.join("not-a-directory");
        std::fs::write(&inaccessible, "file").unwrap();
        assert!(write_entry(&inaccessible, &executable, true).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
