//! Writing protocol records to stdout, one JSON object per LF-terminated line.

use std::io::Write;
use std::sync::Mutex;

use serde_json::Value;

/// A line writer shared by every task of the agent.
pub struct Out {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl Out {
    /// Write to this process's stdout.
    pub fn stdout() -> Self {
        Self::new(Box::new(std::io::stdout()))
    }

    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }

    /// Write one record and flush. A closed stdout means the client is
    /// gone; pi keeps going in that case, and so does the fake.
    pub fn emit(&self, record: &Value) {
        let mut line = record.to_string();
        line.push('\n');
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.flush());
    }
}
