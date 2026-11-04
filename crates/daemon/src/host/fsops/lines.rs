//! A per-file line-offset index, so a windowed read near the end of a large file seeks
//! instead of scanning (design §6: "per-file line-offset index keyed by (inode, size,
//! mtime)"). The index is built by one scan and cached while the file is unchanged.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex, PoisonError};

/// Lines between two checkpoints.
const STRIDE: u64 = 1024;
/// Files whose index is kept.
const CACHED_FILES: usize = 32;

/// Identifies one version of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
}

impl Key {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        }
    }
}

/// Where lines start in one version of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    /// Byte offset of line `1 + k * STRIDE`, for every such line.
    checkpoints: Vec<u64>,
    /// `\n` bytes in the file.
    newlines: u64,
}

impl LineIndex {
    /// Scans `file` from the start.
    fn build(file: &mut File) -> io::Result<Self> {
        let mut checkpoints = vec![0];
        let mut newlines = 0u64;
        let mut offset = 0u64;
        let mut buf = vec![0; 256 * 1024];
        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            for i in memchr::memchr_iter(b'\n', &buf[..n]) {
                newlines += 1;
                if newlines.is_multiple_of(STRIDE) {
                    checkpoints.push(offset + i as u64 + 1);
                }
            }
            offset += n as u64;
        }
        Ok(Self {
            checkpoints,
            newlines,
        })
    }

    /// Lines counted like pi (`text.split("\n")`): a trailing newline adds an empty last
    /// line, and an empty file has one.
    pub fn total_lines(&self) -> u64 {
        self.newlines + 1
    }

    /// The nearest checkpoint at or before `line` (from 1): `(its line, its offset)`.
    pub fn seek_point(&self, line: u64) -> (u64, u64) {
        let k = (line.saturating_sub(1) / STRIDE).min(self.checkpoints.len() as u64 - 1);
        (1 + k * STRIDE, self.checkpoints[k as usize])
    }
}

/// Indexes of recently read files.
#[derive(Default)]
pub struct LineIndexCache(Mutex<HashMap<String, (Key, Arc<LineIndex>)>>);

impl LineIndexCache {
    /// The index of `file` (at `path`), built or reused.
    pub fn get(&self, path: &str, file: &mut File) -> io::Result<Arc<LineIndex>> {
        let key = Key::of(&file.metadata()?);
        {
            let cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((cached, index)) = cache.get(path)
                && *cached == key
            {
                return Ok(index.clone());
            }
        }
        let index = Arc::new(LineIndex::build(file)?);
        let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= CACHED_FILES {
            cache.clear();
        }
        cache.insert(path.to_owned(), (key, index.clone()));
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};

    use super::*;

    #[test]
    fn checkpoints_point_at_line_starts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let mut f = File::create(&path).unwrap();
        for i in 1..=3000 {
            writeln!(f, "line {i}").unwrap();
        }
        let mut f = File::open(&path).unwrap();
        let cache = LineIndexCache::default();
        let index = cache.get("f", &mut f).unwrap();
        assert_eq!(index.total_lines(), 3001);
        let (line, offset) = index.seek_point(2500);
        assert_eq!(line, 2049);
        f.seek(SeekFrom::Start(offset)).unwrap();
        let mut start = [0; 10];
        f.read_exact(&mut start).unwrap();
        assert_eq!(&start, b"line 2049\n");
        assert!(Arc::ptr_eq(&index, &cache.get("f", &mut f).unwrap()));
        assert_eq!(
            LineIndex::build(&mut File::open(&path).unwrap()).unwrap(),
            *index
        );
    }

    #[test]
    fn empty_and_unterminated_files_count_like_pi() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, "").unwrap();
        let index = LineIndex::build(&mut File::open(&path).unwrap()).unwrap();
        assert_eq!(index.total_lines(), 1);
        std::fs::write(&path, "a\nb").unwrap();
        let index = LineIndex::build(&mut File::open(&path).unwrap()).unwrap();
        assert_eq!(index.total_lines(), 2);
        assert_eq!(index.seek_point(2), (1, 0));
    }
}
