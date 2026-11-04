//! One agent command's output: the plain-text log on the host (`log_path`, every line), the
//! text not yet streamed as `tool.output`, and a bounded raw tail that becomes the result
//! (the last 2000 lines or 50 KB, ANSI-stripped, carriage returns collapsed).

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;

use controlroom_supervisor::plain::plain_line;

/// Lines in a result.
pub const MAX_LINES: usize = 2000;
/// Bytes in a result.
pub const MAX_BYTES: usize = 50 * 1024;
/// Raw bytes kept for the result (generous: escape sequences inflate the raw form).
const RAW_TAIL: usize = 1 << 20;

/// Collects a command's output.
pub struct Output {
    log: Option<File>,
    /// The incomplete last line.
    line: Vec<u8>,
    /// Plain text not yet streamed.
    unsent: String,
    /// The last [`RAW_TAIL`] raw bytes.
    tail: VecDeque<u8>,
    /// Raw bytes fell out of the tail.
    dropped: bool,
}

impl Output {
    /// Output logged to `log` (`None`: not logged, e.g. the log couldn't be created).
    pub fn new(log: Option<File>) -> Self {
        Self {
            log,
            line: Vec::new(),
            unsent: String::new(),
            tail: VecDeque::new(),
            dropped: false,
        }
    }

    /// Adds output bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.tail.extend(bytes);
        if self.tail.len() > RAW_TAIL {
            let excess = self.tail.len() - RAW_TAIL;
            self.tail.drain(..excess);
            self.dropped = true;
        }
        let mut rest = bytes;
        while let Some(i) = memchr::memchr(b'\n', rest) {
            self.line.extend_from_slice(&rest[..i]);
            let line = std::mem::take(&mut self.line);
            self.complete(&line);
            rest = &rest[i + 1..];
        }
        self.line.extend_from_slice(rest);
    }

    fn complete(&mut self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        let plain = plain_line(&text);
        if let Some(log) = &mut self.log {
            let _ = writeln!(log, "{plain}");
        }
        self.unsent.push_str(&plain);
        self.unsent.push('\n');
    }

    /// Plain text to stream now (complete lines only).
    pub fn take_unsent(&mut self) -> String {
        std::mem::take(&mut self.unsent)
    }

    /// Ends the output: logs the last partial line and returns the result text and whether it
    /// was cut.
    pub fn finish(&mut self) -> (String, bool) {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.complete(&line);
        }
        if let Some(log) = &mut self.log {
            let _ = log.flush();
        }
        let raw: Vec<u8> = self.tail.iter().copied().collect();
        let text = String::from_utf8_lossy(&raw);
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| plain_line(l).into_owned())
            .collect();
        if self.dropped && !lines.is_empty() {
            // The first line is probably partial.
            lines.remove(0);
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let mut truncated = self.dropped;
        if lines.len() > MAX_LINES {
            lines.drain(..lines.len() - MAX_LINES);
            truncated = true;
        }
        let mut out = lines.join("\n");
        if out.len() > MAX_BYTES {
            let mut cut = out.len() - MAX_BYTES;
            while !out.is_char_boundary(cut) {
                cut += 1;
            }
            out = out[cut..].to_owned();
            if let Some(nl) = out.find('\n') {
                out = out[nl + 1..].to_owned();
            }
            truncated = true;
        }
        (out, truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_streaming_and_a_plain_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("call.log");
        let mut out = Output::new(Some(File::create(&log).unwrap()));
        out.push(b"\x1b[32mok\x1b[0m\r\n10%\r50");
        assert_eq!(out.take_unsent(), "ok\n");
        out.push(b"%\r100%\r\nlast");
        assert_eq!(out.take_unsent(), "100%\n");
        let (text, truncated) = out.finish();
        assert_eq!(text, "ok\n100%\nlast");
        assert!(!truncated);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "ok\n100%\nlast\n");
    }

    #[test]
    fn results_keep_the_tail() {
        let mut out = Output::new(None);
        for i in 0..3000 {
            out.push(format!("line {i}\r\n").as_bytes());
        }
        let (text, truncated) = out.finish();
        assert!(truncated);
        assert_eq!(text.lines().count(), MAX_LINES);
        assert!(text.ends_with("line 2999"));

        let mut out = Output::new(None);
        out.push("x".repeat(80_000).as_bytes());
        out.push(b"\r\nend\r\n");
        let (text, truncated) = out.finish();
        assert!(truncated);
        assert_eq!(text, "end");
    }
}
