//! Startup patch for the game's in-game browser window.
//!
//! When the player leaves cursor mode ("shoot mode"), the client's workspace
//! deactivates every open window except those whose page CodeData carries
//! `StickyVisible='true'` — the chat window survives that way. The web
//! browser page (`ui/ui_pda.inc`, page `WebBrowser`) does not, so a meter
//! opened with `/browser` vanishes as soon as you start fighting.
//!
//! On startup we look for a loose `ui/ui_pda.inc` in the game directory and,
//! if its WebBrowser CodeData lacks the attribute, insert it (keeping a
//! backup). The client reads UI files at launch, so the change applies from
//! the next game start. Without a loose file (stock install: the page lives
//! inside a TRE archive) we only print how to get one.

use std::fs;
use std::path::Path;

pub enum Outcome {
    /// Attribute already present.
    AlreadySet,
    /// File rewritten; backup at the given path.
    Patched(String),
    /// No loose `ui/ui_pda.inc` to patch.
    NoLooseFile,
}

const ATTR: &str = "StickyVisible='true'";

/// Insert `StickyVisible='true'` into the WebBrowser page's CodeData.
/// Returns None when nothing needs to change (already set, or the page /
/// CodeData block could not be found).
pub fn patch_text(src: &str) -> Option<String> {
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

/// Ensure the game's loose `ui/ui_pda.inc` marks the browser window sticky.
pub fn ensure_sticky_browser(game_dir: &Path) -> Result<Outcome, String> {
    let path = game_dir.join("ui").join("ui_pda.inc");
    if !path.is_file() {
        return Ok(Outcome::NoLooseFile);
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

/// Pure-function checks, run by `swglogs --selftest`.
pub fn selfcheck(check: &mut dyn FnMut(bool, &str)) {
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
}
