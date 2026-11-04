# `services.controlroom.agent` (spec §14, design §8 R4(a)): controlroom-agentd, the agent daemon
# that runs every pi session on the controlroom server, as its own systemd unit next to the web
# server (./module.nix imports this file).
#
# - A static user and group `controlroom-agent`; state in /var/lib/controlroom-agent
#   (`StateDirectory`: the sessions, transcripts and pi's agent dir with `auth.json`).
# - `KillMode=process`: the detached supervisors (one per running pi) outlive restarts and
#   upgrades of the daemon; the next daemon reconnects to them.
# - No `MemoryDenyWriteExecute` (pi is Node, which needs W^X pages); nono confines each pi
#   with Landlock, so the unit keeps the hardening that doesn't get in nono's way.
# - The `controlroom-pi` bundle and nono on the agent user's PATH (its per-user profile, which
#   the login shell agents start in puts on PATH).
# - Model credentials as systemd credentials (`LoadCredential=`, e.g. sops-nix outputs): pi's
#   `models.json` names them as `"apiKey": "!cat $CREDENTIALS_DIRECTORY/<name>"`, which only
#   `controlroom-agentd credential` (nono's credential helper, outside the sandbox) resolves.
# - Its configuration (agents.toml, sandbox.toml) in /etc/controlroom-agent.
# - The API socket /run/controlroom-agent/agentd.sock (mode 0660) is reachable by the web
#   unit through the shared group; services.controlroom points `agentd_socket` at it.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.services.controlroom.agent;
  tomlFormat = pkgs.formats.toml { };
  jsonFormat = pkgs.formats.json { };

  user = "controlroom-agent";
  stateDir = "/var/lib/controlroom-agent";
  # nono binds unix sockets in its TMPDIR, so the path must stay short (≤ 28 bytes); not a
  # PrivateTmp or RuntimeDirectory, which systemd removes while detached sessions still use it.
  nonoTmp = "/var/tmp/controlroom-agent";

  configFiles = {
    agents = tomlFormat.generate "agents.toml" cfg.agents;
    sandbox = tomlFormat.generate "sandbox.toml" cfg.sandbox;
  };
  piFiles = lib.filterAttrs (_: v: v != null) {
    "models.json" = cfg.pi.models;
    "settings.json" = cfg.pi.settings;
  };
