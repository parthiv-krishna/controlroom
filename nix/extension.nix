# @controlroom/pi (extensions/pi): its unit tests on Node 22, without pi (they use a fake pi and
# a fake daemon socket). The extension has no dependencies of its own; pi provides its imports.
{ pkgs }:
let
  inherit (pkgs) lib;
in
{
  check = pkgs.stdenvNoCC.mkDerivation {
    pname = "controlroom-pi-extension-check";
    version = (lib.importJSON ../extensions/pi/package.json).version;
    src = lib.cleanSource ../extensions/pi;
    nativeBuildInputs = [ pkgs.nodejs_22 ];
    buildPhase = ''
      runHook preBuild
      node --experimental-strip-types --no-warnings --test test/*.test.ts
      runHook postBuild
    '';
    installPhase = "touch $out";
  };
}
