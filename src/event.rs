//! The normalized combat event — the contract every source produces and every
//! sink (meter aggregator, real-time log writer) consumes. Whether the source
//! is the chatlog tail or the IPC ring, it emits exactly this.

/// What happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Damage,
    Heal,
    /// "X performs Ability." — an ability announcement, used to label the
    /// generic hits/heals that follow it within a short window.
    Ability,
    /// A miss/dodge/parry/etc. by `src` (no amount).
    Avoid,
    /// `tgt` was incapacitated/killed.
    Death,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Damage => "damage",
            Kind::Heal => "heal",
            Kind::Ability => "ability",
            Kind::Avoid => "avoid",
            Kind::Death => "death",
        }
    }
}

/// The result flavor of a damage/avoid line — the context a plain DPS number
/// throws away.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Normal,
    Critical,
    Glancing,
    Blocked,
    Evaded,
    Dodged,
    Parried,
    Miss,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Normal => "normal",
            Outcome::Critical => "critical",
            Outcome::Glancing => "glancing",
            Outcome::Blocked => "blocked",
            Outcome::Evaded => "evaded",
            Outcome::Dodged => "dodged",
            Outcome::Parried => "parried",
            Outcome::Miss => "miss",
        }
    }

    /// Map a parenthetical modifier word ("blocked", "evaded", "dodge", …) to an
    /// outcome. Returns `None` for unknown words.
    pub fn from_mod(word: &str) -> Option<Outcome> {
        match word.trim().to_ascii_lowercase().as_str() {
            "blocked" | "block" => Some(Outcome::Blocked),
            "evaded" | "evade" => Some(Outcome::Evaded),
            "dodge" | "dodged" => Some(Outcome::Dodged),
            "parry" | "parried" => Some(Outcome::Parried),
            "glancing" | "glance" => Some(Outcome::Glancing),
            "critical" | "crit" => Some(Outcome::Critical),
            _ => None,
        }
    }
}

/// Player or NPC. Judged from how the name appears in the line — NPC names
/// arrive with an article or in lowercase ("a kwi", "an elder mamien"),
/// player names are bare and capitalized — and, for the local player's own
/// lines, pinned by the line's color (see [`color_role`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntityKind {
    Player,
    Npc,
    Unknown,
}

impl EntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntityKind::Player => "player",
            EntityKind::Npc => "npc",
            EntityKind::Unknown => "unknown",
        }
    }
}

/// What a scrollback line's color says about it. The client colors combat
/// spam from the local player's point of view; observed on SWG Legends:
/// green `50f111` your hits, orange `f17104` your DoT ticks / absorbed hits,
/// cyan `21e6f7` your glances, red `f30f0f` hits on you, light blue `1bb9c7`
/// heals, white ability announcements + system text, yellow `f4ef1d` NPC
/// misses, dark red `a70d0d` "out of range"-style errors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorRole {
    /// The local player dealt it: `src` is the player.
    Outgoing,
    /// The local player took it: `tgt` is the player.
    Incoming,
    /// A heal: both ends are players.
    Heal,
    Other,
}

pub fn color_role(rgb: u32) -> ColorRole {
    match rgb {
        0x50f111 | 0xf17104 | 0x21e6f7 => ColorRole::Outgoing,
        0xf30f0f => ColorRole::Incoming,
        0x1bb9c7 => ColorRole::Heal,
        _ => ColorRole::Other,
    }
}

/// One normalized combat event.
#[derive(Clone, Debug)]
pub struct Event {
    /// Unix seconds. For chatlog lines this is ingestion time (they carry no
    /// timestamp); for the IPC ring it is the producer's capture time.
    pub ts: f64,
    pub kind: Kind,
    pub src: Option<String>,
    pub tgt: Option<String>,
    pub amount: u64,
    pub outcome: Outcome,
    /// "attack", a damage type ("fire damage"), an ability name, or "heal".
    pub ability: String,
    /// The original text the event was parsed from (kept for the log's `raw`).
    pub raw: String,
    pub src_kind: EntityKind,
    pub tgt_kind: EntityKind,
    /// The line's `\#RRGGBB` color when the source carries one (memory
    /// scrollback); chatlog files have no colors.
    pub color: Option<u32>,
}

impl Event {
    /// Serialize to a single compact JSON object (one JSONL record).
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(128);
        s.push('{');
        s.push_str(&format!("\"ts\":{:.3}", self.ts));
        s.push_str(&format!(",\"iso\":{}", json_str(&fmt_ts(self.ts))));
        s.push_str(&format!(",\"kind\":{}", json_str(self.kind.as_str())));
        push_opt(&mut s, "src", &self.src);
        push_opt(&mut s, "tgt", &self.tgt);
        s.push_str(&format!(",\"amount\":{}", self.amount));
        s.push_str(&format!(",\"outcome\":{}", json_str(self.outcome.as_str())));
        s.push_str(&format!(",\"ability\":{}", json_str(&self.ability)));
        s.push_str(&format!(",\"src_kind\":{}", json_str(self.src_kind.as_str())));
        s.push_str(&format!(",\"tgt_kind\":{}", json_str(self.tgt_kind.as_str())));
        match self.color {
            Some(c) => s.push_str(&format!(",\"color\":\"{:06x}\"", c)),
            None => s.push_str(",\"color\":null"),
        }
        s.push_str(&format!(",\"raw\":{}", json_str(&self.raw)));
        s.push('}');
        s
    }
}

fn push_opt(s: &mut String, key: &str, v: &Option<String>) {
    match v {
        Some(x) => s.push_str(&format!(",\"{}\":{}", key, json_str(x))),
        None => s.push_str(&format!(",\"{}\":null", key)),
    }
}

/// JSON-encode a string (with surrounding quotes).
pub fn json_str(x: &str) -> String {
    let mut out = String::with_capacity(x.len() + 2);
    out.push('"');
    for c in x.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC), no external crates.
/// Civil-from-days per Howard Hinnant's algorithm.
pub fn fmt_ts(secs: f64) -> String {
    let s = secs as i64;
    let days = s.div_euclid(86_400);
    let sod = s.rem_euclid(86_400);
    let (h, mi, se) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, h, mi, se
    )
}
