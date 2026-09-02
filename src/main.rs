//! swglogs — the one executable. Double-click: opens the status window
//! (feature `gui`, on by default). `--headless`: console only. The meter
//! itself lives in the library (`src/lib.rs`).
//!
//!   swglogs [--headless] [--source memory|chatlog|ipc|demo] [--port N] [--host H]
//!           [--gap S] [--player NAME] [--profiles DIR] [--log FILE] [--replay]
//!           [--out combat.jsonl | --no-log] [--game-dir DIR] [--no-ui-patch]
//!           [--selftest]

// No console window of our own; see app::attach_parent_console for terminals.
#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

use swglogs::app;

fn main() {
    app::attach_parent_console();
    let cfg = app::parse_args();
    app::install_exit_after();
    if cfg.selftest {
        std::process::exit(selftest::run());
    }

    let running = app::start(&cfg);

    #[cfg(feature = "gui")]
    if !cfg.headless {
        // Blocks until the window is closed; closing it stops the meter.
        if let Err(e) = swglogs::gui::run(running) {
            eprintln!("[swglogs] window error: {}", e);
            std::process::exit(1);
        }
        return;
    }
    #[cfg(not(feature = "gui"))]
    if !cfg.headless {
        eprintln!("[swglogs] built without the gui feature; running headless");
    }

    // Headless: everything runs on background threads; stay alive until the
    // server dies (e.g. the port was in use), then exit non-zero.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let Some(e) = running.server_error.lock().unwrap().as_ref() {
            eprintln!("[swglogs] exiting: {}", e);
            std::process::exit(1);
        }
    }
}

// --------------------------------------------------------------------------
mod selftest {
    use swglogs::event::fmt_ts;
    use swglogs::meter::Meter;
    use swglogs::parse::{parse_line, Parsed};

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
        swglogs::sources::memory::selfcheck(&mut check);
        swglogs::uipatch::selfcheck(&mut check);
        {
            let mut m2 = Meter::new(8.0, None);
            m2.notice = Some(swglogs::meter::Notice { text: "x".into(), client_pid: Some(1) });
            m2.expire_notice(1000.0, || Some(1));
            check(m2.notice.is_some(), "notice kept while the client pid is unchanged");
            m2.expire_notice(1010.0, || None);
            check(m2.notice.is_none(), "notice cleared once the client is gone/restarted");
            check(m2.snapshot_json().contains("\"notice\":null"), "snapshot carries notice field");
        }

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
