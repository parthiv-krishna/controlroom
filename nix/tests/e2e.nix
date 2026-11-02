# The browser end-to-end test (spec §17 P4) in NixOS VMs: `server` runs controlroom-web
# (dev-token mode on loopback) and drives the UI with headless Chromium (web/e2e-vm); `devbox`
# and `buildbox` are execution hosts (./execution-host.nix). The test script (./e2e.py) runs one
# Playwright spec per scenario and does what a user would do on the hosts in between (install
# the key, block the network, ...).
#
# `run` always succeeds: its $out holds `status` plus, on failure, the Playwright traces and the
# server, daemon and mock-model logs. `check` fails unless the run passed, pointing at them. A
# failed run is cached like any build; re-run it with `nix build --rebuild <run>`.
#
#   nix build .#checks.x86_64-linux.vm-e2e     # the gate
#   nix build .#vm-e2e-run --rebuild -L        # run again; see result/
#   nix build .#vm-e2e-driver -o driver && TMPDIR=$(mktemp -d) driver/bin/nixos-test-driver -o out
#                                              # outside the sandbox (faster to iterate)
{
  pkgs,
  rust,
  web,
  nono,
}:
let
  inherit (pkgs) lib;
  version = (lib.importTOML ../../Cargo.toml).workspace.package.version;

  # The bundled daemons the server deploys: the real ones, an older build (auto-upgrade), and
  # a "newer" one that can't serve (rollback).
  bundles = {
    current = "${rust.controlroomWeb}/share/controlroom/daemons";
    old = pkgs.runCommand "controlroomd-old-bundle" { } ''
      mkdir $out
      install -m 755 ${rust.controlroomdStaticOld}/bin/controlroomd $out/controlroomd-x86_64-linux
      echo ${version}-old > $out/version
    '';
    broken = pkgs.runCommand "controlroomd-broken-bundle" { } ''
      mkdir $out
      cat > $out/controlroomd-x86_64-linux <<'EOF'
      #!/bin/sh
      if [ "$1" = --version ]; then echo 'controlroomd 99.0.0'; exit 0; fi
      echo "this daemon can't serve" >&2
      exit 1
      EOF
      chmod 755 $out/controlroomd-x86_64-linux
      echo 99.0.0 > $out/version
    '';
  };

  host = import ./execution-host.nix {
    inherit pkgs nono;
    fixtureTool = rust.workspace;
  };

  test = pkgs.testers.runNixOSTest {
    name = "controlroom-e2e";

    nodes = {
      server = {
        virtualisation = {
          memorySize = 3072;
          cores = 2;
        };
        environment.systemPackages = [
          web.e2eVm
          pkgs.curl
        ];
        # The test flips this link to switch the bundled daemons, then restarts the server.
        systemd.tmpfiles.rules = [ "L+ /run/controlroom-e2e/daemons - - - - ${bundles.current}" ];
        systemd.services.controlroom-web = {
          wantedBy = [ "multi-user.target" ];
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          environment.RUST_LOG = "info,controlroom_controller=debug";
          # A throwaway master key, kept across restarts (it seals the SSH keys in the store).
          preStart = ''
            if [ ! -s "$STATE_DIRECTORY/master-key" ]; then
              (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$STATE_DIRECTORY/master-key")
            fi
          '';
          serviceConfig = {
            DynamicUser = true;
            StateDirectory = "controlroom";
            ExecStart = lib.concatStringsSep " " [
              "${rust.controlroomWeb}/bin/controlroom-web"
              "--listen 127.0.0.1:8080"
              "--state-dir /var/lib/controlroom"
              "--master-key-file /var/lib/controlroom/master-key"
              "--auth-mode dev-token"
              "--bundled-daemons-dir /run/controlroom-e2e/daemons"
            ];
          };
        };
      };
      devbox = host;
      buildbox = host;
    };

    testScript = ''
      CURRENT_BUNDLE = "${bundles.current}"
      OLD_BUNDLE = "${bundles.old}"
      BROKEN_BUNDLE = "${bundles.broken}"
      VERSION = "${version}"
      RUNNER = "${web.e2eVm}/bin/controlroom-e2e-vm"
    ''
    + builtins.readFile ./e2e.py;
  };

  run = test.overrideTestDerivation (_: {
    buildCommand = ''
      mkdir -p $out
      export LOGFILE=/dev/null
      if ${test.driver}/bin/nixos-test-driver -o $out; then
        echo passed > $out/status
      else
        echo failed > $out/status
      fi
    '';
  });
in
{
  inherit test run;

  check = pkgs.runCommand "controlroom-vm-e2e" { } ''
    if [ "$(cat ${run}/status)" != passed ]; then
      echo "The VM end-to-end test failed. Traces and logs: ${run}" >&2
      exit 1
    fi
    touch $out
  '';
}
