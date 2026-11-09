# The browser end-to-end test (spec §14, design §8 R2(a)) in NixOS VMs, in the remote-tools
# topology:
#
# - `server` runs everything of controlroom on the server through the NixOS module
#   (../module.nix): controlroom-web (dev-token mode on loopback) and controlroom-agentd
#   (services.controlroom.agent) with the controlroom-pi bundle and nono, so every pi runs here,
#   under nono, against the fixture tool's mock model, whose API key is a systemd credential of
#   the agent daemon. A mock push service stands in for Web Push. Headless Chromium drives the
#   UI (web/e2e-vm).
# - `devbox` and `buildbox` (./execution-host.nix) carry only sshd, bash and git: no nono. The
#   server bootstraps controlroomd over SSH, as users would, with the nono bundled with it, and
#   the agent's tools run there under that nono.
#
# The test script (./e2e.py) runs one Playwright spec per scenario and does what a user (or the
# network, or a power button) would do in between: install the key, cut a link, reboot a host.
#
# `run` always succeeds: its $out holds `status`, `seconds` (how long it took) and, on failure,
# the Playwright traces and the server, agent daemon, host daemon and mock-model logs. `check`
# fails unless the run passed, pointing at them. A failed run is cached like any build: delete
# it to run it again (`--rebuild` runs it but keeps the first output, since logs differ).
#
#   nix build .#checks.x86_64-linux.vm-e2e     # the gate
#   nix store delete $(nix path-info .#vm-e2e-run) && nix build -L .#vm-e2e-run   # run again
#   nix build .#vm-e2e-run -o result           # result/ holds the logs
#   nix build .#vm-e2e-driver -o driver && mkdir out \
#     && TMPDIR=$(mktemp -d) driver/bin/nixos-test-driver -o out   # outside the sandbox
#     (faster to iterate; `--test-script` runs an edited copy of the script on the same VMs)
{
  pkgs,
  module,
  rust,
  web,
  # services.controlroom.agent's packages: {package, piBundle, nono}.
  agent,
}:
let
  inherit (pkgs) lib;
  version = (lib.importTOML ../../Cargo.toml).workspace.package.version;
  # The static nono every bundle carries for the hosts.
  nono = rust.nonoStatic.x86_64;
  bundleNono = ''
    install -m 755 ${nono}/bin/nono $out/nono-x86_64-linux
    echo ${nono.version} > $out/nono-version
  '';

  # The bundled daemons the server deploys: the real ones, an older build (auto-upgrade), and
  # a "newer" one that can't serve (rollback); all with the same nono.
  bundles = {
    current = "${rust.controlroomWeb}/share/controlroom/daemons";
    old = pkgs.runCommand "controlroomd-old-bundle" { } ''
      mkdir $out
      install -m 755 ${rust.controlroomdStaticOld}/bin/controlroomd $out/controlroomd-x86_64-linux
      echo ${version}-old > $out/version
      ${bundleNono}
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
      ${bundleNono}
    '';
  };

  # The server's throwaway secrets.
  secrets = "/var/lib/controlroom-e2e";

  # The mock model, on the server's loopback (where pi runs).
  mockPort = 4000;
  mockOrigin = "http://127.0.0.1:${toString mockPort}";
  # Must match `controlroom_sandbox::agent_dir::PLACEHOLDER`: the key the session's models.json
  # carries instead of the real one.
  placeholder = "controlroom-placeholder";

  # The agent command. A loopback upstream is reachable from pi's sandbox only through the
  # credential route's reverse proxy ($CR_<PROVIDER>_BASE_URL, authenticated by the phantom
  # token $CR_<PROVIDER>_PHANTOM, which pi resolves from its environment), so every mock
  # provider in the session's models.json (the server's `mock`, and the `mock-ui` gateway added
  # in Settings › Models) is pointed there first, as in the agent daemon's acceptance test. Real
  # providers are HTTPS and need none of this.
  piMock = pkgs.writeShellScriptBin "pi-mock" ''
    m="$PI_CODING_AGENT_DIR/models.json"
    if [ -f "$m" ]; then
      ${pkgs.jq}/bin/jq --arg origin ${mockOrigin} --arg placeholder ${placeholder} '
        .providers |= with_entries(
          ("CR_" + (.key | ascii_upcase | gsub("[^A-Z0-9]"; "_"))) as $v
          | if ((.value.baseUrl // "") | startswith($origin)) and env[$v + "_BASE_URL"] != null
            then .value.baseUrl = env[$v + "_BASE_URL"] + (.value.baseUrl | ltrimstr($origin))
              | if .value.apiKey == $placeholder then .value.apiKey = ("$" + $v + "_PHANTOM") else . end
            else . end)' "$m" > "$m.tmp" \
        && ${pkgs.coreutils}/bin/mv "$m.tmp" "$m" || exit 1
    fi
    export PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0
    exec controlroom-pi -- "$@"
  '';

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

  host = import ./execution-host.nix { inherit pkgs; };

  secretsUnit = [ "controlroom-e2e-secrets.service" ];

  test = pkgs.testers.runNixOSTest {
    name = "controlroom-e2e";

    nodes = {
      # controlroom-web and controlroom-agentd through the NixOS module (../module.nix), so
      # their hardening is what runs SSH to the hosts, pi under nono and Web Push. Dev-token
      # mode on loopback, throwaway secrets.
      server = {
        imports = [ module ];
        virtualisation = {
          memorySize = 4096;
          cores = 4;
        };
        environment.systemPackages = [
          web.e2eVm
          pkgs.curl
          pkgs.jq
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

          agent = {
            inherit (agent) package piBundle nono;
            extraPackages = [ piMock ];
            agents.agents.pi.command = "pi-mock";
            sandbox = {
              # Tests only: a loopback model route (see `crates/agentd/src/config/sandbox.rs`).
              allow_loopback_upstreams = true;
              routes = [
                {
                  provider = "mock";
                  upstream = mockOrigin;
                  allow = [ "POST /v1/chat/completions" ];
                }
              ];
            };
            pi = {
              models.providers.mock = {
                baseUrl = "${mockOrigin}/v1";
                api = "openai-completions";
                # Resolved by `controlroom-agentd credential` (nono's credential helper) only.
                apiKey = "!cat $CREDENTIALS_DIRECTORY/mock";
                models = [
                  {
                    id = "mock-model";
                    name = "Mock";
                    input = [ "text" ];
                    contextWindow = 128000;
                    maxTokens = 4096;
                    cost = {
                      input = 0;
                      output = 0;
                      cacheRead = 0;
                      cacheWrite = 0;
                    };
                  }
                ];
              };
              settings = {
                defaultProvider = "mock";
                defaultModel = "mock-model";
                quietStartup = true;
              };
            };
            credentials.mock = "${secrets}/model-key";
          };
        };
        systemd.services.controlroom = {
          requires = secretsUnit;
          after = secretsUnit;
          environment.RUST_LOG = "info,controlroom_controller=debug";
        };
        systemd.services.controlroom-agent = {
          requires = secretsUnit;
          after = secretsUnit;
          environment.CONTROLROOM_AGENTD_LOG = "info,controlroom_runtime=debug";
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
            [ -s ${secrets}/model-key ] || echo "sk-e2e-$(openssl rand -hex 16)" > ${secrets}/model-key
          '';
        };

        # The mock model refuses completions without the model key, so a reply proves the
        # credential went from LoadCredential through nono's proxy, never through pi.
        systemd.services.mock-model = {
          wantedBy = [ "multi-user.target" ];
          requires = secretsUnit;
          after = secretsUnit;
          serviceConfig = {
            ExecStart = "${rust.workspace}/bin/controlroom-fixture-tool mock-model --listen 127.0.0.1:${toString mockPort} --api-key-file %d/key";
            LoadCredential = "key:${secrets}/model-key";
            DynamicUser = true;
          };
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
      NONO_VERSION = "${nono.version}"
      RUNNER = "${web.e2eVm}/bin/controlroom-e2e-vm"
    ''
    + builtins.readFile ./e2e.py;
  };

  run = test.overrideTestDerivation (_: {
    buildCommand = ''
      mkdir -p $out
      export LOGFILE=/dev/null
      start=$(date +%s)
      if ${test.driver}/bin/nixos-test-driver -o $out; then
        echo passed > $out/status
      else
        echo failed > $out/status
      fi
      echo $(( $(date +%s) - start )) > $out/seconds
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
