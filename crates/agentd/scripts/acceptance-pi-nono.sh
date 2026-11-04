#!/usr/bin/env bash
# Acceptance (design §1, §4, spec §11): `controlroom-agentd serve` runs real pi through the
# controlroom-pi bundle (with @controlroom/pi from extensions/pi) under the pinned nono at level
# strict, with fake credentials and a local mock model; a fake controller in the test plays the
# workspace's host. See tests/acceptance.rs. Needs Nix, Landlock and pi:
#
#   crates/agentd/scripts/acceptance-pi-nono.sh
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
nix build -o "$work/pi-bundle" "$repo#controlroom-pi"
export CR_TEST_PI_BUNDLE
CR_TEST_PI_BUNDLE=$(readlink -f "$work/pi-bundle")
export CR_TEST_NONO="$work/nono/bin/nono"
export CR_TEST_ROOT="$work"
version=$("$CR_TEST_NONO" --version)
[ "$version" = "nono 0.80.0" ] || { echo "expected nono 0.80.0, got: $version" >&2; exit 1; }

cd "$repo"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
cargo test -q -p controlroom-agentd --test acceptance --no-run
cargo build -q -p controlroom-fake-agent
mkdir -p "$work/home" "$work/xdg"/{config,data,state,cache}
HOME="$work/home" XDG_CONFIG_HOME="$work/xdg/config" XDG_DATA_HOME="$work/xdg/data" \
  XDG_STATE_HOME="$work/xdg/state" XDG_CACHE_HOME="$work/xdg/cache" PI_CODING_AGENT_DIR="" \
  cargo test -p controlroom-agentd --test acceptance -- --ignored --nocapture "$@"
