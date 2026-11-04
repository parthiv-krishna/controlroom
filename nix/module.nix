# The NixOS module `services.controlroom` (spec §14): controlroom-web as a hardened systemd
# service, next to the agent daemon that runs the pi sessions (`services.controlroom.agent`,
# ./agent-module.nix). The flake exports it as `nixosModules.default`, with the packages
# defaulting to the flake's own.
#
# Secrets never enter the Nix store: they are absolute paths to files (e.g. sops-nix or agenix
# outputs, readable by root only), handed to the service with `LoadCredential=`, which systemd
# reads as root. The generated configuration names them by credential: master-key,
# oidc-client-id, oidc-client-secret and vapid-key.
#
# The server listens on loopback; the reverse proxy (TLS, the public name) and the identity
# provider's client registration belong to the deployer's own configuration.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.services.controlroom;
  oidc = cfg.auth.oidc;
  settingsFormat = pkgs.formats.toml { };

  credentials = {
    master-key = cfg.masterKeyFile;
    oidc-client-id = if cfg.auth.mode == "oidc" then oidc.clientIdFile else null;
    oidc-client-secret = if cfg.auth.mode == "oidc" then oidc.clientSecretFile else null;
    vapid-key = cfg.push.vapidPrivateKeyFile;
  };
  loadedCredentials = lib.filterAttrs (_: file: file != null) credentials;

  listen =
    if lib.hasInfix ":" cfg.listenAddress then
      "[${cfg.listenAddress}]:${toString cfg.port}"
    else
      "${cfg.listenAddress}:${toString cfg.port}";
  isLoopback = lib.hasPrefix "127." cfg.listenAddress || cfg.listenAddress == "::1";

  configFile = settingsFormat.generate "controlroom-web.toml" cfg.settings;
