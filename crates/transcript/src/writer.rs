//! Append-only transcript writer.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::index::IndexWriter;
use crate::record::{Body, Record, unix_ms_now};
use crate::scan::{LineScanner, MAX_RECORD_BYTES, Scan, rfind_newline, to_usize};

/// Appends records to a transcript file and maintains its seq index.
///
/// The writer holds an exclusive `flock` on the file for its whole lifetime, so only
/// one writer (one supervisor) owns a transcript at a time. Every [`append`] writes a
/// complete line with one `write(2)`, so readers see at most a temporarily incomplete
/// final line. Durability is explicit: [`sync`] (or a [`Syncer`]) fsyncs.
///
/// [`append`]: Writer::append
/// [`sync`]: Writer::sync
#[derive(Debug)]
pub struct Writer {
    file: File,
    /// Length of the file, which always ends with a complete line.
    len: u64,
    last_seq: u64,
    /// `None` once an index write failed; the next writer repairs the index.
    index: Option<IndexWriter>,
    /// Set if a failed append could not be rolled back; appends are refused.
    poisoned: bool,
}

/// What [`Writer::open`] found in an existing transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenReport {
    /// Highest seq already in the file (0 if empty).
    pub last_seq: u64,
    /// Bytes of an incomplete final line (from a crash mid-write) that were truncated.
    pub truncated_bytes: u64,
}

/// Fsyncs a transcript without the [`Writer`], so appends can continue meanwhile.
#[derive(Debug)]
pub struct Syncer {
    file: File,
}

impl Syncer {
    /// Fsyncs the data of every record appended before this call.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }
}

impl Writer {
    /// Opens (creating with mode 0600 if needed) a transcript for appending.
    ///
    /// Fails with [`io::ErrorKind::WouldBlock`] if another writer holds the lock. An
    /// incomplete final line left by a crash is truncated away, so new records never
    /// get glued onto it; the number of dropped bytes is reported. The seq index is
    /// verified and brought up to date.
    pub fn open(path: &Path) -> io::Result<(Self, OpenReport)> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("transcript {} is locked by another writer", path.display()),
                ));
            }
            Err(TryLockError::Error(e)) => return Err(e),
        }

        let len = file.metadata()?.len();
        let complete_len = rfind_newline(&file, len)?.map_or(0, |pos| pos + 1);
        let truncated_bytes = len - complete_len;
        if truncated_bytes > 0 {
            file.set_len(complete_len)?;
            file.sync_data()?;
        }
        let last_seq = last_seq(&file, complete_len)?;
        // The index is a cache: if it can't be opened, readers scan instead.
        let index = IndexWriter::open(path).ok();
        Ok((
            Self {
                file,
                len: complete_len,
                last_seq,
                index,
                poisoned: false,
            },
            OpenReport {
                last_seq,
                truncated_bytes,
            },
        ))
    }

    /// Appends a record with the next seq and the current time, returning it.
    ///
    /// The line reaches the page cache (visible to readers) but is not fsynced. A
    /// failed write is rolled back so the file still ends with a complete line.
    pub fn append(&mut self, body: Body) -> io::Result<Record> {
        if self.poisoned {
            return Err(io::Error::other(
                "transcript writer is unusable after a failed rollback",
            ));
        }
        let record = Record {
            seq: self.last_seq + 1,
            ts: unix_ms_now(),
            body,
        };
        let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');
        if line.len() > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "record of {} bytes exceeds the {MAX_RECORD_BYTES}-byte limit",
                    line.len()
                ),
            ));
        }
        if let Err(e) = self.file.write_all(&line) {
            if self.file.set_len(self.len).is_err() {
                self.poisoned = true;
            }
            return Err(e);
        }
        let offset = self.len;
        self.len += line.len() as u64;
        self.last_seq = record.seq;
        if let Some(index) = &mut self.index
            && index.record(record.seq, offset).is_err()
        {
            self.index = None;
        }
        Ok(record)
    }

    /// Fsyncs every appended record.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// A handle that fsyncs this transcript independently of the writer.
    pub fn syncer(&self) -> io::Result<Syncer> {
        Ok(Syncer {
            file: self.file.try_clone()?,
        })
    }

    /// Seq of the last record in the file (0 if empty).
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }
}

