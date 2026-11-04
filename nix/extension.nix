# @controlroom/pi (extensions/pi): type-checked and unit-tested against the pinned pi. The
# extension has no dependencies of its own: pi provides its imports at runtime (its tools reuse
# pi's edit logic, image reads and renderers), so the check links pi's package where Node and
# tsc resolve it. The tests use a fake pi API and a fake agent daemon socket.
#
# Locally (from extensions/pi):
#   mkdir -p node_modules/@earendil-works
#   ln -sfn "$(nix build --no-link --print-out-paths --inputs-from ../.. nixpkgs#pi-coding-agent)/lib/node_modules/pi-monorepo" \
#     node_modules/@earendil-works/pi-coding-agent
#   node --test test/*.test.ts
{ pkgs }:
let
  inherit (pkgs) lib;
  pi = pkgs.pi-coding-agent;
in
{
  check = pkgs.stdenvNoCC.mkDerivation {
    pname = "controlroom-pi-extension-check";
    version = (lib.importJSON ../extensions/pi/package.json).version;
    src = lib.cleanSource ../extensions/pi;
    # pi's own runtime (it runs the extension) and the TypeScript compiler.
    nativeBuildInputs = [
      pkgs.nodejs
      pkgs.typescript
    ];
    buildPhase = ''
      runHook preBuild
      mkdir -p node_modules/@earendil-works
      ln -s ${pi}/lib/node_modules/pi-monorepo node_modules/@earendil-works/pi-coding-agent
      tsc -p tsconfig.json
      HOME=$TMPDIR node --experimental-strip-types --no-warnings --test test/*.test.ts
      runHook postBuild
    '';
    installPhase = "touch $out";
  };
}
