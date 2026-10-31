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

  pnpmDeps = pkgs.fetchPnpmDeps {
    pname = "controlroom-web";
    inherit version src pnpm;
    fetcherVersion = 4;
    hash = "sha256-fYpE4mUTmRaiW2sSmvNik9mqZr1ohdX4n+/00G7lnnw=";
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
