#!/usr/bin/env bash
# Re-runs the nono spike's checks against generated profiles on this host (spec §11.5:
# re-run on every nono bump). Needs Nix, Landlock and the dev shell's cargo.
#
#   crates/sandbox/scripts/validate-nono.sh            # all checks, pi from PATH if present
#   CR_TEST_PI=/path/to/pi crates/sandbox/scripts/validate-nono.sh
#
# Inputs are built into a temporary directory with GC roots and removed afterwards. Model
# traffic only reaches a local mock server; no real provider or key is used.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
work=$(mktemp -d "${TMPDIR:-/tmp}/cr-validate-nono.XXXXXX")
trap 'rm -rf "$work"' EXIT

nix build -o "$work/nono" nixpkgs#nono
nix build -o "$work/env" --impure --expr '
  with import <nixpkgs> {};
  buildEnv {
    name = "cr-sandbox-test-env";
    paths = [ bashInteractive coreutils-full findutils gnugrep git openssh curl getent util-linux nodejs ];
  }'
# coreutils-full, as on a NixOS system PATH: /usr/bin/env (plain coreutils) is then not on PATH.
# A second, different git store copy: calling it by absolute path must not bypass mediation.
nix build -o "$work/shadow" nixpkgs#gitMinimal

(cd "$repo" && cargo build -q -p controlroomd)

export CR_TEST_NONO="$work/nono/bin/nono"
CR_TEST_PATH="$(readlink -f "$work/env")/bin"
export CR_TEST_PATH
CR_TEST_SHADOW_PATH="$(readlink -f "$work/shadow")/bin"
export CR_TEST_SHADOW_PATH
export CR_TEST_CONTROLROOMD="$repo/target/debug/controlroomd"
if [ -z "${CR_TEST_PI:-}" ] && command -v pi >/dev/null; then
  CR_TEST_PI=$(readlink -f "$(command -v pi)")
  export CR_TEST_PI
fi

version=$("$CR_TEST_NONO" --version)
[ "$version" = "nono 0.80.0" ] || { echo "expected nono 0.80.0, got: $version" >&2; exit 1; }

skip=()
[ -n "${CR_TEST_PI:-}" ] || skip=(--skip pi_rpc_stdout_is_clean_jsonl)
cd "$repo"
cargo test -p controlroom-sandbox --test nono -- --ignored --test-threads=1 "${skip[@]}" "$@"
