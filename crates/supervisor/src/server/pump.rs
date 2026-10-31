//! Logging the child's stdout and stderr, line by line, with a line cap.

use crate::diag::diag;
use std::io::{ErrorKind, Read};
use std::sync::Arc;
use std::time::Duration;

use controlroom_transcript::{Body, Line};

use super::log::{AppendError, Log};

/// How long a pump waits before retrying a failed append (e.g. a full disk). The
/// child is backpressured meanwhile rather than losing output.
const APPEND_RETRY: Duration = Duration::from_secs(1);

/// Splits a byte stream into [`Line`]s of at most `max` bytes.
///
/// An overlong line keeps its first `max` bytes (cut back to a UTF-8 character
/// boundary) and is emitted once its end arrives, with `truncated` counting the
/// dropped bytes, so memory stays bounded by `max`.
pub(crate) struct LineSplitter {
    max: usize,
    buf: Vec<u8>,
    dropped: u64,
}

impl LineSplitter {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            max,
            buf: Vec::new(),
            dropped: 0,
        }
    }

    /// Feeds bytes, appending every completed line to `out`.
    pub(crate) fn push(&mut self, mut chunk: &[u8], out: &mut Vec<Line>) {
        while !chunk.is_empty() {
            let newline = chunk.iter().position(|&b| b == b'\n');
            let part = &chunk[..newline.unwrap_or(chunk.len())];
            let room = self.max - self.buf.len();
            let kept = part.len().min(room);
            self.buf.extend_from_slice(&part[..kept]);
            self.dropped += (part.len() - kept) as u64;
            match newline {
                Some(i) => {
                    out.push(self.take());
                    chunk = &chunk[i + 1..];
                }
                None => chunk = &[],
            }
        }
    }

    /// Emits an unterminated final line, if any.
    pub(crate) fn finish(&mut self, out: &mut Vec<Line>) {
        if !self.buf.is_empty() || self.dropped > 0 {
            out.push(self.take());
        }
    }

    fn take(&mut self) -> Line {
        let mut bytes = std::mem::take(&mut self.buf);
        let mut truncated = std::mem::take(&mut self.dropped);
        if truncated > 0
            && let Err(e) = std::str::from_utf8(&bytes)
            && e.error_len().is_none()
        {
            // The cut split a character: drop its leading bytes too.
            truncated += (bytes.len() - e.valid_up_to()) as u64;
            bytes.truncate(e.valid_up_to());
        }
        match String::from_utf8(bytes) {
            Ok(raw) => Line {
                raw,
                truncated,
                lossy: false,
            },
            Err(e) => Line {
                raw: String::from_utf8_lossy(e.as_bytes()).into_owned(),
                truncated,
                lossy: true,
            },
        }
    }
}

/// Logs every line of `src` until EOF or until the log is closed. Runs on its own
/// thread; blocking reads and appends are fine here.
pub(crate) fn pump(mut src: impl Read, log: Arc<Log>, max: usize, wrap: fn(Line) -> Body) {
    let mut splitter = LineSplitter::new(max);
    let mut chunk = vec![0; 64 * 1024];
    let mut lines = Vec::new();
    loop {
        let eof = match src.read(&mut chunk) {
            Ok(0) => true,
            Ok(n) => {
                splitter.push(&chunk[..n], &mut lines);
                false
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                diag!("reading child output: {e}");
                true
            }
        };
        if eof {
            splitter.finish(&mut lines);
        }
        for line in lines.drain(..) {
            if !append_retrying(&log, wrap(line)) {
                return;
            }
        }
        if eof {
            return;
        }
    }
}

/// Appends, retrying while the disk fails. False once the log is closed.
fn append_retrying(log: &Log, body: Body) -> bool {
    let mut reported = false;
    loop {
        match log.append(body.clone()) {
            Ok(_) => return true,
            Err(AppendError::Closed) => return false,
            Err(e) => {
                if !reported {
                    diag!("transcript append failed, retrying: {e}");
                    reported = true;
                }
                std::thread::sleep(APPEND_RETRY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(max: usize, chunks: &[&[u8]]) -> Vec<Line> {
        let mut splitter = LineSplitter::new(max);
        let mut out = Vec::new();
        for chunk in chunks {
            splitter.push(chunk, &mut out);
        }
        splitter.finish(&mut out);
        out
    }

    fn line(raw: &str, truncated: u64) -> Line {
        Line {
            raw: raw.into(),
            truncated,
            lossy: false,
        }
    }

    #[test]
    fn splits_across_chunks() {
        assert_eq!(
            split(100, &[b"ab", b"c\nd\n\n", b"tail"]),
            [line("abc", 0), line("d", 0), line("", 0), line("tail", 0)]
        );
    }

    #[test]
    fn caps_long_lines_and_counts_dropped_bytes() {
        assert_eq!(
            split(4, &[b"1234567", b"89\nok\n", b"abcdef"]),
            [line("1234", 5), line("ok", 0), line("abcd", 2)]
        );
        assert_eq!(split(4, &[b"1234\n"]), [line("1234", 0)]);
    }

    #[test]
    fn cut_never_splits_a_character() {
        // "é" is two bytes; a cap of 2 after "a" would split it.
        assert_eq!(split(2, &["aé\n".as_bytes()]), [line("a", 2)]);
    }

    #[test]
    fn invalid_utf8_is_marked_lossy() {
        let lines = split(100, &[b"a\xffb\n"]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].raw, "a\u{fffd}b");
        assert!(lines[0].lossy);
    }
}
