#!/usr/bin/env bash
# Acceptance (spec §11): a real `pi --mode rpc` under the pinned nono, fake credentials only, a
# local mock model. `acceptance_pi_nono` drives it through the launch glue and raw RPC lines
# (plain pi, and pi through the controlroom-pi bundle's launcher);
# `acceptance_serve` drives it through `controlroomd serve`, SIGKILLs the daemon mid-run and
# checks the run completes with no duplicate prompt, and runs a mediated `git push` that the
# test approves, then denies, through the daemon's approval webhook (needs git on PATH);
# `acceptance_agent` loads @controlroom/pi into pi: todos, auto-continue, a question and the
# answer that wakes the agent.
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
# controlroom-pi with the stub @controlroom/pi (the flake checks' bundle); its launcher goes on PATH,
# as home-manager puts it on an execution host's.
nix build -o "$work/bundle" "$repo#checks.$(nix eval --raw --impure --expr builtins.currentSystem).controlroom-pi-launcher.BUNDLE"
export CR_TEST_BUNDLE
CR_TEST_BUNDLE=$(readlink -f "$work/bundle")
export PATH="$CR_TEST_BUNDLE/bin:$PATH"
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
cargo test -q -p controlroomd --test acceptance_pi_nono --test acceptance_serve --test acceptance_agent --no-run
cargo build -q -p controlroom-fake-agent
mkdir -p "$work/home" "$work/xdg"/{config,data,state,cache}
HOME="$work/home" XDG_CONFIG_HOME="$work/xdg/config" XDG_DATA_HOME="$work/xdg/data" \
  XDG_STATE_HOME="$work/xdg/state" XDG_CACHE_HOME="$work/xdg/cache" PI_CODING_AGENT_DIR="" \
  cargo test -p controlroomd --test acceptance_pi_nono --test acceptance_serve --test acceptance_agent -- \
  --ignored --nocapture "$@"
