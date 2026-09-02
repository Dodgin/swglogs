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
//! With the client's VERBOSE combat spam on, hits carry the ability (or the
//! weapon for basic attacks), the armor numbers and a damage-type breakdown:
//!
//!   [Combat]  Yourname attacks a giant baz nitch with Sweeping Fire 3 and hits for 1179 points (1084 energy and 95 cold). Armor absorbed 613 points out of 1792.
//!   [Combat]  Yourname attacks a giant baz nitch using [UA][Lvl90][Cold] T21 Rifle and hits (162 points blocked) for 454 points (359 energy and 95 cold). Armor absorbed 240 points out of 694.
//!   [Combat]  Yourname attacks a giant baz nitch with Mine 2: Plasma Mine.And hits (8% evaded) for 489 points (489 energy).  Armor absorbed 254 points out of 743.
//!   [Combat]  Yourname attacks a giant baz nitch using [UA][Lvl90][Cold] T21 Rifle and strikes through (24%) for 545 points (433 energy and 112 cold).
//!   [Combat]  A giant baz nitch attacks Yourname with Bite (4) and hits (400 points blocked) for 159 points (159 kinetic). Armor absorbed 959 points out of 1118.
//!   [Combat]  A giant baz nitch attacks Yourname and hits for 151 points (151 kinetic). Armor absorbed 259 points out of 410.
//!   [Combat]  Yourname attacks a giant baz nitch using [UA][Lvl90][Cold] T21 Rifle and misses (dodge).
//!
//! Verbose lines need no "performs" window: the ability is right there.
//! Basic attacks are labeled with the weapon (tags like [UA][Lvl90] dropped).
//!
//! The same function serves both sources: chatlog lines and IPC-ring combat
//! text are identical strings.

use crate::event::{color_role, ColorRole, EntityKind, Event, Kind, Outcome};

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
    parse_line_colored(raw_line, ts, None)
}

/// Player or NPC, from how `name` appears in `body`: NPC names carry an
/// article ("a kwi", "an elder mamien", "the ...") or start lowercase;
/// player names are bare and capitalized. "You" is the player.
fn kind_in(body: &str, name: &str) -> EntityKind {
    if name == "You" {
        return EntityKind::Player;
    }
    let low = body.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    if n.is_empty() {
        return EntityKind::Unknown;
    }
    for (i, _) in low.match_indices(&n) {
        let pre = &low[..i];
        for art in ["a ", "an ", "the "] {
            if let Some(q) = pre.strip_suffix(art) {
                if q.is_empty() || q.ends_with(' ') || q.ends_with('(') || q.ends_with(')') {
                    return EntityKind::Npc;
                }
            }
        }
        if body[i..].chars().next().map_or(false, |c| c.is_lowercase()) {
            return EntityKind::Npc;
        }
    }
    EntityKind::Player
}

