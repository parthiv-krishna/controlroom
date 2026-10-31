//! Low-level line scanning over a transcript file.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::FileExt;

/// Longest transcript line (one serialized record) that readers accept.
///
/// The writer refuses to append longer records, so a longer line can only come
/// from corruption; readers skip it without buffering it.
pub const MAX_RECORD_BYTES: usize = 128 << 20;

/// Chunk size for scanning backwards.
const BACK_CHUNK: u64 = 64 * 1024;

/// What [`LineScanner::next`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scan {
    /// A complete line starting at `offset`; its bytes are in [`LineScanner::line`].
    Line { offset: u64 },
    /// A complete line starting at `offset` that is longer than the limit; skipped.
    TooLong { offset: u64, len: u64 },
    /// No further complete line yet.
    End,
}

/// Reads `\n`-terminated lines with a length limit, tracking byte offsets.
///
/// An incomplete final line stays buffered (or counted, if already over the limit)
/// and is completed by a later call once the file has grown.
#[derive(Debug)]
pub(crate) struct LineScanner {
    inner: BufReader<File>,
    /// File offset of the first byte of the line being assembled.
    offset: u64,
    /// The line being assembled (complete after a `Scan::Line`).
    pending: Vec<u8>,
    /// Bytes of the line being assembled that were discarded for exceeding the limit.
    overflow: u64,
    /// Whether `pending` holds a complete line already returned.
    complete: bool,
    max: usize,
}

impl LineScanner {
    /// Scans `file` from byte `offset`.
    pub(crate) fn new(mut file: File, offset: u64, max: usize) -> io::Result<Self> {
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            inner: BufReader::new(file),
            offset,
            pending: Vec::new(),
            overflow: 0,
            complete: false,
            max,
        })
    }

    /// The line returned by the last `Scan::Line`, without its `\n`.
    pub(crate) fn line(&self) -> &[u8] {
        &self.pending
    }

    /// Offset just past the last complete line (where the next line starts).
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// Bytes of an incomplete final line seen at the last `Scan::End`.
    pub(crate) fn pending_len(&self) -> u64 {
        if self.complete {
            0
        } else {
            self.pending.len() as u64 + self.overflow
        }
    }

    /// Advances to the next complete line.
    pub(crate) fn next(&mut self) -> io::Result<Scan> {
        if self.complete {
            self.pending.clear();
            self.complete = false;
        }
        loop {
            let available = match self.inner.fill_buf() {
                Ok(buf) => buf,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                return Ok(Scan::End);
            }
            let newline = available.iter().position(|&b| b == b'\n');
            let take = newline.unwrap_or(available.len());
            let over = self.overflow > 0 || self.pending.len() + take > self.max;
            if over {
                self.overflow += (self.pending.len() + take) as u64;
                self.pending.clear();
            } else {
                self.pending.extend_from_slice(&available[..take]);
            }
            let Some(_) = newline else {
                self.inner.consume(take);
                continue;
            };
            self.inner.consume(take + 1);
            let offset = self.offset;
            if over {
                let len = self.overflow;
                self.overflow = 0;
                self.offset += len + 1;
                return Ok(Scan::TooLong { offset, len });
            }
            self.offset += self.pending.len() as u64 + 1;
            self.complete = true;
            return Ok(Scan::Line { offset });
        }
    }
}

/// Position of the last `\n` in `file[..end]`.
pub(crate) fn rfind_newline(file: &File, end: u64) -> io::Result<Option<u64>> {
    let mut chunk_end = end;
    let mut buf = Vec::new();
    while chunk_end > 0 {
        let chunk_start = chunk_end.saturating_sub(BACK_CHUNK);
        buf.resize(to_usize(chunk_end - chunk_start)?, 0);
        file.read_exact_at(&mut buf, chunk_start)?;
        if let Some(i) = buf.iter().rposition(|&b| b == b'\n') {
            return Ok(Some(chunk_start + i as u64));
        }
        chunk_end = chunk_start;
    }
    Ok(None)
}

/// Whether `offset` is the start of a line (the file start or just after a `\n`).
pub(crate) fn is_line_start(file: &File, offset: u64) -> io::Result<bool> {
    if offset == 0 {
        return Ok(true);
    }
    let mut byte = [0];
    match file.read_exact_at(&mut byte, offset - 1) {
        Ok(()) => Ok(byte[0] == b'\n'),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

pub(crate) fn to_usize(n: u64) -> io::Result<usize> {
    usize::try_from(n).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn scanner(content: &[u8], max: usize) -> (tempfile::NamedTempFile, LineScanner) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(content).unwrap();
        let scanner = LineScanner::new(tmp.reopen().unwrap(), 0, max).unwrap();
        (tmp, scanner)
    }

    #[test]
    fn splits_lines_with_offsets() {
        let (_tmp, mut s) = scanner(b"ab\n\ncde\nf", 100);
        assert_eq!(s.next().unwrap(), Scan::Line { offset: 0 });
        assert_eq!(s.line(), b"ab");
        assert_eq!(s.next().unwrap(), Scan::Line { offset: 3 });
        assert_eq!(s.line(), b"");
        assert_eq!(s.next().unwrap(), Scan::Line { offset: 4 });
        assert_eq!(s.line(), b"cde");
        assert_eq!(s.next().unwrap(), Scan::End);
        assert_eq!((s.offset(), s.pending_len()), (8, 1));
    }

    #[test]
    fn overlong_lines_are_skipped_and_scanning_resumes() {
        let (_tmp, mut s) = scanner(b"12345\nab\n1234567890123\nc\n", 4);
        assert_eq!(s.next().unwrap(), Scan::TooLong { offset: 0, len: 5 });
        assert_eq!(s.next().unwrap(), Scan::Line { offset: 6 });
        assert_eq!(s.line(), b"ab");
        assert_eq!(s.next().unwrap(), Scan::TooLong { offset: 9, len: 13 });
        assert_eq!(s.next().unwrap(), Scan::Line { offset: 23 });
        assert_eq!(s.line(), b"c");
    }

    #[test]
    fn incomplete_line_completes_after_growth() {
        let (mut tmp, mut s) = scanner(b"ab", 100);
        assert_eq!(s.next().unwrap(), Scan::End);
        assert_eq!(s.pending_len(), 2);
        tmp.write_all(b"c\n").unwrap();
        assert_eq!(s.next().unwrap(), Scan::Line { offset: 0 });
        assert_eq!(s.line(), b"abc");
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn backwards_newline_search() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let mut content = b"a\n".to_vec();
        content.extend(std::iter::repeat_n(b'x', 3 * BACK_CHUNK as usize));
        tmp.write_all(&content).unwrap();
        let file = tmp.reopen().unwrap();
        let len = content.len() as u64;
        assert_eq!(rfind_newline(&file, len).unwrap(), Some(1));
        assert_eq!(rfind_newline(&file, 1).unwrap(), None);
        assert!(is_line_start(&file, 2).unwrap());
        assert!(!is_line_start(&file, 3).unwrap());
    }
}
