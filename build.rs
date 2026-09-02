//! Embed `assets/swglogs.ico` as the executable's icon on Windows, plus an
//! application manifest that makes the exe always ask for Administrator
//! (the memory source has to open the elevated game client).
//!
//! The .res is written by hand (no `rc.exe`, no build-dependency): a Win32
//! resource file is just a sequence of RESOURCEHEADER + data records. MSVC's
//! linker takes a .res straight on the command line; on the GNU toolchain we
//! fall back to `windres` if it is on PATH, otherwise icon + manifest are
//! skipped.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ICO: &str = "assets/swglogs.ico";
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_MANIFEST: u16 = 24;

/// UAC: always run elevated. Everything else is left at Windows defaults.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="swglogs" version="1.0.0.0" processorArchitecture="*"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed={}", ICO);
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = match fs::read(ICO) {
        Ok(b) => b,
        Err(_) => {
            println!("cargo:warning=no {} (run assets/gen_ico.py); exe gets no icon", ICO);
            Vec::new()
        }
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if msvc {
        let res = out.join("swglogs.res");
        fs::write(&res, build_res(&ico)).unwrap();
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        // GNU: windres compiles an .rc into a COFF object we can link.
        let unix = |p: &Path| p.display().to_string().replace('\\', "/");
        let manifest = out.join("swglogs.manifest");
        fs::write(&manifest, MANIFEST).unwrap();
        let mut rc = format!("1 24 \"{}\"\n", unix(&manifest));
        if !ico.is_empty() {
            rc.push_str(&format!("1 ICON \"{}\"\n", unix(&fs::canonicalize(ICO).unwrap())));
        }
        let rc_path = out.join("swglogs.rc");
        fs::write(&rc_path, rc).unwrap();
        let obj = out.join("swglogs_res.o");
        match Command::new("windres")
            .args([rc_path.to_str().unwrap(), "-O", "coff", "-o", obj.to_str().unwrap()])
            .status()
        {
            Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", obj.display()),
            _ => println!("cargo:warning=windres not available; exe gets no icon/manifest"),
        }
    }
}

/// Emit a .res with the manifest (RT_MANIFEST id 1) and, if an .ico was
/// given, one RT_ICON per image plus the RT_GROUP_ICON directory (id 1)
/// that ties them together.
fn build_res(ico: &[u8]) -> Vec<u8> {
    let mut res = Vec::new();
    // leading empty record = file signature
    res.extend_from_slice(&record(0xFFFF, 0, &[]));
    res.extend_from_slice(&record(RT_MANIFEST, 1, MANIFEST.as_bytes()));
    if ico.is_empty() {
        return res;
    }

    let u16le = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([ico[o], ico[o + 1], ico[o + 2], ico[o + 3]]);
    assert!(ico.len() >= 6 && u16le(0) == 0 && u16le(2) == 1, "not an .ico");
    let count = u16le(4) as usize;

    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + i * 16;
        let size = u32le(e + 8) as usize;
        let off = u32le(e + 12) as usize;
        let id = (i + 1) as u16;
        res.extend_from_slice(&record(RT_ICON, id, &ico[off..off + size]));
        // GRPICONDIRENTRY = ICONDIRENTRY minus the offset, plus the resource id
        group.extend_from_slice(&ico[e..e + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    res.extend_from_slice(&record(RT_GROUP_ICON, 1, &group));
    res
}

/// One RESOURCEHEADER (numeric type + name) followed by the data, DWORD-padded.
fn record(rtype: u16, id: u16, data: &[u8]) -> Vec<u8> {
    let mut r = Vec::with_capacity(32 + data.len() + 3);
    r.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    r.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    r.extend_from_slice(&0xFFFFu16.to_le_bytes()); // type: numeric
    r.extend_from_slice(&rtype.to_le_bytes());
    r.extend_from_slice(&0xFFFFu16.to_le_bytes()); // name: numeric
    r.extend_from_slice(&id.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    r.extend_from_slice(&0x1010u16.to_le_bytes()); // MemoryFlags: MOVEABLE | DISCARDABLE
    r.extend_from_slice(&0x0409u16.to_le_bytes()); // LanguageId: en-US
    r.extend_from_slice(&0u32.to_le_bytes()); // Version
    r.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    r.extend_from_slice(data);
    while r.len() % 4 != 0 {
        r.push(0);
    }
    r
}
