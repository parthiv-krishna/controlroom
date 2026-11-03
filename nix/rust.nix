# Rust builds via crane. Cargo is the source of truth; this wraps it for
# `nix build` / `nix flake check`. `webDist` is the built UI (nix/web.nix).
{
  pkgs,
  crane,
  webDist,
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
    ../crates/daemon/tests/golden
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

  # Fully static controlroomd for arbitrary Linux hosts, cross-compiled with
  # the matching musl toolchain (C dependencies such as bundled SQLite too).
  mkStaticDaemon =
    {
      target,
      crossPkgs,
      # Build it reporting another version (the VM test's "older" daemon). It reuses the
      # regular build's dependencies; cargo updates the workspace versions in Cargo.lock.
      versionOverride ? null,
    }:
    let
      cc = crossPkgs.stdenv.cc;
      targetEnv = lib.toUpper (builtins.replaceStrings [ "-" ] [ "_" ] target);
      ccExe = "${cc}/bin/${cc.targetPrefix}cc";
      args = commonArgs // {
        pname = "controlroomd";
        cargoExtraArgs = "--locked -p controlroomd";
        CARGO_BUILD_TARGET = target;
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
        "CARGO_TARGET_${targetEnv}_LINKER" = ccExe;
        "CC_${builtins.replaceStrings [ "-" ] [ "_" ] target}" = ccExe;
        HOST_CC = "${pkgs.stdenv.cc}/bin/cc";
        depsBuildBuild = [ cc ];
        doCheck = false;
      };
      cargoArtifacts = craneLib.buildDepsOnly args;
      regular = craneLib.buildPackage (
        args
        // {
          inherit cargoArtifacts;
          doInstallCheck = true;
          nativeInstallCheckInputs = [ pkgs.file ];
          installCheckPhase = ''
            file -L $out/bin/controlroomd | tee /dev/stderr | grep -Eq 'static(-pie|ally) linked'
          '';
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

  controlroomdStatic = {
    x86_64 = mkStaticDaemon {
      target = "x86_64-unknown-linux-musl";
      crossPkgs = pkgs.pkgsCross.musl64;
    };
    aarch64 = mkStaticDaemon {
      target = "aarch64-unknown-linux-musl";
      crossPkgs = pkgs.pkgsCross.aarch64-multiplatform-musl;
    };
  };

  # An x86_64 daemon that reports an older (pre-release) version, for the VM test's auto-upgrade.
  controlroomdStaticOld = mkStaticDaemon {
    target = "x86_64-unknown-linux-musl";
    crossPkgs = pkgs.pkgsCross.musl64;
    versionOverride = "${commonArgs.version}-old";
  };

  # The web server with the UI embedded (copied to web/dist, where rust-embed
  # reads it) and both static daemons bundled for deployment. The bundled
  # directory is compiled in as the default for `bundled_daemons_dir`.
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
    controlroomdStatic
    controlroomdStaticOld
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
}
