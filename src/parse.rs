//! Chatlog / combat-spam line -> normalized `Event`. Hand-written (no regex
//! dependency). Grammar verified against a real SWG Legends chatlog (Sep 2026):
//!
//!   [Combat]  Yourname hits an elder mamien 844 pts
//!   [Combat]  Yourname hits (blocked) an elder mamien 973 pts
//!   [Combat]  Yourname crits an elder mamien 1302 pts
//!   [Combat]  Yourname glances a crystal snake 337 pts
//!   [Combat]  Yourname strikes through a crystal snake 496 pts
//!   [Combat]  Yourname has caused an elder mamien to take 581 points of fire damage. (119 absorbed)
//!   [Combat]  Yourname has taken 296 points of poison damage. (174 absorbed / 0 resisted. )
//!   [Combat]  You have healed Yourname for 3465 points of damage.
//!   [Combat]  Yourname performs Killing Spree.
//!   [Combat]  Yourname misses (dodge)
//!
//! The same function serves both sources: chatlog lines and IPC-ring combat
//! text are identical strings.

use crate::event::{Event, Kind, Outcome};

/// Outcome of a parse attempt.
pub enum Parsed {
    /// A real combat event.
    Event(Event),
    /// Recognized as non-combat/known-noise; ignore silently.
    Skip,
    /// A `[Combat]` line the grammar didn't recognize — surface at /debug.
    Unknown,
}

const HIT_VERBS: &[&str] = &["hits", "hit", "crits", "crit", "glances", "glance",
                             "strikes through", "strike through"];

/// Damage-type words seen after "points of <X> damage".
fn normalize_name(raw: &str) -> Option<String> {
    let mut n = raw.trim().trim_matches(|c| c == '.' || c == '!' || c == ',').trim();
    let low = n.to_ascii_lowercase();
    if low == "you" || low == "yourself" || low == "your" {
        return Some("You".to_string());
    }
    for art in ["a ", "an ", "the "] {
        if low.starts_with(art) {
            n = &n[art.len()..];
            break;
        }
    }
    if n.is_empty() {
        return None;
    }
    // Capitalize first letter.
    let mut c = n.chars();
    let first = c.next().unwrap();
    Some(first.to_uppercase().collect::<String>() + c.as_str())
}

/// Parse an integer with optional commas ("1,204" -> 1204). Returns the value
/// and how many bytes it consumed from the front of `s`.
fn parse_int_prefix(s: &str) -> Option<(u64, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut val: u64 = 0;
    let mut any = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_digit() {
            val = val.saturating_mul(10).saturating_add((b - b'0') as u64);
            any = true;
            i += 1;
        } else if b == b',' && any {
            i += 1;
        } else {
            break;
        }
    }
    if any {
        Some((val, i))
    } else {
        None
    }
}

/// Strip a leading channel tag. Returns (body, was_combat_channel).
/// `None` body means: skip this line.
fn strip_channel(line: &str) -> Option<(String, bool)> {
    let t = line.trim_start_matches('\u{feff}').trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let chan = rest[..end].trim().to_ascii_lowercase();
            let body = rest[end + 1..].trim().to_string();
            let is_combat = chan == "combat";
            return Some((body, is_combat));
        }
    }
    // Untagged line (e.g. from the IPC ring, already de-tagged). Treat as combat
    // body unless it's a login marker.
    if t.starts_with("Logging In") {
        return None;
    }
    Some((t.to_string(), true))
}

fn is_noise(body: &str) -> bool {
    const N: &[&str] = &[
        "maximum action", "has caused the action", "to be reduced",
        "not a valid target", "you have been", "you are no longer",
        "paused command", "you have sustained", "your poison",
        "you cannot use", "out of range", "resisted the dot",
        "no damage to heal", "chat logging", "chat log file",
        "channel", "new mail", "joined the channel", "left the channel",
    ];
    let low = body.to_ascii_lowercase();
    N.iter().any(|n| low.contains(n))
}

