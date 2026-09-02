//! swglogs — SWG Legends combat logs + Details-style meter.
//!
//! One normalized combat-event stream, two sinks: a live web meter and a
//! real-time structured log (JSONL). Sources are pluggable:
//!   --source memory   (default) read the client's combat scrollback directly
//!                     (run as Administrator; zero flush latency)
//!   --source chatlog  tail the player's own chatlog file
//!   --source ipc      drain the shared-memory IPC ring (external producer)
//!   --source demo     synthetic combat, no game needed
//!
//! The library holds everything; `src/main.rs` is the one executable: it
//! opens the `gui` window (feature on by default) or runs headless.

pub mod app;
pub mod event;
#[cfg(feature = "gui")]
pub mod gui;
pub mod logwriter;
pub mod meter;
pub mod parse;
pub mod server;
pub mod sources;
pub mod uipatch;
