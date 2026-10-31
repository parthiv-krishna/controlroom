#!/usr/bin/env bash
# Playwright smoke tests against the fixture build. Not part of `nix flake check` (they need browsers).
# Browsers: $PLAYWRIGHT_BROWSERS_PATH if set, else playwright-driver.browsers from a pinned nixpkgs whose
# version matches @playwright/test in package.json (1.63).
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -z "${PLAYWRIGHT_BROWSERS_PATH:-}" ] && command -v nix >/dev/null; then
  PLAYWRIGHT_BROWSERS_PATH="$(nix build --no-link --print-out-paths github:NixOS/nixpkgs/55bfb4d7437bd4e09ab7cba58a43bd68eb310044#playwright-driver.browsers)"
  export PLAYWRIGHT_BROWSERS_PATH
fi
export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true
pnpm build:fixture
exec pnpm exec playwright test "$@"
