//! Reading newline-delimited messages with a length cap.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// What [`read_line`] found.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Line {
    /// A complete line (without its newline) is in the buffer. A final line
    /// without a trailing newline counts too.
    Complete,
    /// The line exceeded the cap; it was consumed and discarded.
    TooLong,
    /// End of stream, with no partial line pending.
    Eof,
}

/// Reads one line into `buf` (cleared first), keeping at most `max` bytes.
///
/// A longer line is consumed through its newline and reported as
/// [`Line::TooLong`], so one oversized message never desynchronizes the
/// stream or grows memory without bound.
pub(super) async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Line> {
    buf.clear();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(match (too_long, buf.is_empty()) {
                (true, _) => Line::TooLong,
                (false, true) => Line::Eof,
                (false, false) => Line::Complete,
            });
        }
        let (chunk, consumed, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], i + 1, true),
            None => (available, available.len(), false),
        };
        if !too_long {
            if buf.len() + chunk.len() > max {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        reader.consume(consumed);
        if done {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Complete
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn caps_lines_and_resynchronizes() {
        let input = b"short\nthis line is too long\nok\nlast".as_slice();
        // A tiny buffer makes lines span several reads.
        let mut reader = BufReader::with_capacity(4, input);
        let mut buf = Vec::new();
        let mut read = async || {
            let line = read_line(&mut reader, &mut buf, 8).await.unwrap();
            (line, String::from_utf8(buf.clone()).unwrap())
        };
        assert_eq!(read().await, (Line::Complete, "short".into()));
        assert_eq!(read().await, (Line::TooLong, String::new()));
        assert_eq!(read().await, (Line::Complete, "ok".into()));
        assert_eq!(read().await, (Line::Complete, "last".into()));
        assert_eq!(read().await, (Line::Eof, String::new()));
    }
}
