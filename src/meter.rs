//! Encounter aggregation. Consumes `Event`s, splits encounters on an activity
//! gap, tracks per-entity totals, and serves JSON snapshots to the web page.
//! Mirrors the proven Python meter's model.

use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{json_str, Event, Kind, Outcome};
use crate::event::EntityKind;

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Default, Clone)]
struct Ent {
    dmg: u64,
    heal: u64,
    taken: u64,
    hits: u64,
    crits: u64,
    max: u64,
    deaths: u64,
    avoids: u64,
    /// damage dealt, by ability
    abil: BTreeMap<String, u64>,
    /// healing done, by ability and by recipient
    habil: BTreeMap<String, u64>,
    htgt: BTreeMap<String, u64>,
    /// player / npc votes from the lines this entity appeared in
    pv: u32,
    nv: u32,
}

#[derive(Default, Clone)]
struct Encounter {
    start: f64,
    end: f64,
    entities: BTreeMap<String, Ent>,
}

impl Encounter {
    fn dur(&self) -> f64 {
        (self.end - self.start).max(1.0)
    }
}

/// A one-line message shown at the top of the meter page (and on /debug).
/// While `client_pid` is set, the notice is dropped automatically once that
/// game client process is gone — i.e. the player has restarted the client.
pub struct Notice {
    pub text: String,
    pub client_pid: Option<u32>,
}

pub struct Meter {
    gap: f64,
    pub player: Option<String>,
    current: Option<Encounter>,
    last: Option<Encounter>,
    overall: Encounter,
    overall_dur: f64,
    encounter_count: u64,

    // ability labeling: src -> (ability, announced-at, still fresh). "Fresh"
    // means no other line from that source has arrived since the "performs"
    // — the heal that belongs to a heal announcement is the very next line.
    last_abil: BTreeMap<String, (String, f64, bool)>,
    /// What each announced ability turned out to be (damage or heal), learned
    /// from the first line it labeled — so "Heal 4" never labels a hit and
    /// "Killing Spree" never labels a heal.
    abil_kind: BTreeMap<String, Kind>,
    /// Damage lines seen in verbose ("attacks ... for N points") vs terse
    /// ("hits X N pts") form — shown on /debug. Verbose is the contract:
    /// terse lines carry no ability, so they land in the "attack" bucket.
    verbose_hits: u64,
    terse_hits: u64,

    // stats / debug
    pub lines: u64,
    pub events: u64,
    pub log_path: String,
    pub last_write: Option<f64>,
    unparsed: VecDeque<String>,
    pub notice: Option<Notice>,
    notice_checked: f64,
}

impl Meter {
    pub fn new(gap: f64, player: Option<String>) -> Self {
        Meter {
            gap,
            player,
            current: None,
            last: None,
            overall: Encounter::default(),
            overall_dur: 0.0,
            encounter_count: 0,
            last_abil: BTreeMap::new(),
            abil_kind: BTreeMap::new(),
            verbose_hits: 0,
            terse_hits: 0,
            lines: 0,
            events: 0,
            log_path: String::new(),
            last_write: None,
            unparsed: VecDeque::new(),
            notice: None,
            notice_checked: 0.0,
        }
    }

    pub fn note_unparsed(&mut self, line: &str) {
        if self.unparsed.len() >= 200 {
            self.unparsed.pop_front();
        }
        self.unparsed.push_back(line.to_string());
    }

