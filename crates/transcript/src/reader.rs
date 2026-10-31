//! Transcript reader.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::index::start_offset;
use crate::record::Record;
use crate::scan::{LineScanner, MAX_RECORD_BYTES, Scan};

/// Errors from reading a transcript.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// Reading the file failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A complete line that is not a valid record. The reader has already moved past
    /// it, so the caller may keep reading.
    #[error("corrupt transcript line at byte {offset}: {source}")]
    Corrupt {
        /// Byte offset of the line.
        offset: u64,
        /// Why it didn't parse.
        #[source]
        source: serde_json::Error,
    },
    /// A complete line longer than [`MAX_RECORD_BYTES`]; skipped like a corrupt line.
    #[error("transcript line at byte {offset} is {len} bytes, over the limit")]
    TooLong {
        /// Byte offset of the line.
        offset: u64,
        /// Its length without the `\n`.
        len: u64,
    },
}

/// Bytes after the last `\n` that do not (yet) form a complete line.
///
/// While a supervisor is writing this is usually a line in flight; in a transcript
/// whose writer has crashed it is a truncated final line, which readers ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartialTail {
    /// Byte offset where the incomplete line starts.
    pub offset: u64,
    /// Number of bytes in it.
    pub len: u64,
}

/// Incremental reader over a transcript, starting at a given seq.
///
/// [`open`](Reader::open) uses the seq index to start near `from_seq` instead of
/// scanning from the beginning. [`next_record`](Reader::next_record) returns
/// `Ok(None)` at end of file; calling it again later picks up records appended in
/// the meantime, so a `Reader` is also the building block for tailing. Incomplete
/// final lines are never returned; they are buffered until their `\n` arrives and
/// reported by [`partial_tail`](Reader::partial_tail).
#[derive(Debug)]
pub struct Reader {
    scanner: LineScanner,
    from_seq: u64,
}

impl Reader {
    /// Opens `path`; records with `seq < from_seq` are skipped.
    pub fn open(path: &Path, from_seq: u64) -> io::Result<Self> {
        let file = File::open(path)?;
        let offset = start_offset(path, from_seq);
        Ok(Self {
            scanner: LineScanner::new(file, offset, MAX_RECORD_BYTES)?,
            from_seq,
        })
    }

    /// Next complete record with `seq >= from_seq`, or `None` at end of file.
    pub fn next_record(&mut self) -> Result<Option<Record>, ReadError> {
        loop {
            match self.scanner.next()? {
                Scan::End => return Ok(None),
                Scan::TooLong { offset, len } => return Err(ReadError::TooLong { offset, len }),
                Scan::Line { offset } => {
                    match serde_json::from_slice::<Record>(self.scanner.line()) {
                        Ok(record) if record.seq >= self.from_seq => return Ok(Some(record)),
                        Ok(_) => {}
                        Err(source) => return Err(ReadError::Corrupt { offset, source }),
                    }
                }
            }
        }
    }

    /// The incomplete line seen at the last end of file, if any.
    pub fn partial_tail(&self) -> Option<PartialTail> {
        let len = self.scanner.pending_len();
        (len > 0).then_some(PartialTail {
            offset: self.scanner.offset(),
            len,
        })
    }
}

/// Everything [`read_all`] found in a transcript.
#[derive(Debug, Default)]
pub struct ReadAll {
    /// Valid records with `seq >= from_seq`, in file order.
    pub records: Vec<Record>,
    /// Complete lines that failed to parse, as `(byte offset, error message)`.
    pub corrupt: Vec<(u64, String)>,
    /// A truncated final line (e.g. after a crash), which was ignored.
    pub partial_tail: Option<PartialTail>,
}

