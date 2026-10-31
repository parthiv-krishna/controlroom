//! Shell quoting for commands sent over SSH exec.
//!
//! sshd runs an exec command through the user's login shell, which may be
//! bash, zsh, dash or fish. Multi-statement scripts are therefore wrapped in
//! `sh -c '...'` ([`sh_c`]), and every word is quoted ([`quote`]) so that each
//! of those shells reads it back unchanged.

/// Quotes `s` as one shell word that POSIX shells and fish read back verbatim.
///
/// Text goes inside single quotes. Single quotes and backslashes are emitted
/// outside the quotes as `\'` and `\\`: inside single quotes POSIX shells
/// treat a backslash literally but fish treats `\\` and `\'` as escapes, while
/// outside quotes every one of these shells reads `\x` as a literal `x`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str(r"'\''"),
            '\\' => out.push_str(r"'\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Wraps a POSIX shell script so it runs under `sh`, whatever the login shell is.
pub fn sh_c(script: &str) -> String {
    format!("sh -c {}", quote(script))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_quotes_and_backslashes() {
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(r"a\b"), r"'a'\\'b'");
        assert_eq!(quote(""), "''");
        assert_eq!(sh_c("echo 'x'"), r"sh -c 'echo '\''x'\'''");
    }

    #[test]
    fn sh_reads_quoted_words_back_verbatim() {
        let tricky = "a 'b' \\' \\\\ \"c\" $HOME `x` \n\t*";
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", quote(tricky)))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), tricky);
    }
}
