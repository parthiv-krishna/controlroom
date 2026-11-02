# An execution host for the VM end-to-end test (devbox, buildbox): sshd, a user `alice` with a
# login shell, pi 1.2.0, git, the pinned nono, and the fixture tool's mock model on loopback.
# alice's pi config points at the mock model with a fake key; no real provider exists anywhere.
# Nothing of controlroom is installed: the server bootstraps the daemon over SSH.
{
  pkgs,
  nono,
  fixtureTool,
}:
let
  mockPort = 4000;
  mockOrigin = "http://127.0.0.1:${toString mockPort}";
  # Must match `controlroom_sandbox::agent_dir::PLACEHOLDER`: the key the session's models.json
  # carries instead of the real one.
  placeholder = "controlroom-placeholder";

  models = pkgs.writeText "models.json" (
    builtins.toJSON {
      providers.mock = {
        baseUrl = "${mockOrigin}/v1";
        api = "openai-completions";
        apiKey = "sk-fake-test-key";
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
    }
  );
  settings = pkgs.writeText "settings.json" (
    builtins.toJSON {
      defaultProvider = "mock";
      defaultModel = "mock-model";
      quietStartup = true;
    }
  );

  # The agent command. A loopback upstream is reachable from the sandbox only through the
  # credential route's reverse proxy ($CR_MOCK_BASE_URL, authenticated by the phantom token
  # $CR_MOCK_PHANTOM), so the session's models.json is pointed there first (see the daemon's
  # acceptance tests). Real providers are HTTPS and need none of this.
  # It is installed on PATH: with command mediation on, nono lets a session execute only what
  # is reachable from PATH (and the interpreters and exec targets of those scripts).
  piMock = pkgs.writeShellScriptBin "pi-mock" ''
    if [ -n "''${CR_MOCK_BASE_URL:-}" ]; then
      m="$PI_CODING_AGENT_DIR/models.json"
      sed -e "s#${mockOrigin}#$CR_MOCK_BASE_URL#" -e 's#"${placeholder}"#"$CR_MOCK_PHANTOM"#' "$m" > "$m.tmp" \
        && mv "$m.tmp" "$m" || exit 1
    fi
    export PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0
    exec pi "$@"
  '';
  agents = pkgs.writeText "agents.toml" ''
    [agents.pi]
    command = "pi-mock"
    driver = "pi"
  '';
  sandbox = pkgs.writeText "sandbox.toml" ''
    nono = "${nono}/bin/nono"
    allow_loopback_upstreams = true
    [[routes]]
    provider = "mock"
    upstream = "${mockOrigin}"
    allow = ["POST /v1/chat/completions"]
  '';
in
{
  virtualisation = {
    memorySize = 2048;
    cores = 2;
  };

  services.openssh = {
    enable = true;
    settings = {
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
    };
  };

  users.users.alice = {
    isNormalUser = true;
    shell = pkgs.bashInteractive;
  };

  environment.systemPackages = [
    pkgs.pi-coding-agent
    piMock
    pkgs.git
    nono
    pkgs.curl
    pkgs.jq
  ];

  systemd.services.mock-model = {
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${fixtureTool}/bin/controlroom-fixture-tool mock-model --listen 127.0.0.1:${toString mockPort}";
      DynamicUser = true;
    };
  };

  # alice's own configuration, as she would write it: pi's agent dir and controlroom's
  # agents.toml and sandbox.toml.
  systemd.services.alice-config = {
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      User = "alice";
    };
    script = ''
      install -Dm644 ${models} ~/.pi/agent/models.json
      install -Dm644 ${settings} ~/.pi/agent/settings.json
      install -Dm644 ${agents} ~/.config/controlroom/agents.toml
      install -Dm644 ${sandbox} ~/.config/controlroom/sandbox.toml
      mkdir -p ~/work
    '';
  };
}
