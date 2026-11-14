//! Which Linux this is, so advice names its own way of installing things: apt on Debian and
//! Ubuntu, dnf on Fedora, pacman on Arch, zypper on openSUSE, Homebrew or rpm-ostree on
//! Fedora's image-based systems (Bazzite, Silverblue), where dnf can't install.

use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Family {
    /// Fedora's image-based systems (Bazzite, Silverblue, Kinoite): /run/ostree-booted.
    Atomic,
    Fedora,
    Debian,
    Arch,
    Suse,
    Other,
}

/// A program's package name on each family; "" where it has none.
pub struct Package {
    pub apt: &'static str,
    pub dnf: &'static str,
    pub pacman: &'static str,
    pub zypper: &'static str,
    /// Homebrew's, for Atomic systems (Bazzite ships brew); else rpm-ostree with `dnf`.
    pub brew: &'static str,
}

pub const SEVEN_ZIP: Package = Package { apt: "7zip", dnf: "7zip", pacman: "7zip", zypper: "7zip", brew: "sevenzip" };
pub const MONO: Package = Package { apt: "mono-complete", dnf: "mono-complete", pacman: "mono", zypper: "mono-complete", brew: "mono" };
pub const PYTHON: Package = Package { apt: "python3", dnf: "python3", pacman: "python", zypper: "python3", brew: "python3" };

pub fn family() -> Family {
    family_of(&fs::read_to_string("/etc/os-release").unwrap_or_default(), Path::new("/run/ostree-booted").exists())
}

fn family_of(os_release: &str, ostree: bool) -> Family {
    let field = |key: &str| os_release.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(|v| v.trim_matches('"').to_lowercase()).unwrap_or_default();
    let names = format!("{} {}", field("ID"), field("ID_LIKE"));
    let has = |n: &str| names.split_whitespace().any(|w| w == n);
    if has("fedora") || has("rhel") || has("centos") {
        if ostree { Family::Atomic } else { Family::Fedora }
    } else if has("debian") || has("ubuntu") {
        Family::Debian
    } else if has("arch") {
        Family::Arch
    } else if has("suse") || has("opensuse") {
        Family::Suse
    } else {
        Family::Other
    }
}

/// The command that installs `package` here, typed in a terminal; None where the family isn't
/// known or has no such package.
pub fn install_command(package: &Package) -> Option<String> {
    install_on(family(), package, which("brew"))
}

fn install_on(family: Family, p: &Package, brew: bool) -> Option<String> {
    let cmd = |tool: &str, name: &str| (!name.is_empty()).then(|| format!("{tool} {name}"));
    match family {
        Family::Debian => cmd("sudo apt install", p.apt),
        Family::Fedora => cmd("sudo dnf install", p.dnf),
        Family::Arch => cmd("sudo pacman -S", p.pacman),
        Family::Suse => cmd("sudo zypper install", p.zypper),
        Family::Atomic if brew && !p.brew.is_empty() => cmd("brew install", p.brew),
        Family::Atomic => cmd("rpm-ostree install", p.dnf).map(|c| format!("{c} (then restart)")),
        Family::Other => None,
    }
}

/// "install it with: <command>" for a message, or a plain request where the command isn't known.
pub fn how_to_install(package: &Package, what: &str) -> String {
    match install_command(package) {
        Some(command) => format!("install {what} with: {command}"),
        None => format!("install {what} from your system's packages"),
    }
}

fn which(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join(name).is_file()) || (name == "brew" && Path::new("/home/linuxbrew/.linuxbrew/bin/brew").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_and_commands() {
        assert_eq!(family_of("NAME=\"Bazzite\"\nID=bazzite\nID_LIKE=\"fedora\"\n", true), Family::Atomic);
        assert_eq!(family_of("ID=fedora\n", false), Family::Fedora);
        assert_eq!(family_of("ID=ubuntu\nID_LIKE=debian\n", false), Family::Debian);
        assert_eq!(family_of("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n", false), Family::Debian);
        assert_eq!(family_of("ID=arch\n", false), Family::Arch);
        assert_eq!(family_of("ID=cachyos\nID_LIKE=arch\n", false), Family::Arch);
        assert_eq!(family_of("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n", false), Family::Suse);
        assert_eq!(install_on(Family::Debian, &MONO, false).as_deref(), Some("sudo apt install mono-complete"));
        assert_eq!(install_on(Family::Atomic, &MONO, true).as_deref(), Some("brew install mono"));
        assert_eq!(install_on(Family::Atomic, &SEVEN_ZIP, false).as_deref(), Some("rpm-ostree install 7zip (then restart)"));
        assert_eq!(install_on(Family::Other, &MONO, true), None);
    }
}