/// Find the first hit-verb in `body`, returning (verb, start, end) byte offsets.
/// Verbs are matched surrounded by spaces so they don't fire mid-word.
fn find_verb(body: &str) -> Option<(&'static str, usize, usize)> {
    let low = body.to_ascii_lowercase();
    let mut best: Option<(&'static str, usize, usize)> = None;
    for &v in HIT_VERBS {
        let needle = format!(" {} ", v);
        if let Some(pos) = low.find(&needle) {
            let start = pos + 1; // skip leading space
            let end = start + v.len();
            if best.map_or(true, |(_, bs, _)| start < bs) {
                best = Some((v, start, end));
            }
        }
    }
    best
}

pub fn parse_line(raw_line: &str, ts: f64) -> Parsed {
    let (body, is_combat) = match strip_channel(raw_line) {
        Some(x) => x,
        None => return Parsed::Skip,
    };
    if !is_combat {
        return Parsed::Skip; // [Chat] and friends: never combat spam
    }
    if body.is_empty() {
        return Parsed::Skip;
    }
    let ev = |kind, src, tgt, amount, outcome, ability: String| {
        Parsed::Event(Event {
            ts,
            kind,
            src,
            tgt,
            amount,
            outcome,
            ability,
            raw: body.clone(),
        })
    };
    let low = body.to_ascii_lowercase();

    // --- "<src> has caused <tgt> to take N points of <dtype> damage" (applied DoT)
    if let Some(cpos) = low.find(" has caused ") {
        if let Some(tpos) = low.find(" to take ") {
            if let Some((amt, dtype)) = parse_points_of(&body[tpos + " to take ".len()..]) {
                let src = normalize_name(&body[..cpos]);
                let tgt = normalize_name(&body[cpos + " has caused ".len()..tpos]);
                return ev(Kind::Damage, src, tgt, amt, Outcome::Normal,
                          format!("{} damage", dtype));
            }
        }
    }

    // --- "<tgt> has taken N points of <dtype> damage" (self DoT tick, no source)
    if let Some(tpos) = low.find(" has taken ").or_else(|| low.find(" have taken ")) {
        let marker = if low[tpos..].starts_with(" has taken ") { " has taken " } else { " have taken " };
        if let Some((amt, dtype)) = parse_points_of(&body[tpos + marker.len()..]) {
            let tgt = normalize_name(&body[..tpos]);
            return ev(Kind::Damage, None, tgt, amt, Outcome::Normal,
                      format!("{} damage", dtype));
        }
    }

    // --- "<src> (has healed|have healed|heals) <tgt> for N points ..."
    for marker in [" has healed ", " have healed ", " heals "] {
        if let Some(hpos) = low.find(marker) {
            let rest = &body[hpos + marker.len()..];
            let rlow = rest.to_ascii_lowercase();
            if let Some(fp) = rlow.find(" for ") {
                if let Some((amt, _)) = parse_int_prefix(rest[fp + " for ".len()..].trim_start()) {
                    let src = normalize_name(&body[..hpos]);
                    let tgt = normalize_name(&rest[..fp]);
                    return ev(Kind::Heal, src, tgt, amt, Outcome::Normal, "heal".to_string());
                }
            }
        }
    }

    // --- "<src> <verb> [(mod)] <tgt> N pts"
    if low.trim_end().ends_with("pts") {
        if let Some((verb, vstart, vend)) = find_verb(&body) {
            // amount: last integer immediately before " pts"
            if let Some((amt, tgt_str, outcome_from_verb)) =
                parse_hit_tail(&body, verb, vend)
            {
                let src = normalize_name(&body[..vstart]);
                let (tgt, outcome) = tgt_str;
                let final_outcome = outcome.unwrap_or(outcome_from_verb);
                return ev(Kind::Damage, src, normalize_name(&tgt), amt,
                          final_outcome, "attack".to_string());
            }
        }
    }

    // --- "<src> performs <ability>."
    if let Some(ppos) = low.find(" performs ") {
        let src = normalize_name(&body[..ppos]);
        let ability = body[ppos + " performs ".len()..]
            .trim()
            .trim_end_matches('.')
            .trim()
            .to_string();
        return ev(Kind::Ability, src, None, 0, Outcome::Normal, ability);
    }

    // --- "<src> misses [(mod)]"
    if let Some(mpos) = low.find(" misses") {
        let src = normalize_name(&body[..mpos]);
        let mut outcome = Outcome::Miss;
        if let Some(op) = body[mpos..].find('(') {
            if let Some(cp) = body[mpos + op..].find(')') {
                let word = &body[mpos + op + 1..mpos + op + cp];
                if let Some(o) = Outcome::from_mod(word) {
                    outcome = o;
                }
            }
        }
        return ev(Kind::Avoid, src, None, 0, outcome, String::new());
    }

    // --- deaths
    for kw in ["incapacitated", "killed", "defeated", "destroyed", "slain"] {
        if low.contains(kw)
            && (low.contains("has been") || low.contains("have been")
                || low.contains(" was ") || low.contains(" is "))
        {
            // target is the text before the state verb
            let cut = low.find("has been").or_else(|| low.find("have been"))
                .or_else(|| low.find(" was ")).or_else(|| low.find(" is "))
                .unwrap_or(body.len());
            let tgt = normalize_name(&body[..cut]);
            return ev(Kind::Death, None, tgt, 0, Outcome::Normal, String::new());
        }
    }

    if is_noise(&body) {
        return Parsed::Skip;
    }
    Parsed::Unknown
}

/// Parse "N points of <dtype> damage" from the front of `s`.
fn parse_points_of(s: &str) -> Option<(u64, String)> {
    let s = s.trim_start();
    let (amt, used) = parse_int_prefix(s)?;
    let rest = s[used..].trim_start();
    let rest = rest.strip_prefix("points of ").or_else(|| rest.strip_prefix("point of "))?;
    // dtype = first word before " damage"
    let low = rest.to_ascii_lowercase();
    let dpos = low.find(" damage")?;
    let dtype = rest[..dpos].trim().to_ascii_lowercase();
    Some((amt, dtype))
}

/// From "<verb>[ (mod)] <tgt> N pts", starting just after the verb, return
/// (amount, (tgt_string, mod_outcome), verb_outcome).
fn parse_hit_tail(
    body: &str,
    verb: &str,
    vend: usize,
) -> Option<(u64, (String, Option<Outcome>), Outcome)> {
    let verb_outcome = match verb {
        "crits" | "crit" => Outcome::Critical,
        "glances" | "glance" => Outcome::Glancing,
        "strikes through" | "strike through" => Outcome::Normal,
        _ => Outcome::Normal,
    };
    let mut rest = body[vend..].trim_start();
    // optional "(mod) "
    let mut mod_outcome = None;
    if let Some(stripped) = rest.strip_prefix('(') {
        if let Some(cp) = stripped.find(')') {
            let word = &stripped[..cp];
            mod_outcome = Outcome::from_mod(word);
            rest = stripped[cp + 1..].trim_start();
        }
    }
    // rest = "<tgt> N pts" ; strip trailing "pts"
    let low = rest.to_ascii_lowercase();
    let pts_pos = low.rfind("pts")?;
    let before_pts = rest[..pts_pos].trim_end();
    // the integer sits at the end of before_pts
    let num_start = before_pts
        .rfind(|c: char| !(c.is_ascii_digit() || c == ','))
        .map(|i| i + 1)
        .unwrap_or(0);
    let num_str = &before_pts[num_start..];
    let (amt, _) = parse_int_prefix(num_str)?;
    let tgt = before_pts[..num_start].trim().to_string();
    if tgt.is_empty() {
        return None;
    }
    Some((amt, (tgt, mod_outcome), verb_outcome))
}
