//! The shell integration: the rcfile every agent shell starts with (`bash --noprofile --rcfile
//! <it> -i`), and the markers it prints.
//!
//! - It emulates a login shell first (`/etc/profile`, then the first of `~/.bash_profile`,
//!   `~/.bash_login`, `~/.profile`, else `~/.bashrc`), so `PATH` and the user's setup apply
//!   (in a strict sandbox the home files are unreadable and skipped).
//! - It sets non-interactive defaults for agent commands (spec §22.1: pagers off, no git
//!   credential prompts) and keeps `__cr_run` lines out of the history.
//! - `PROMPT_COMMAND` reports every prompt: OSC 133 `D;<exit status>`, OSC 7 with the cwd,
//!   then OSC 133 `A` (prompt start).
//! - `__cr_run <marker> <file>` runs an agent command: it shows the command (dimmed) for
//!   humans watching the terminal, prints OSC 133 `C;<marker>`, clears the positional
//!   parameters and sources the file. The command's output is what the terminal shows between
//!   that `C` and the next `D`.

/// The rcfile.
pub const BASHRC: &str = r#"# controlroom shell integration (written by controlroomd)
if [ -z "${__cr_init-}" ]; then
  __cr_init=1
  [ -r /etc/profile ] && . /etc/profile
  if [ -r ~/.bash_profile ]; then . ~/.bash_profile
  elif [ -r ~/.bash_login ]; then . ~/.bash_login
  elif [ -r ~/.profile ]; then . ~/.profile
  elif [ -r ~/.bashrc ]; then . ~/.bashrc
  fi
fi
export PAGER=cat GIT_PAGER=cat MANPAGER=cat GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive
HISTCONTROL=ignoreboth
if [ -n "${CR_CWD-}" ]; then cd -- "$CR_CWD" 2>/dev/null; fi
unset CR_CWD
__cr_prompt() {
  local s=$?
  printf '\033]133;D;%s\007\033]7;file://%s%s\007\033]133;A\007' "$s" "${HOSTNAME-}" "$PWD"
  return $s
}
PROMPT_COMMAND=(__cr_prompt)
__cr_run() {
  local __cr_f=$2
  printf '\033[2m%s\033[0m\n' "$(< "$__cr_f")"
  printf '\033]133;C;%s\007' "$1"
  set --
  . "$__cr_f"
}
"#;

/// The marker of a call: a short digest of its id (call ids may hold any character).
pub fn marker(call_id: &str) -> String {
    let digest = super::super::fsops::sha256_hex(call_id.as_bytes());
    digest[..24].to_owned()
}

/// The line typed into the shell to run the command in `file` (leading space: not in the
/// history).
pub fn run_line(marker: &str, file: &str) -> String {
    format!(
        " __cr_run {marker} {}\n",
        controlroom_sandbox::launch::shell_quote(file)
    )
}
