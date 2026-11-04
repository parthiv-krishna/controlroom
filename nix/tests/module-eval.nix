# Evaluates `services.controlroom` (../module.nix) in a few configurations and compares its
# assertions and the generated settings and credentials with what is expected. Pure
# evaluation, no VM: the runtime side is ./module.nix.
#
#   nix build .#checks.x86_64-linux.module-eval
{ pkgs, module }:
let
  inherit (pkgs) lib;

  nixos =
    services:
    import "${pkgs.path}/nixos/lib/eval-config.nix" {
      system = null;
      modules = [
        module
        {
          nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
          boot.isContainer = true;
          system.stateVersion = lib.trivial.release;
          services.controlroom = {
            enable = true;
            # Never built: only the module's own logic is evaluated.
            package = pkgs.emptyDirectory;
            agent = {
              package = pkgs.emptyDirectory;
              piBundle = pkgs.emptyDirectory;
              nono = pkgs.emptyDirectory;
            };
          };
        }
        { inherit services; }
      ];
    };
  eval = services: (nixos services).config;
  evalOptions = services: (nixos services).options;

  # The failed assertions defined by the module (other modules' aren't evaluated: some of them
  # need a complete system).
  failures =
    services:
    let
      definitions = (evalOptions services).assertions.definitionsWithLocations;
      ours = lib.concatMap (d: d.value) (lib.filter (d: d.file == toString module) definitions);
    in
    map (a: a.message) (lib.filter (a: !a.assertion) ours);

  rendered =
    services:
    let
      config = eval services;
    in
    {
      inherit (config.services.controlroom) settings;
      inherit (config.systemd.services.controlroom.serviceConfig) LoadCredential;
    };

  oidc = {
    controlroom = {
      publicUrl = "https://controlroom.example.com";
      masterKeyFile = "/run/secrets/master-key";
      auth.oidc = {
        issuer = "https://id.example.com";
        clientIdFile = "/run/secrets/client-id";
        clientSecretFile = "/run/secrets/client-secret";
        allowedGroups = [ "admin" ];
      };
      push.vapidPrivateKeyFile = "/run/secrets/vapid";
    };
  };

  actual = {
    nothingConfigured = failures { };
    oidc = failures oidc;
    oidcRendered = rendered oidc;
    bothClientIds = failures (
      lib.recursiveUpdate oidc { controlroom.auth.oidc.clientId = "controlroom"; }
    );
    devTokenOnLoopback = failures {
      controlroom = {
        masterKeyFile = "/run/secrets/master-key";
        auth.mode = "dev-token";
      };
    };
    devTokenRendered = rendered {
      controlroom = {
        masterKeyFile = "/run/secrets/master-key";
        auth.mode = "dev-token";
        port = 9000;
        settings.session_ttl_days = 7;
      };
    };
    devTokenEverywhere = failures {
      controlroom = {
        masterKeyFile = "/run/secrets/master-key";
        auth.mode = "dev-token";
        listenAddress = "0.0.0.0";
      };
    };
  };

  expected = {
    nothingConfigured = [
      "services.controlroom.masterKeyFile is required: the master key seals the SSH keys in the database and is never generated (e.g. `openssl rand -base64 32`, kept outside the Nix store)."
      "services.controlroom.publicUrl must be set to an https origin with OIDC (it defines the redirect URI and the allowed Host and Origin)."
      "services.controlroom.auth.oidc needs issuer and clientSecretFile."
      "services.controlroom.auth.oidc needs exactly one of clientId and clientIdFile."
      "services.controlroom.auth.oidc needs allowedSubjects and/or allowedGroups; nobody could sign in."
    ];
    oidc = [ ];
    oidcRendered = {
      settings = {
        listen = "127.0.0.1:8080";
        agentd_socket = "/run/controlroom-agent/agentd.sock";
        public_url = "https://controlroom.example.com";
        master_key_file = "master-key";
        auth = {
          mode = "oidc";
          oidc = {
            issuer = "https://id.example.com";
            client_id_file = "oidc-client-id";
            client_secret_file = "oidc-client-secret";
            allowed_subjects = [ ];
            allowed_groups = [ "admin" ];
          };
        };
        push.vapid_key_file = "vapid-key";
      };
      LoadCredential = [
        "master-key:/run/secrets/master-key"
        "oidc-client-id:/run/secrets/client-id"
        "oidc-client-secret:/run/secrets/client-secret"
        "vapid-key:/run/secrets/vapid"
      ];
    };
    bothClientIds = [
      "services.controlroom.auth.oidc needs exactly one of clientId and clientIdFile."
    ];
    devTokenOnLoopback = [ ];
    devTokenRendered = {
      settings = {
        listen = "127.0.0.1:9000";
        agentd_socket = "/run/controlroom-agent/agentd.sock";
        master_key_file = "master-key";
        auth.mode = "dev-token";
        session_ttl_days = 7;
      };
      LoadCredential = [ "master-key:/run/secrets/master-key" ];
    };
    devTokenEverywhere = [
      "services.controlroom.auth.mode = \"dev-token\" is only allowed on a loopback listenAddress."
    ];
  };

  json = name: value: pkgs.writeText name (builtins.toJSON value);
in
pkgs.runCommand "controlroom-module-eval" { nativeBuildInputs = [ pkgs.jq ]; } ''
  if ! diff -u <(jq -S . ${json "expected.json" expected}) <(jq -S . ${json "actual.json" actual}); then
    echo "services.controlroom evaluates differently than expected (- expected, + actual)" >&2
    exit 1
  fi
  touch $out
''