in
{
  imports = [ ./agent-module.nix ];

  options.services.controlroom = {
    enable = lib.mkEnableOption "controlroom, the web server supervising pi coding agents over SSH";

    package = mkOption {
      type = types.package;
      description = "The controlroom-web package (it bundles the UI and the static daemons).";
    };

    listenAddress = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = ''
        The address to listen on. Keep it on loopback behind a TLS-terminating reverse proxy;
        a non-loopback address requires OIDC.
      '';
    };

    port = mkOption {
      type = types.port;
      default = 8080;
      description = "The TCP port to listen on.";
    };

    publicUrl = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "https://controlroom.example.com";
      description = ''
        The origin browsers use: a bare https origin (the app is served at `/`). It defines
        the OIDC redirect URI (`<publicUrl>/auth/callback`), the Host allowlist and the
        expected Origin; forwarded headers are never trusted. Required with OIDC.
      '';
    };

    masterKeyFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/run/secrets/controlroom/master_key";
      description = ''
        The master key that seals the SSH keys in the database (required): 32 random bytes,
        raw, hex or base64, e.g. `openssl rand -base64 32`. Keep it outside the Nix store; it
        is loaded with `LoadCredential=`. Losing it means re-adding every machine's key.
      '';
    };

    auth = {
      mode = mkOption {
        type = types.enum [
          "oidc"
          "dev-token"
        ];
        default = "oidc";
        description = ''
          `oidc` for deployments. `dev-token` (a random sign-in token in the state directory)
          is for development and tests on a loopback address only.
        '';
      };

      oidc = {
        issuer = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "https://id.example.com";
          description = "The OIDC issuer; its discovery document is fetched from `<issuer>/.well-known/openid-configuration`.";
        };

        clientId = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "controlroom";
          description = "The client id. Set this or `clientIdFile`.";
        };

        clientIdFile = mkOption {
          type = types.nullOr types.path;
          default = null;
          description = "A file holding the client id, for identity providers whose client ids are generated secrets. Set this or `clientId`.";
        };

        clientSecretFile = mkOption {
          type = types.nullOr types.path;
          default = null;
          description = "A file holding the client secret (the confidential client authenticates with `client_secret_basic`).";
        };

        allowedSubjects = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "OIDC subjects (`sub`) that may sign in. Re-checked on every request.";
        };

        allowedGroups = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "admin" ];
          description = "Members of these groups (the `groups` claim) may sign in. Re-checked on every request.";
        };
      };
    };

    push = {
      vapidPrivateKeyFile = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          The VAPID private key for Web Push: a PKCS#8 PEM P-256 key, e.g.
          `openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256`, or a base64url raw
          scalar. The public key is derived from it. Without it, Web Push is off.
        '';
      };

      subject = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "mailto:alice@example.com";
        description = "The VAPID contact (`mailto:` or `https:`) push services see. Defaults to `publicUrl`.";
      };
    };

    settings = mkOption {
      type = types.submodule { freeformType = settingsFormat.type; };
      default = { };
      example = {
        session_ttl_days = 14;
      };
      description = ''
        controlroom-web's TOML configuration (see `crates/web/src/config`). The options above
        set their keys; use this for the others, e.g. `session_ttl_days`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions =
      let
        isOidc = cfg.auth.mode == "oidc";
      in
      [
        {
          assertion = cfg.masterKeyFile != null;
          message = "services.controlroom.masterKeyFile is required: the master key seals the SSH keys in the database and is never generated (e.g. `openssl rand -base64 32`, kept outside the Nix store).";
        }
        {
          assertion = isOidc -> cfg.publicUrl != null && lib.hasPrefix "https://" cfg.publicUrl;
          message = "services.controlroom.publicUrl must be set to an https origin with OIDC (it defines the redirect URI and the allowed Host and Origin).";
        }
        {
          assertion = isOidc -> oidc.issuer != null && oidc.clientSecretFile != null;
          message = "services.controlroom.auth.oidc needs issuer and clientSecretFile.";
        }
        {
          assertion = isOidc -> (oidc.clientId == null) != (oidc.clientIdFile == null);
          message = "services.controlroom.auth.oidc needs exactly one of clientId and clientIdFile.";
        }
        {
          assertion = isOidc -> oidc.allowedSubjects != [ ] || oidc.allowedGroups != [ ];
          message = "services.controlroom.auth.oidc needs allowedSubjects and/or allowedGroups; nobody could sign in.";
        }
        {
          assertion = !isOidc -> isLoopback;
          message = "services.controlroom.auth.mode = \"dev-token\" is only allowed on a loopback listenAddress.";
        }
      ];

    services.controlroom.settings = lib.mkMerge [
      {
        inherit listen;
        # Relative secret paths name credentials (LoadCredential below).
        master_key_file = "master-key";
        auth.mode = cfg.auth.mode;
      }
      (lib.mkIf cfg.agent.enable { agentd_socket = cfg.agent.socket; })
      (lib.mkIf (cfg.publicUrl != null) { public_url = cfg.publicUrl; })
      (lib.mkIf (cfg.auth.mode == "oidc") {
        auth.oidc = {
          issuer = oidc.issuer;
          client_secret_file = "oidc-client-secret";
          allowed_subjects = oidc.allowedSubjects;
          allowed_groups = oidc.allowedGroups;
        }
        // (
          if oidc.clientIdFile != null then
            { client_id_file = "oidc-client-id"; }
          else
            { client_id = oidc.clientId; }
        );
      })
      (lib.mkIf (cfg.push.vapidPrivateKeyFile != null) {
        push = {
          vapid_key_file = "vapid-key";
        }
        // lib.optionalAttrs (cfg.push.subject != null) { inherit (cfg.push) subject; };
      })
    ];

    systemd.services.controlroom = {
      description = "controlroom web server";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [
        "network-online.target"
      ]
      # The agent daemon's socket exists before the controller connects (it retries anyway).
      ++ lib.optional cfg.agent.enable "controlroom-agent.service";
      # Keep restarting (e.g. while a credential is still missing) instead of giving up.
      startLimitIntervalSec = 0;

      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "controlroom-web"} --config ${configFile}";
        Restart = "on-failure";
        RestartSec = "5s";

        DynamicUser = true;
        # /var/lib/private/controlroom, exposed as /var/lib/controlroom ($STATE_DIRECTORY).
        StateDirectory = "controlroom";
        StateDirectoryMode = "0700";
        UMask = "0077";
        LoadCredential = lib.mapAttrsToList (name: file: "${name}:${toString file}") loadedCredentials;
        # The agent daemon's socket (group controlroom-agent, mode 0660).
        SupplementaryGroups = lib.optional cfg.agent.enable "controlroom-agent";

        # Hardening. The server only serves HTTP, talks SSH to the machines and HTTPS to the
        # identity provider and push services; it starts no processes.
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        PrivateUsers = true;
        PrivateIPC = true;
        DevicePolicy = "closed";
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RemoveIPC = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
        SystemCallErrorNumber = "EPERM";
      };
    };
  };
}