in
{
  options.services.controlroom.agent = {
    enable = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Run controlroom-agentd, the agent daemon, on this machine (with services.controlroom).
        Sessions need it: pi runs here, its tools run on the machines' workspaces.
      '';
    };

    package = mkOption {
      type = types.package;
      description = "The controlroom-agentd package.";
    };

    piBundle = mkOption {
      type = types.package;
      description = "The controlroom-pi bundle (pinned pi, @controlroom/pi, its launcher and the OAuth helper).";
    };

    nono = mkOption {
      type = types.package;
      description = "The nono sandbox, pinned to the version controlroom validates (the flake's `nono`).";
    };

    extraPackages = mkOption {
      type = types.listOf types.package;
      default = [ ];
      description = "More programs on the agent user's PATH, e.g. a wrapper named in `agents`.";
    };

    agents = mkOption {
      type = types.submodule { freeformType = tomlFormat.type; };
      default = { };
      example = {
        agents.pi.command = "controlroom-pi --keep /nix/store/…-my-extension";
      };
      description = ''
        `agents.toml` (see `crates/agentd/src/config/agents.rs`). Without an `agents.pi`
        entry, sessions run the bundle's `controlroom-pi`.
      '';
    };

    sandbox = mkOption {
      type = types.submodule { freeformType = tomlFormat.type; };
      default = { };
      example = {
        routes = [
          {
            provider = "gateway";
            upstream = "https://llm.example.com";
            allow = [ "POST /v1/chat/completions" ];
          }
        ];
      };
      description = ''
        `sandbox.toml` (see `crates/agentd/src/config/sandbox.rs`): above all the model
        `routes`, one credential route per provider. `nono`, `oauth_helper` and `nono_tmp`
        default to this module's packages and directories.
      '';
    };

    pi = {
      models = mkOption {
        type = types.nullOr jsonFormat.type;
        default = null;
        example = {
          providers.gateway = {
            baseUrl = "https://llm.example.com/v1";
            api = "openai-completions";
            apiKey = "!cat $CREDENTIALS_DIRECTORY/gateway";
          };
        };
        description = ''
          pi's `models.json` in the agent dir, installed at every start (null: managed by
          hand). Keys belong in `credentials`, named by `"apiKey": "!cat
          $CREDENTIALS_DIRECTORY/<name>"`; sessions only ever see placeholders.
        '';
      };

      settings = mkOption {
        type = types.nullOr jsonFormat.type;
        default = null;
        example = {
          defaultProvider = "gateway";
          defaultModel = "some-model";
        };
        description = "pi's `settings.json` in the agent dir, installed at every start (null: managed by hand).";
      };
    };

    credentials = mkOption {
      type = types.attrsOf types.path;
      default = { };
      example = {
        gateway = "/run/secrets/controlroom/gateway-key";
      };
      description = ''
        Model credentials: name → a root-only file outside the Nix store, handed to the
        daemon with `LoadCredential=` (readable as `$CREDENTIALS_DIRECTORY/<name>`).
      '';
    };

    socket = mkOption {
      type = types.str;
      readOnly = true;
      default = "/run/controlroom-agent/agentd.sock";
      description = "The agent daemon API socket (group `controlroom-agent`, mode 0660).";
    };
  };

  config = lib.mkIf (config.services.controlroom.enable && cfg.enable) {
    services.controlroom.agent = {
      agents.agents.pi.command = lib.mkDefault "controlroom-pi";
      sandbox = {
        nono = lib.mkDefault (lib.getExe cfg.nono);
        oauth_helper = lib.mkDefault "${cfg.piBundle}/bin/controlroom-pi-oauth";
        nono_tmp = lib.mkDefault nonoTmp;
      };
    };

    users.users.${user} = {
      isSystemUser = true;
      group = user;
      home = stateDir;
      # Agents start in its login shell (`$SHELL -lc`), whose profile puts this user's
      # packages on PATH; so does `sudo -u controlroom-agent -i` for the daemon's CLI.
      shell = pkgs.bashInteractive;
      packages = [
        cfg.package
        cfg.piBundle
        cfg.nono
      ]
      ++ cfg.extraPackages;
    };
    users.groups.${user} = { };

    # /etc/controlroom-agent ($CONFIGURATION_DIRECTORY). Not a store path: pi's sandbox reads
    # /nix/store, and the daemon's configuration must stay out of its reach.
    environment.etc = {
      "controlroom-agent/agents.toml".source = configFiles.agents;
      "controlroom-agent/sandbox.toml".source = configFiles.sandbox;
    };

    systemd.tmpfiles.settings.controlroom-agent.${nonoTmp}.d = {
      inherit user;
      group = user;
      mode = "0700";
    };

    systemd.services.controlroom-agent = {
      description = "controlroom agent daemon (pi sessions)";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      # Restarting is harmless: running sessions outlive it.
      restartTriggers = lib.attrValues configFiles;
      path = [
        cfg.piBundle
        cfg.nono
      ];
      # pi's agent dir, with the declared files (copies: pi rewrites settings.json).
      preStart = ''
        install -d -m 0700 ${stateDir}/agent
      ''
      + lib.concatStrings (
        lib.mapAttrsToList (name: value: ''
          install -m 0600 ${jsonFormat.generate name value} ${stateDir}/agent/${name}
        '') piFiles
      );
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve";
        User = user;
        Group = user;
        # Detached supervisors (one per running pi) outlive restarts; the next daemon
        # reconnects to them.
        KillMode = "process";
        Restart = "on-failure";
        RestartSec = "5s";
        StateDirectory = "controlroom-agent";
        ConfigurationDirectory = "controlroom-agent";
        StateDirectoryMode = "0700";
        RuntimeDirectory = "controlroom-agent";
        RuntimeDirectoryMode = "0750";
        UMask = "0077";
        LoadCredential = lib.mapAttrsToList (name: file: "${name}:${toString file}") cfg.credentials;
        ReadWritePaths = [ nonoTmp ];

        # Hardening that leaves nono (Landlock, seccomp, its proxy) and Node working: no
        # MemoryDenyWriteExecute, no system call filter or namespace restrictions of our own.
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateDevices = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        SystemCallArchitectures = "native";
      };
    };
  };
}
