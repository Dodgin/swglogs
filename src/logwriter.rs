//! Real-time structured combat log. Every accepted `Event` is appended as one
//! JSON object (JSONL) and flushed immediately — this is *our* log, so unlike
//! the game's buffered chatlog there is no flush latency, and the schema is
//! whatever `Event::to_json` emits (full outcome/ability context).

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::event::Event;

pub struct LogWriter {
    out: BufWriter<File>,
    pub path: String,
}

impl LogWriter {
    pub fn open(path: &str) -> std::io::Result<LogWriter> {
        if let Some(dir) = Path::new(path).parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(LogWriter {
            out: BufWriter::new(f),
            path: path.to_string(),
        })
    }

    /// Append one event and flush so a tailer/crash sees it immediately.
    pub fn write(&mut self, ev: &Event) {
        let line = ev.to_json();
        if self.out.write_all(line.as_bytes()).is_ok() {
            let _ = self.out.write_all(b"\n");
            let _ = self.out.flush();
        }
    }
}