    pub fn feed(&mut self, mut ev: Event) {
        // Normalize the player's alias: the log uses "You" in some lines and the
        // character name in others.
        if let Some(p) = &self.player {
            if ev.src.as_deref() == Some("You") {
                ev.src = Some(p.clone());
            }
            if ev.tgt.as_deref() == Some("You") {
                ev.tgt = Some(p.clone());
            }
        }
        let ts = ev.ts;

        if ev.kind == Kind::Ability {
            if let Some(src) = &ev.src {
                self.last_abil.insert(src.clone(), (ev.ability.clone(), ts, true));
            }
            self.events += 1;
            return;
        }

        // Verbose damage lines name their ability: remember it is a damage
        // ability so a heal announcement window can never adopt it.
        if ev.kind == Kind::Damage && ev.ability != "attack" && ev.raw.contains(" attacks ") {
            self.abil_kind.entry(ev.ability.clone()).or_insert(Kind::Damage);
        }

        if ev.kind == Kind::Damage {
            if ev.raw.contains(" attacks ") {
                self.verbose_hits += 1;
            } else if ev.raw.trim_end().ends_with("pts") {
                self.terse_hits += 1;
            }
        }

        // Label a generic heal with the last announced ability ("performs
        // Heal 4" then "X heals Y for N"). Damage is never labeled this way:
        // verbose combat spam names the ability on the hit line itself, and a
        // heal announcement must not label the auto-attacks that land in the
        // same few seconds.
        if ev.kind == Kind::Heal && ev.ability == "heal" {
            if let Some(src) = &ev.src {
                if let Some((ab, at, fresh)) = self.last_abil.get(src) {
                    let known = self.abil_kind.get(ab).copied().or_else(|| abil_kind_hint(ab));
                    if *fresh && ts - at <= 3.0 && known.map_or(true, |k| k == ev.kind) {
                        self.abil_kind.entry(ab.clone()).or_insert(ev.kind);
                        ev.ability = ab.clone();
                    }
                }
            }
        }

        if let Some(src) = &ev.src {
            if let Some(e) = self.last_abil.get_mut(src) {
                e.2 = false;
            }
        }

        if let Some(cur) = &self.current {
            if ts - cur.end > self.gap {
                self.close();
            }
        }
        if self.current.is_none() {
            self.current = Some(Encounter { start: ts, end: ts, entities: BTreeMap::new() });
            self.encounter_count += 1;
        }
        let cur = self.current.as_mut().unwrap();
        if ts > cur.end {
            cur.end = ts;
        }
        apply(&mut cur.entities, &ev);
        self.events += 1;
    }

    fn close(&mut self) {
        if let Some(c) = self.current.take() {
            if c.entities.is_empty() {
                return;
            }
            self.overall_dur += c.dur();
            merge(&mut self.overall.entities, &c.entities);
            self.last = Some(c);
        }
    }

    /// Close the open encounter after `gap` seconds of silence.
    pub fn tick(&mut self, now: f64) {
        if let Some(cur) = &self.current {
            if now - cur.end > self.gap {
                self.close();
            }
        }
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.last = None;
        self.overall = Encounter::default();
        self.overall_dur = 0.0;
        self.encounter_count = 0;
        self.events = 0;
        self.last_abil.clear();
    }

    // ---- JSON output ----

    pub fn snapshot_json(&self) -> String {
        let cur = self.current.as_ref().map(enc_json);
        let last = self.last.as_ref().map(enc_json);

        // overall = stored overall + open current
        let mut ov_ents = self.overall.entities.clone();
        let mut ov_dur = self.overall_dur;
        if let Some(c) = &self.current {
            merge(&mut ov_ents, &c.entities);
            ov_dur += c.dur();
        }
        let overall = if ov_ents.is_empty() {
            "null".to_string()
        } else {
            entities_enc_json(ov_dur.max(1.0), 0.0, &ov_ents)
        };

        let stale = match self.last_write {
            Some(w) => format!("{:.1}", now_secs() - w),
            None => "null".to_string(),
        };

        format!(
            "{{\"current\":{},\"last\":{},\"overall\":{},\"meta\":{{\
             \"player\":{},\"stale\":{},\"encounters\":{},\"events\":{},\
             \"lines\":{},\"unparsed\":{},\"log\":{},\"notice\":{},\"now\":{:.3}}}}}",
            cur.unwrap_or_else(|| "null".to_string()),
            last.unwrap_or_else(|| "null".to_string()),
            overall,
            self.player.as_deref().map(json_str).unwrap_or_else(|| "null".to_string()),
            stale,
            self.encounter_count,
            self.events,
            self.lines,
            self.unparsed.len(),
            json_str(&self.log_path),
            self.notice.as_ref().map(|n| json_str(&n.text)).unwrap_or_else(|| "null".to_string()),
            now_secs(),
        )
    }

