//! Dolphin's right-click "gamefix" submenu, switched on and off by the user.
//!
//! The menu is a KDE service menu file; its actions call the copy of gamefix in ~/.local/bin
//! with `--dialog`, so results show in a window instead of a terminal.

use crate::setup::exec_arg;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn menu_file() -> PathBuf {
    home().join(".local/share/kio/servicemenus/gamefix.desktop")
}

fn installed_binary() -> PathBuf {
    home().join(".local/bin/gamefix")
}

/// Game folders, Linux programs (some games' are marked as libraries), Windows programs and
/// installers, disc images, and one-file games another program runs (AppImages, Java, Flash,
/// Game Boy). A .love file is just a zip to KDE, so it's set up through its folder.
const MIME_TYPES: &str = "inode/directory;application/x-executable;application/x-pie-executable;application/x-sharedlib;application/vnd.microsoft.portable-executable;application/x-ms-dos-executable;application/x-msdownload;application/x-cd-image;application/vnd.efi.iso;application/vnd.appimage;application/x-iso9660-appimage;application/java-archive;application/vnd.adobe.flash.movie;application/x-gameboy-rom;application/x-gameboy-color-rom;application/x-gba-rom;";

const ACTIONS: [(&str, &str, &str, &str); 6] = [
    ("check", "Check what's wrong", "dialog-information", "check"),
    ("menu", "Fix and add to the app menu", "applications-games", "setup --menu"),
    ("steam", "Fix and add to Steam", "steam", "setup --steam"),
    ("both", "Fix and add to the app menu and Steam", "applications-games", "setup --menu --steam"),
    ("undo", "Undo gamefix's changes", "edit-undo", "undo"),
    ("feedback", "Report a problem…", "tools-report-bug", "feedback"),
];

pub fn enabled() -> bool {
    menu_file().is_file()
}

/// Copies this gamefix to ~/.local/bin (so the menu calls a program that stays put) and
/// writes the menu. Running it again refreshes both.
pub fn enable() -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin = installed_binary();
    if fs::canonicalize(&me).ok() != fs::canonicalize(&bin).ok() {
        fs::create_dir_all(bin.parent().unwrap()).map_err(|e| e.to_string())?;
        let tmp = bin.with_extension("new");
        fs::copy(&me, &tmp).map_err(|e| e.to_string())?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &bin).map_err(|e| e.to_string())?;
        done.push(format!("Installed gamefix as {}", bin.display()));
    }

    let mut text = format!(
        "[Desktop Entry]\nType=Service\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nMimeType={MIME_TYPES}\nActions={};\nX-KDE-Submenu=gamefix\nIcon=applications-games\n",
        ACTIONS.map(|a| a.0).join(";")
    );
    for (id, name, icon, command) in ACTIONS {
        text.push_str(&format!(
            "\n[Desktop Action {id}]\nName={name}\nIcon={icon}\nExec={} {command} --dialog %f\n",
            exec_arg(&bin.display().to_string())
        ));
    }
    let file = menu_file();
    fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&file, text).map_err(|e| e.to_string())?;
    // KDE only uses service menus in the home folder that are marked executable.
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    done.push("Added \"gamefix\" to Dolphin's right-click menu for folders and game programs.".into());
    Ok(done)
}

pub fn disable() -> Result<Vec<String>, String> {
    match fs::remove_file(menu_file()) {
        Ok(()) => Ok(vec!["Removed \"gamefix\" from Dolphin's right-click menu.".into()]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec!["The right-click menu was already off.".into()]),
        Err(e) => Err(e.to_string()),
    }
}
