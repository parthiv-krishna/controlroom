//! `find` and `grep` with ripgrep's crates: a walk that honors ignore files (`.gitignore`
//! inside git repositories, `.ignore`, `.rgignore`), shows dot files but never descends into
//! `.git`, and skips binary files when searching.

use std::path::Path;

use controlroom_protocol::host::{GrepLine, HostResult};
use controlroom_protocol::{RemotePath, RpcError};
use globset::{GlobBuilder, GlobMatcher};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{
    BinaryDetection, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch,
};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

use super::{fs_error, invalid};

/// Default `find` limit.
const FIND_LIMIT: u64 = 1000;
/// Default `grep` limit (matches).
const GREP_LIMIT: u64 = 100;
/// Longest line shown by `grep`, in characters.
const GREP_LINE_CHARS: usize = 500;
/// Largest regex accepted (compiled size).
const REGEX_SIZE_LIMIT: usize = 10 << 20;

/// The walk shared by both ops.
fn walker(root: &str) -> WalkBuilder {
    let mut walk = WalkBuilder::new(root);
    walk.hidden(false)
        .sort_by_file_name(std::ffi::OsStr::cmp)
        .filter_entry(|entry| entry.file_name() != ".git");
    walk
}

fn glob(pattern: &str) -> Result<GlobMatcher, RpcError> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| invalid(format!("bad glob {pattern:?}: {e}")))
}

/// Paths under `root` matching the glob `pattern`: against the file name when the pattern
/// has no `/`, else against the path relative to `root`. Sorted, at most `limit`.
pub fn find(root: &str, pattern: &str, limit: Option<u64>) -> Result<HostResult, RpcError> {
    let limit = limit.unwrap_or(FIND_LIMIT).max(1);
    let matcher = glob(pattern)?;
    let by_path = pattern.contains('/');
    if !Path::new(root).is_dir() {
        return Err(fs_error(root, "not a directory"));
    }
    let mut paths = Vec::new();
    let mut limit_reached = false;
    for entry in walker(root).build() {
        let entry = match entry {
            Ok(entry) => entry,
            // Unreadable subdirectories (e.g. outside the sandbox) are skipped.
            Err(_) => continue,
        };
        if entry.depth() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let hit = if by_path {
            matcher.is_match(rel)
        } else {
            matcher.is_match(Path::new(entry.file_name()))
        };
        if !hit {
            continue;
        }
        if paths.len() as u64 >= limit {
            limit_reached = true;
            break;
        }
        paths.push(rel.to_string_lossy().into_owned());
    }
    paths.sort();
    Ok(HostResult::Find {
        path: RemotePath::from(root),
        paths,
        limit_reached,
    })
}

/// What `grep` looks for.
#[derive(Debug, Clone)]
pub struct GrepQuery {
    /// A regex, or literal text.
    pub pattern: String,
    /// Only files matching this glob.
    pub glob: Option<String>,
    /// Case-insensitive.
    pub ignore_case: bool,
    /// `pattern` is literal.
    pub literal: bool,
    /// Context lines around matches.
    pub context: u32,
    /// Most matches.
    pub limit: Option<u64>,
}

/// Matching lines (and context) under `path` (a directory or one file), in file order.
pub fn grep(path: &str, query: &GrepQuery) -> Result<HostResult, RpcError> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(query.ignore_case)
        .fixed_strings(query.literal)
        .size_limit(REGEX_SIZE_LIMIT)
        .build(&query.pattern)
        .map_err(|e| invalid(format!("bad pattern: {e}")))?;
    let context = usize::try_from(query.context).unwrap_or(usize::MAX).min(100);
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(context)
        .after_context(context)
        .binary_detection(BinaryDetection::quit(0))
        .build();
    let target = Path::new(path);
    let meta = std::fs::metadata(target).map_err(|e| fs_error(path, e))?;
    let base = if meta.is_dir() {
        target
    } else {
        target.parent().unwrap_or(target)
    };
    let mut walk = walker(path);
    if let Some(glob) = &query.glob {
        let mut overrides = OverrideBuilder::new(path);
        overrides
            .add(glob)
            .map_err(|e| invalid(format!("bad glob {glob:?}: {e}")))?;
        walk.overrides(
            overrides
                .build()
                .map_err(|e| invalid(format!("bad glob {glob:?}: {e}")))?,
        );
    }
    let mut sink = Collect {
        limit: query.limit.unwrap_or(GREP_LIMIT).max(1),
        file: String::new(),
        lines: Vec::new(),
        matches: 0,
        limit_reached: false,
        lines_truncated: false,
    };
    for entry in walk.build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        sink.file = entry
            .path()
            .strip_prefix(base)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        // An unreadable file is skipped, like ripgrep does.
        let _ = searcher.search_path(&matcher, entry.path(), &mut sink);
        if sink.limit_reached {
            break;
        }
    }
    Ok(HostResult::Grep {
        path: RemotePath::from(path),
        lines: sink.lines,
        matches: sink.matches,
        limit_reached: sink.limit_reached,
        lines_truncated: sink.lines_truncated,
    })
}

