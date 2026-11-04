#!/usr/bin/env bash
# Acceptance of the remote-tools model (see crates/e2e/tests/acceptance.rs): the controller, the
# real agent daemon running real pi through the controlroom-pi bundle under the pinned nono, and
# two real host daemons whose workspaces run under nono, with a local mock model. Needs Nix,
# Landlock and pi:
#
#   crates/e2e/scripts/acceptance-pi-nono.sh
#
# HOME, TMPDIR and the XDG dirs point into a scratch root under /var/tmp (nono refuses state
# dirs under /tmp, which its system groups grant). Everything is removed afterwards; the user's
# own pi config is never read. CR_KEEP=1 keeps the scratch root.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
work=$(mktemp -d /var/tmp/cr-e2e.XXXXXX)
cleanup() { chmod -R u+w "$work" 2>/dev/null || true; rm -rf "$work"; }
[ -n "${CR_KEEP:-}" ] || trap cleanup EXIT

nix build -o "$work/nono" "$repo#nono"
nix build -o "$work/pi-bundle" "$repo#controlroom-pi"
export CR_TEST_PI_BUNDLE
CR_TEST_PI_BUNDLE=$(readlink -f "$work/pi-bundle")
export CR_TEST_NONO="$work/nono/bin/nono"
export CR_TEST_BASH
CR_TEST_BASH=$(command -v bash)
version=$("$CR_TEST_NONO" --version)
[ "$version" = "nono 0.80.0" ] || { echo "expected nono 0.80.0, got: $version" >&2; exit 1; }

cd "$repo"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
cargo build -q --workspace --bins
cargo test -q -p controlroom-e2e --test acceptance --no-run
mkdir -p "$work/home" "$work/tmp" "$work/xdg"/{config,data,state,cache}
HOME="$work/home" TMPDIR="$work/tmp" XDG_CONFIG_HOME="$work/xdg/config" \
  XDG_DATA_HOME="$work/xdg/data" XDG_STATE_HOME="$work/xdg/state" \
  XDG_CACHE_HOME="$work/xdg/cache" PI_CODING_AGENT_DIR="" \
  cargo test -p controlroom-e2e --test acceptance -- --ignored --nocapture "$@"
