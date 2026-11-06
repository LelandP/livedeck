//! Embeds the app icon (packaging/windows/livedeck.ico, made by packaging/windows/make-icon.ps1)
//! in livedeck.exe, so Explorer, the taskbar and shortcuts show it.
//!
//! The icon is written as a compiled resource file (.res) by hand and handed to the MSVC linker,
//! which takes .res files directly, so there is no resource compiler or extra dependency to find.

use std::{env, fs, path::PathBuf};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;
// MOVEABLE | DISCARDABLE (| PURE for the group), as rc.exe writes them.
const ICON_FLAGS: u16 = 0x1010;
const GROUP_FLAGS: u16 = 0x1030;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico = manifest.join("../../packaging/windows/livedeck.ico");
    println!("cargo:rerun-if-changed={}", ico.display());

    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !(windows && msvc) {
        return;
    }

    let bytes = fs::read(&ico).unwrap_or_else(|e| panic!("reading {}: {e}", ico.display()));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("livedeck.res");
    fs::write(&out, icon_res(&bytes)).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// A .res file holding every image of `ico` as an RT_ICON and one RT_GROUP_ICON (id 1) that
/// lists them. Windows uses the first icon group as the executable's icon.
fn icon_res(ico: &[u8]) -> Vec<u8> {
    let u16_at = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);
    assert!(
        ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1,
        "not an .ico file"
    );
    let count = u16_at(4);

    let mut res = Vec::new();
    // A .res file starts with an empty resource.
    resource(&mut res, 0, 0, 0, 0, &[]);

    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&count.to_le_bytes());
    for i in 0..count {
        let entry = 6 + 16 * usize::from(i);
        let size = u32_at(entry + 8);
        let offset = u32_at(entry + 12) as usize;
        let image = &ico[offset..offset + size as usize];
        let id = i + 1;
        resource(&mut res, RT_ICON, id, ICON_FLAGS, LANG_EN_US, image);
        // GRPICONDIRENTRY: the ICONDIRENTRY's first 12 bytes, then the RT_ICON id.
        group.extend_from_slice(&ico[entry..entry + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    resource(&mut res, RT_GROUP_ICON, 1, GROUP_FLAGS, LANG_EN_US, &group);
    res
}

/// Appends one resource (header with ordinal type and name, then the data padded to 4 bytes).
fn resource(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, lang: u16, data: &[u8]) {
    let len = u32::try_from(data.len()).expect("resource over 4 GiB");
    res.extend_from_slice(&len.to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes()); // header size
    for word in [0xFFFF, kind, 0xFFFF, name] {
        res.extend_from_slice(&word.to_le_bytes());
    }
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&lang.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    res.resize(res.len().next_multiple_of(4), 0);
}
