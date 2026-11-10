# Rust builds via crane. Cargo is the source of truth; this wraps it for
# `nix build` / `nix flake check`. `webDist` is the built UI (nix/web.nix); `nono` is the
# pinned nono (nix/nono.nix), whose source the static host build reuses.
{
  pkgs,
  crane,
  webDist,
  nono,
}:
let
  inherit (pkgs) lib;

  craneLib = (crane.mkLib pkgs).overrideToolchain (
    p: p.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml
  );

  # Cargo sources plus test data that crates read at test time.
  # Workstreams add their data directories here (e.g. ../fixtures).
  testDataDirs = lib.filter builtins.pathExists [
    ../fixtures
    ../crates/sandbox/tests/golden
    ../crates/agentd/tests/golden
    ../crates/drivers/tests/golden
  ];
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions ([ (craneLib.fileset.commonCargoSources ../.) ] ++ testDataDirs);
  };

  commonArgs = {
    inherit src;
    pname = "controlroom";
    # Crates inherit `version.workspace = true`, which crane can't resolve.
    version = (lib.importTOML ../Cargo.toml).workspace.package.version;
    strictDeps = true;
  };

  # Dependencies only; cached separately from workspace code.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  workspace = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      doCheck = false; # tests run in the `test` check
    }
  );

  # The cross-compilation settings of a fully static binary for `target`, built with the
  # matching musl toolchain (C dependencies such as bundled SQLite or aws-lc too).
  staticArgs =
    { target, crossPkgs }:
    let
      cc = crossPkgs.stdenv.cc;
      targetEnv = lib.toUpper (builtins.replaceStrings [ "-" ] [ "_" ] target);
      ccExe = "${cc}/bin/${cc.targetPrefix}cc";
    in
    {
      CARGO_BUILD_TARGET = target;
      CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
      "CARGO_TARGET_${targetEnv}_LINKER" = ccExe;
      "CC_${builtins.replaceStrings [ "-" ] [ "_" ] target}" = ccExe;
      HOST_CC = "${pkgs.stdenv.cc}/bin/cc";
      depsBuildBuild = [ cc ];
      doCheck = false;
    };

  # Asserts that `$out/bin/<name>` is statically linked.
  staticCheck = name: ''
    file -L $out/bin/${name} | tee /dev/stderr | grep -Eq 'static(-pie|ally) linked'
  '';

  # The cross targets of the static host binaries.
  targets = {
    x86_64 = {
      target = "x86_64-unknown-linux-musl";
      crossPkgs = pkgs.pkgsCross.musl64;
    };
    aarch64 = {
      target = "aarch64-unknown-linux-musl";
      crossPkgs = pkgs.pkgsCross.aarch64-multiplatform-musl;
    };
  };

  # Fully static controlroomd for arbitrary Linux hosts.
  mkStaticDaemon =
    {
      target,
      crossPkgs,
      # Build it reporting another version (the VM test's "older" daemon). It reuses the
      # regular build's dependencies; cargo updates the workspace versions in Cargo.lock.
      versionOverride ? null,
    }:
    let
      args =
        commonArgs
        // staticArgs { inherit target crossPkgs; }
        // {
          pname = "controlroomd";
          cargoExtraArgs = "--locked -p controlroomd";
        };
      cargoArtifacts = craneLib.buildDepsOnly args;
      regular = craneLib.buildPackage (
        args
        // {
          inherit cargoArtifacts;
          doInstallCheck = true;
          nativeInstallCheckInputs = [ pkgs.file ];
          installCheckPhase = staticCheck "controlroomd";
        }
      );
    in
    if versionOverride == null then
      regular
    else
      craneLib.buildPackage (
        args
        // {
          version = versionOverride;
          cargoExtraArgs = "-p controlroomd";
          inherit cargoArtifacts;
          postPatch = ''
            sed -i 's/^version = "${commonArgs.version}"$/version = "${versionOverride}"/' Cargo.toml
          '';
          doInstallCheck = true;
          installCheckPhase = ''
            [ "$($out/bin/controlroomd --version | head -n 1)" = "controlroomd ${versionOverride}" ]
          '';
        }
      );

  # The TypeScript drift test, with the checked-in bindings (web UI and pi extension) in the
  # source so it compares instead of skipping (the plain `test` check doesn't see them).
  protocolTsDrift = craneLib.cargoTest (
    commonArgs
    // {
      pname = "controlroom-protocol-ts-drift";
      src = lib.fileset.toSource {
        root = ../.;
        fileset = lib.fileset.unions [
          (craneLib.fileset.commonCargoSources ../.)
          ../web/src/lib/protocol
          ../extensions/pi/src/protocol
        ];
      };
      inherit cargoArtifacts;
      cargoTestExtraArgs = "-p controlroom-protocol --test typescript";
      CONTROLROOM_TS_DRIFT_REQUIRED = "1";
    }
  );

  controlroomdStatic = lib.mapAttrs (_: mkStaticDaemon) targets;

  # An x86_64 daemon that reports an older (pre-release) version, for the VM test's auto-upgrade.
  controlroomdStaticOld = mkStaticDaemon (
    targets.x86_64 // { versionOverride = "${commonArgs.version}-old"; }
  );

  # The pinned nono, fully static and stripped (its release profile strips), for the hosts:
  # bundled next to controlroomd and uploaded with it, so hosts need no nono of their own. The
  # same source as ./nono.nix; its Linux keyring backend is pure Rust (no libdbus). The x86_64
  # build runs `nono --version`; the aarch64 one is checked to be static, as controlroomd's is.
  mkStaticNono =
    arch:
    { target, crossPkgs }:
    let
      args = staticArgs { inherit target crossPkgs; } // {
        pname = "nono";
        inherit (nono) version src;
        strictDeps = true;
        cargoExtraArgs = "--locked -p nono-cli";
      };
    in
    craneLib.buildPackage (
      args
      // {
        cargoArtifacts = craneLib.buildDepsOnly args;
        # A musl-only bug: approved mediated commands couldn't start (see the patch).
        patches = [ ./nono-musl-o-path.patch ];
        doInstallCheck = true;
        nativeInstallCheckInputs = [ pkgs.file ];
        installCheckPhase = ''
          runHook preInstallCheck
          ${staticCheck "nono"}
          file -L $out/bin/nono | grep -q ', stripped'
        ''
        + lib.optionalString (arch == "x86_64") ''
          [ "$($out/bin/nono --version)" = "nono ${nono.version}" ]
        ''
        + ''
          runHook postInstallCheck
        '';
        meta = nono.meta // {
          description = "nono, static for controlroom's execution hosts";
        };
      }
    );
  nonoStatic = lib.mapAttrs mkStaticNono targets;

  # The agent daemon for the controlroom server (services.controlroom.agent): runs every pi
  # session; its supervisors are started from this binary's own path.
  controlroomAgentd = craneLib.buildPackage (
    commonArgs
    // {
      pname = "controlroom-agentd";
      inherit cargoArtifacts;
      cargoExtraArgs = "--locked -p controlroom-agentd";
      doCheck = false;
      doInstallCheck = true;
      installCheckPhase = ''
        [ "$($out/bin/controlroom-agentd --version)" = "controlroom-agentd ${commonArgs.version}" ]
      '';
      meta.mainProgram = "controlroom-agentd";
    }
  );

  # The web server with the UI embedded (copied to web/dist, where rust-embed
  # reads it) and both static daemons, with the static nono for each, bundled for
  # deployment. The bundled directory is compiled in as the default for
  # `bundled_daemons_dir`.
  controlroomWeb = craneLib.buildPackage (
    commonArgs
    // {
      pname = "controlroom-web";
      inherit cargoArtifacts;
      cargoExtraArgs = "--locked -p controlroom-web";
      doCheck = false;
      CONTROLROOM_BUNDLED_DAEMONS_DIR = "${placeholder "out"}/share/controlroom/daemons";
      preBuild = ''
        mkdir -p web
        cp -r --no-preserve=mode ${webDist} web/dist
      '';
      postInstall = ''
        daemons=$out/share/controlroom/daemons
        mkdir -p $daemons
        install -m 755 ${controlroomdStatic.x86_64}/bin/controlroomd $daemons/controlroomd-x86_64-linux
        install -m 755 ${controlroomdStatic.aarch64}/bin/controlroomd $daemons/controlroomd-aarch64-linux
        echo ${commonArgs.version} > $daemons/version
        install -m 755 ${nonoStatic.x86_64}/bin/nono $daemons/nono-x86_64-linux
        install -m 755 ${nonoStatic.aarch64}/bin/nono $daemons/nono-aarch64-linux
        echo ${nono.version} > $daemons/nono-version
      '';
      doInstallCheck = true;
      installCheckPhase = ''
        runHook preInstallCheck
        [ "$($out/bin/controlroom-web --version)" = "controlroom-web ${commonArgs.version}" ]
        # The UI's hashed entry chunk and the bundled daemons directory are compiled in.
        grep -qF "$(basename ${webDist}/assets/index-*.js)" $out/bin/controlroom-web
        grep -qF "$out/share/controlroom/daemons" $out/bin/controlroom-web
        runHook postInstallCheck
      '';
    }
  );
in
{
  inherit
    craneLib
    workspace
    controlroomWeb
    controlroomAgentd
    controlroomdStatic
    controlroomdStaticOld
    nonoStatic
    protocolTsDrift
    ;

  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets -- --deny warnings";
    }
  );

  fmt = craneLib.cargoFmt { src = craneLib.cleanCargoSource ../.; };

  test = craneLib.cargoTest (commonArgs // { inherit cargoArtifacts; });

  # The real pi request path of a pasted Anthropic subscription token: the agent daemon's
  # ignored `pi_subscription_token` test, run against `piBundle` (controlroom-pi).
  piSubscriptionToken =
    piBundle:
    craneLib.cargoTest (
      commonArgs
      // {
        pname = "controlroom-pi-subscription-token";
        inherit cargoArtifacts;
        cargoTestExtraArgs = "-p controlroom-agentd --test pi_subscription_token -- --ignored";
        CR_TEST_PI_BUNDLE = piBundle;
      }
    );
}