    /// Drop a client-restart notice once the client process it was raised
    /// for no longer exists. `client_pid` is polled at most every 5 s.
    pub fn expire_notice(&mut self, now: f64, client_pid: impl Fn() -> Option<u32>) {
        let pid = match &self.notice {
            Some(Notice { client_pid: Some(p), .. }) => *p,
            _ => return,
        };
        if now - self.notice_checked < 5.0 {
            return;
        }
        self.notice_checked = now;
        if client_pid() != Some(pid) {
            self.notice = None;
        }
    }

    pub fn debug_text(&self) -> String {
        let mut s = format!(
            "swglogs debug\nsource/log: {}\nlines read: {}  events: {}  \
             encounters: {}  unparsed [Combat] lines: {}\n",
            self.log_path, self.lines, self.events, self.encounter_count,
            self.unparsed.len()
        );
        if let Some(n) = &self.notice {
            s.push_str("notice: ");
            s.push_str(&n.text);
            s.push('\n');
        }
        s.push_str(&format!("damage lines: {} verbose, {} terse\n", self.verbose_hits, self.terse_hits));
        if self.unparsed.is_empty() {
            s.push_str("\nNo unparsed combat lines yet.\n");
        } else {
            s.push_str("\nUnparsed lines (extend parse.rs from these):\n");
            for l in &self.unparsed {
                s.push_str("  ");
                s.push_str(l);
                s.push('\n');
            }
        }
        s
    }
}

/// Kind of an announced ability from its name alone, before any line has
/// told us: the obvious heal names never label damage.
fn abil_kind_hint(ability: &str) -> Option<Kind> {
    let a = ability.to_ascii_lowercase();
    if ["heal", "bacta", "cure", "mend", "revive", "stim", "rejuven", "restor"].iter().any(|w| a.contains(w)) {
        Some(Kind::Heal)
    } else {
        None
    }
}

/// Certain evidence (article / lowercase name, "You", the color-pinned end of
/// your own lines, a heal) weighs 3; the "other end of your own line is an
/// enemy" inference weighs 1, so a few certain lines always outvote it.
const STRONG: u32 = 3;
const WEAK: u32 = 1;

fn vote(ents: &mut BTreeMap<String, Ent>, name: &Option<String>, kind: EntityKind, w: u32) {
    if let Some(n) = name {
        let e = ents.entry(n.clone()).or_default();
        match kind {
            EntityKind::Player => e.pv += w,
            EntityKind::Npc => e.nv += w,
            EntityKind::Unknown => {}
        }
    }
}

