//! Diagnostics on stderr (`supervisor.log` for a detached supervisor).

/// Prints a `supervise:` diagnostic to stderr. Unlike `eprintln!`, it never panics,
/// e.g. when stderr is a closed pipe.
macro_rules! diag {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "supervise: {}", format_args!($($arg)*));
    }};
}

pub(crate) use diag;
