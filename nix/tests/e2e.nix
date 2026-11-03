# The browser end-to-end test (spec §17 P4) in NixOS VMs: `server` runs controlroom-web through
# the NixOS module (../module.nix; dev-token mode on loopback) next to a mock push service, and
# drives the UI with headless Chromium (web/e2e-vm); `devbox` and `buildbox` are execution hosts
# (./execution-host.nix). The test script (./e2e.py) runs one Playwright spec per scenario and
# does what a user would do on the hosts in between (install the key, block the network, ...).
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
  module,
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

  # The server's throwaway secrets.
  secrets = "/var/lib/controlroom-e2e";

  pushAddress = "11.0.0.1";
  pushTls = pkgs.runCommand "push-mock-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 3650 \
      -subj /CN=push.example.com -addext subjectAltName=DNS:push.example.com \
      -keyout $out/key.pem -out $out/cert.pem
  '';
  # Logs the server name of each TLS handshake and how it ended (the server, not trusting the
  # certificate, gives up).
  pushMock = pkgs.writers.writePython3 "push-mock" { flakeIgnore = [ "E501" ]; } ''
    import socket
    import ssl

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain("${pushTls}/cert.pem", "${pushTls}/key.pem")
    context.num_tickets = 0
    server_name = None


    def remember(_socket, name, _context):
        global server_name
        server_name = name


    context.sni_callback = remember
    listener = socket.create_server(("${pushAddress}", 443))
    while True:
        connection, _ = listener.accept()
        server_name = None
        try:
            with context.wrap_socket(connection, server_side=True) as tls:
                tls.recv(1)
                outcome = "closed"
        except (ssl.SSLError, OSError) as e:
            outcome = str(e)
        print(f"TLS handshake for {server_name}: {outcome}", flush=True)
  '';

  host = import ./execution-host.nix {
    inherit pkgs nono;
    fixtureTool = rust.workspace;
  };

  test = pkgs.testers.runNixOSTest {
    name = "controlroom-e2e";

    nodes = {
      # controlroom-web through the NixOS module (../module.nix), so its hardening is what runs
      # SSH to the hosts and Web Push. Dev-token mode on loopback, throwaway secrets.
      server = {
        imports = [ module ];
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
        services.controlroom = {
          enable = true;
          package = rust.controlroomWeb;
          auth.mode = "dev-token";
          masterKeyFile = "${secrets}/master-key";
          push = {
            vapidPrivateKeyFile = "${secrets}/vapid-key";
            subject = "mailto:alice@example.com";
          };
          settings.bundled_daemons_dir = "/run/controlroom-e2e/daemons";
        };
        systemd.services.controlroom = {
          requires = [ "controlroom-e2e-secrets.service" ];
          after = [ "controlroom-e2e-secrets.service" ];
          environment.RUST_LOG = "info,controlroom_controller=debug";
        };
        # Root-only secrets, made once per VM as a deployer would (kept across restarts: the
        # master key seals the SSH keys in the store).
        systemd.services.controlroom-e2e-secrets = {
          path = [ pkgs.openssl ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          script = ''
            umask 077
            mkdir -p ${secrets}
            [ -s ${secrets}/master-key ] || openssl rand -base64 32 > ${secrets}/master-key
            [ -s ${secrets}/vapid-key ] \
              || openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 > ${secrets}/vapid-key
          '';
        };

        # The mock push service: push.example.com resolves to an address the server's SSRF guard
        # counts as public, which exists only on this VM's loopback. Its certificate isn't
        # trusted (the server only trusts the public web PKI), so a push ends after the TLS
        # handshake; the mock logs the name each handshake asked for.
        networking.hosts.${pushAddress} = [ "push.example.com" ];
        systemd.services.push-mock = {
          wantedBy = [ "multi-user.target" ];
          path = [ pkgs.iproute2 ];
          preStart = "ip address replace ${pushAddress}/32 dev lo";
          serviceConfig.ExecStart = pushMock;
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
