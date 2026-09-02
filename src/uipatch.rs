//! Startup patch for the game's in-game browser window.
//!
//! When the player leaves cursor mode ("shoot mode"), the client's workspace
//! deactivates every open window except those whose page CodeData carries
//! `StickyVisible='true'` — the chat window survives that way. The web
//! browser page (`ui/ui_pda.inc`, page `WebBrowser`) does not, so a meter
//! opened with `/browser` vanishes as soon as you start fighting.
//!
//! Escape closes it too: the UI system presses whichever button a window
//! flags as its cancel button, and the browser's title-bar X carries that
//! flag. Clearing the flag keeps the X clickable but takes it off Escape, so
//! spamming Escape in a fight no longer kills the meter.
//!
//! On startup we look for a loose `ui/ui_pda.inc` in the game directory and
//! apply both changes if they are missing (keeping a backup). Without a
//! loose file — a stock install, or a UI mod that does not ship this page —
//! we write one from the copy of Legends' page bundled in the exe (the way
//! UI mods distribute these files), already patched. The client reads UI
//! files at launch, so either change applies from the next game start.
//! `--restore-ui` undoes it: the backup goes back, or the file we created is
//! removed.

use std::fs;
use std::path::Path;

pub enum Outcome {
    /// Attribute already present.
    AlreadySet,
    /// File rewritten; backup at the given path.
    Patched(String),
    /// No loose `ui/ui_pda.inc` existed; the bundled, pre-patched page was
    /// written there.
    Installed,
}

const ATTR: &str = "StickyVisible='true'";

/// Legends' `ui/ui_pda.inc` as shipped in the client (`EMBEDDED_VERSION`
/// says which), unmodified; patched at write time by the same code that
/// patches a user's own file.
pub const EMBEDDED: &str = include_str!("../assets/ui_pda.inc");
pub const EMBEDDED_VERSION: &str = "SWG Legends client 26.08.16 (page captured 2026-09-02)";

/// The bundled page with both fixes applied.
pub fn embedded_patched() -> String {
    patch_text(EMBEDDED).unwrap_or_else(|| EMBEDDED.to_string())
}

/// Apply every browser-window fix that is missing. None = nothing to change
/// (or no WebBrowser page found).
pub fn patch_text(src: &str) -> Option<String> {
    let sticky = patch_sticky(src);
    let after_sticky = sticky.as_deref().unwrap_or(src);
    match patch_escape(after_sticky) {
        Some(s) => Some(s),
        None => sticky,
    }
}

/// Take the WebBrowser window's close button off the Escape key: inside the
/// page, the `<Button ... Name='close' ...>` element gets
/// `IsCancelButton='false'`. None when already so (or absent, which the UI
/// treats as false) or when the page / button is not found.
fn patch_escape(src: &str) -> Option<String> {
    let page = src.find("Name='WebBrowser'")?;
    let close_name = src[page..].find("Name='close'")? + page;
    let open = src[..close_name].rfind("<Button")?;
    let end = src[close_name..].find('>')? + close_name;
    let elem = &src[open..end];
    let flag = elem.find("IsCancelButton='true'")? + open;
    let mut out = String::with_capacity(src.len() + 1);
    out.push_str(&src[..flag]);
    out.push_str("IsCancelButton='false'");
    out.push_str(&src[flag + "IsCancelButton='true'".len()..]);
    Some(out)
}

/// Insert `StickyVisible='true'` into the WebBrowser page's CodeData.
/// Returns None when nothing needs to change (already set, or the page /
/// CodeData block could not be found).
fn patch_sticky(src: &str) -> Option<String> {
    let page = src.find("Name='WebBrowser'")?;
    // The CodeData block is the <Data ... /> after the page start that maps
    // the browser image; `browserimage=` is unique to it.
    let key = src[page..].find("browserimage=")? + page;
    let open = src[..key].rfind("<Data")?;
    let close = src[key..].find("/>")? + key + 2;
    let block = &src[open..close];
    if block.contains("StickyVisible=") {
        if block.contains(ATTR) {
            return None;
        }
        // explicit false (or garbage): flip it to true
        let start = block.find("StickyVisible=")? + open;
        let end = src[start + "StickyVisible='".len()..].find('\'')? + start + "StickyVisible='".len() + 1;
        let mut out = String::with_capacity(src.len() + 8);
        out.push_str(&src[..start]);
        out.push_str(ATTR);
        out.push_str(&src[end..]);
        return Some(out);
    }
    // Insert as the first attribute line, matching the file's newline and the
    // indentation of the line that follows "<Data".
    let after_tag = open + "<Data".len();
    let nl_end = src[after_tag..].find('\n')? + after_tag + 1;
    let line_end = &src[after_tag..nl_end]; // e.g. "\r\n" or "\n"
    let indent: String = src[nl_end..]
        .chars()
        .take_while(|c| *c == '\t' || *c == ' ')
        .collect();
    let mut out = String::with_capacity(src.len() + 40);
    out.push_str(&src[..nl_end]);
    out.push_str(&indent);
    out.push_str(ATTR);
    out.push_str(line_end.trim_start_matches(|c| c != '\r' && c != '\n'));
    out.push_str(&src[nl_end..]);
    Some(out)
}

