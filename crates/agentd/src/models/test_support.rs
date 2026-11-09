//! Test helpers.

use std::path::Path;
use std::process::Command;

/// Writes an executable script from a child shell: writing it from this process would race with
/// concurrent forks of other tests, which then fail to execute it with ETXTBSY.
pub(crate) fn write_executable(path: &Path, content: &str) {
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#)
        .arg("sh")
        .arg(content)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}
