//! The seq→offset index: a sidecar file that lets readers start at any seq
//! without scanning the transcript from the beginning.
//!
//! The index is an array of little-endian `u64` byte offsets: entry `k` (at byte
//! `8 * (k - 1)`) is the offset of the line holding seq `k`. It is a cache: the
//! writer appends to it without fsyncing, verifies and repairs it when it opens a
//! transcript, and readers check every entry they use against the transcript (the
//! line must start at that offset and hold that seq), falling back to a scan from
//! the start. A missing, stale or damaged index therefore costs time, never
//! correctness.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::record::Record;
use crate::scan::{LineScanner, MAX_RECORD_BYTES, Scan, is_line_start};

const ENTRY: u64 = 8;

/// The index file of the transcript at `transcript` (`transcript.jsonl` →
/// `transcript.idx`).
pub fn index_path(transcript: &Path) -> PathBuf {
    transcript.with_extension("idx")
}

/// The writer's handle on the index.
#[derive(Debug)]
pub(crate) struct IndexWriter {
    file: File,
    /// Number of entries in the file (the highest indexed seq).
    entries: u64,
}

impl IndexWriter {
    /// Opens or creates the index of `transcript` and brings it up to date with the
    /// complete lines of the transcript.
    ///
    /// The last existing entry is verified; if it is wrong the index is rebuilt.
    /// Entries for records appended since are added by scanning only the new part.
    pub(crate) fn open(transcript: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(index_path(transcript))?;
        let mut entries = file.metadata()?.len() / ENTRY;
        let mut resume = None;
        while entries > 0 {
            let offset = read_entry(&file, entries)?;
            if let Some(next) = verify(transcript, offset, entries)? {
                resume = Some(next);
                break;
            }
            // Entries past the transcript's end (e.g. after a truncated tail) or
            // damage: drop the last entry if it points past the end, else rebuild.
            if offset >= std::fs::metadata(transcript)?.len() {
                entries -= 1;
            } else {
                entries = 0;
            }
        }
        file.set_len(entries * ENTRY)?;
        let mut index = Self { file, entries };
        index.catch_up(transcript, resume.unwrap_or(0))?;
        Ok(index)
    }

    /// Records that seq `seq` starts at `offset`.
    ///
    /// Seqs normally arrive in order. A gap (which only corruption can cause) is
    /// filled with `offset`; readers detect the mismatch and scan instead.
    pub(crate) fn record(&mut self, seq: u64, offset: u64) -> io::Result<()> {
        if seq <= self.entries {
            return Ok(());
        }
        let count = seq - self.entries;
        let mut buf = Vec::with_capacity(crate::scan::to_usize(count * ENTRY)?);
        for _ in 0..count {
            buf.extend_from_slice(&offset.to_le_bytes());
        }
        self.file.write_all_at(&buf, self.entries * ENTRY)?;
        self.entries = seq;
        Ok(())
    }

    /// Indexes every record in `transcript` from byte `from` to its end.
    fn catch_up(&mut self, transcript: &Path, from: u64) -> io::Result<()> {
        let mut scanner = LineScanner::new(File::open(transcript)?, from, MAX_RECORD_BYTES)?;
        loop {
            match scanner.next()? {
                Scan::End => return Ok(()),
                Scan::TooLong { .. } => {}
                Scan::Line { offset } => {
                    if let Ok(record) = serde_json::from_slice::<Record>(scanner.line()) {
                        self.record(record.seq, offset)?;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> u64 {
        self.entries
    }
}

/// Where a reader looking for `from_seq` should start scanning `transcript`.
///
/// Uses the index entry for `from_seq` (or the last entry, if the index doesn't
/// reach that far yet) when it verifies, else the start of the file.
pub(crate) fn start_offset(transcript: &Path, from_seq: u64) -> u64 {
    if from_seq <= 1 {
        return 0;
    }
    let lookup = || -> io::Result<u64> {
        let index = File::open(index_path(transcript))?;
        let seq = from_seq.min(index.metadata()?.len() / ENTRY);
        if seq == 0 {
            return Ok(0);
        }
        let offset = read_entry(&index, seq)?;
        Ok(verify(transcript, offset, seq)?.map_or(0, |_| offset))
    };
    lookup().unwrap_or(0)
}

fn read_entry(index: &File, seq: u64) -> io::Result<u64> {
    let mut entry = [0; ENTRY as usize];
    index.read_exact_at(&mut entry, (seq - 1) * ENTRY)?;
    Ok(u64::from_le_bytes(entry))
}

/// Checks that the line at `offset` holds seq `seq`; returns the offset after it.
fn verify(transcript: &Path, offset: u64, seq: u64) -> io::Result<Option<u64>> {
    let file = File::open(transcript)?;
    if offset >= file.metadata()?.len() || !is_line_start(&file, offset)? {
        return Ok(None);
    }
    let mut scanner = LineScanner::new(file, offset, MAX_RECORD_BYTES)?;
    if scanner.next()? != (Scan::Line { offset }) {
        return Ok(None);
    }
    let matches = serde_json::from_slice::<Record>(scanner.line()).is_ok_and(|r| r.seq == seq);
    Ok(matches.then(|| scanner.offset()))
}

#[cfg(test)]
pub(crate) fn write_raw(transcript: &Path, entries: &[u64]) {
    use std::io::Write;
    let mut file = File::create(index_path(transcript)).unwrap();
    for offset in entries {
        file.write_all(&offset.to_le_bytes()).unwrap();
    }
}
