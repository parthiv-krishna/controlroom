# The Svelte UI (web/), built with pnpm in the Nix sandbox on Node 22.
# `pnpmDeps` is a fixed-output derivation: after changing web/pnpm-lock.yaml,
# set its hash to "" and copy the hash Nix reports back in.
{ pkgs }:
let
  inherit (pkgs) lib;

  nodejs = pkgs.nodejs_22;
  pnpm = pkgs.pnpm.override { nodejs-slim = pkgs.nodejs-slim_22; };

  version = (lib.importJSON ../web/package.json).version;

  src = lib.cleanSourceWith {
    name = "controlroom-web-src";
    src = lib.cleanSource ../web;
    filter =
      path: _type:
      !builtins.elem (baseNameOf path) [
        "node_modules"
        "dist"
        "test-results"
        "playwright-report"
      ];
  };
  # The UI build doesn't read the VM specs, so editing them doesn't rebuild the server.
  distSrc = lib.cleanSourceWith {
    name = "controlroom-web-dist-src";
    inherit src;
    filter = path: _type: baseNameOf path != "e2e-vm";
  };

  pnpmDeps = pkgs.fetchPnpmDeps {
    pname = "controlroom-web";
    inherit version src pnpm;
    fetcherVersion = 4;
    hash = "sha256-ZiM7WbUNL02kt+uXZEhP761XSMQLxjV5gdpjrTHxiFo=";
  };

  mkWeb =
    args:
    pkgs.stdenvNoCC.mkDerivation (
      {
        inherit version src pnpmDeps;
        nativeBuildInputs = [
          nodejs
          pnpm
          pkgs.pnpmConfigHook
        ];
      }
      // args
    );
in
{
  inherit nodejs pnpm pnpmDeps;

  # The production build (web/dist).
  dist = mkWeb {
    pname = "controlroom-web-dist";
    src = distSrc;
    buildPhase = ''
      runHook preBuild
      pnpm build
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      test -f dist/index.html
      cp -r dist $out
      runHook postInstall
    '';
  };

  # The Playwright specs against the real server (web/e2e-vm), with their node_modules and the
  # browsers pinned to the same Playwright version; the NixOS VM test runs them.
  # `controlroom-e2e-vm [playwright test args]` reads its settings from E2E_* variables (see
  # web/e2e-vm/playwright.config.ts).
  e2eVm = mkWeb {
    pname = "controlroom-e2e-vm";
    nativeBuildInputs = [
      nodejs
      pnpm
      pkgs.pnpmConfigHook
      pkgs.makeWrapper
    ];
    dontBuild = true;
    installPhase = ''
      runHook preInstall
      lib=$out/lib/controlroom-e2e-vm
      mkdir -p $lib
      cp -r node_modules package.json tsconfig.json e2e-vm $lib/
      [ "$(node -p 'require("./node_modules/@playwright/test/package.json").version')" = "${pkgs.playwright-driver.version}" ]
      makeWrapper ${nodejs}/bin/node $out/bin/controlroom-e2e-vm \
        --chdir $lib \
        --set PLAYWRIGHT_BROWSERS_PATH ${pkgs.playwright-driver.browsers} \
        --set PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS true \
        --add-flags "$lib/node_modules/@playwright/test/cli.js test --config e2e-vm/playwright.config.ts"
      runHook postInstall
    '';
  };

  # svelte-check and the vitest suite.
  check = mkWeb {
    pname = "controlroom-web-check";
    buildPhase = ''
      runHook preBuild
      pnpm check
      pnpm test
      runHook postBuild
    '';
    installPhase = "touch $out";
  };
}
