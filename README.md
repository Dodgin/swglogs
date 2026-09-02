# swglogs

[![CI](https://github.com/Dodgin/swglogs/actions/workflows/ci.yml/badge.svg)](https://github.com/Dodgin/swglogs/actions/workflows/ci.yml)

![SWG Logs meter in the in-game browser](docs/readme-ex-1.png)

SWG Legends combat logs + Details-style meter, in Rust. **One normalized
combat-event stream, two sinks:** a live web meter and a real-time structured
log (JSONL). The event source is pluggable, so the same meter/log run on top of
either input:

```
                                      ┌─ meter aggregator ── http://127.0.0.1:8666/  (in-game /browser)
source ──> Event (normalized) ──┤
                                      └─ real-time log ────── combat-log.jsonl  (flushed per event)

sources:  memory (default)      the client's combat scrollback, read directly   (zero flush latency; run as Admin)
          chatlog tail          the player's own chatlog file                   (no elevation; lags until the game flushes)
          IPC ring              combat lines from an external producer         (zero flush latency)
          demo                  synthetic combat                               (no game needed)
```

Core is dependency-free (std only); only the window pulls in egui (see below).

## Build & run

Prebuilt `swglogs.exe`: every push to `main` uploads one as a CI artifact, and
tagging `vX.Y` attaches it to a GitHub release. From source:

```
cargo build --release
./target/release/swglogs                   # (as Administrator) opens the status window; reads the client's combat scrollback, serves :8666, logs combat-log.jsonl
./target/release/swglogs --headless        # same, console only (no window)
./target/release/swglogs --source chatlog  # tail the newest chatlog file instead (no elevation needed)
./target/release/swglogs --source demo     # synthetic combat, preview the page
./target/release/swglogs --source ipc    # drain the IPC ring (needs a producer feeding EV_COMBAT_SPAM)
./target/release/swglogs --selftest      # parser/aggregator checks vs real log lines
```

In game: `/browser http://127.0.0.1:8666/` (also `/healing`, `/taken` open on
that metric — the client spawns an independent browser window per call, so you
can run several).

### Options

`--source memory|chatlog|ipc|demo` (default memory) · `--port N` · `--host H` · `--gap S` (encounter
gap, default 8) · `--player NAME` (highlighted; unset by default) · `--profiles
DIR` · `--log FILE` (pin an exact chatlog) · `--replay` (parse the whole log
first, then tail) · `--out FILE` (JSONL log, default `combat-log.jsonl`) ·
`--no-log` · `--selftest` · `--game-dir DIR` (game install; by default the
folder of the running `SwgClient_r.exe`, else the parent of `--profiles`) ·
`--no-ui-patch`.

On startup swglogs patches the game's loose `ui\ui_pda.inc` (if present) so the
`/browser` window carries `StickyVisible='true'` and stays open when you leave
cursor mode — otherwise the client hides every window in shoot mode — and so
its close button is no longer the window's Escape-key cancel button, so
spamming Escape in a fight doesn't close the meter (the X still works). A
backup is kept as `ui_pda.inc.pre-swglogs`; the change applies from the next game
start. With no loose file at all (a stock install, or a UI mod that doesn't ship this page) swglogs writes one from the copy of Legends' page bundled in the exe (`assets/ui_pda.inc`, the page as shipped in the client, the same thing UI mods distribute), already patched. The window and the meter page report what happened. `--no-ui-patch` disables it; `--restore-ui` undoes it (backup back, or our file removed).

## The window

One executable. Double-clicking `swglogs.exe` opens a 640x480 egui window with
a single **Logging** tab: whether SWG Logs is running, the in-game `/browser`
macro to open the meter, and the restart-the-client notice when the UI patch
just landed. Closing the window stops the meter. More tabs
are coming. `--headless` skips the window (console only). Launched with any
flag from a terminal, the exe attaches to that terminal so `--headless`,
`--selftest` and `--help` print normally; launched bare (double-click, or a
plain `swglogs.exe`) it stays detached, the prompt comes straight back, and
closing the window is all it takes to stop it.

The exe always asks for Administrator (an embedded UAC manifest): the memory
source has to open the elevated game client, so this saves a right-click. It
applies to every mode, `--headless` and `--selftest` included. From an
un-elevated `cmd.exe` Windows refuses to start it ("requires elevation");
PowerShell and Explorer show the UAC prompt.

The window is Cargo feature `gui` (on by default; pulls in `eframe`/`egui`).
`cargo build --release --no-default-features` gives a std-only console build
that needs no registry access.

## The two outputs

**Meter page** — ranked bars, header cycles Now/Last/All and Damage/Healing/
Taken and Players (default) / NPCs / Everyone. Click a row
for its ability breakdown (hits/crits/max). Player vs NPC: an article or a
lowercase name means NPC ("a kwi", "an elder mamien"); the line's color pins
you (the client colors combat spam from your point of view: green your hits,
orange your DoT ticks, red hits on you, light blue heals); and whatever you
hit or get hit by counts as an enemy — an NPC, in PvE — unless it ever shows
player behaviour (heals, gets healed). Title-case humanoids like "Tusken Relic
Worshiper" land on the NPC side that way. Group members get no color and stay
"unknown", which the Players view includes. Shows a `⚠Ns`
staleness marker when the chatlog hasn't advanced (the game buffers its writes;
our own log below does not).

**Real-time log** (`combat-log.jsonl`) — one JSON object per event, flushed
immediately, with the full context the game's four-number native meter throws
away:

```json
{"ts":1788305368.019,"iso":"2026-09-01T23:29:28Z","kind":"damage","src":"Yourname","tgt":"Elder mamien","amount":973,"outcome":"blocked","ability":"attack","raw":"Yourname hits (blocked) an elder mamien 973 pts"}
```

`outcome` is one of normal/critical/glancing/blocked/evaded/dodged/parried/miss.

## Sources

- **memory** (`src/sources.rs::memory`, default) — opens `SwgClient_r.exe`
  read-only (no injection) and reads the combat chat scrollback straight out of
  the client, so lines arrive the moment the game renders them. Needs swglogs
  to run as Administrator because the client runs elevated. Locates the live
  buffer by watching which candidate text actually advances, resyncs on the
  buffer's own line text (the scrollback shifts on every append once it hits
  its line cap), and holds the newest line back one tick so a line caught
  mid-write is never consumed torn.

**Verbose combat spam is required.** In the game's Options, under the combat
spam settings: set **Combat Spam** to **Verbose** (not Brief), turn on **Show
Weapon** (basic attacks are then labeled with the weapon), and set the combat
spam **filter** to include your group if you want a group meter. Show Damage
Detail and Show Armor Absorption are optional. Chat timestamps (the
`HH:MM:SS` prefix on every line) are fine either way: they are stripped, and
the chatlog source uses them — together with the file's `Logging In [date]`
markers — as the real time of each line, so `--replay` of an old log keeps
its true timing and encounter breaks. Verbose lines name the ability
on every hit, which is the only way damage can be attributed; brief lines
("Shootin hits a kwi 500 pts") still count as damage but land in a plain
"attack" bucket.

- **chatlog** (`src/sources.rs::chatlog_tail`) — finds the newest
  `profiles/*/*/*_chatlog.txt`, tails it, parses each `[Combat]` line
  (`src/parse.rs`, grammar verified against a real Sep-2026 log — hits, crits,
  glances, strikes-through, applied + self DoTs, heals, `performs`-based ability
  labeling, misses, deaths). No elevation needed. Cost: the game buffers
  chatlog writes, so lines can lag until a flush.

- **ipc** (`src/sources.rs::ipc`) — opens a shared-memory mapping and drains
  `EV_COMBAT_SPAM` records, whose payload is the combat line, through the same
  parser. Zero flush latency, and it can carry group combat. Requires an
  external producer that pushes those records; the reader here is ready for
  when one feeds it. Ring layout constants must match the producer.

## Assets

`assets/` holds the icon (`swglogs-icon.svg` + PNG renders, `favicon.ico`)
and `ui_pda.inc`, Legends' in-game PDA/browser UI page as shipped in the
client, bundled so the browser-window fix can be installed where no loose
copy exists (see The window).
Everything ships inside the single exe: `build.rs` embeds `assets/swglogs.ico`
as the executable's icon (generate it with `python assets/gen_ico.py` after
changing the source PNG) together with the UAC manifest, the window icon is
the 256 px PNG via `include_bytes!`, and the meter page serves `favicon.ico`
from the binary.

## Layout

`lib.rs` the library root · `event.rs` schema + JSON · `parse.rs` line→event
grammar · `meter.rs` encounter aggregation + snapshot JSON · `logwriter.rs`
JSONL sink · `server.rs` std-only HTTP + `page.html` · `sources.rs` the four
sources + the `Sink` fan-out · `uipatch.rs` the in-game browser StickyVisible
patch · `app.rs` config/args + startup wiring shared by both binaries ·
`main.rs` the entry point + self-test · `gui.rs` the egui window (feature `gui`).
