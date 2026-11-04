//! `read` (a window of a text file, with pi's limits) and `read_bytes`.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use controlroom_protocol::RemotePath;
use controlroom_protocol::host::{HostResult, ReadContent, ReadCut};
use sha2::{Digest, Sha256};

use super::lines::LineIndexCache;
use super::{fs_error, invalid};

/// pi's line limit per read.
pub const MAX_LINES: u64 = 2000;
/// pi's byte limit per read.
pub const MAX_BYTES: usize = 50 * 1024;
/// Most bytes one `read_bytes` returns.
pub const MAX_READ_BYTES: u64 = 20 << 20;

/// Image types pi attaches, by magic number.
fn image_mime(head: &[u8]) -> Option<&'static str> {
    const MAGIC: [(&[u8], &str); 5] = [
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"BM", "image/bmp"),
    ];
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    MAGIC
        .iter()
        .find(|(magic, _)| head.starts_with(magic))
        .map(|(_, mime)| *mime)
}

fn open_file(path: &str) -> Result<(File, u64), controlroom_protocol::RpcError> {
    let file = File::open(path).map_err(|e| fs_error(path, e))?;
    let meta = file.metadata().map_err(|e| fs_error(path, e))?;
    if meta.is_dir() {
        return Err(invalid(format!("{path} is a directory")));
    }
    Ok((file, meta.len()))
}

/// A window of lines `offset..` (from 1) of the file at `path`, like pi's read tool: lines as
/// `text.split("\n")`, at most `limit`, [`MAX_LINES`] and [`MAX_BYTES`].
pub fn read(
    path: &str,
    cache: &LineIndexCache,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<HostResult, controlroom_protocol::RpcError> {
    let (mut file, size) = open_file(path)?;
    let mut head = [0; 16];
    let n = read_up_to(&mut file, &mut head).map_err(|e| fs_error(path, e))?;
    if let Some(mime) = image_mime(&head[..n]) {
        return Ok(HostResult::Read {
            path: RemotePath::from(path),
            size,
            content: ReadContent::Image {
                mime_type: mime.to_owned(),
            },
        });
    }
    file.rewind().map_err(|e| fs_error(path, e))?;
    let index = cache.get(path, &mut file).map_err(|e| fs_error(path, e))?;
    let total = index.total_lines();
    let start = offset.unwrap_or(1).max(1);
    if start > total {
        return Err(invalid(format!(
            "Offset {start} is beyond end of file ({total} lines total)"
        )));
    }
    let (mut line, seek) = index.seek_point(start);
    file.seek(SeekFrom::Start(seek))
        .map_err(|e| fs_error(path, e))?;
    let mut reader = BufReader::new(file);
    while line < start {
        skip_line(&mut reader).map_err(|e| fs_error(path, e))?;
        line += 1;
    }
    let end = limit.map_or(total, |l| (start + l.max(1) - 1).min(total));
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut cut = None;
    let mut first_line_too_long = false;
    while line <= end {
        if lines.len() as u64 >= MAX_LINES {
            cut = Some(ReadCut::Lines);
            break;
        }
        let text = read_line_capped(&mut reader, MAX_BYTES + 1).map_err(|e| fs_error(path, e))?;
        let cost = text.len() + usize::from(!lines.is_empty());
        if bytes + cost > MAX_BYTES {
            cut = Some(ReadCut::Bytes);
            first_line_too_long = lines.is_empty();
            break;
        }
        bytes += cost;
        lines.push(text);
        line += 1;
    }
    Ok(HostResult::Read {
        path: RemotePath::from(path),
        size,
        content: ReadContent::Text {
            lines,
            start_line: start,
            total_lines: total,
            cut,
            first_line_too_long,
        },
    })
}

/// Raw bytes `offset..offset+length` (at most [`MAX_READ_BYTES`]), base64-encoded, with the
/// SHA-256 of the whole file when all of it was read.
pub fn read_bytes(
    path: &str,
    offset: Option<u64>,
    length: Option<u64>,
) -> Result<HostResult, controlroom_protocol::RpcError> {
    let (mut file, size) = open_file(path)?;
    let start = offset.unwrap_or(0);
    let want = length
        .unwrap_or_else(|| size.saturating_sub(start))
        .min(MAX_READ_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| fs_error(path, e))?;
    let mut data = Vec::new();
    file.take(want)
        .read_to_end(&mut data)
        .map_err(|e| fs_error(path, e))?;
    let whole = start == 0 && data.len() as u64 == size;
    Ok(HostResult::ReadBytes {
        path: RemotePath::from(path),
        length: data.len() as u64,
        size,
        sha256: whole.then(|| sha256_hex(&data)),
        data: STANDARD.encode(&data),
    })
}

/// SHA-256 as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Consumes one line (up to and including `\n`, or to EOF) without buffering it whole.
fn skip_line(reader: &mut impl BufRead) -> std::io::Result<()> {
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(());
        }
        if let Some(i) = memchr::memchr(b'\n', buf) {
            reader.consume(i + 1);
            return Ok(());
        }
        let n = buf.len();
        reader.consume(n);
    }
}

