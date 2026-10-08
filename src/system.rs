//! What the host has installed: its libraries (from ld.so's cache) and its glibc version.

use std::collections::HashMap;
use std::process::Command;

pub struct System {
    /// Library file name → bit 1 for 32-bit, bit 2 for 64-bit x86.
    libs: HashMap<String, u8>,
    pub glibc: Option<(u32, u32)>,
}

fn run(candidates: &[&str], args: &[&str]) -> Option<String> {
    candidates.iter().find_map(|cmd| {
        let out = Command::new(cmd).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

impl System {
    pub fn load() -> System {
        let mut libs = HashMap::new();
        // Lines look like "\tlibGL.so.1 (libc6,x86-64) => /lib64/libGL.so.1"; 32-bit x86 is plain "(libc6)".
        for line in run(&["/usr/sbin/ldconfig", "/sbin/ldconfig", "ldconfig"], &["-p"]).unwrap_or_default().lines().skip(1) {
            let Some((name, rest)) = line.trim().split_once(" (") else { continue };
            let tag = rest.split(')').next().unwrap_or("");
            let bit = match tag.split(',').nth(1).map(str::trim) {
                Some("x86-64") => 2,
                None => 1,
                Some(a) if a.starts_with("OS ABI") || a.starts_with("hwcap") => 1,
                _ => continue,
            };
            *libs.entry(name.to_string()).or_insert(0) |= bit;
        }
        // "ldd (GNU libc) 2.43": the host's glibc, which a static build can't ask about directly.
        let glibc = run(&["/usr/bin/ldd", "ldd"], &["--version"]).and_then(|out| {
            let version = out.lines().next()?.rsplit(' ').next()?.to_string();
            let (major, minor) = version.split_once('.')?;
            Some((major.parse().ok()?, minor.parse().ok()?))
        });
        System { libs, glibc }
    }

    pub fn has(&self, name: &str, bits: u8) -> bool {
        let bit = if bits == 64 { 2 } else { 1 };
        self.libs.get(name).is_some_and(|b| b & bit != 0)
    }

    pub fn refuses_exec_stack(&self) -> bool {
        self.glibc.is_some_and(|v| v >= (2, 41))
    }
}