/// Collects lines until the match limit.
struct Collect {
    limit: u64,
    file: String,
    lines: Vec<GrepLine>,
    matches: u64,
    limit_reached: bool,
    lines_truncated: bool,
}

impl Collect {
    fn push(&mut self, line: u64, bytes: &[u8], context: bool) {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches(['\n', '\r']);
        let text = match text.char_indices().nth(GREP_LINE_CHARS) {
            Some((cut, _)) => {
                self.lines_truncated = true;
                format!("{}...", &text[..cut])
            }
            None => text.to_owned(),
        };
        self.lines.push(GrepLine {
            path: self.file.clone(),
            line,
            text,
            context,
        });
    }
}

impl Sink for Collect {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _: &grep_searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        if self.matches >= self.limit {
            self.limit_reached = true;
            return Ok(false);
        }
        self.matches += 1;
        self.push(mat.line_number().unwrap_or(0), mat.bytes(), false);
        Ok(true)
    }

    fn context(
        &mut self,
        _: &grep_searcher::Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        if self.limit_reached || matches!(ctx.kind(), SinkContextKind::Other) {
            return Ok(!self.limit_reached);
        }
        // After-context of the last match past the limit is still shown.
        self.push(ctx.line_number().unwrap_or(0), ctx.bytes(), true);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "TODO in git\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    // TODO: x\n}\n").unwrap();
        std::fs::write(root.join("src/deep/lib.rs"), "a\nb\ntodo here\nc\n").unwrap();
        std::fs::write(root.join(".env"), "TODO=1\n").unwrap();
        std::fs::write(root.join("blob.bin"), b"TODO\0binary").unwrap();
        dir
    }

    #[test]
    fn find_by_name_and_by_path() {
        let dir = tree();
        let root = dir.path().to_str().unwrap();
        let HostResult::Find { paths, .. } = find(root, "*.rs", None).unwrap() else {
            panic!()
        };
        assert_eq!(paths, ["src/deep/lib.rs", "src/main.rs"]);
        let HostResult::Find { paths, .. } = find(root, "src/*.rs", None).unwrap() else {
            panic!()
        };
        assert_eq!(paths, ["src/main.rs"]);
        let HostResult::Find {
            paths,
            limit_reached,
            ..
        } = find(root, "*", Some(2)).unwrap()
        else {
            panic!()
        };
        assert!(limit_reached && paths.len() == 2);
        assert!(find(root, "[", None).is_err());
    }

    #[test]
    fn grep_with_options_context_and_limits() {
        let dir = tree();
        let root = dir.path().to_str().unwrap();
        let query = |pattern: &str| GrepQuery {
            pattern: pattern.into(),
            glob: None,
            ignore_case: false,
            literal: false,
            context: 0,
            limit: None,
        };
        let HostResult::Grep { lines, matches, .. } = grep(root, &query("TODO")).unwrap() else {
            panic!()
        };
        // Not in .git, not in the binary file; dot files are searched.
        let files: Vec<_> = lines.iter().map(|l| l.path.as_str()).collect();
        assert_eq!(files, [".env", "src/main.rs"]);
        assert_eq!(matches, 2);

        let mut q = query("todo");
        q.ignore_case = true;
        q.context = 1;
        q.glob = Some("*.rs".into());
        let HostResult::Grep { lines, matches, .. } = grep(root, &q).unwrap() else {
            panic!()
        };
        assert_eq!(matches, 2);
        let deep: Vec<_> = lines
            .iter()
            .filter(|l| l.path == "src/deep/lib.rs")
            .map(|l| (l.line, l.context))
            .collect();
        assert_eq!(deep, [(2, true), (3, false), (4, true)]);

        let mut q = query("(");
        q.literal = true;
        let HostResult::Grep { matches, .. } = grep(root, &q).unwrap() else {
            panic!()
        };
        assert_eq!(matches, 1);
        assert!(grep(root, &query("(")).is_err());

        let mut q = query("[a-z]");
        q.limit = Some(1);
        let HostResult::Grep {
            matches,
            limit_reached,
            ..
        } = grep(root, &q).unwrap()
        else {
            panic!()
        };
        assert!(limit_reached && matches == 1);

        let file = format!("{root}/src/main.rs");
        let HostResult::Grep { lines, .. } = grep(&file, &query("main")).unwrap() else {
            panic!()
        };
        assert_eq!(lines[0].path, "main.rs");
    }
}
