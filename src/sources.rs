//! Event sources. All produce the same `Event`s into a `Sink`, so the meter and
//! log don't care where combat came from:
//!   * `chatlog_tail` — tails the player's own chatlog (works today).
//!   * `ipc_ring`     — drains combat spam from a shared-memory ring fed by an
//!                      external producer (zero flush latency).
//!   * `demo`         — synthetic combat, no game needed.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::event::{EntityKind, Event, Kind, Outcome};
use crate::logwriter::LogWriter;
use crate::meter::{now_secs, Meter};
use crate::parse::{parse_line_colored, Parsed};

/// Fan-out target: parse+meter+log. Cloneable handle over shared state.
#[derive(Clone)]
pub struct Sink {
    pub meter: Arc<Mutex<Meter>>,
    pub log: Option<Arc<Mutex<LogWriter>>>,
}

impl Sink {
    /// Ingest one raw text line (chatlog or IPC combat text).
    pub fn line(&self, raw: &str, ts: f64) {
        self.line_colored(raw, None, ts);
    }

    /// Ingest one raw text line together with its `\#RRGGBB` color (memory
    /// scrollback), which pins which end of the line is the local player.
    pub fn line_colored(&self, raw: &str, color: Option<u32>, ts: f64) {
        let parsed = parse_line_colored(raw, ts, color);
        {
            let mut m = self.meter.lock().unwrap();
            m.lines += 1;
            match &parsed {
                Parsed::Skip => return,
                Parsed::Unknown => {
                    m.note_unparsed(raw.trim());
                    return;
                }
                Parsed::Event(_) => {}
            }
        }
        if let Parsed::Event(ev) = parsed {
            self.event(ev);
        }
    }

    /// Ingest an already-structured event (e.g. a future structured IPC payload).
    pub fn event(&self, ev: Event) {
        if let Some(l) = &self.log {
            l.lock().unwrap().write(&ev);
        }
        self.meter.lock().unwrap().feed(ev);
    }

    pub fn mark_fresh(&self, ts: f64) {
        self.meter.lock().unwrap().last_write = Some(ts);
    }

    pub fn set_log_label(&self, label: &str) {
        self.meter.lock().unwrap().log_path = label.to_string();
    }
}

// --------------------------------------------------------------------------
// chatlog tail
// --------------------------------------------------------------------------