/// Ensure the game's loose `ui/ui_pda.inc` marks the browser window sticky
/// and off the Escape key — patching the user's file, or writing ours.
pub fn ensure_sticky_browser(game_dir: &Path) -> Result<Outcome, String> {
    let ui = game_dir.join("ui");
    let path = ui.join("ui_pda.inc");
    if !path.is_file() {
        if !game_dir.is_dir() {
            return Err(format!("{} is not a directory", game_dir.display()));
        }
        fs::create_dir_all(&ui).map_err(|e| format!("create {}: {}", ui.display(), e))?;
        fs::write(&path, embedded_patched().as_bytes()).map_err(|e| format!("write {}: {}", path.display(), e))?;
        return Ok(Outcome::Installed);
    }
    let bytes = fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let src = String::from_utf8_lossy(&bytes);
    let patched = match patch_text(&src) {
        Some(p) => p,
        None => {
            return if src.contains("browserimage=") {
                Ok(Outcome::AlreadySet)
            } else {
                Err(format!("{}: WebBrowser page not found", path.display()))
            };
        }
    };
    let backup = path.with_extension("inc.pre-swglogs");
    if !backup.exists() {
        fs::copy(&path, &backup).map_err(|e| format!("backup {}: {}", backup.display(), e))?;
    }
    fs::write(&path, patched.as_bytes()).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(Outcome::Patched(backup.display().to_string()))
}

/// Undo: put the backup back if there is one, else remove the file if it is
/// the one we wrote (byte-identical to our patched page). A user's own,
/// hand-edited file is never deleted. Returns what was done.
pub fn restore(game_dir: &Path) -> Result<String, String> {
    let path = game_dir.join("ui").join("ui_pda.inc");
    let backup = path.with_extension("inc.pre-swglogs");
    if backup.is_file() {
        fs::rename(&backup, &path).map_err(|e| format!("restore {}: {}", path.display(), e))?;
        return Ok(format!("restored {} from its pre-swglogs backup", path.display()));
    }
    if !path.is_file() {
        return Ok(format!("nothing to restore: no {}", path.display()));
    }
    let current = fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    if current == embedded_patched().as_bytes() {
        fs::remove_file(&path).map_err(|e| format!("remove {}: {}", path.display(), e))?;
        return Ok(format!("removed {} (it was swglogs' bundled copy)", path.display()));
    }
    Ok(format!("left {} alone: not swglogs' copy and no backup to restore", path.display()))
}

/// Pure-function checks, run by `swglogs --selftest`.
pub fn selfcheck(check: &mut dyn FnMut(bool, &str)) {
    let emb = embedded_patched();
    check(
        emb.contains("StickyVisible='true'") && !emb.contains("IsCancelButton='true'\r\n\t\t\t\t\t\tName='close'")
            && emb.len() > EMBEDDED.len() && patch_text(&emb).is_none(),
        "uipatch: bundled Legends page patches cleanly and is then a fixed point",
    );
    let page = "\t\t<Page\r\n\t\t\tName='WebBrowser'\r\n\t\t>\r\n\t\t\t<Data\r\n\t\t\t\tback='buttonBack'\r\n\t\t\t\tbrowserimage='webBrowserControl.webBrowserScreen'\r\n\t\t\t\tName='CodeData'\r\n\t\t\t/>\r\n\t\t</Page>\r\n";
    let out = patch_text(page).unwrap_or_default();
    check(
        out.contains("\t\t\t<Data\r\n\t\t\t\tStickyVisible='true'\r\n\t\t\t\tback='buttonBack'"),
        "uipatch: inserts StickyVisible with file's indent + CRLF",
    );
    check(patch_text(&out).is_none(), "uipatch: idempotent once set");
    let off = page.replace("back='buttonBack'", "StickyVisible='false'\r\n\t\t\t\tback='buttonBack'");
    let fixed = patch_text(&off).unwrap_or_default();
    check(
        fixed.contains("StickyVisible='true'") && !fixed.contains("StickyVisible='false'"),
        "uipatch: flips an explicit false",
    );
    check(patch_text("<Data browserimage='x' />").is_none(), "uipatch: no WebBrowser page -> untouched");
    let with_close = "\t\t<Page\r\n\t\t\tName='WebBrowser'\r\n\t\t>\r\n\t\t\t<Data\r\n\t\t\t\tStickyVisible='true'\r\n\t\t\t\tbrowserimage='x'\r\n\t\t\t/>\r\n\t\t\t\t\t<Button\r\n\t\t\t\t\t\tIsCancelButton='true'\r\n\t\t\t\t\t\tName='close'\r\n\t\t\t\t\t></Button>\r\n\t\t</Page>\r\n";
    let out2 = patch_text(with_close).unwrap_or_default();
    check(
        out2.contains("IsCancelButton='false'\r\n\t\t\t\t\t\tName='close'") && !out2.contains("IsCancelButton='true'"),
        "uipatch: close button taken off the Escape key",
    );
    check(patch_text(&out2).is_none(), "uipatch: idempotent once both fixes are in");
    // a cancel button elsewhere on the page (not the close X) is left alone
    let other = "\t\t<Page\r\n\t\t\tName='WebBrowser'\r\n\t\t>\r\n\t\t\t<Data StickyVisible='true' browserimage='x' />\r\n\t\t\t<Button IsCancelButton='true' Name='buttonStop'></Button>\r\n\t\t\t<Button Name='close'></Button>\r\n\t\t</Page>\r\n";
    check(patch_text(other).is_none(), "uipatch: only the close button's cancel flag is touched");
}
