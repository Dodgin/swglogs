//! Diagnostic trace for the memory source (`--trace [DIR]`). Off by default
//! and free when off. When on, the folder holds:
//!
//!   * `trace.jsonl` — one JSON record per follower decision (scan, tick,
//!     switch, resync, dropped, torn, stall, truncated, ...), stamped with the
//!     same clock as `combat-log.jsonl`, so the two interleave by `ts`.
//!   * `raw.txt` — every line handed from memory to the parser, with its
//!     timestamp, region and byte offset: the memory-side twin of the combat
//!     log, so a missing event can be traced to "never read" or "read but not
//!     parsed".
//!   * `win_<ms>_<tag>_<base>_<lo>_<hi>.bin` + `.txt` — raw snapshots of the
//!     followed window at every interesting moment plus a slow heartbeat, and
//!     the lines the decoder extracted from them. `--trace-replay DIR` feeds
//!     these back through the real decoder and follower without the game.
//!
//! Snapshots are capped at a rolling `SNAP_CAP` bytes (oldest deleted), so a
//! long session cannot fill the disk.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::event::json_str;

/// Rolling cap on snapshot bytes kept on disk.
pub const SNAP_CAP: u64 = 100 * 1024 * 1024;

/// A JSON object under construction: `{"kind":..,"ts":..,<fields>}`.
pub struct Rec {
    body: String,
}

impl Rec {
    pub fn new(kind: &str, ts: f64) -> Rec {
        Rec { body: format!("{{\"kind\":{},\"ts\":{:.3}", json_str(kind), ts) }
    }
    pub fn s(mut self, k: &str, v: &str) -> Rec {
        self.body.push_str(&format!(",{}:{}", json_str(k), json_str(v)));
        self
    }
    pub fn u(mut self, k: &str, v: usize) -> Rec {
        self.body.push_str(&format!(",{}:{}", json_str(k), v));
        self
    }
    pub fn i(mut self, k: &str, v: i64) -> Rec {
        self.body.push_str(&format!(",{}:{}", json_str(k), v));
        self
    }
    pub fn f(mut self, k: &str, v: f64) -> Rec {
        self.body.push_str(&format!(",{}:{:.3}", json_str(k), v));
        self
    }
    pub fn b(mut self, k: &str, v: bool) -> Rec {
        self.body.push_str(&format!(",{}:{}", json_str(k), v));
        self
    }
    /// Address as `"0x0ABCDEF0"`.
    pub fn hex(mut self, k: &str, v: usize) -> Rec {
        self.body.push_str(&format!(",{}:\"0x{:08X}\"", json_str(k), v));
        self
    }
    /// Region as `"0x..-0x.."`.
    pub fn region(self, k: &str, lo: usize, hi: usize) -> Rec {
        self.s(k, &format!("0x{:08X}-0x{:08X}", lo, hi))
    }
    pub fn strs(mut self, k: &str, v: &[String]) -> Rec {
        self.body.push_str(&format!(",{}:[", json_str(k)));
        for (i, s) in v.iter().enumerate() {
            if i > 0 {
                self.body.push(',');
            }
            self.body.push_str(&json_str(s));
        }
        self.body.push(']');
        self
    }
    /// A pre-built JSON array of objects (see `Tracer::obj`).
    pub fn raw_arr(mut self, k: &str, items: &[String]) -> Rec {
        self.body.push_str(&format!(",{}:[{}]", json_str(k), items.join(",")));
        self
    }
    fn finish(mut self) -> String {
        self.body.push('}');
        self.body
    }
}

pub struct Tracer {
    pub dir: PathBuf,
    log: Mutex<BufWriter<File>>,
    raw: Mutex<BufWriter<File>>,
    snaps: Mutex<(VecDeque<(PathBuf, u64)>, u64)>,
}

