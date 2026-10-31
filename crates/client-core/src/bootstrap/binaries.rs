//! Where the daemon binaries a controller deploys come from.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use super::platform::{Arch, Version};

/// Supplies the daemon binaries a controller can deploy, e.g. the static musl
/// builds bundled with the web server package.
pub trait DaemonBinarySource {
    /// The version every binary from this source reports.
    fn version(&self) -> &Version;
    /// The static binary for `arch`. Errors (e.g. `NotFound`) mean that arch
    /// cannot be deployed.
    fn binary(&self, arch: Arch) -> std::io::Result<Cow<'_, [u8]>>;
}

/// A [`DaemonBinarySource`] backed by in-memory bytes.
#[derive(Debug, Clone)]
pub struct InMemoryBinaries {
    /// The version the binaries report.
    pub version: Version,
    /// The x86_64 binary, if any.
    pub x86_64: Option<Vec<u8>>,
    /// The aarch64 binary, if any.
    pub aarch64: Option<Vec<u8>>,
}

impl DaemonBinarySource for InMemoryBinaries {
    fn version(&self) -> &Version {
        &self.version
    }

    fn binary(&self, arch: Arch) -> std::io::Result<Cow<'_, [u8]>> {
        let bytes = match arch {
            Arch::X86_64 => self.x86_64.as_deref(),
            Arch::Aarch64 => self.aarch64.as_deref(),
        };
        bytes.map(Cow::Borrowed).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no controlroomd {arch} binary bundled"),
            )
        })
    }
}

/// The file name of the bundled binary for `arch`.
pub fn bundled_binary_name(arch: Arch) -> String {
    format!("controlroomd-{arch}-linux")
}

/// Bundled daemon binaries in a local directory, as the `controlroom-web`
/// package ships them:
///
/// ```text
/// <dir>/controlroomd-x86_64-linux
/// <dir>/controlroomd-aarch64-linux
/// <dir>/version                     # e.g. "0.1.0"
/// ```
///
/// Either binary may be missing; that architecture then can't be deployed.
#[derive(Debug, Clone)]
pub struct DirBinaries {
    dir: PathBuf,
    version: Version,
}

impl DirBinaries {
    /// Opens `dir`. Fails if it holds no `controlroomd-<arch>-linux` binary or
    /// its `version` file is missing or invalid.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        if !Arch::ALL
            .iter()
            .any(|arch| dir.join(bundled_binary_name(*arch)).is_file())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no controlroomd-<arch>-linux binaries in {}", dir.display()),
            ));
        }
        let version_file = dir.join("version");
        let text = std::fs::read_to_string(&version_file).map_err(|e| {
            std::io::Error::new(e.kind(), format!("{}: {e}", version_file.display()))
        })?;
        let version = Version::parse(text.trim()).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", version_file.display()),
            )
        })?;
        Ok(Self { dir, version })
    }

    /// The directory the binaries are read from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl DaemonBinarySource for DirBinaries {
    fn version(&self) -> &Version {
        &self.version
    }

    fn binary(&self, arch: Arch) -> std::io::Result<Cow<'_, [u8]>> {
        let path = self.dir.join(bundled_binary_name(arch));
        std::fs::read(&path)
            .map(Cow::Owned)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_source() {
        let src = InMemoryBinaries {
            version: Version::new(0, 1, 0),
            x86_64: Some(b"bin".to_vec()),
            aarch64: None,
        };
        assert_eq!(&*src.binary(Arch::X86_64).unwrap(), b"bin");
        assert_eq!(
            src.binary(Arch::Aarch64).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn dir_source_reads_binaries_and_version() {
        let dir = tempfile::tempdir().unwrap();
        assert!(DirBinaries::open(dir.path()).is_err());
        std::fs::write(dir.path().join("controlroomd-x86_64-linux"), b"x86").unwrap();
        // The version file is required.
        assert!(DirBinaries::open(dir.path()).is_err());
        std::fs::write(dir.path().join("version"), "0.2.0\n").unwrap();
        let src = DirBinaries::open(dir.path()).unwrap();
        assert_eq!(src.version(), &Version::new(0, 2, 0));
        assert_eq!(&*src.binary(Arch::X86_64).unwrap(), b"x86");
        assert_eq!(
            src.binary(Arch::Aarch64).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::write(dir.path().join("version"), "nope").unwrap();
        assert!(DirBinaries::open(dir.path()).is_err());
    }
}