fn newest_chatlog(profiles: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    // profiles/<account>/<galaxy>/<id>_chatlog.txt
    for acct in read_dirs(profiles) {
        for gal in read_dirs(&acct) {
            if let Ok(rd) = fs::read_dir(&gal) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.file_name()
                        .and_then(|n| n.to_str())
                        .map_or(false, |n| n.ends_with("_chatlog.txt"))
                    {
                        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                            if best.as_ref().map_or(true, |(bt, _)| m > *bt) {
                                best = Some((m, p));
                            }
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

fn read_dirs(p: &Path) -> Vec<PathBuf> {
    fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

/// Tail the newest chatlog (or a fixed file), forever. If `replay`, parse the
/// existing file from the top on first open; otherwise start at the end.
pub fn chatlog_tail(sink: Sink, profiles: PathBuf, fixed: Option<PathBuf>, replay: bool) {
    let mut path: Option<PathBuf> = None;
    let mut file: Option<fs::File> = None;
    let mut first = true;
    let mut carry = String::new();

    loop {
        {
            let mut m = sink.meter.lock().unwrap();
            m.tick(now_secs());
        }
        let np = fixed.clone().or_else(|| newest_chatlog(&profiles));
        if let Some(np) = np {
            if path.as_ref() != Some(&np) {
                path = Some(np.clone());
                sink.set_log_label(&np.display().to_string());
                if let Ok(mut f) = fs::File::open(&np) {
                    if !(first && replay) {
                        let _ = f.seek(SeekFrom::End(0));
                    }
                    file = Some(f);
                    carry.clear();
                }
                first = false;
            }
        }
        if let Some(f) = file.as_mut() {
            // handle truncation/rotation
            if let (Ok(meta), Ok(pos)) = (f.metadata(), f.stream_position()) {
                if meta.len() < pos {
                    let _ = f.seek(SeekFrom::Start(0));
                    carry.clear();
                }
            }
            let mut chunk = String::new();
            if f.read_to_string(&mut chunk).is_ok() && !chunk.is_empty() {
                carry.push_str(&chunk);
                let ts = now_secs();
                let mut consumed = 0;
                while let Some(nl) = carry[consumed..].find('\n') {
                    let line = carry[consumed..consumed + nl].to_string();
                    consumed += nl + 1;
                    sink.line(&line, ts);
                }
                carry.drain(..consumed);
                sink.mark_fresh(ts);
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

// --------------------------------------------------------------------------
// demo
// --------------------------------------------------------------------------

pub fn demo(sink: Sink) {
    sink.set_log_label("(demo mode)");
    let party = ["You", "Vexa", "Kestra", "Rush-9", "Talon"];
    let abils = ["Snipe", "Overcharge Shot", "Strike", "Bleed", "Burst"];
    let mut seed: u64 = 0x1234_5678;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    loop {
        let ts = now_secs();
        sink.mark_fresh(ts);
        for n in party {
            if rng() % 10 < 8 {
                let base = 80 + rng() % 820;
                let amt = if rng() % 100 < 12 { base * 3 } else { base };
                let crit = rng() % 100 < 15;
                sink.event(Event {
                    ts,
                    kind: Kind::Damage,
                    src: Some(n.to_string()),
                    tgt: Some("Rancor".to_string()),
                    amount: amt,
                    outcome: if crit { Outcome::Critical } else { Outcome::Normal },
                    ability: abils[(rng() as usize) % abils.len()].to_string(),
                    raw: String::new(),
                    src_kind: EntityKind::Player,
                    tgt_kind: EntityKind::Npc,
                    color: None,
                });
            }
        }
        if rng() % 2 == 0 {
            sink.event(Event {
                ts,
                kind: Kind::Heal,
                src: Some("Kestra".to_string()),
                tgt: Some(party[(rng() as usize) % party.len()].to_string()),
                amount: 100 + rng() % 400,
                outcome: Outcome::Normal,
                ability: "Bacta Burst".to_string(),
                raw: String::new(),
                src_kind: EntityKind::Player,
                tgt_kind: EntityKind::Player,
                color: None,
            });
        }
        sink.event(Event {
            ts,
            kind: Kind::Damage,
            src: Some("Rancor".to_string()),
            tgt: Some(party[(rng() as usize) % party.len()].to_string()),
            amount: 50 + rng() % 350,
            outcome: Outcome::Normal,
            ability: "Bite".to_string(),
            raw: String::new(),
            src_kind: EntityKind::Npc,
            tgt_kind: EntityKind::Player,
            color: None,
        });
        // Occasionally go quiet longer than the encounter gap, so the "Now"
        // encounter closes and rolls into "Last" — mimics a fight ending.
        if rng() % 100 < 8 {
            std::thread::sleep(Duration::from_millis(11_000));
        } else {
            std::thread::sleep(Duration::from_millis(700));
        }
    }
}

// --------------------------------------------------------------------------
// IPC ring (shared-memory ring fed by an external producer)
// --------------------------------------------------------------------------
//
// Mirrors the producer's shared-memory contract. The producer is the sole
// writer; we drain EV_COMBAT_SPAM
// records, whose payload is the UTF-8 combat line, and feed them through the
// same parser as the chatlog. Zero disk-flush latency.

#[cfg(windows)]
pub mod ipc {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ---- ring layout constants (must match the producer) ----
    const SHM_NAME: &[u8] = b"Local\\swglogs_shm_v1\0";
    const ABI_VERSION: u32 = 1;
    const RING_DATA_OFFSET: usize = 4096;
    const RING_CAPACITY: u32 = 0x40000;
    const RING_MASK: u32 = RING_CAPACITY - 1;
    const ABI_MAGIC: u32 = 0x53_57_47_4C; // "SWGL"
    // Header field byte offsets (see struct Header):
    const OFF_MAGIC: usize = 0;
    const OFF_ABI: usize = 4;
    const OFF_RING_WRITE: usize = 24;
    const OFF_RING_READ: usize = 28;
    const EV_COMBAT_SPAM: u8 = 3;

    const FILE_MAP_READ: u32 = 0x0004;
    const FILE_MAP_WRITE: u32 = 0x0002;

    type Handle = *mut core::ffi::c_void;
    extern "system" {
        fn OpenFileMappingA(access: u32, inherit: i32, name: *const u8) -> Handle;
        fn MapViewOfFile(h: Handle, access: u32, hi: u32, lo: u32, len: usize) -> *mut core::ffi::c_void;
        fn UnmapViewOfFile(p: *const core::ffi::c_void) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }

    struct Mapping {
        handle: Handle,
        base: *mut u8,
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe {
                if !self.base.is_null() {
                    UnmapViewOfFile(self.base as *const _);
                }
                if !self.handle.is_null() {
                    CloseHandle(self.handle);
                }
            }
        }
    }

    unsafe fn at(base: *mut u8, off: usize) -> &'static AtomicU32 {
        &*(base.add(off) as *const AtomicU32)
    }
    unsafe fn data_ptr(base: *mut u8, pos: u32) -> *mut u8 {
        base.add(RING_DATA_OFFSET + (pos & RING_MASK) as usize)
    }

    fn open() -> Option<Mapping> {
        unsafe {
            let h = OpenFileMappingA(FILE_MAP_READ | FILE_MAP_WRITE, 0, SHM_NAME.as_ptr());
            if h.is_null() {
                return None;
            }
            let base = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0) as *mut u8;
            if base.is_null() {
                CloseHandle(h);
                return None;
            }
            let m = Mapping { handle: h, base };
            if at(base, OFF_MAGIC).load(Ordering::Acquire) != ABI_MAGIC
                || at(base, OFF_ABI).load(Ordering::Relaxed) != ABI_VERSION
            {
                return None; // producer not ready / version mismatch
            }
            Some(m)
        }
    }

    /// Drain available records; returns how many combat lines were fed.
    unsafe fn drain(map: &Mapping, sink: &Sink) -> u32 {
        let base = map.base;
        let mut fed = 0u32;
        loop {
            let r = at(base, OFF_RING_READ).load(Ordering::Relaxed);
            let w = at(base, OFF_RING_WRITE).load(Ordering::Acquire);
            if r == w {
                break;
            }
            let kind = *data_ptr(base, r);
            let len = (*data_ptr(base, r + 2) as u16) | ((*data_ptr(base, r + 3) as u16) << 8);
            let mut payload = Vec::with_capacity(len as usize);
            for i in 0..len as u32 {
                payload.push(*data_ptr(base, r + 4 + i));
            }
            at(base, OFF_RING_READ).store(r.wrapping_add(4 + len as u32), Ordering::Release);
            if kind == EV_COMBAT_SPAM {
                if let Ok(text) = String::from_utf8(payload) {
                    sink.line(&text, now_secs());
                    fed += 1;
                }
            }
        }
        fed
    }

    /// Poll the ring forever, reconnecting if the client or producer restarts.
    pub fn ipc_ring(sink: Sink) {
        sink.set_log_label("(IPC ring — waiting for producer)");
        loop {
            match open() {
                Some(map) => {
                    sink.set_log_label("(IPC ring — connected)");
                    loop {
                        {
                            let mut m = sink.meter.lock().unwrap();
                            m.tick(now_secs());
                        }
                        let fed = unsafe { drain(&map, &sink) };
                        if fed > 0 {
                            sink.mark_fresh(now_secs());
                        }
                        // If the producer went away, magic clears — bail to reconnect.
                        let alive = unsafe {
                            at(map.base, OFF_MAGIC).load(Ordering::Acquire) == ABI_MAGIC
                        };
                        if !alive {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                None => std::thread::sleep(Duration::from_millis(1000)),
            }
        }
    }
}

#[cfg(not(windows))]
pub mod ipc {
    use super::Sink;
    pub fn ipc_ring(_sink: Sink) {
        eprintln!("[swglogs] IPC source is Windows-only");
    }
}

// --------------------------------------------------------------------------
// memory (external read-only reader — no injection)
// --------------------------------------------------------------------------
//
// Opens SwgClient_r.exe and reads its combat chat-scrollback directly via
// ReadProcessMemory. The client runs elevated, so run swglogs as Admin. The
// scrollback is one contiguous UTF-16 buffer, lines separated by `\n` with
// inline SWG `\#RRGGBB`/`\#.` color codes; combat lines match a strict grammar
// (name + hits/crits/glances/strikes/heals + "N pts"), which cleanly rejects
// UI/table junk that merely contains " pts". We locate the buffer ONCE (a
// one-time grammar scan), then each tick read only that region and emit new
// lines — never a per-tick wide scan.

#[cfg(windows)]
pub mod memory {
    use super::*;
    use core::ffi::c_void;

    type Handle = *mut c_void;
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
    const PROCESS_VM_READ: u32 = 0x0010;

    #[repr(C)]
    struct ProcessEntry32 {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u8; 260],
    }

    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32First(snap: Handle, entry: *mut ProcessEntry32) -> i32;
        fn Process32Next(snap: Handle, entry: *mut ProcessEntry32) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn ReadProcessMemory(h: Handle, addr: usize, buf: *mut u8, n: usize, read: *mut usize) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }

    /// PID of the running game client, if any (used to notice a restart).
    pub fn client_pid() -> Option<u32> {
        find_pid()
    }

    fn find_pid() -> Option<u32> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap.is_null() {
                return None;
            }
            let mut e: ProcessEntry32 = core::mem::zeroed();
            e.dw_size = core::mem::size_of::<ProcessEntry32>() as u32;
            let mut ok = Process32First(snap, &mut e);
            let mut found = None;
            while ok != 0 {
                let name = &e.sz_exe_file;
                let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                let s = String::from_utf8_lossy(&name[..end]).to_ascii_lowercase();
                if s.starts_with("swgclient") {
                    found = Some(e.th32_process_id);
                    break;
                }
                ok = Process32Next(snap, &mut e);
            }
            CloseHandle(snap);
            found
        }
    }

    struct Proc(Handle);
    impl Drop for Proc {
        fn drop(&mut self) {
            unsafe {
                if !self.0.is_null() {
                    CloseHandle(self.0);
                }
            }
        }
    }
    impl Proc {
        fn read(&self, addr: usize, buf: &mut [u8]) -> usize {
            let mut got = 0usize;
            let ok = unsafe {
                ReadProcessMemory(self.0, addr, buf.as_mut_ptr(), buf.len(), &mut got)
            };
            if ok != 0 { got } else { 0 }
        }
    }

    /// Cheap sanity filter shared by both acceptors: plausible length and
    /// almost entirely clean printable text (rejects heap junk).
    fn looks_clean(l: &str) -> bool {
        if !(8 < l.len() && l.len() < 400) {
            return false;
        }
        let clean = l.chars().filter(|c| c.is_ascii_alphanumeric() || " '()-.,:/".contains(*c)).count();
        clean * 10 >= l.len() * 9
    }

    /// Strict acceptor used only to LOCATE the scrollback: a hit/DoT/heal line
    /// ("name + verb + ... + N pts" or "points of"). These are by far the
    /// densest lines in a combat buffer, so they make the best density signal.
    fn grammar_ok(l: &str) -> bool {
        if !looks_clean(l) {
            return false;
        }
        let has_verb = [" hits", " crits", " glances", " strikes", " heals", " healed ", "has caused", "has taken", "have taken", " attacks "]
            .iter().any(|v| l.contains(v));
        let ends = l.trim_end().ends_with("pts")
            && l.rsplit(' ').nth(1).map_or(false, |w| w.chars().all(|c| c.is_ascii_digit()));
        has_verb && (ends || l.contains("points of") || l.contains(" points ("))
    }

    /// Wide acceptor used while TAILING: everything `parse.rs` understands —
    /// hits, DoTs, heals, misses/avoids, ability announcements, deaths. The
    /// strict grammar alone silently dropped every miss and every "performs"
    /// line, so this source never counted avoids or labeled abilities.
    fn combat_like(l: &str) -> bool {
        if grammar_ok(l) {
            return true;
        }
        if !looks_clean(l) {
            return false;
        }
        if l.contains(" performs ") || l.contains(" misses") {
            return true;
        }
        if l.contains(" attacks ") && (l.contains(" for ") || l.contains("nd misses")) {
            return true;
        }
        let low = l.to_ascii_lowercase();
        ["incapacitated", "killed", "defeated", "destroyed", "slain"].iter().any(|k| low.contains(k))
            && ["has been", "have been", " was ", " is "].iter().any(|k| low.contains(k))
    }

    /// Truncate a line at its natural combat terminator, dropping any trailing
    /// buffer junk after it (a shorter line written over a longer stale one
    /// leaves the old tail in place, e.g. "...181 ptsbse)2 pts50or#..."). Hit
    /// lines end at the FIRST "<digits> pts" (the stale tail can itself contain
    /// a later " pts"); DoT/heal lines end at "damage"; performs/deaths at the
    /// first period; misses at the closing paren.
    fn trim_combat(l: &str) -> &str {
        // verbose: keep through the damage breakdown ")" (or the miss's "."),
        // dropping "Armor absorbed ..." and any stale tail.
        if let Some(a) = l.find(" attacks ") {
            // amount = "for <digits> points", optionally followed by the
            // "(<breakdown>)" when Show Damage Detail is on
            let mut from = a;
            while let Some(rel) = l[from..].find(" for ") {
                let p = from + rel + " for ".len();
                let digits = l[p..].chars().take_while(|c| c.is_ascii_digit() || *c == ',').count();
                if digits > 0 && l[p + digits..].starts_with(" points") {
                    let mut end = p + digits + " points".len();
                    if l[end..].starts_with(" (") {
                        if let Some(c) = l[end..].find(')') {
                            end += c + 1;
                        }
                    }
                    return &l[..end];
                }
                from = p;
            }
            if let Some(m) = l.find("nd misses") {
                if let Some(d) = l[m..].find('.') {
                    return &l[..m + d + 1];
                }
            }
        }
        let mut from = 0;
        while let Some(rel) = l[from..].find(" pts") {
            let p = from + rel;
            let num_ok = l[..p].rsplit(' ').next().map_or(false, |w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == ','));
            if num_ok {
                return &l[..p + 4];
            }
            from = p + 4;
        }
        if let Some(p) = l.find(" damage") {
            return &l[..p + 7];
        }
        if let Some(p) = l.find(" performs ") {
            if let Some(d) = l[p..].find('.') {
                return &l[..p + d + 1];
            }
        }
        if let Some(p) = l.find(" misses") {
            if let Some(c) = l[p..].find(')') {
                return &l[..p + c + 1];
            }
        }
        if let Some(d) = l.find('.') {
            return &l[..d + 1];
        }
        l
    }

    /// One accepted scrollback line: where it sits in the window, its clean
    /// text, and the `\#RRGGBB` color it was rendered in (None if the line
    /// carried no color code).
    #[derive(Clone, Debug, PartialEq)]
    pub struct Line {
        pub off: usize,
        pub text: String,
        pub color: Option<u32>,
    }

    /// Decode a UTF-16LE window into NUL-separated RUNS of `\n`-separated
    /// lines, SWG color codes stripped (the line's leading color is kept as
    /// `Line::color`); only accepted combat lines are kept. The live
    /// scrollback is one contiguous string: lines joined by `\n`, the newest
    /// line ending right at a NUL, and stale fragments of older (longer) text
    /// beyond that NUL. A line cut off by the END of the window is discarded —
    /// it is not a real line.
    fn extract_runs(win: &[u8], accept: fn(&str) -> bool) -> Vec<Vec<Line>> {
        let mut runs: Vec<Vec<Line>> = Vec::new();
        let mut run: Vec<Line> = Vec::new();
        let mut cur = String::new();
        let mut color: Option<u32> = None;
        let mut start = 0usize;
        let mut begun = false;
        let mut k = 0usize;
        while k + 1 < win.len() {
            let c = (win[k] as u16) | ((win[k + 1] as u16) << 8);
            if !begun {
                start = k;
                begun = true;
            }
            if c == 0x0A || c == 0x0D || c == 0 || cur.len() > 400 {
                let t = trim_combat(cur.trim());
                if accept(t) {
                    run.push(Line { off: start, text: t.to_string(), color });
                }
                cur.clear();
                color = None;
                begun = false;
                if c == 0 && !run.is_empty() {
                    runs.push(std::mem::take(&mut run));
                }
                k += 2;
                continue;
            }
            if c == 0x5C {
                let n1 = if k + 3 < win.len() { (win[k + 2] as u16) | ((win[k + 3] as u16) << 8) } else { 0 };
                if n1 == 0x23 {
                    let mut j = k + 4;
                    let mut hexn = 0;
                    let mut val = 0u32;
                    while j + 1 < win.len() && hexn < 6 {
                        let h = (win[j] as u16) | ((win[j + 1] as u16) << 8);
                        if h == 0x2E { j += 2; break; }
                        match (h as u8 as char).to_digit(16) {
                            Some(d) if h < 0x80 => { val = val * 16 + d; hexn += 1; j += 2; }
                            _ => break,
                        }
                    }
                    if hexn == 6 && cur.trim().is_empty() {
                        color = Some(val); // the color the line starts in
                    }
                    k = j;
                    continue;
                }
            }
            if (0x20..=0x7E).contains(&c) {
                cur.push(c as u8 as char);
            }
            k += 2;
        }
        if !run.is_empty() {
            runs.push(run);
        }
        runs
    }

    /// All accepted lines in the window, in offset order (used to locate).
    fn extract(win: &[u8], accept: fn(&str) -> bool) -> Vec<Line> {
        extract_runs(win, accept).into_iter().flatten().collect()
    }

    /// The live scrollback in a window read at `base`: the run that covers the
    /// candidate region `lo..hi` — a bigger, frozen copy elsewhere in the
    /// window must not win just by size. Among covering runs the largest; if
    /// none covers it, the largest overall. Stale fragments past the run's NUL
    /// and unrelated heap text are left out, so the run's last line really is
    /// the newest line in the buffer.
    fn live_run(win: &[u8], base: usize, lo: usize, hi: usize) -> Vec<Line> {
        let runs = extract_runs(win, combat_like);
        let covers = |r: &Vec<Line>| r.iter().any(|l| (lo..hi).contains(&(base + l.off)));
        runs.iter()
            .filter(|r| covers(r))
            .max_by_key(|r| r.len())
            .or_else(|| runs.iter().max_by_key(|r| r.len()))
            .cloned()
            .unwrap_or_default()
    }

    /// Full grammar-scan of the 32-bit space; per 64 KiB bin record the combat
    /// line count and the most recent (last) line's text.
    fn scan_bins(p: &Proc) -> std::collections::HashMap<usize, (u32, String)> {
        use std::collections::HashMap;
        let mut bins: HashMap<usize, (u32, String)> = HashMap::new();
        let mut buf = vec![0u8; 0x100000];
        let mut addr = 0x10000usize;
        while addr < 0x7FFF_0000 {
            let got = p.read(addr, &mut buf);
            if got > 0 {
                for l in extract(&buf[..got], grammar_ok) {
                    let e = bins.entry((addr + l.off) >> 16).or_insert((0, String::new()));
                    e.0 += 1;
                    e.1 = l.text;
                }
                addr += got.max(1);
            } else {
                addr += 0x100000;
            }
        }
        bins
    }

    /// Candidate scrollback regions from a scan: maximal spans of consecutive
    /// 64 KiB bins holding at least 3 combat lines, as (start, end) addresses.
    fn candidates(bins: &std::collections::HashMap<usize, (u32, String)>) -> Vec<(usize, usize)> {
        let mut starts: Vec<usize> = bins.iter().filter(|(_, (c, _))| *c >= 3).map(|(&b, _)| b << 16).collect();
        starts.sort_unstable();
        let mut out: Vec<(usize, usize)> = Vec::new();
        for st in starts {
            match out.last_mut() {
                Some(last) if last.1 == st => last.1 = st + 0x10000,
                _ => out.push((st, st + 0x10000)),
            }
        }
        out
    }

    /// One candidate region and what we know about it. The reader follows
    /// ALL of them every tick and emits from the one that is actually
    /// advancing, so a frozen copy of the scrollback can never trap it.
    struct Cand {
        start: usize,
        end: usize,
        anchor: Option<Anchor>,
        prev_tail: Option<String>,
        /// when its newest line last changed (0 = never seen changing)
        last_change: f64,
        changed_now: bool,
        empty_ticks: u32,
        nlines: usize,
        emitted_any: bool,
    }

    /// Where we are in the buffer: the last line we emitted, identified by the
    /// text of it plus a few lines before it, with its byte offset as a
    /// tiebreak. Offsets are nearly useless on their own: once the scrollback
    /// is at its line cap every append trims the oldest line and shifts the
    /// whole text. Text alone breaks on repeated identical hits. Context
    /// resolves both.
    #[derive(Clone)]
    struct Anchor {
        off: usize,
        /// Up to CTX lines ending with the anchor line itself.
        ctx: Vec<String>,
    }
    const CTX: usize = 6;

    impl Anchor {
        fn seed(lines: &[Line], i: usize) -> Anchor {
            let lo = (i + 1).saturating_sub(CTX);
            Anchor { off: lines[i].off, ctx: lines[lo..=i].iter().map(|l| l.text.clone()).collect() }
        }

        /// Index of the anchor line in `lines`, or None if it is gone.
        fn find(&self, lines: &[Line]) -> Option<usize> {
            let last = self.ctx.last()?;
            let mut best: Option<(usize, usize, bool)> = None; // (idx, score, off_match)
            for (i, l) in lines.iter().enumerate() {
                if l.text != *last {
                    continue;
                }
                let mut score = 1;
                while score < self.ctx.len()
                    && i >= score
                    && lines[i - score].text == self.ctx[self.ctx.len() - 1 - score]
                {
                    score += 1;
                }
                let om = l.off == self.off;
                let better = match best {
                    None => true,
                    Some((_, bs, bom)) => score > bs || (score == bs && (om && !bom || om == bom)),
                };
                if better {
                    best = Some((i, score, om));
                }
            }
            best.map(|(i, _, _)| i)
        }
    }

    pub fn run(sink: Sink) {
        let pid = match find_pid() {
            Some(p) => p,
            None => {
                sink.set_log_label("(memory: SwgClient_r.exe not running)");
                std::thread::sleep(Duration::from_secs(2));
                return run(sink);
            }
        };
        let h = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
        if h.is_null() {
            sink.set_log_label("(memory: OpenProcess failed — run swglogs as Administrator)");
            eprintln!("[swglogs] OpenProcess failed; run as Administrator");
            return;
        }
        let proc = Proc(h);
        sink.set_log_label(&format!("(memory: SwgClient_r.exe pid {} — scanning for combat text)", pid));

        const SLACK: usize = 0x8000; // read this much before/after a region
        let mut cands: Vec<Cand> = Vec::new();
        let mut primary: Option<usize> = None;
        let mut last_scan = 0.0f64;
        let mut resyncs = 0u32;
        let mut last_label = String::new();
        let mut dump_n = 0u32;
        let mut win = vec![0u8; 0x80000]; // up to 512 KiB per candidate read
        let mut win2 = vec![0u8; 0x80000]; // second read of the primary: torn-read check
        let pinned = std::env::var("SWGLOGS_MEMREGION").ok()
            .and_then(|v| usize::from_str_radix(v.trim_start_matches("0x"), 16).ok());

        loop {
            {
                let mut m = sink.meter.lock().unwrap();
                m.tick(now_secs());
            }
            let now = now_secs();

            // (Re)scan the client for combat text: at start, whenever nothing
            // is being followed, every 10 s while the followed region has been
            // quiet for 20 s (it may have been reallocated), else every 2 min.
            // A freshly started client has no combat text at all until the
            // first fight; rescan every 5 s in that state.
            let idle = primary.map_or(true, |p| now - cands[p].last_change > 20.0);
            if (cands.is_empty() && now - last_scan > 5.0) || (idle && now - last_scan > 10.0) || now - last_scan > 120.0 {
                last_scan = now;
                let regions = match pinned {
                    Some(r) => vec![(r, r + 0x10000)],
                    None => candidates(&scan_bins(&proc)),
                };
                let prim_region = primary.map(|p| (cands[p].start, cands[p].end));
                let mut next: Vec<Cand> = Vec::with_capacity(regions.len());
                for (st, en) in regions {
                    match cands.iter().position(|c| c.start == st && c.end == en) {
                        Some(i) => next.push(cands.swap_remove(i)),
                        None => next.push(Cand {
                            start: st, end: en, anchor: None, prev_tail: None, last_change: 0.0,
                            changed_now: false, empty_ticks: 0, nlines: 0, emitted_any: false,
                        }),
                    }
                }
                cands = next;
                primary = prim_region.and_then(|(st, en)| cands.iter().position(|c| c.start == st && c.end == en));
                if cands.is_empty() {
                    sink.set_log_label(&format!(
                        "(memory: pid {} — the client has no combat text yet (fresh login / no fight since it started);                          hit something and it appears within a few seconds)",
                        pid
                    ));
                }
            }

            if cands.is_empty() {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }

            // Read every candidate once; note whose newest line moved.
            let mut lines_of: Vec<Vec<Line>> = Vec::with_capacity(cands.len());
            for c in cands.iter_mut() {
                let base = c.start.saturating_sub(SLACK);
                let len = (c.end + SLACK - base).min(win.len());
                let got = proc.read(base, &mut win[..len]);
                let lines = if got > 0 { live_run(&win[..got], base, c.start, c.end) } else { Vec::new() };
                c.nlines = lines.len();
                let tail = lines.last().map(|l| l.text.clone());
                c.changed_now = tail.is_some() && tail != c.prev_tail;
                if c.changed_now && c.prev_tail.is_some() {
                    c.last_change = now;
                }
                if tail.is_some() {
                    c.prev_tail = tail;
                }
                lines_of.push(lines);
            }

            // Follow the region that advanced most recently. Switch away from
            // the current one only once it has been quiet for 5 s while
            // another one moved (so two mirrors of the same text never
            // double-emit: only one region is ever read for events).
            let best = cands.iter().enumerate()
                .filter(|(_, c)| c.last_change > 0.0)
                .max_by(|a, b| a.1.last_change.partial_cmp(&b.1.last_change).unwrap())
                .map(|(i, _)| i);
            match (primary, best) {
                (None, Some(b)) => primary = Some(b),
                (Some(p), Some(b)) if b != p && now - cands[p].last_change > 5.0 && cands[b].last_change > cands[p].last_change => {
                    // Carry the position across: the mirrors hold the same
                    // text, so the old anchor (text + context) is found in
                    // the new region and nothing in between is lost. If it is
                    // not found there, the anchor logic below reseeds.
                    cands[b].anchor = cands[p].anchor.clone();
                    cands[b].emitted_any = cands[p].emitted_any;
                    primary = Some(b);
                    resyncs += 1;
                }
                _ => {}
            }
            let p = match primary {
                Some(p) => p,
                None => {
                    let mut lab = format!("(memory: pid {} — {} candidate region(s); waiting for combat to see which one is live)", pid, cands.len());
                    for c in &cands {
                        lab.push_str(&format!("
   0x{:08X}-0x{:08X} {:4} lines{}", c.start, c.end, c.nlines,
                            if c.last_change > 0.0 { format!(" moved {:.0}s ago", now - c.last_change) } else { String::new() }));
                    }
                    if lab != last_label {
                        sink.set_log_label(&lab);
                        last_label = lab;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
            };

            let lines = std::mem::take(&mut lines_of[p]);
            if lines.is_empty() {
                cands[p].empty_ticks += 1;
                if cands[p].empty_ticks > 24 {
                    // gone (freed/reallocated): forget it and rescan
                    cands.remove(p);
                    primary = None;
                    last_scan = 0.0;
                }
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            cands[p].empty_ticks = 0;

            // Torn reads: the game shifts the whole scrollback text while we
            // copy it, and a copy that straddles that shift splices two
            // versions of a line together ("Shoon attacks", "fire dage").
            // Such a splice is transient, so a line is trusted only when a
            // second, immediate read of the window shows the same text.
            let base = cands[p].start.saturating_sub(SLACK);
            let len = (cands[p].end + SLACK - base).min(win2.len());
            let got2 = proc.read(base, &mut win2[..len]);
            // Diagnostic: SWGLOGS_MEMDUMP=<dir> writes the raw window for the
            // first 12 ticks with a followed region, for offline analysis.
            if let Ok(dir) = std::env::var("SWGLOGS_MEMDUMP") {
                if dump_n < 12 {
                    let _ = fs::write(format!("{}/win_{:02}_{:08X}_{}.bin", dir, dump_n, base, got2), &win2[..got2]);
                    dump_n += 1;
                    if dump_n == 12 {
                        eprintln!("[swglogs] memdump complete");
                        std::process::exit(0);
                    }
                }
            }
            let confirm: std::collections::HashSet<String> = if got2 > 0 {
                live_run(&win2[..got2], base, cands[p].start, cands[p].end).into_iter().map(|l| l.text).collect()
            } else {
                std::collections::HashSet::new()
            };

            // Hold-back: the newest line is the one the game may still be
            // writing; it is consumed only once it reads the same on two
            // consecutive ticks, or a newer line follows it.
            let stable = !cands[p].changed_now;

            // Where do the new lines start?
            let from = match &cands[p].anchor {
                None => {
                    // seed to the end without replaying the backlog
                    cands[p].anchor = Some(Anchor::seed(&lines, lines.len() - 1));
                    lines.len()
                }
                Some(a) => match a.find(&lines) {
                    Some(i) => i + 1,
                    None => {
                        // Anchor vanished (buffer replaced under us, or the
                        // first seed was a torn tail): reseed rather than guess.
                        if cands[p].emitted_any {
                            resyncs += 1;
                        }
                        cands[p].anchor = Some(Anchor::seed(&lines, lines.len() - 1));
                        lines.len()
                    }
                },
            };
            let mut to = if stable { lines.len() } else { lines.len() - 1 };
            // stop at the first line the second read does not confirm; it is
            // re-read next tick (nothing is skipped)
            if let Some(i) = lines[from..to.max(from)].iter().position(|l| !confirm.contains(&l.text)) {
                to = from + i;
            }

            if from < to {
                let ts = now_secs();
                for l in &lines[from..to] {
                    sink.line_colored(&l.text, l.color, ts);
                }
                cands[p].anchor = Some(Anchor::seed(&lines, to - 1));
                sink.mark_fresh(ts);
                cands[p].emitted_any = true;
            }

            let mut lab = format!(
                "(memory: pid {} region 0x{:08X}-0x{:08X}, {} lines, {} candidate region(s){})",
                pid, cands[p].start, cands[p].end, cands[p].nlines, cands.len(),
                if resyncs == 0 { String::new() } else { format!(", {} switches", resyncs) }
            );
            for c in &cands {
                lab.push_str(&format!("
   0x{:08X}-0x{:08X} {:4} lines{}", c.start, c.end, c.nlines,
                    if c.last_change > 0.0 { format!(" moved {:.0}s ago", now - c.last_change) } else { String::new() }));
            }
            if lab != last_label {
                sink.set_log_label(&lab);
                last_label = lab;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Pure-function checks, run by `swglogs --selftest`.
    pub fn selfcheck(check: &mut dyn FnMut(bool, &str)) {
        check(trim_combat("Yourname hits a bolma female 181 ptsbse)2 pts50or#.rget") == "Yourname hits a bolma female 181 pts",
              "memory: stale tail trimmed at FIRST numeric ' pts'");
        check(trim_combat("Yourname performs Killing Spree.@zCs@junk") == "Yourname performs Killing Spree.",
              "memory: performs line trimmed at period");
        let vb = "Yourname attacks a giant baz nitch with Mine 2: Plasma Mine.And hits (8% evaded) for 489 points (489 energy).  Armor absorbed 254 points out of 743.junk";
        check(trim_combat(vb) == "Yourname attacks a giant baz nitch with Mine 2: Plasma Mine.And hits (8% evaded) for 489 points (489 energy)"
                  && combat_like(trim_combat(vb)) && grammar_ok(trim_combat(vb)),
              "memory: verbose line trimmed after the breakdown and accepted");
        check(trim_combat("Yourname attacks a kwi using [UA] T21 Rifle and misses (dodge).junk") == "Yourname attacks a kwi using [UA] T21 Rifle and misses (dodge).",
              "memory: verbose miss trimmed at its period");
        check(trim_combat("Yourname attacks a kwi with Mine 2: Plasma Mine.And hits for 533 points.junk") == "Yourname attacks a kwi with Mine 2: Plasma Mine.And hits for 533 points",
              "memory: verbose line without damage detail trimmed after 'points'");
        check(combat_like("Yourname misses (dodge)") && combat_like("Yourname performs Heal 4.")
              && combat_like("An elder mamien has been incapacitated."),
              "memory: wide acceptor passes miss/performs/death");
        check(!combat_like("@terminal_name:terminal_bank$tDtn Yourname") && !grammar_ok("Yourname performs Heal 4."),
              "memory: junk rejected; strict locate grammar stays hit-only");
        let l = |v: &[(usize, &str)]| v.iter().map(|(o, s)| Line { off: *o, text: s.to_string(), color: None }).collect::<Vec<_>>();
        let t1 = l(&[(0, "A hits B 100 pts"), (50, "A hits B 100 pts"), (100, "A crits B 300 pts"), (150, "A hits B 100 pts")]);
        let t2 = l(&[(0, "A hits B 100 pts"), (50, "A hits B 100 pts"), (100, "A crits B 300 pts"), (150, "A hits B 100 pts"), (200, "A hits B 100 pts")]);
        let a = Anchor::seed(&t1, 1); // the second of two identical hits
        check(a.find(&t2) == Some(1), "memory: anchor lands on the right duplicate (offset+context)");
        // same text shifted (front of scrollback trimmed): offsets useless, context resolves
        let t3 = l(&[(10, "A hits B 100 pts"), (60, "A crits B 300 pts"), (110, "A hits B 100 pts"), (160, "A hits B 100 pts")]);
        let a2 = Anchor::seed(&t1, 3);
        check(a2.find(&t3) == Some(2), "memory: anchor resyncs after front-trim shift via context");
        // color capture through the real UTF-16 decoder
        let txt = "\\#50f111Yourname hits a kwi 500 pts\\#.\n\\#f30f0fA kwi hits Yourname 200 pts\\#.\nYourname misses (dodge)\\#.\n";
        let mut win = Vec::new();
        for u in txt.encode_utf16() {
            win.extend_from_slice(&u.to_le_bytes());
        }
        win.extend_from_slice(&[0, 0]);
        let ls = live_run(&win, 0, 0, 0x10000);
        check(
            ls.len() == 3 && ls[0].color == Some(0x50f111) && ls[1].color == Some(0xf30f0f) && ls[2].color.is_none()
                && ls[0].text == "Yourname hits a kwi 500 pts",
            "memory: line colors captured, text stripped",
        );
    }
}

#[cfg(not(windows))]
pub mod memory {
    use super::Sink;
    pub fn run(_sink: Sink) {
        eprintln!("[swglogs] memory source is Windows-only");
    }
    pub fn selfcheck(_check: &mut dyn FnMut(bool, &str)) {}
    pub fn client_pid() -> Option<u32> {
        None
    }
}