/// `parse_line` with the line's color, when the source has one; the color
/// pins which end of the line is the local player (see `event::color_role`).
pub fn parse_line_colored(raw_line: &str, ts: f64, color: Option<u32>) -> Parsed {
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
    let ev = |kind, src: Option<String>, tgt: Option<String>, amount, outcome, ability: String| {
        let mut src_kind = src.as_deref().map_or(EntityKind::Unknown, |n| kind_in(&body, n));
        let mut tgt_kind = tgt.as_deref().map_or(EntityKind::Unknown, |n| kind_in(&body, n));
        match color.map(color_role) {
            Some(ColorRole::Outgoing) if src.is_some() => src_kind = EntityKind::Player,
            Some(ColorRole::Incoming) if tgt.is_some() => tgt_kind = EntityKind::Player,
            Some(ColorRole::Heal) => {
                if src.is_some() {
                    src_kind = EntityKind::Player;
                }
                if tgt.is_some() {
                    tgt_kind = EntityKind::Player;
                }
            }
            _ => {}
        }
        Parsed::Event(Event {
            ts,
            kind,
            src,
            tgt,
            amount,
            outcome,
            ability,
            raw: body.clone(),
            src_kind,
            tgt_kind,
            color,
        })
    };
    let low = body.to_ascii_lowercase();

    // --- verbose: "<src> attacks <tgt> [with <ability>|using <weapon>] and <verb>[ (mod)] for N points (...)"
    if let Some(v) = parse_verbose(&body, &low) {
        return ev(v.kind, v.src, v.tgt, v.amount, v.outcome, v.ability);
    }

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

struct Verbose {
    kind: Kind,
    src: Option<String>,
    tgt: Option<String>,
    amount: u64,
    outcome: Outcome,
    ability: String,
}

/// Verbose combat spam. Returns None when `body` is not that shape.
fn parse_verbose(body: &str, low: &str) -> Option<Verbose> {
    let apos = low.find(" attacks ")?;
    let src = normalize_name(&body[..apos]);
    let rest = &body[apos + " attacks ".len()..];
    let rlow = &low[apos + " attacks ".len()..];

    // The verb clause: "and <verb>" — also ".And <verb>" when the ability
    // name ends in a period ("Mine 2: Plasma Mine.And hits ...").
    const VERBS: &[(&str, Outcome)] = &[
        ("strikes through", Outcome::Normal),
        ("hits", Outcome::Normal),
        ("crits", Outcome::Critical),
        ("glances", Outcome::Glancing),
        ("misses", Outcome::Miss),
    ];
    let mut best: Option<(usize, &str, Outcome)> = None;
    for &(verb, out) in VERBS {
        let needle = format!("and {}", verb);
        let mut from = 0;
        while let Some(rel) = rlow[from..].find(&needle) {
            let p = from + rel;
            let prev = rlow[..p].chars().last();
            if matches!(prev, Some(' ') | Some('.')) && best.map_or(true, |(bp, _, _)| p < bp) {
                best = Some((p, verb, out));
            }
            from = p + needle.len();
        }
    }
    let (vpos, verb, verb_outcome) = best?;

    // "<tgt>[ with <ability>| using <weapon>]"
    let head = rest[..vpos].trim().trim_end_matches('.').trim();
    let hlow = head.to_ascii_lowercase();
    let (tgt, ability) = match (hlow.find(" with "), hlow.find(" using ")) {
        (Some(w), u) if u.map_or(true, |u| w < u) => (
            &head[..w],
            head[w + " with ".len()..].trim().trim_end_matches('.').trim().to_string(),
        ),
        (_, Some(u)) => (&head[..u], strip_tags(&head[u + " using ".len()..])),
        _ => (head, "attack".to_string()),
    };
    let tgt = normalize_name(tgt);

    // "[ (mod)] for N points (...)"
    let mut tail = rest[vpos + "and ".len() + verb.len()..].trim_start();
    let mut outcome = verb_outcome;
    if let Some(stripped) = tail.strip_prefix('(') {
        if let Some(cp) = stripped.find(')') {
            let word = stripped[..cp].rsplit(' ').next().unwrap_or("");
            if let Some(o) = Outcome::from_mod(word) {
                outcome = o;
            }
            tail = stripped[cp + 1..].trim_start();
        }
    }
    if verb == "misses" {
        return Some(Verbose { kind: Kind::Avoid, src, tgt, amount: 0, outcome, ability });
    }
    let tlow = tail.to_ascii_lowercase();
    let fpos = tlow.find("for ")?;
    let (amount, _) = parse_int_prefix(tail[fpos + "for ".len()..].trim_start())?;
    Some(Verbose { kind: Kind::Damage, src, tgt, amount, outcome, ability })
}

/// "[UA][Lvl90][Cold] T21 Rifle" -> "T21 Rifle".
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let t = out.trim().trim_end_matches('.').trim();
    if t.is_empty() { "attack".to_string() } else { t.to_string() }
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
