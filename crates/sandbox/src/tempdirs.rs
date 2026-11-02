//! Cleanup of temporary directories nono leaks (`/tmp/nono-*`) when it is killed (§11.5).
//!
//! A directory is removed only if it is owned by this user, older than a minimum age, and not
//! referenced by any live process of this user: no open file descriptor, working directory,
//! environment value or command-line argument under it, and no bound unix socket in it.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Name prefix of nono's temporary directories.
pub const NONO_TEMP_PREFIX: &str = "nono-";

/// Removes leaked nono directories directly under `tmp` that are at least `min_age` old and
/// unused. Returns the removed paths. Errors on individual entries are skipped.
pub fn clean_leaked_nono_dirs(tmp: &Path, min_age: Duration) -> Vec<PathBuf> {
    let uid = current_uid();
    let now = SystemTime::now();
    let candidates: Vec<PathBuf> = fs::read_dir(tmp)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(NONO_TEMP_PREFIX)
        })
        .filter(|e| {
            e.path().symlink_metadata().is_ok_and(|m| {
                m.is_dir()
                    && m.uid() == uid
                    && m.modified()
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                        .is_some_and(|age| age >= min_age)
            })
        })
        .map(|e| e.path())
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let refs = live_references(uid);
    candidates
        .into_iter()
        .filter(|dir| !refs.iter().any(|r| references(r, dir)))
        .filter(|dir| remove_tree(dir))
        .collect()
}

/// Removes `dir` and everything in it. nono's command-mediation dirs hold a read-only
/// `shims/` directory, so on failure the tree's directories are made writable (never
/// following symlinks) and the removal is tried once more.
fn remove_tree(dir: &Path) -> bool {
    if fs::remove_dir_all(dir).is_ok() {
        return true;
    }
    make_dirs_writable(dir);
    fs::remove_dir_all(dir).is_ok()
}

fn make_dirs_writable(dir: &Path) {
    let Ok(meta) = dir.symlink_metadata() else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(meta.mode() | 0o700));
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        make_dirs_writable(&entry.path());
    }
}

/// True if the string `r` mentions `dir` or something beneath it.
fn references(r: &str, dir: &Path) -> bool {
    let d = dir.to_string_lossy();
    r.match_indices(d.as_ref()).any(|(i, _)| {
        matches!(
            r.as_bytes().get(i + d.len()),
            None | Some(b'/' | b':' | b' ' | b'\0')
        )
    })
}

fn current_uid() -> u32 {
    fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(u32::MAX)
}

/// Paths and strings referenced by this user's processes, plus bound unix socket paths.
fn live_references(uid: u32) -> Vec<String> {
    let mut refs = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let proc_dir = entry.path();
        if !fs::metadata(&proc_dir).is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        if let Ok(cwd) = fs::read_link(proc_dir.join("cwd")) {
            refs.push(cwd.to_string_lossy().into_owned());
        }
        for fd in fs::read_dir(proc_dir.join("fd"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Ok(target) = fs::read_link(fd.path()) {
                refs.push(target.to_string_lossy().into_owned());
            }
        }
        for file in ["environ", "cmdline"] {
            if let Ok(bytes) = fs::read(proc_dir.join(file)) {
                refs.push(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
    }
    if let Ok(unix) = fs::read_to_string("/proc/net/unix") {
        refs.extend(
            unix.lines()
                .filter_map(|l| l.split_whitespace().nth(7))
                .map(str::to_owned),
        );
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_old_unused_nono_dirs() {
        let t = tempfile::tempdir().expect("tempdir");
        let tmp = t.path();
        for d in ["nono-old", "nono-busy", "other-dir"] {
            fs::create_dir(tmp.join(d)).expect("mkdir");
        }
        // A command-mediation dir: its shims are read-only.
        let shims = tmp.join("nono-tool-sandbox-1/shims");
        fs::create_dir_all(&shims).expect("mkdir");
        fs::write(shims.join("git"), "").expect("write");
        fs::set_permissions(&shims, fs::Permissions::from_mode(0o500)).expect("chmod");
        fs::write(tmp.join("nono-file"), "").expect("write");
        // Hold a file open in nono-busy: it is referenced by this process.
        let _busy = fs::File::create(tmp.join("nono-busy/held")).expect("create");

        // Nothing is old enough yet.
        assert!(clean_leaked_nono_dirs(tmp, Duration::from_secs(3600)).is_empty());

        let mut removed = clean_leaked_nono_dirs(tmp, Duration::ZERO);
        removed.sort();
        assert_eq!(
            removed,
            vec![tmp.join("nono-old"), tmp.join("nono-tool-sandbox-1")]
        );
        assert!(tmp.join("nono-busy").exists());
        assert!(tmp.join("other-dir").exists());
        assert!(tmp.join("nono-file").exists());
    }

    #[test]
    fn reference_matching_respects_component_boundaries() {
        let dir = Path::new("/tmp/nono-a");
        assert!(references("/tmp/nono-a", dir));
        assert!(references("/tmp/nono-a/sock", dir));
        assert!(references("PATH=/tmp/nono-a/shims:/bin\0", dir));
        assert!(!references("/tmp/nono-ab", dir));
    }
}
