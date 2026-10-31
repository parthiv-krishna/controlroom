//! Absolute host paths as validated UTF-8 strings.
//!
//! nono profiles are JSON, so every path that ends up in one must be valid UTF-8. [`AbsPath`]
//! guarantees that, plus absoluteness and a normalized form (no `.`/`..` components, no
//! duplicate or trailing slashes), so ancestor checks are plain component comparisons.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

/// Why a string is not an acceptable [`AbsPath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The path does not start with `/`.
    #[error("path is not absolute: {0:?}")]
    NotAbsolute(String),
    /// The path contains a `.` or `..` component.
    #[error("path contains `.` or `..`: {0:?}")]
    NotNormalized(String),
    /// The path contains a NUL byte or a newline.
    #[error("path contains a control character: {0:?}")]
    ControlCharacter(String),
    /// The path is not valid UTF-8.
    #[error("path is not valid UTF-8: {0:?}")]
    NotUtf8(PathBuf),
}

/// An absolute, normalized, UTF-8 host path (e.g. `/home/alice/project`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AbsPath(String);

impl AbsPath {
    /// Validates and normalizes `s`: collapses repeated slashes and drops a trailing slash.
    pub fn new(s: &str) -> Result<Self, PathError> {
        if !s.starts_with('/') {
            return Err(PathError::NotAbsolute(s.to_owned()));
        }
        if s.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
            return Err(PathError::ControlCharacter(s.to_owned()));
        }
        let mut out = String::with_capacity(s.len());
        for part in s.split('/').filter(|p| !p.is_empty()) {
            if part == "." || part == ".." {
                return Err(PathError::NotNormalized(s.to_owned()));
            }
            out.push('/');
            out.push_str(part);
        }
        if out.is_empty() {
            out.push('/');
        }
        Ok(Self(out))
    }

    /// Converts a filesystem path, rejecting non-UTF-8 paths.
    pub fn from_path(path: &Path) -> Result<Self, PathError> {
        let s = path
            .to_str()
            .ok_or_else(|| PathError::NotUtf8(path.to_owned()))?;
        Self::new(s)
    }

    /// The path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path as a [`Path`] for filesystem calls.
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// Appends one or more relative components (`"agent"`, `"agent/settings.json"`).
    ///
    /// # Panics
    /// If `rel` is absolute or contains `.`/`..`; callers pass compile-time constants or
    /// already validated names.
    pub fn join(&self, rel: &str) -> AbsPath {
        assert!(
            !rel.starts_with('/'),
            "join expects a relative path: {rel:?}"
        );
        let joined = if self.0 == "/" {
            format!("/{rel}")
        } else {
            format!("{}/{rel}", self.0)
        };
        AbsPath::new(&joined).expect("joining validated components yields a valid path")
    }

    /// The parent directory, or `None` for `/`.
    pub fn parent(&self) -> Option<AbsPath> {
        if self.0 == "/" {
            return None;
        }
        let idx = self.0.rfind('/').expect("absolute paths contain a slash");
        Some(AbsPath(if idx == 0 {
            "/".to_owned()
        } else {
            self.0[..idx].to_owned()
        }))
    }

    /// The last component, or `None` for `/`.
    pub fn file_name(&self) -> Option<&str> {
        self.0.rsplit('/').next().filter(|s| !s.is_empty())
    }

    /// True if `self` equals `other` or is one of its ancestors.
    pub fn contains(&self, other: &AbsPath) -> bool {
        self.0 == "/"
            || other.0 == self.0
            || (other.0.starts_with(&self.0) && other.0.as_bytes()[self.0.len()] == b'/')
    }
}

impl fmt::Display for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<Path> for AbsPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl<'de> Deserialize<'de> for AbsPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        AbsPath::new(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> AbsPath {
        AbsPath::new(s).unwrap()
    }

    #[test]
    fn normalizes_slashes() {
        assert_eq!(p("//home///alice/").as_str(), "/home/alice");
        assert_eq!(p("/").as_str(), "/");
    }

    #[test]
    fn rejects_bad_paths() {
        assert!(matches!(
            AbsPath::new("rel/x"),
            Err(PathError::NotAbsolute(_))
        ));
        assert!(matches!(
            AbsPath::new("/a/../b"),
            Err(PathError::NotNormalized(_))
        ));
        assert!(matches!(
            AbsPath::new("/a/./b"),
            Err(PathError::NotNormalized(_))
        ));
        assert!(matches!(
            AbsPath::new("/a\nb"),
            Err(PathError::ControlCharacter(_))
        ));
    }

    #[test]
    fn containment_is_component_wise() {
        assert!(p("/home/alice").contains(&p("/home/alice")));
        assert!(p("/home/alice").contains(&p("/home/alice/x")));
        assert!(!p("/home/alice").contains(&p("/home/alicex")));
        assert!(!p("/home/alice/x").contains(&p("/home/alice")));
        assert!(p("/").contains(&p("/anything")));
    }

    #[test]
    fn join_parent_and_name() {
        let base = p("/srv/cr");
        assert_eq!(base.join("sessions/x").as_str(), "/srv/cr/sessions/x");
        assert_eq!(p("/").join("tmp").as_str(), "/tmp");
        assert_eq!(base.parent().unwrap().as_str(), "/srv");
        assert_eq!(p("/srv").parent().unwrap().as_str(), "/");
        assert_eq!(p("/").parent(), None);
        assert_eq!(base.file_name(), Some("cr"));
    }

    #[test]
    fn deserialize_validates() {
        let ok: AbsPath = serde_json::from_str("\"/a//b/\"").unwrap();
        assert_eq!(ok.as_str(), "/a/b");
        assert!(serde_json::from_str::<AbsPath>("\"a\"").is_err());
    }
}
