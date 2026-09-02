//! swglogs — SWG Legends combat logs + Details-style meter.
//!
//! One normalized combat-event stream, two sinks: a live web meter and a
//! real-time structured log (JSONL). Sources are pluggable:
//!   --source chatlog  (default) tail the player's own chatlog
//!   --source ipc      drain the shared-memory IPC ring (external producer)
//!   --source demo     synthetic combat, no game needed
//!
//!   swglogs [--source chatlog|ipc|demo] [--port N] [--host H] [--gap S]
//!           [--player NAME] [--profiles DIR] [--log FILE] [--replay]
//!           [--out combat.jsonl | --no-log] [--selftest]

mod event;
mod logwriter;
mod meter;
mod parse;
mod server;
mod sources;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use logwriter::LogWriter;
use meter::Meter;
use sources::Sink;

struct Args {
    source: String,
    host: String,
    port: u16,
    gap: f64,
    player: Option<String>,
    profiles: PathBuf,
    log: Option<PathBuf>,
    out: Option<String>,
    replay: bool,
    selftest: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            source: "chatlog".into(),
            host: "127.0.0.1".into(),
            port: 8666,
            gap: 8.0,
            player: None,
            profiles: PathBuf::from(r"C:\SWG Legends\profiles"),
            log: None,
            out: Some("combat-log.jsonl".into()),
            replay: false,
            selftest: false,
        }
    }
}

fn parse_args() -> Args {
    let mut a = Args::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let next = |i: &mut usize| -> String {
        *i += 1;
        argv.get(*i).cloned().unwrap_or_default()
    };
    while i < argv.len() {
        match argv[i].as_str() {
            "--source" => a.source = next(&mut i),
            "--host" => a.host = next(&mut i),
            "--port" => a.port = next(&mut i).parse().unwrap_or(8666),
            "--gap" => a.gap = next(&mut i).parse().unwrap_or(8.0),
            "--player" => a.player = Some(next(&mut i)),
            "--profiles" => a.profiles = PathBuf::from(next(&mut i)),
            "--log" => a.log = Some(PathBuf::from(next(&mut i))),
            "--out" => a.out = Some(next(&mut i)),
            "--no-log" => a.out = None,
            "--replay" => a.replay = true,
            "--selftest" => a.selftest = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => eprintln!("[swglogs] ignoring unknown arg: {}", other),
        }
        i += 1;
    }
    a
}

fn print_help() {
    println!(
        "swglogs — SWG Legends combat logs + meter\n\n\
         --source chatlog|ipc|demo   event source (default chatlog)\n\
         --port N       HTTP port (default 8666)\n\
         --host H       bind host (default 127.0.0.1)\n\
         --gap S        seconds of silence that ends an encounter (default 8)\n\
         --player NAME  your character, highlighted (default: none)\n\
         --profiles DIR chatlog search root (default C:\\SWG Legends\\profiles)\n\
         --log FILE     tail this exact chatlog instead of auto-detecting\n\
         --replay       parse the whole existing chatlog first, then tail\n\
         --out FILE     real-time JSONL log (default combat-log.jsonl)\n\
         --no-log       disable the real-time log\n\
         --selftest     run parser/aggregator checks and exit\n\n\
         In game:  /browser http://127.0.0.1:8666/   (also /healing, /taken)"
    );
}

fn main() {
    let args = parse_args();
    // Diagnostic: SWGLOGS_EXIT_AFTER=<secs> ends the process after that long
    // (lets a timed, elevated test run stop on its own).
    if let Some(secs) = std::env::var("SWGLOGS_EXIT_AFTER").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            std::process::exit(0);
        });
    }
    if args.selftest {
        std::process::exit(selftest::run());
    }

    let meter = Arc::new(Mutex::new(Meter::new(args.gap, args.player.clone())));
    let log = match &args.out {
        Some(path) => match LogWriter::open(path) {
            Ok(w) => {
                println!("[swglogs] real-time log: {}", w.path);
                Some(Arc::new(Mutex::new(w)))
            }
            Err(e) => {
                eprintln!("[swglogs] could not open log {}: {}", path, e);
                None
            }
        },
        None => None,
    };

    let sink = Sink { meter: Arc::clone(&meter), log };

    // spawn the chosen source
    {
        let sink = sink.clone();
        let source = args.source.clone();
        let profiles = args.profiles.clone();
        let fixed = args.log.clone();
        let replay = args.replay;
        std::thread::spawn(move || match source.as_str() {
            "demo" => sources::demo(sink),
            "ipc" => sources::ipc::ipc_ring(sink),
            "memory" => sources::memory::run(sink),
            _ => sources::chatlog_tail(sink, profiles, fixed, replay),
        });
    }

    let addr = format!("{}:{}", args.host, args.port);
    println!("[swglogs] serving http://{}/   (debug: /debug)", addr);
    println!("[swglogs] in game:  /browser http://{}/", addr);
    if let Err(e) = server::serve(&addr, meter) {
        eprintln!("[swglogs] server error: {}", e);
        std::process::exit(1);
    }
}

// --------------------------------------------------------------------------
mod selftest {
    use crate::event::fmt_ts;
    use crate::meter::Meter;
    use crate::parse::{parse_line, Parsed};