/// Reads one line without its `\n`, keeping at most `max` bytes (the rest is skipped);
/// decoded lossily. At EOF it is empty (pi's last line after a trailing newline).
fn read_line_capped(reader: &mut impl BufRead, max: usize) -> std::io::Result<String> {
    let mut kept = Vec::new();
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let (part, done) = match memchr::memchr(b'\n', buf) {
            Some(i) => (&buf[..i], Some(i + 1)),
            None => (buf, None),
        };
        let room = max.saturating_sub(kept.len());
        kept.extend_from_slice(&part[..part.len().min(room)]);
        let used = done.unwrap_or(buf.len());
        reader.consume(used);
        if done.is_some() {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&kept).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(result: HostResult) -> (Vec<String>, u64, u64, Option<ReadCut>, bool) {
        let HostResult::Read {
            content:
                ReadContent::Text {
                    lines,
                    start_line,
                    total_lines,
                    cut,
                    first_line_too_long,
                },
            ..
        } = result
        else {
            panic!("not text: {result:?}");
        };
        (lines, start_line, total_lines, cut, first_line_too_long)
    }

    #[test]
    fn windows_follow_pis_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let body: String = (1..=5000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, body).unwrap();
        let path = path.to_str().unwrap();
        let cache = LineIndexCache::default();

        let (lines, start, total, cut, _) = text(read(path, &cache, None, None).unwrap());
        assert_eq!(
            (start, total, lines.len(), cut),
            (1, 5001, 2000, Some(ReadCut::Lines))
        );
        let (lines, start, _, cut, _) = text(read(path, &cache, Some(4999), None).unwrap());
        assert_eq!(lines, ["line 4999", "line 5000", ""]);
        assert_eq!((start, cut), (4999, None));
        let (lines, _, _, cut, _) = text(read(path, &cache, Some(2500), Some(2)).unwrap());
        assert_eq!(
            (lines, cut),
            (vec!["line 2500".into(), "line 2501".into()], None)
        );
        assert!(read(path, &cache, Some(5002), None).is_err());

        std::fs::write(
            dir.path().join("long"),
            format!("{}\nx\n", "a".repeat(60_000)),
        )
        .unwrap();
        let long = dir.path().join("long");
        let (lines, _, _, cut, too_long) =
            text(read(long.to_str().unwrap(), &cache, None, None).unwrap());
        assert!(lines.is_empty() && too_long && cut == Some(ReadCut::Bytes));
    }

    #[test]
    fn images_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let cache = LineIndexCache::default();
        let result = read(png.to_str().unwrap(), &cache, None, None).unwrap();
        assert!(matches!(
            result,
            HostResult::Read { content: ReadContent::Image { ref mime_type }, .. } if mime_type == "image/png"
        ));
        let HostResult::ReadBytes {
            data,
            length,
            size,
            sha256,
            ..
        } = read_bytes(png.to_str().unwrap(), None, None).unwrap()
        else {
            panic!()
        };
        assert_eq!((length, size), (12, 12));
        assert_eq!(STANDARD.decode(data).unwrap(), b"\x89PNG\r\n\x1a\nrest");
        assert_eq!(sha256, Some(sha256_hex(b"\x89PNG\r\n\x1a\nrest")));
        let HostResult::ReadBytes { sha256, length, .. } =
            read_bytes(png.to_str().unwrap(), Some(8), Some(2)).unwrap()
        else {
            panic!()
        };
        assert_eq!((sha256, length), (None, 2));
    }
}