impl Tracer {
    pub fn open(dir: &Path) -> std::io::Result<Tracer> {
        fs::create_dir_all(dir)?;
        let open = |name: &str| -> std::io::Result<BufWriter<File>> {
            Ok(BufWriter::new(OpenOptions::new().create(true).append(true).open(dir.join(name))?))
        };
        let t = Tracer {
            dir: dir.to_path_buf(),
            log: Mutex::new(open("trace.jsonl")?),
            raw: Mutex::new(open("raw.txt")?),
            snaps: Mutex::new((VecDeque::new(), 0)),
        };
        // Adopt snapshots left by an earlier run so the cap covers them too.
        let mut old: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with("win_") {
                    if let Ok(md) = e.metadata() {
                        old.push((p, md.len(), md.modified().unwrap_or(std::time::UNIX_EPOCH)));
                    }
                }
            }
        }
        old.sort_by_key(|(_, _, m)| *m);
        {
            let mut s = t.snaps.lock().unwrap();
            for (p, n, _) in old {
                s.1 += n;
                s.0.push_back((p, n));
            }
        }
        Ok(t)
    }

    /// Append one decision record and flush it.
    pub fn rec(&self, r: Rec) {
        let line = r.finish();
        if let Ok(mut w) = self.log.lock() {
            let _ = w.write_all(line.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }

    /// One raw line as handed to the parser.
    pub fn raw(&self, ts: f64, region_lo: usize, addr: usize, color: Option<u32>, text: &str) {
        if let Ok(mut w) = self.raw.lock() {
            let col = match color {
                Some(c) => format!("#{:06x}", c),
                None => "-".to_string(),
            };
            let _ = writeln!(w, "{:.3}\t0x{:08X}\t0x{:08X}\t{}\t{}", ts, region_lo, addr, col, text);
            let _ = w.flush();
        }
    }

    /// JSON object literal helper for `Rec::raw_arr`.
    pub fn obj(fields: &[(&str, String)]) -> String {
        let mut s = String::from("{");
        for (i, (k, v)) in fields.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&json_str(k));
            s.push(':');
            s.push_str(v);
        }
        s.push('}');
        s
    }

    /// Save a raw window plus its decoded lines. `lines` are
    /// `(offset-in-window, text)`. Returns the `.bin` file name.
    pub fn snapshot(&self, ts: f64, tag: &str, base: usize, lo: usize, hi: usize, win: &[u8], lines: &[(usize, String)]) -> String {
        let name = format!("win_{:013}_{}_{:08X}_{:08X}_{:08X}", (ts * 1000.0) as u64, tag, base, lo, hi);
        let bin = self.dir.join(format!("{}.bin", name));
        let txt = self.dir.join(format!("{}.txt", name));
        let _ = fs::write(&bin, win);
        let mut t = String::with_capacity(lines.len() * 96);
        t.push_str(&format!("# {} base=0x{:08X} region=0x{:08X}-0x{:08X} bytes={} lines={}\n", tag, base, lo, hi, win.len(), lines.len()));
        for (off, text) in lines {
            t.push_str(&format!("0x{:08X}\t{}\n", base + off, text));
        }
        let _ = fs::write(&txt, t.as_bytes());
        let added = win.len() as u64 + t.len() as u64;
        if let Ok(mut s) = self.snaps.lock() {
            s.1 += added;
            s.0.push_back((bin.clone(), win.len() as u64));
            s.0.push_back((txt, t.len() as u64));
            while s.1 > SNAP_CAP {
                match s.0.pop_front() {
                    Some((p, n)) => {
                        let _ = fs::remove_file(&p);
                        s.1 = s.1.saturating_sub(n);
                    }
                    None => break,
                }
            }
        }
        format!("{}.bin", name)
    }
}

/// Parse a snapshot file name back into `(ts, tag, base, lo, hi)`.
pub fn parse_snapshot_name(name: &str) -> Option<(f64, String, usize, usize, usize)> {
    let stem = name.strip_suffix(".bin")?;
    let rest = stem.strip_prefix("win_")?;
    let mut parts: Vec<&str> = rest.rsplitn(4, '_').collect();
    // rsplitn gives [hi, lo, base, "<ms>_<tag>"] in reverse
    if parts.len() != 4 {
        return None;
    }
    parts.reverse();
    let (ms_tag, base, lo, hi) = (parts[0], parts[1], parts[2], parts[3]);
    let (ms, tag) = ms_tag.split_once('_')?;
    let ts = ms.parse::<u64>().ok()? as f64 / 1000.0;
    let h = |s: &str| usize::from_str_radix(s, 16).ok();
    Some((ts, tag.to_string(), h(base)?, h(lo)?, h(hi)?))
}
