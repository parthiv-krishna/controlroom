#!/usr/bin/env bash
# Acceptance for the agent launch glue (spec §11): a real `pi --mode rpc` under the pinned nono,
# fake credentials only, a local mock model, driven by raw RPC lines through its supervisor.
# Needs Nix, Landlock and pi:
#
#   crates/daemon/scripts/acceptance-pi-nono.sh             # pi from PATH
#   CR_TEST_PI=/path/to/pi crates/daemon/scripts/acceptance-pi-nono.sh
#
# HOME and the XDG dirs point into a scratch root under /var/tmp (nono refuses a state dir
# under /tmp, which its system groups grant, and sockets need short paths). Everything is
# removed afterwards; the user's own pi config is never read.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
work=$(mktemp -d /var/tmp/cr-acc.XXXXXX)
cleanup() { chmod -R u+w "$work" 2>/dev/null || true; rm -rf "$work"; }
[ -n "${CR_KEEP:-}" ] || trap cleanup EXIT

nix build -o "$work/nono" "$repo#nono"
if [ -z "${CR_TEST_PI:-}" ]; then
  CR_TEST_PI=$(readlink -f "$(command -v pi)")
fi
export CR_TEST_PI
export CR_TEST_NONO="$work/nono/bin/nono"
export CR_TEST_ROOT="$work"
version=$("$CR_TEST_NONO" --version)
[ "$version" = "nono 0.80.0" ] || { echo "expected nono 0.80.0, got: $version" >&2; exit 1; }

cd "$repo"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
cargo test -q -p controlroomd --test acceptance_pi_nono --no-run
mkdir -p "$work/home" "$work/xdg"/{config,data,state,cache}
HOME="$work/home" XDG_CONFIG_HOME="$work/xdg/config" XDG_DATA_HOME="$work/xdg/data" \
  XDG_STATE_HOME="$work/xdg/state" XDG_CACHE_HOME="$work/xdg/cache" PI_CODING_AGENT_DIR="" \
  cargo test -p controlroomd --test acceptance_pi_nono -- --ignored --nocapture "$@"
