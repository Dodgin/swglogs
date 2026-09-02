# swglogs

SWG Legends combat logs + Details-style meter, in Rust. **One normalized
combat-event stream, two sinks:** a live web meter and a real-time structured
log (JSONL). The event source is pluggable, so the same meter/log run on top of
either input:

```
                                      ┌─ meter aggregator ── http://127.0.0.1:8666/  (in-game /browser)
source ──> Event (normalized) ──┤
                                      └─ real-time log ────── combat-log.jsonl  (flushed per event)

sources:  chatlog tail          the player's own chatlog          (works today, sanctioned)
          IPC ring              combat lines from an external producer   (zero flush latency)
          demo                  synthetic combat                  (no game needed)
```

Dependency-free (std only) — builds with no registry access.

## Build & run

```
cargo build --release
./target/release/swglogs                 # tail newest chatlog, serve :8666, log combat-log.jsonl
./target/release/swglogs --source demo   # synthetic combat, preview the page
./target/release/swglogs --source ipc    # drain the IPC ring (needs a producer feeding EV_COMBAT_SPAM)
./target/release/swglogs --selftest      # parser/aggregator checks vs real log lines
```

In game: `/browser http://127.0.0.1:8666/` (also `/healing`, `/taken` open on
that metric — the client spawns an independent browser window per call, so you
can run several).

### Options

`--source chatlog|ipc|demo` · `--port N` · `--host H` · `--gap S` (encounter
gap, default 8) · `--player NAME` (highlighted; unset by default) · `--profiles
DIR` · `--log FILE` (pin an exact chatlog) · `--replay` (parse the whole log
first, then tail) · `--out FILE` (JSONL log, default `combat-log.jsonl`) ·
`--no-log` · `--selftest`.

## The two outputs

**Meter page** — ranked bars, header cycles Now/Last/All and Damage/Healing/
Taken; click a row for its ability breakdown (hits/crits/max). Shows a `⚠Ns`
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

- **chatlog** (`src/sources.rs::chatlog_tail`) — finds the newest
  `profiles/*/*/*_chatlog.txt`, tails it, parses each `[Combat]` line
  (`src/parse.rs`, grammar verified against a real Sep-2026 log — hits, crits,
  glances, strikes-through, applied + self DoTs, heals, `performs`-based ability
  labeling, misses, deaths). Works today, fully sanctioned. Cost: the game
  buffers chatlog writes, so lines can lag until a flush.

- **ipc** (`src/sources.rs::ipc`) — opens a shared-memory mapping and drains
  `EV_COMBAT_SPAM` records, whose payload is the combat line, through the same
  parser. Zero flush latency, and it can carry group combat. Requires an
  external producer that pushes those records; the reader here is ready for
  when one feeds it. Ring layout constants must match the producer.

## Layout

`event.rs` schema + JSON · `parse.rs` line→event grammar · `meter.rs` encounter
aggregation + snapshot JSON · `logwriter.rs` JSONL sink · `server.rs` std-only
HTTP + `page.html` · `sources.rs` the three sources + the `Sink` fan-out ·
`main.rs` args/wiring/self-test.