    // Verbatim real chatlog lines + a couple not-yet-seen guesses.
    const LINES: &[&str] = &[
        "Logging In [Tue Sep  1 15:44:27 2026] ",
        "[Chat]  Chat logging ON",
        "[Combat]  Yourname hits an elder mamien 844 pts",
        "[Combat]  Yourname hits (blocked) an elder mamien 973 pts",
        "[Combat]  Yourname crits an elder mamien 1302 pts",
        "[Combat]  Yourname has caused an elder mamien to take 581 points of fire damage. (119 absorbed)",
        "[Chat]  You have no damage to heal.",
        "[Combat]  Yourname misses (dodge)",
        "[Combat]  An elder mamien crits (blocked) Yourname 174 pts",
        "[Combat]  An elder mamien misses",
        "[Combat]  A mamien youth hits Yourname 181 pts",
        "[Combat]  A crystal snake strikes through Yourname 396 pts",
        "[Combat]  Yourname glances a crystal snake 337 pts",
        "[Combat]  Yourname performs Killing Spree.",
        "[Combat]  Yourname hits a crystal snake 500 pts",
        "[Combat]  Yourname has taken 296 points of poison damage. (174 absorbed / 0 resisted. )",
        "[Combat]  You have sustained more poison!",
        "[Combat]  Yourname performs Heal 4.",
        "[Combat]  You have healed Yourname for 3465 points of damage.",
        "[Combat]  That target is out of range.",
        "[Combat]  Some future spam variant dealing 999 mystery hurt",
    ];

    pub fn run() -> i32 {
        let mut m = Meter::new(8.0, Some("Yourname".into()));
        let mut t = 1000.0f64;
        let mut unparsed = 0;
        for line in LINES {
            m.lines += 1;
            match parse_line(line, t) {
                Parsed::Skip => {}
                Parsed::Unknown => unparsed += 1,
                Parsed::Event(ev) => m.feed(ev),
            }
            t += 1.0;
        }
        m.tick(t + 100.0); // force-close
        let snap = m.snapshot_json();

        let mut ok = true;
        let mut check = |cond: bool, msg: &str| {
            println!("  {}  {}", if cond { "PASS" } else { "FAIL" }, msg);
            ok = ok && cond;
        };

        // Yourname dmg: 844+973+1302+581(DoT)+337(glance)+500 = 4537
        check(field(&snap, "Yourname", "dmg") == 844 + 973 + 1302 + 581 + 337 + 500,
              "Yourname damage incl. glance + DoT + post-perform hit");
        check(field(&snap, "Yourname", "crits") == 1, "Yourname crit counted");
        check(field(&snap, "Yourname", "taken") == 174 + 181 + 396 + 296,
              "Yourname taken incl. strikes-through + poison tick (1047)");
        check(field(&snap, "Yourname", "avoids") == 1, "Yourname miss counted");
        check(field(&snap, "Yourname", "heal") == 3465, "player heal 3465");
        check(field(&snap, "Elder mamien", "dmg") == 174, "elder mamien dmg 174 (crit+blocked)");
        check(field(&snap, "Elder mamien", "taken") == 3700, "elder mamien taken 3700");
        check(field(&snap, "Crystal snake", "dmg") == 396, "crystal snake dmg 396");
        check(field(&snap, "Crystal snake", "taken") == 337 + 500, "crystal snake taken 837");
        check(field(&snap, "Mamien youth", "dmg") == 181, "mamien youth dmg 181");
        check(unparsed == 1, "only the unknown variant is unparsed (noise filtered)");
        check(has_ability(&snap, "Yourname", "Killing Spree"), "hit labeled 'Killing Spree'");
        check(has_ability(&snap, "Yourname", "fire damage"), "DoT labeled 'fire damage'");
        check(fmt_ts(1_756_744_800.0).starts_with("2025-"), "fmt_ts civil date sane");
        crate::sources::memory::selfcheck(&mut check);

        println!("\nself-test {}", if ok { "PASSED" } else { "FAILED" });
        if ok { 0 } else { 1 }
    }

    // crude JSON scraping for the test (avoids a json dep): find the entity
    // object in "overall" and read an integer field.
    fn field(snap: &str, entity: &str, key: &str) -> u64 {
        let ov = match snap.find("\"overall\":") {
            Some(i) => &snap[i..],
            None => return 0,
        };
        let ent_key = format!("\"{}\":{{", entity);
        let start = match ov.find(&ent_key) {
            Some(i) => i + ent_key.len(),
            None => return 0,
        };
        let obj = &ov[start..];
        let end = obj.find('}').unwrap_or(obj.len());
        let obj = &obj[..end];
        let fk = format!("\"{}\":", key);
        if let Some(i) = obj.find(&fk) {
            let rest = &obj[i + fk.len()..];
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            return num.parse().unwrap_or(0);
        }
        0
    }

    fn has_ability(snap: &str, entity: &str, ability: &str) -> bool {
        let ov = match snap.find("\"overall\":") {
            Some(i) => &snap[i..],
            None => return false,
        };
        let ent_key = format!("\"{}\":{{", entity);
        let start = match ov.find(&ent_key) {
            Some(i) => i,
            None => return false,
        };
        // the entity object ends at the first "}}" (closes abil then entity)
        let seg = &ov[start..];
        let end = seg.find("}}").map(|i| i + 2).unwrap_or(seg.len());
        seg[..end].contains(&format!("\"{}\":", ability))
    }
}