/// Reads a whole transcript from `from_seq`, skipping (and reporting) bad lines.
pub fn read_all(path: &Path, from_seq: u64) -> io::Result<ReadAll> {
    let mut reader = Reader::open(path, from_seq)?;
    let mut all = ReadAll::default();
    loop {
        match reader.next_record() {
            Ok(Some(record)) => all.records.push(record),
            Ok(None) => break,
            Err(ReadError::Io(e)) => return Err(e),
            Err(e @ (ReadError::Corrupt { offset, .. } | ReadError::TooLong { offset, .. })) => {
                all.corrupt.push((offset, e.to_string()));
            }
        }
    }
    all.partial_tail = reader.partial_tail();
    Ok(all)
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;
    use crate::index::{index_path, write_raw};
    use crate::record::{Body, Line};
    use crate::writer::Writer;

    fn out(raw: &str) -> Body {
        Body::Out(Line::new(raw))
    }

    fn seqs(path: &Path, from_seq: u64) -> Vec<u64> {
        let all = read_all(path, from_seq).unwrap();
        assert!(all.corrupt.is_empty(), "{:?}", all.corrupt);
        all.records.iter().map(|r| r.seq).collect()
    }

    fn write_lines(path: &Path, raws: &[&str]) {
        let (mut w, _) = Writer::open(path).unwrap();
        for raw in raws {
            w.append(out(raw)).unwrap();
        }
    }

    #[test]
    fn reads_from_seq() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        write_lines(&path, &["a", "b", "c", "d"]);
        assert_eq!(seqs(&path, 3), [3, 4]);
        assert_eq!(seqs(&path, 0), [1, 2, 3, 4]);
        assert_eq!(seqs(&path, 5), Vec::<u64>::new());
        assert_eq!(seqs(&path, 99), Vec::<u64>::new());
    }

    #[test]
    fn index_skips_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        write_lines(&path, &["a", "b", "c", "d"]);
        // Corrupt seq 1 in place: a scan from the start would report it.
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        std::os::unix::fs::FileExt::write_all_at(&file, b"X", 0).unwrap();
        drop(file);
        assert_eq!(seqs(&path, 2), [2, 3, 4]);
        assert_eq!(read_all(&path, 1).unwrap().corrupt.len(), 1);
    }

    #[test]
    fn reads_beyond_the_index_scan_from_its_last_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        write_lines(&path, &["a", "b", "c", "d", "e"]);
        let full = std::fs::read(index_path(&path)).unwrap();
        std::fs::write(index_path(&path), &full[..2 * 8]).unwrap();
        assert_eq!(seqs(&path, 4), [4, 5]);
    }

    #[test]
    fn bad_index_entries_fall_back_to_a_full_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        write_lines(&path, &["a", "b", "c", "d"]);
        let full = std::fs::read(index_path(&path)).unwrap();
        let offset_of =
            |seq: usize| u64::from_le_bytes(full[(seq - 1) * 8..seq * 8].try_into().unwrap());
        // Pointing at the wrong record, mid-line, past the end, and nowhere.
        for bad in [offset_of(4), offset_of(2) + 3, 1 << 40, 0] {
            write_raw(&path, &[0, offset_of(2), bad, offset_of(4)]);
            assert_eq!(seqs(&path, 3), [3, 4], "entry {bad}");
        }
        std::fs::remove_file(index_path(&path)).unwrap();
        assert_eq!(seqs(&path, 3), [3, 4]);
    }

    #[test]
    fn truncated_final_line_is_ignored_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        write_lines(&path, &["a", "b"]);
        let complete_len = std::fs::metadata(&path).unwrap().len();
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"seq":3,"ts":1,"dir":"out","ra"#).unwrap();
        drop(f);

        let all = read_all(&path, 0).unwrap();
        assert_eq!(all.records.len(), 2);
        assert!(all.corrupt.is_empty());
        assert_eq!(
            all.partial_tail,
            Some(PartialTail {
                offset: complete_len,
                len: 31
            })
        );
    }

    #[test]
    fn partial_line_completes_on_later_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, br#"{"seq":1,"ts":1,"dir":"out","#).unwrap();
        let mut reader = Reader::open(&path, 0).unwrap();
        assert!(reader.next_record().unwrap().is_none());
        assert!(reader.partial_tail().is_some());

        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"\"raw\":\"x\"}\n").unwrap();
        drop(f);
        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.raw(), Some("x"));
        assert!(reader.partial_tail().is_none());
    }

    #[test]
    fn reader_follows_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let (mut w, _) = Writer::open(&path).unwrap();
        w.append(out("a")).unwrap();
        let mut reader = Reader::open(&path, 0).unwrap();
        assert_eq!(reader.next_record().unwrap().unwrap().raw(), Some("a"));
        assert!(reader.next_record().unwrap().is_none());
        w.append(out("b")).unwrap();
        assert_eq!(reader.next_record().unwrap().unwrap().seq, 2);
    }

    #[test]
    fn corrupt_line_is_skipped_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(
            &path,
            "{\"seq\":1,\"ts\":1,\"dir\":\"out\",\"raw\":\"a\"}\nnot json\n{\"seq\":2,\"ts\":1,\"dir\":\"out\",\"raw\":\"b\"}\n",
        )
        .unwrap();
        let all = read_all(&path, 0).unwrap();
        assert_eq!(all.records.len(), 2);
        assert_eq!(all.corrupt.len(), 1);
        assert_eq!(all.corrupt[0].0, 39);
    }
}