fn apply(ents: &mut BTreeMap<String, Ent>, ev: &Event) {
    if ev.kind != Kind::Ability {
        vote(ents, &ev.src, ev.src_kind, STRONG);
        vote(ents, &ev.tgt, ev.tgt_kind, STRONG);
        // The other end of one of YOUR lines is your enemy: an NPC, in PvE.
        // Weak, so an entity that ever shows player behaviour (heals, gets
        // healed, is "You") still ends up a player.
        if ev.kind == Kind::Damage || ev.kind == Kind::Avoid {
            match ev.color.map(crate::event::color_role) {
                Some(crate::event::ColorRole::Outgoing) if ev.tgt_kind == EntityKind::Unknown => {
                    vote(ents, &ev.tgt, EntityKind::Npc, WEAK)
                }
                Some(crate::event::ColorRole::Incoming) if ev.src_kind == EntityKind::Unknown => {
                    vote(ents, &ev.src, EntityKind::Npc, WEAK)
                }
                _ => {}
            }
        }
    }
    match ev.kind {
        Kind::Damage => {
            if let Some(src) = &ev.src {
                let e = ents.entry(src.clone()).or_default();
                e.dmg += ev.amount;
                e.hits += 1;
                if ev.outcome == Outcome::Critical {
                    e.crits += 1;
                }
                if ev.amount > e.max {
                    e.max = ev.amount;
                }
                let ab = if ev.ability.is_empty() { "attack" } else { &ev.ability };
                *e.abil.entry(ab.to_string()).or_default() += ev.amount;
            }
            if let Some(tgt) = &ev.tgt {
                ents.entry(tgt.clone()).or_default().taken += ev.amount;
            }
        }
        Kind::Heal => {
            if let Some(src) = &ev.src {
                let e = ents.entry(src.clone()).or_default();
                e.heal += ev.amount;
                let ab = if ev.ability.is_empty() { "heal" } else { &ev.ability };
                *e.habil.entry(ab.to_string()).or_default() += ev.amount;
                let to = ev.tgt.clone().unwrap_or_else(|| src.clone());
                *e.htgt.entry(to).or_default() += ev.amount;
            }
        }
        Kind::Death => {
            if let Some(tgt) = &ev.tgt {
                ents.entry(tgt.clone()).or_default().deaths += 1;
            }
        }
        Kind::Avoid => {
            if let Some(src) = &ev.src {
                ents.entry(src.clone()).or_default().avoids += 1;
            }
        }
        Kind::Ability => {}
    }
}

fn merge(dst: &mut BTreeMap<String, Ent>, src: &BTreeMap<String, Ent>) {
    for (name, e) in src {
        let d = dst.entry(name.clone()).or_default();
        d.dmg += e.dmg;
        d.heal += e.heal;
        d.taken += e.taken;
        d.hits += e.hits;
        d.crits += e.crits;
        d.deaths += e.deaths;
        d.avoids += e.avoids;
        d.pv += e.pv;
        d.nv += e.nv;
        if e.max > d.max {
            d.max = e.max;
        }
        for (ab, v) in &e.abil {
            *d.abil.entry(ab.clone()).or_default() += *v;
        }
        for (ab, v) in &e.habil {
            *d.habil.entry(ab.clone()).or_default() += *v;
        }
        for (t, v) in &e.htgt {
            *d.htgt.entry(t.clone()).or_default() += *v;
        }
    }
}

fn enc_json(c: &Encounter) -> String {
    entities_enc_json(c.dur(), c.start, &c.entities)
}

fn entities_enc_json(dur: f64, start: f64, ents: &BTreeMap<String, Ent>) -> String {
    let mut s = format!("{{\"dur\":{:.1},\"start\":{:.3},\"entities\":{{", dur, start);
    let mut first = true;
    for (name, e) in ents {
        if !first {
            s.push(',');
        }
        first = false;
        s.push_str(&json_str(name));
        s.push(':');
        s.push_str(&ent_json(e));
    }
    s.push_str("}}");
    s
}

fn map_json(m: &BTreeMap<String, u64>) -> String {
    let mut s = String::from("{");
    let mut first = true;
    for (k, v) in m {
        if !first {
            s.push(',');
        }
        first = false;
        s.push_str(&json_str(k));
        s.push_str(&format!(":{}", v));
    }
    s.push('}');
    s
}

fn ent_json(e: &Ent) -> String {
    let abil = map_json(&e.abil);
    let kind = if e.pv > e.nv {
        "player"
    } else if e.nv > e.pv {
        "npc"
    } else {
        "unknown"
    };
    format!(
        "{{\"dmg\":{},\"heal\":{},\"taken\":{},\"hits\":{},\"crits\":{},\
         \"max\":{},\"deaths\":{},\"avoids\":{},\"kind\":\"{}\",\"abil\":{},\"habil\":{},\"htgt\":{}}}",
        e.dmg, e.heal, e.taken, e.hits, e.crits, e.max, e.deaths, e.avoids, kind, abil,
        map_json(&e.habil), map_json(&e.htgt)
    )
}
