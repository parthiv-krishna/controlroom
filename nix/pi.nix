# controlroom-pi (spec §9, §14, §20): the pinned pi, @controlroom/pi, the OAuth helper and the
# launcher, used by the agent daemon on the controlroom server. pi runs there with
# `--no-builtin-tools`: @controlroom/pi's file and shell tools work on the workspaces' hosts.
#
#   bin/controlroom-pi             pi with controlroom's layering: `--no-extensions -e
#                                  <@controlroom/pi> -e <keep-list…>` (nix/pi/controlroom-pi.sh;
#                                  refuses overlapping extensions and pi-lens)
#   bin/controlroom-pi-oauth       prints a fresh OAuth access token, refreshed by pi under the
#                                  auth.json lock (nix/pi/oauth-token.mjs); controlroomd runs it
#   libexec/controlroom-pi/tools   fd and rg as pi's wrapper uses them, for sandbox.toml's
#                                  `exec_dirs` (nono only executes PATH and listed programs)
#   share/controlroom-pi/extension @controlroom/pi (extensions/pi), when present
#
# Bump pi only together with the RPC fixtures (fixtures/pi-rpc) and the nono validation runs.
{ pkgs }:
let
  inherit (pkgs) lib;

  pi = pkgs.pi-coding-agent;
  piVersion = "1.2.0";
  piPackage = "${pi}/lib/node_modules/pi-monorepo";
  inherit (pkgs) nodejs;
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  # @controlroom/pi as a package directory (TypeScript, loaded by pi's jiti).
  extensionFrom =
    src:
    pkgs.runCommand "controlroom-pi-extension" { } ''
      cp -r ${src} $out
      test -f $out/package.json
    '';

  # pi's wrapper puts these first on PATH; the same store paths, so nono allows them.
  tools = pkgs.runCommand "controlroom-pi-tools" { } ''
    grep -q '${pkgs.fd}/bin' ${pi}/bin/pi
    grep -q '${pkgs.ripgrep}/bin' ${pi}/bin/pi
    mkdir -p $out/libexec/controlroom-pi/tools
    ln -s ${pkgs.fd}/bin/fd ${pkgs.ripgrep}/bin/rg $out/libexec/controlroom-pi/tools/
  '';

  # The helper script next to links to pi's own packages, which it imports.
  oauthScript = pkgs.runCommand "controlroom-pi-oauth-script" { } ''
    mkdir -p $out/node_modules/@earendil-works
    cp ${./pi/oauth-token.mjs} $out/oauth-token.mjs
    ln -s ${piPackage} $out/node_modules/@earendil-works/pi-coding-agent
    ln -s ${piPackage}/node_modules/@earendil-works/pi-ai $out/node_modules/@earendil-works/pi-ai
  '';

  oauthHelper = pkgs.writeShellApplication {
    name = "controlroom-pi-oauth";
    text = ''
      PI_OFFLINE=1 PI_TELEMETRY=0 exec ${nodejs}/bin/node ${oauthScript}/oauth-token.mjs "$@"
    '';
  };

  launcher =
    extension:
    pkgs.writeShellApplication {
      name = "controlroom-pi";
      text =
        builtins.replaceStrings
          [ "@pi@" "@extension@" ]
          [
            "${pi}/bin/pi"
            (if extension == null then "" else "${extension}")
          ]
          (builtins.readFile ./pi/controlroom-pi.sh);
    };

  # The bundle around one build of @controlroom/pi (or none).
  mkBundle =
    extension:
    assert lib.assertMsg (
      pi.version == piVersion
    ) "controlroom-pi pins pi ${piVersion}; nixpkgs has ${pi.version}";
    pkgs.symlinkJoin {
      name = "controlroom-pi-${version}";
      paths = [
        (launcher extension)
        oauthHelper
        tools
      ]
      ++ lib.optional (extension != null) (
        pkgs.runCommand "controlroom-pi-extension-link" { } ''
          mkdir -p $out/share/controlroom-pi
          ln -s ${extension} $out/share/controlroom-pi/extension
        ''
      );
      passthru = { inherit pi extension; };
      meta = {
        description = "pi pinned for controlroom, with @controlroom/pi and its launcher";
        mainProgram = "controlroom-pi";
        platforms = lib.platforms.linux;
      };
    };

  # What pi loads: the manifest and the sources (no tests, no tsconfig). It has no npm
  # dependencies; pi provides its imports.
  extension =
    if builtins.pathExists ../extensions/pi then
      extensionFrom (
        lib.fileset.toSource {
          root = ../extensions/pi;
          fileset = lib.fileset.unions [
            ../extensions/pi/package.json
            ../extensions/pi/src
          ];
        }
      )
    else
      null;

  # The launcher checks use a stub: it registers a command they can see in `get_commands`, while
  # the real extension registers nothing outside a controlroom session.
  testBundle = mkBundle (extensionFrom ./pi/tests/stub-extension);

  check =
    name: script:
    pkgs.runCommand "controlroom-pi-${name}-check"
      {
        BUNDLE = testBundle;
        JQ = "${pkgs.jq}/bin/jq";
        NODE = "${nodejs}/bin/node";
        MOCK = ./pi/tests/mock-oauth.mjs;
        nativeBuildInputs = [ pkgs.shellcheck ];
      }
      ''
        shellcheck --shell=bash ${script}
        set -euo pipefail
        source ${script}
        touch $out
      '';
in
{
  bundle = mkBundle extension;
  checks = {
    controlroom-pi-launcher = check "launcher" ./pi/tests/launcher.sh;
    controlroom-pi-oauth = check "oauth" ./pi/tests/oauth.sh;
  };
}