/// Seq of the last parseable record in `file[..complete_len]`, scanning backwards
/// past corrupt lines (0 if there is none).
fn last_seq(file: &File, complete_len: u64) -> io::Result<u64> {
    let mut line_end = complete_len;
    while line_end > 0 {
        let line_start = rfind_newline(file, line_end - 1)?.map_or(0, |p| p + 1);
        let len = to_usize(line_end - line_start)?;
        if len <= MAX_RECORD_BYTES {
            let mut scanner = LineScanner::new(file.try_clone()?, line_start, len)?;
            if let Scan::Line { .. } = scanner.next()?
                && let Ok(record) = serde_json::from_slice::<Record>(scanner.line())
            {
                return Ok(record.seq);
            }
        }
        line_end = line_start;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::index::{index_path, write_raw};
    use crate::read_all;
    use crate::record::Line;

    fn out(raw: &str) -> Body {
        Body::Out(Line::new(raw))
    }

    fn raws(path: &Path, from_seq: u64) -> Vec<String> {
        read_all(path, from_seq)
            .unwrap()
            .records
            .iter()
            .filter_map(|r| r.raw().map(str::to_owned))
            .collect()
    }

    #[test]
    fn appends_monotonic_seq_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        {
            let (mut w, report) = Writer::open(&path).unwrap();
            assert_eq!(report.last_seq, 0);
            assert_eq!(w.append(out("a")).unwrap().seq, 1);
            assert_eq!(w.append(out("b")).unwrap().seq, 2);
            w.sync().unwrap();
            w.syncer().unwrap().sync().unwrap();
        }
        let (mut w, report) = Writer::open(&path).unwrap();
        assert_eq!(
            report,
            OpenReport {
                last_seq: 2,
                truncated_bytes: 0
            }
        );
        assert_eq!(w.append(out("c")).unwrap().seq, 3);
        assert_eq!(raws(&path, 0), ["a", "b", "c"]);
    }

    #[test]
    fn second_writer_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let _w = Writer::open(&path).unwrap();
        let err = Writer::open(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn truncates_partial_tail_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        {
            let (mut w, _) = Writer::open(&path).unwrap();
            w.append(out("a")).unwrap();
        }
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"seq":2,"ts":1,"dir":"o"#).unwrap();
        drop(f);

        let (mut w, report) = Writer::open(&path).unwrap();
        assert_eq!(report.last_seq, 1);
        assert_eq!(report.truncated_bytes, 24);
        assert_eq!(w.append(out("b")).unwrap().seq, 2);
        let all = read_all(&path, 0).unwrap();
        assert!(all.corrupt.is_empty());
        assert!(all.partial_tail.is_none());
        assert_eq!(all.records.len(), 2);
    }

    #[test]
    fn long_last_line_spanning_scan_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let big = "x".repeat(300 * 1024);
        {
            let (mut w, _) = Writer::open(&path).unwrap();
            w.append(out("a")).unwrap();
            w.append(out(&big)).unwrap();
        }
        let (_, report) = Writer::open(&path).unwrap();
        assert_eq!(report.last_seq, 2);
    }

    #[test]
    fn corrupt_last_line_falls_back_to_earlier_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        {
            let (mut w, _) = Writer::open(&path).unwrap();
            w.append(out("a")).unwrap();
            w.append(out("b")).unwrap();
        }
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"garbage\n").unwrap();
        drop(f);
        let (mut w, report) = Writer::open(&path).unwrap();
        assert_eq!(report.last_seq, 2);
        assert_eq!(w.append(out("c")).unwrap().seq, 3);
        assert_eq!(raws(&path, 3), ["c"]);
    }

    #[test]
    fn index_tracks_appends_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        {
            let (mut w, _) = Writer::open(&path).unwrap();
            for i in 0..50 {
                w.append(out(&format!("line {i}"))).unwrap();
            }
            assert_eq!(w.index.as_ref().unwrap().entries(), 50);
        }
        assert_eq!(std::fs::metadata(index_path(&path)).unwrap().len(), 50 * 8);
        let (mut w, _) = Writer::open(&path).unwrap();
        assert_eq!(w.index.as_ref().unwrap().entries(), 50);
        w.append(out("more")).unwrap();
        assert_eq!(raws(&path, 50), ["line 49", "more"]);
    }

    #[test]
    fn index_is_repaired_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        {
            let (mut w, _) = Writer::open(&path).unwrap();
            for i in 1..=10 {
                w.append(out(&i.to_string())).unwrap();
            }
        }
        let expected: Vec<String> = (4..=10).map(|i| i.to_string()).collect();
        let entries = |w: &Writer| w.index.as_ref().unwrap().entries();

        // Missing index: rebuilt.
        std::fs::remove_file(index_path(&path)).unwrap();
        let (w, _) = Writer::open(&path).unwrap();
        assert_eq!(entries(&w), 10);
        drop(w);
        assert_eq!(raws(&path, 4), expected);

        // Lagging index (e.g. a crash before the entry was written): caught up.
        let full = std::fs::read(index_path(&path)).unwrap();
        std::fs::write(index_path(&path), &full[..3 * 8 + 5]).unwrap();
        let (w, _) = Writer::open(&path).unwrap();
        assert_eq!(entries(&w), 10);
        drop(w);
        assert_eq!(std::fs::read(index_path(&path)).unwrap(), full);

        // Garbage entries: rebuilt.
        write_raw(&path, &[0; 10]);
        let (w, _) = Writer::open(&path).unwrap();
        assert_eq!(entries(&w), 10);
        drop(w);
        assert_eq!(std::fs::read(index_path(&path)).unwrap(), full);
    }

    #[test]
    fn index_entries_past_a_truncated_tail_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        {
            let (mut w, _) = Writer::open(&path).unwrap();
            for raw in ["a", "b", "c"] {
                w.append(out(raw)).unwrap();
            }
        }
        // Simulate a crash that kept the index entry for seq 3 but only part of its line.
        let len = std::fs::metadata(&path).unwrap().len();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(len - 5).unwrap();
        drop(file);

        let (mut w, report) = Writer::open(&path).unwrap();
        assert_eq!(report.last_seq, 2);
        assert_eq!(w.index.as_ref().unwrap().entries(), 2);
        assert_eq!(w.append(out("c2")).unwrap().seq, 3);
        assert_eq!(w.index.as_ref().unwrap().entries(), 3);
        drop(w);
        assert_eq!(raws(&path, 3), ["c2"]);
        assert_eq!(raws(&path, 2), ["b", "c2"]);
    }

    #[test]
    fn oversized_record_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let (mut w, _) = Writer::open(&path).unwrap();
        let huge = "x".repeat(MAX_RECORD_BYTES);
        let err = w.append(out(&huge)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(w.append(out("ok")).unwrap().seq, 1);
    }
}
