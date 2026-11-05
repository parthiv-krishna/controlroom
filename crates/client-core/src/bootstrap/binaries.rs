//! Where the binaries a controller deploys come from: `controlroomd` and the nono it
//! sandboxes workspaces with.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use super::platform::{Arch, Version};

/// A program a controller deploys to machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Program {
    /// The host daemon.
    Controlroomd,
    /// The sandbox, installed next to the daemon.
    Nono,
}

impl Program {
    /// Its file name on the machine, e.g. `controlroomd`.
    pub fn as_str(self) -> &'static str {
        match self {
            Program::Controlroomd => "controlroomd",
            Program::Nono => "nono",
        }
    }
}

impl std::fmt::Display for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Supplies the binaries a controller can deploy, e.g. the static musl builds bundled with
/// the web server package.
pub trait DaemonBinarySource {
    /// The version `program`'s binaries from this source report.
    fn version(&self, program: Program) -> &Version;
    /// `program`'s static binary for `arch`. Errors (e.g. `NotFound`) mean that arch cannot
    /// be deployed.
    fn binary(&self, program: Program, arch: Arch) -> std::io::Result<Cow<'_, [u8]>>;
}

/// One program's binaries in memory.
#[derive(Debug, Clone)]
pub struct InMemoryProgram {
    /// The version the binaries report.
    pub version: Version,
    /// The x86_64 binary, if any.
    pub x86_64: Option<Vec<u8>>,
    /// The aarch64 binary, if any.
    pub aarch64: Option<Vec<u8>>,
}

/// A [`DaemonBinarySource`] backed by in-memory bytes.
#[derive(Debug, Clone)]
pub struct InMemoryBinaries {
    /// controlroomd.
    pub daemon: InMemoryProgram,
    /// nono.
    pub nono: InMemoryProgram,
}

impl InMemoryBinaries {
    fn program(&self, program: Program) -> &InMemoryProgram {
        match program {
            Program::Controlroomd => &self.daemon,
            Program::Nono => &self.nono,
        }
    }
}

impl DaemonBinarySource for InMemoryBinaries {
    fn version(&self, program: Program) -> &Version {
        &self.program(program).version
    }

    fn binary(&self, program: Program, arch: Arch) -> std::io::Result<Cow<'_, [u8]>> {
        let p = self.program(program);
        let bytes = match arch {
            Arch::X86_64 => p.x86_64.as_deref(),
            Arch::Aarch64 => p.aarch64.as_deref(),
        };
        bytes.map(Cow::Borrowed).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no {program} {arch} binary bundled"),
            )
        })
    }
}

/// The file name of `program`'s bundled binary for `arch`, e.g. `nono-x86_64-linux`.
pub fn bundled_binary_name(program: Program, arch: Arch) -> String {
    format!("{program}-{arch}-linux")
}

/// The file holding the version of `program`'s bundled binaries.
fn version_file_name(program: Program) -> &'static str {
    match program {
        Program::Controlroomd => "version",
        Program::Nono => "nono-version",
    }
}

/// Bundled binaries in a local directory, as the `controlroom-web` package ships them:
///
/// ```text
/// <dir>/controlroomd-x86_64-linux
/// <dir>/controlroomd-aarch64-linux
/// <dir>/version                     # controlroomd's, e.g. "0.1.0"
/// <dir>/nono-x86_64-linux
/// <dir>/nono-aarch64-linux
/// <dir>/nono-version                # e.g. "0.80.0"
/// ```
///
/// Any binary may be missing; an architecture without both can't be deployed.
#[derive(Debug, Clone)]
pub struct DirBinaries {
    dir: PathBuf,
    version: Version,
    nono_version: Version,
}

impl DirBinaries {
    /// Opens `dir`. Fails if it holds no `controlroomd-<arch>-linux` binary or a version file
    /// is missing or invalid.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        if !Arch::ALL.iter().any(|arch| {
            dir.join(bundled_binary_name(Program::Controlroomd, *arch))
                .is_file()
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no controlroomd-<arch>-linux binaries in {}", dir.display()),
            ));
        }
        Ok(Self {
            version: read_version(&dir, Program::Controlroomd)?,
            nono_version: read_version(&dir, Program::Nono)?,
            dir,
        })
    }

    /// The directory the binaries are read from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn read_version(dir: &Path, program: Program) -> std::io::Result<Version> {
    let file = dir.join(version_file_name(program));
    let text = std::fs::read_to_string(&file)
        .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", file.display())))?;
    Version::parse(text.trim()).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: {e}", file.display()),
        )
    })
}

impl DaemonBinarySource for DirBinaries {
    fn version(&self, program: Program) -> &Version {
        match program {
            Program::Controlroomd => &self.version,
            Program::Nono => &self.nono_version,
        }
    }

    fn binary(&self, program: Program, arch: Arch) -> std::io::Result<Cow<'_, [u8]>> {
        let path = self.dir.join(bundled_binary_name(program, arch));
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
            daemon: InMemoryProgram {
                version: Version::new(0, 1, 0),
                x86_64: Some(b"bin".to_vec()),
                aarch64: None,
            },
            nono: InMemoryProgram {
                version: Version::new(0, 80, 0),
                x86_64: None,
                aarch64: Some(b"nono".to_vec()),
            },
        };
        assert_eq!(
            &*src.binary(Program::Controlroomd, Arch::X86_64).unwrap(),
            b"bin"
        );
        assert_eq!(src.version(Program::Nono), &Version::new(0, 80, 0));
        assert_eq!(&*src.binary(Program::Nono, Arch::Aarch64).unwrap(), b"nono");
        let err = src.binary(Program::Nono, Arch::X86_64).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(err.to_string().contains("nono x86_64"), "{err}");
    }

    #[test]
    fn dir_source_reads_binaries_and_versions() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: &str| std::fs::write(dir.path().join(name), content);
        assert!(DirBinaries::open(dir.path()).is_err());
        write("controlroomd-x86_64-linux", "x86").unwrap();
        // Both version files are required.
        assert!(DirBinaries::open(dir.path()).is_err());
        write("version", "0.2.0\n").unwrap();
        assert!(DirBinaries::open(dir.path()).is_err());
        write("nono-version", "0.80.0\n").unwrap();
        write("nono-x86_64-linux", "nono").unwrap();
        let src = DirBinaries::open(dir.path()).unwrap();
        assert_eq!(src.version(Program::Controlroomd), &Version::new(0, 2, 0));
        assert_eq!(src.version(Program::Nono), &Version::new(0, 80, 0));
        assert_eq!(
            &*src.binary(Program::Controlroomd, Arch::X86_64).unwrap(),
            b"x86"
        );
        assert_eq!(&*src.binary(Program::Nono, Arch::X86_64).unwrap(), b"nono");
        assert_eq!(
            src.binary(Program::Controlroomd, Arch::Aarch64)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        write("nono-version", "nope").unwrap();
        assert!(DirBinaries::open(dir.path()).is_err());
    }
}
