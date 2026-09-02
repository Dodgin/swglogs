//! Encounter aggregation. Consumes `Event`s, splits encounters on an activity
//! gap, tracks per-entity totals, and serves JSON snapshots to the web page.
//! Mirrors the proven Python meter's model.

use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{json_str, Event, Kind, Outcome};

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
    abil: BTreeMap<String, u64>,
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

    // ability labeling: src -> (ability, ts)
    last_abil: BTreeMap<String, (String, f64)>,
    abil_window: f64,

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
            abil_window: 6.0,
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
                self.last_abil.insert(src.clone(), (ev.ability.clone(), ts));
            }
            self.events += 1;
            return;
        }

        // Label a generic hit/heal with the last announced ability.
        if (ev.kind == Kind::Damage || ev.kind == Kind::Heal)
            && (ev.ability == "attack" || ev.ability == "heal")
        {
            if let Some(src) = &ev.src {
                if let Some((ab, at)) = self.last_abil.get(src) {
                    if ts - at <= self.abil_window {
                        ev.ability = ab.clone();
                    }
                }
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

fn apply(ents: &mut BTreeMap<String, Ent>, ev: &Event) {
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
                ents.entry(src.clone()).or_default().heal += ev.amount;
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
        if e.max > d.max {
            d.max = e.max;
        }
        for (ab, v) in &e.abil {
            *d.abil.entry(ab.clone()).or_default() += *v;
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

fn ent_json(e: &Ent) -> String {
    let mut abil = String::from("{");
    let mut first = true;
    for (k, v) in &e.abil {
        if !first {
            abil.push(',');
        }
        first = false;
        abil.push_str(&json_str(k));
        abil.push_str(&format!(":{}", v));
    }
    abil.push('}');
    format!(
        "{{\"dmg\":{},\"heal\":{},\"taken\":{},\"hits\":{},\"crits\":{},\
         \"max\":{},\"deaths\":{},\"avoids\":{},\"abil\":{}}}",
        e.dmg, e.heal, e.taken, e.hits, e.crits, e.max, e.deaths, e.avoids, abil
    )
}
