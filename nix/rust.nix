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
    { target, crossPkgs }:
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
    in
    craneLib.buildPackage (
      args
      // {
        cargoArtifacts = craneLib.buildDepsOnly args;
        doInstallCheck = true;
        nativeInstallCheckInputs = [ pkgs.file ];
        installCheckPhase = ''
          file -L $out/bin/controlroomd | tee /dev/stderr | grep -Eq 'static(-pie|ally) linked'
        '';
      }
    );

  # The TypeScript drift test, with the checked-in bindings in the source so it
  # compares instead of skipping (the plain `test` check doesn't see web/).
  protocolTsDrift = craneLib.cargoTest (
    commonArgs
    // {
      pname = "controlroom-protocol-ts-drift";
      src = lib.fileset.toSource {
        root = ../.;
        fileset = lib.fileset.unions [
          (craneLib.fileset.commonCargoSources ../.)
          ../web/src/lib/protocol
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

  # The web server with the UI copied to web/dist (embedded once the web crate
  # uses rust-embed) and both static daemons bundled for deployment.
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
    }
  );
in
{
  inherit
    craneLib
    workspace
    controlroomWeb
    controlroomdStatic
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
