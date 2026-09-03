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
    // Only a console-style invocation (any flag: --headless, --selftest,
    // --help, ...) attaches to the launching terminal. A bare launch, from a
    // double-click or a plain `swglogs.exe`, is window mode: it must not write
    // over the terminal's prompt, and a Ctrl+C there must not kill the meter
    // (Ctrl+C goes to every process attached to that console).
    if std::env::args().len() > 1 {
        app::attach_parent_console();
    }
    let cfg = app::parse_args();
    app::install_exit_after();
    if cfg.selftest {
        std::process::exit(selftest::run());
    }
    if let Some(dir) = &cfg.trace_replay {
        std::process::exit(swglogs::sources::memory::replay(dir));
    }
    if cfg.restore_ui {
        match app::restore_ui(&cfg) {
            Ok(msg) => {
                println!("[swglogs] {}", msg);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[swglogs] restore failed: {}", e);
                std::process::exit(1);
            }
        }
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
        "[Combat]  Yourname hits a crystal snake 250 pts",
        // verbose combat spam
        "[Combat]  Yourname attacks a giant baz nitch with Sweeping Fire 3 and hits for 1179 points (1084 energy and 95 cold). Armor absorbed 613 points out of 1792.",
        "[Combat]  Yourname attacks a giant baz nitch using [UA][Lvl90][Cold] T21 Rifle and hits (162 points blocked) for 454 points (359 energy and 95 cold). Armor absorbed 240 points out of 694.",
        "[Combat]  Yourname attacks a giant baz nitch with Mine 2: Plasma Mine.And hits (8% evaded) for 489 points (489 energy).  Armor absorbed 254 points out of 743.",
        "[Combat]  Yourname attacks a giant baz nitch using [UA][Lvl90][Cold] T21 Rifle and strikes through (24%) for 545 points (433 energy and 112 cold).",
        "[Combat]  A giant baz nitch attacks Yourname with Bite (4) and hits (400 points blocked) for 159 points (159 kinetic). Armor absorbed 959 points out of 1118.",
        "[Combat]  A giant baz nitch attacks Yourname and hits for 151 points (151 kinetic). Armor absorbed 259 points out of 410.",
        "[Combat]  Yourname attacks a giant baz nitch using [UA][Lvl90][Cold] T21 Rifle and misses (dodge).",
        // a damage announcement followed by its verbose hit, then a heal: the heal must NOT take the grenade's name
        "[Combat]  Yourname performs Cryoban Grenade 2.",
        "[Combat]  Yourname attacks a giant baz nitch with Cryoban Grenade 2 and hits for 100 points (100 acid).",
        "[Combat]  Yourname heals Yourname for 50 points of damage.",
        // chat timestamps on: "HH:MM:SS " before the text (also seen before the tag)
        "[Combat]  18:05:34 Yourname attacks a bull bantha and hits for 1881 points (1414 kinetic and 467 heat)",
        "18:05:35 [Combat]  A bull bantha attacks Yourname and hits for 200 points (200 kinetic).",
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
        let verbose_out = 1179 + 454 + 489 + 545 + 100;
        check(field(&snap, "Yourname", "dmg") > 0 && field(&snap, "Bull bantha", "taken") == 1881 && field(&snap, "Bull bantha", "dmg") == 200
                  && !snap.contains("18:05:3"),
              "chat timestamps: 'HH:MM:SS ' prefix stripped (before or after the tag), no time in names");
        check(field(&snap, "Yourname", "dmg") == 844 + 973 + 1302 + 581 + 337 + 500 + 250 + verbose_out + 1881,
              "Yourname damage incl. glance + DoT + post-perform hit");
        check(field(&snap, "Yourname", "crits") == 1, "Yourname crit counted");
        check(field(&snap, "Yourname", "taken") == 174 + 181 + 396 + 296 + 159 + 151 + 200,
              "Yourname taken incl. strikes-through + poison tick + verbose hits");
        check(field(&snap, "Yourname", "avoids") == 2, "Yourname misses counted (terse + verbose)");
        check(field(&snap, "Giant baz nitch", "taken") == verbose_out && field(&snap, "Giant baz nitch", "dmg") == 159 + 151,
              "verbose: giant baz nitch taken/dealt");
        check(has_ability(&snap, "Yourname", "Sweeping Fire 3") && has_ability(&snap, "Yourname", "T21 Rifle")
                  && has_ability(&snap, "Yourname", "Mine 2: Plasma Mine") && has_ability(&snap, "Giant baz nitch", "Bite (4)"),
              "verbose: abilities read off the line; weapon tags stripped");
        check(field(&snap, "Yourname", "heal") == 3465 + 50, "player heal 3515");
        check(!has_in(&snap, "Yourname", "habil", "Cryoban Grenade 2") && has_in(&snap, "Yourname", "habil", "heal")
                  && has_ability(&snap, "Yourname", "Cryoban Grenade 2"),
              "heal after a damage announcement + its hit is NOT labeled with the grenade");
        check(has_in(&snap, "Yourname", "habil", "Heal 4") && has_in(&snap, "Yourname", "htgt", "Yourname"),
              "healing split by ability ('Heal 4') and by recipient");
        check(field(&snap, "Elder mamien", "dmg") == 174, "elder mamien dmg 174 (crit+blocked)");
        check(field(&snap, "Elder mamien", "taken") == 3700, "elder mamien taken 3700");
        check(field(&snap, "Crystal snake", "dmg") == 396, "crystal snake dmg 396");
        check(field(&snap, "Crystal snake", "taken") == 337 + 500 + 250, "crystal snake taken 1087");
        check(field(&snap, "Mamien youth", "dmg") == 181, "mamien youth dmg 181");
        check(unparsed == 1, "only the unknown variant is unparsed (noise filtered)");
        check(!has_ability(&snap, "Yourname", "Killing Spree"),
              "terse hit is NOT window-labeled from 'performs' (verbose is the contract)");
        check(has_ability(&snap, "Yourname", "fire damage"), "DoT labeled 'fire damage'");
        check(!has_ability(&snap, "Yourname", "Heal 4"), "hit after 'performs Heal 4' is NOT labeled as the heal");
        check(fmt_ts(1_756_744_800.0).starts_with("2025-"), "fmt_ts civil date sane");
        {
            use swglogs::parse::{login_epoch, split_timestamp};
            check(split_timestamp("18:05:34 Yourname attacks") == (Some(18 * 3600 + 5 * 60 + 34), "Yourname attacks")
                      && split_timestamp("[6:05 PM] x") == (Some(18 * 3600 + 5 * 60), "x")
                      && split_timestamp("Yourname hits a kwi 12:30 pts") == (None, "Yourname hits a kwi 12:30 pts")
                      && split_timestamp("1200:00 x") == (None, "1200:00 x"),
                  "split_timestamp: 24h / 12h / bracketed; untouched when not a prefix");
            let e = login_epoch("Logging In [Wed Sep  2 17:16:35 2026] ", 0).unwrap_or(0);
            check(fmt_ts(e as f64) == "2026-09-02T17:16:35Z", "login_epoch parses the chatlog session marker");
            check(login_epoch("Logging In [Wed Sep  2 17:16:35 2026] ", -4 * 3600) == Some(e + 4 * 3600),
                  "login_epoch applies the local UTC offset");
        }
        swglogs::sources::memory::selfcheck(&mut check);
        swglogs::uipatch::selfcheck(&mut check);
        {
            use swglogs::event::EntityKind as K;
            let k = |line: &str, color: Option<u32>| match swglogs::parse::parse_line_colored(line, 1.0, color) {
                Parsed::Event(e) => (e.src_kind, e.tgt_kind),
                _ => (K::Unknown, K::Unknown),
            };
            check(k("[Combat]  Yourname hits an elder mamien 844 pts", None) == (K::Unknown, K::Npc),
                  "kinds: article = npc; a bare capitalized name proves nothing");
            check(k("[Combat]  A crystal snake strikes through Yourname 396 pts", None) == (K::Npc, K::Unknown),
                  "kinds: leading article -> npc");
            check(k("[Combat]  You have healed Yourname for 3465 points of damage.", None) == (K::Player, K::Unknown),
                  "kinds: You is the player");
            check(k("Axkva Min hits Yourname 500 pts", Some(0xf30f0f)).1 == K::Player
                      && k("Yourname hits Axkva Min 500 pts", Some(0x50f111)).0 == K::Player
                      && k("Kestra heals Yourname for 200 points of damage.", Some(0x1bb9c7)) == (K::Player, K::Player),
                  "kinds: red/green/blue color pins the local player");
            let ent = |name: &str| -> String {
                let key = format!("\"{}\":{{", name);
                let o = snap.find("\"overall\":").unwrap_or(0);
                let i = snap[o..].find(&key).map(|j| o + j).unwrap_or(0);
                let seg = &snap[i..];
                seg[..seg.find("}}").map(|x| x + 2).unwrap_or(seg.len())].to_string()
            };
            check(ent("Elder mamien").contains("\"kind\":\"npc\"") && ent("Yourname").contains("\"kind\":\"player\"")
                      && ent("Giant baz nitch").contains("\"kind\":\"npc\""),
                  "snapshot: entities carry a player/npc kind");
            // Title-case humanoid NPC with a named weapon: your green/red lines make it an NPC
            let mut m4 = Meter::new(8.0, None);
            let feed = |m: &mut Meter, line: &str, color: u32| {
                if let Parsed::Event(e) = swglogs::parse::parse_line_colored(line, 1.0, Some(color)) {
                    m.feed(e);
                }
            };
            feed(&mut m4, "Tusken Relic Worshiper attacks Yourname using Gaderiffi Baton and crits for 570 points (570 kinetic).", 0xf30f0f);
            feed(&mut m4, "Yourname attacks Tusken Relic Worshiper with Armor Shredder and hits for 420 points (420 energy).", 0x50f111);
            feed(&mut m4, "Kestra heals Yourname for 300 points of damage.", 0x1bb9c7);
            m4.tick(2000.0);
            let s4 = m4.snapshot_json();
            let kind_of = |name: &str| -> String {
                let key = format!("\"{}\":{{", name);
                let i = s4.find(&key).unwrap_or(0);
                let seg = &s4[i..];
                let j = seg.find("\"kind\":\"").map(|x| x + 8).unwrap_or(0);
                seg[j..].chars().take_while(|c| *c != '"').collect()
            };
            check(kind_of("Tusken Relic Worshiper") == "npc" && kind_of("Yourname") == "player" && kind_of("Kestra") == "player",
                  "kinds: Title-case NPC -> npc via your line colors; healer -> player");
            let vb = |line: &str| match swglogs::parse::parse_line(line, 1.0) {
                Parsed::Event(e) => (e.amount, e.outcome, e.ability),
                _ => (u64::MAX, swglogs::event::Outcome::Normal, String::new()),
            };
            check(vb("Yourname attacks a giant baz nitch using [UA][Lvl90][Cold] T21 Rifle and hits (162 points blocked) for 454 points (359 energy and 95 cold).")
                      == (454, swglogs::event::Outcome::Blocked, "T21 Rifle".to_string()),
                  "verbose: '(N points blocked)' -> blocked, weapon as ability");
            check(vb("Yourname attacks a giant baz nitch with Mine 2: Plasma Mine.And hits (8% evaded) for 489 points (489 energy).")
                      == (489, swglogs::event::Outcome::Evaded, "Mine 2: Plasma Mine".to_string()),
                  "verbose: '.And' after a dotted ability name, '(N% evaded)' -> evaded");
            check(vb("Yourname attacks a giant baz nitch and crits for 900 points.")
                      == (900, swglogs::event::Outcome::Critical, "attack".to_string()),
                  "verbose: no weapon clause, no damage detail -> still parses");

        }
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
        has_in(snap, entity, "abil", ability)
    }

    /// Does `entity`'s `map` object ("abil", "habil", "htgt") in "overall" hold `key`?
    fn has_in(snap: &str, entity: &str, map: &str, key: &str) -> bool {
        let ov = match snap.find("\"overall\":") {
            Some(i) => &snap[i..],
            None => return false,
        };
        let ent_key = format!("\"{}\":{{", entity);
        let start = match ov.find(&ent_key) {
            Some(i) => i,
            None => return false,
        };
        let seg = &ov[start..];
        let mk = format!("\"{}\":{{", map);
        let ms = match seg.find(&mk) {
            Some(i) => i + mk.len(),
            None => return false,
        };
        let me = seg[ms..].find('}').map(|i| ms + i).unwrap_or(seg.len());
        seg[ms..me].contains(&format!("\"{}\":", key))
    }
}
