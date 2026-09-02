//! Startup wiring for the executable, window or headless: configuration +
//! argument parsing, the in-game browser UI patch, and bringing up the log,
//! the event source and the HTTP server.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::logwriter::LogWriter;
use crate::meter::{Meter, Notice};
use crate::server;
use crate::sources::{self, Sink};
use crate::uipatch;

pub struct Config {
    pub source: String,
    pub host: String,
    pub port: u16,
    pub gap: f64,
    pub player: Option<String>,
    pub profiles: PathBuf,
    pub log: Option<PathBuf>,
    pub out: Option<String>,
    pub replay: bool,
    pub selftest: bool,
    pub game_dir: Option<PathBuf>,
    pub ui_patch: bool,
    /// Console only: never open the desktop window.
    pub headless: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            source: "memory".into(),
            host: "127.0.0.1".into(),
            port: 8666,
            gap: 8.0,
            player: None,
            profiles: PathBuf::from(r"C:\SWG Legends\profiles"),
            log: None,
            out: Some("combat-log.jsonl".into()),
            replay: false,
            selftest: false,
            game_dir: None,
            ui_patch: true,
            headless: false,
        }
    }
}

/// Parse the process arguments. `-h`/`--help` prints usage and exits.
pub fn parse_args() -> Config {
    let mut a = Config::default();
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
            "--game-dir" => a.game_dir = Some(PathBuf::from(next(&mut i))),
            "--no-ui-patch" => a.ui_patch = false,
            "--headless" => a.headless = true,
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

pub fn print_help() {
    println!(
        "swglogs — SWG Legends combat logs + meter\n\n\
         --source memory|chatlog|ipc|demo   event source (default memory;\n\
                        memory needs Administrator)\n\
         --port N       HTTP port (default 8666)\n\
         --host H       bind host (default 127.0.0.1)\n\
         --gap S        seconds of silence that ends an encounter (default 8)\n\
         --player NAME  your character, highlighted (default: none)\n\
         --profiles DIR chatlog search root (default C:\\SWG Legends\\profiles)\n\
         --log FILE     tail this exact chatlog instead of auto-detecting\n\
         --replay       parse the whole existing chatlog first, then tail\n\
         --out FILE     real-time JSONL log (default combat-log.jsonl)\n\
         --no-log       disable the real-time log\n\
         --selftest     run parser/aggregator checks and exit\n\
         --game-dir DIR game install (default: parent of --profiles); its\n\
                        ui\\ui_pda.inc is patched so the /browser window stays\n\
                        open in shoot mode (StickyVisible)\n\
         --no-ui-patch  skip that patch\n\
         --headless     console only, don't open the window\n\n\
         In game:  /browser http://127.0.0.1:8666/   (also /healing, /taken)"
    );
}

/// The exe is a GUI-subsystem program on Windows (so a double-click opens
/// just the window, no stray console). Such a process gets no console of its
/// own — `swglogs --headless` or `--selftest` typed into a terminal would
/// print nothing. If stdout is not already connected (a pipe or a redirect
/// is left alone), attach to the terminal that launched us.
pub fn attach_parent_console() {
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(pid: u32) -> i32;
            fn GetStdHandle(id: u32) -> *mut core::ffi::c_void;
        }
        const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
        const ATTACH_PARENT_PROCESS: u32 = -1i32 as u32;
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h.is_null() || h as isize == -1 {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Diagnostic: `SWGLOGS_EXIT_AFTER=<secs>` ends the process after that long
/// (lets a timed, elevated test run stop on its own).
pub fn install_exit_after() {
    if let Some(secs) = std::env::var("SWGLOGS_EXIT_AFTER").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            std::process::exit(0);
        });
    }
}

/// Handles to a running meter.
pub struct Running {
    pub meter: Arc<Mutex<Meter>>,
    /// `host:port` the meter page is served on.
    pub addr: String,
    /// Set if the HTTP server failed to start (e.g. the port is in use).
    pub server_error: Arc<Mutex<Option<String>>>,
}

/// Apply the UI patch, open the real-time log, spawn the event source and the
/// HTTP server. Returns immediately; everything runs on background threads.
pub fn start(cfg: &Config) -> Running {
    let notice = apply_ui_patch(cfg);

    let meter = Arc::new(Mutex::new(Meter::new(cfg.gap, cfg.player.clone())));
    if let Some(n) = notice {
        meter.lock().unwrap().notice = Some(n);
    }
    let log = match &cfg.out {
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
        let source = cfg.source.clone();
        let profiles = cfg.profiles.clone();
        let fixed = cfg.log.clone();
        let replay = cfg.replay;
        std::thread::spawn(move || match source.as_str() {
            "demo" => sources::demo(sink),
            "ipc" => sources::ipc::ipc_ring(sink),
            "memory" => sources::memory::run(sink),
            _ => sources::chatlog_tail(sink, profiles, fixed, replay),
        });
    }

    let addr = format!("{}:{}", cfg.host, cfg.port);
    println!("[swglogs] serving http://{}/   (debug: /debug)", addr);
    println!("[swglogs] in game:  /browser http://{}/", addr);
    let server_error = Arc::new(Mutex::new(None));
    {
        let meter = Arc::clone(&meter);
        let addr = addr.clone();
        let err = Arc::clone(&server_error);
        std::thread::spawn(move || {
            if let Err(e) = server::serve(&addr, meter) {
                eprintln!("[swglogs] server error: {}", e);
                *err.lock().unwrap() = Some(e.to_string());
            }
        });
    }

    Running { meter, addr, server_error }
}

/// Make sure the game's `/browser` window survives action mode (see
/// `uipatch`). Returns a notice for the meter page when an already-running
/// client still has the old file loaded and must be restarted.
fn apply_ui_patch(cfg: &Config) -> Option<Notice> {
    if !cfg.ui_patch {
        return None;
    }
    let game_dir = cfg
        .game_dir
        .clone()
        .or_else(|| cfg.profiles.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    match uipatch::ensure_sticky_browser(&game_dir) {
        Ok(uipatch::Outcome::AlreadySet) => None,
        Ok(uipatch::Outcome::Patched(backup)) => {
            println!(
                "[swglogs] patched {}\\ui\\ui_pda.inc so the /browser meter stays open in action mode \
                 (StickyVisible='true'; backup {}).",
                game_dir.display(),
                backup
            );
            println!(
                "[swglogs] *** RESTART THE GAME CLIENT *** — the UI file is read at launch, so the meter \
                 will keep hiding in action mode until SwgClient_r.exe is restarted."
            );
            // Only a client that is ALREADY running has the old file loaded; a
            // client launched later reads the patched one. Show the banner on
            // the meter page until that client process is gone.
            sources::memory::client_pid().map(|pid| Notice {
                text: "UI patched \u{2014} restart the game client for this meter to stay open in action mode."
                    .to_string(),
                client_pid: Some(pid),
            })
        }
        Ok(uipatch::Outcome::NoLooseFile) => {
            println!(
                "[swglogs] no loose ui\\ui_pda.inc in {} — the /browser window will hide in shoot mode. \
                 Extract ui/ui_pda.inc from the game's TRE files into {}\\ui\\ and rerun, or pass --no-ui-patch.",
                game_dir.display(),
                game_dir.display()
            );
            None
        }
        Err(e) => {
            eprintln!("[swglogs] ui patch skipped: {}", e);
            None
        }
    }
}
