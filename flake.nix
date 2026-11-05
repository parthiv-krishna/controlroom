{
  description = "controlroom: supervise pi coding-agent sessions across machines";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
      webFor = pkgs: import ./nix/web.nix { inherit pkgs; };
      nonoFor = pkgs: import ./nix/nono.nix { inherit pkgs; };
      piFor = pkgs: import ./nix/pi.nix { inherit pkgs; };
      extensionFor = pkgs: import ./nix/extension.nix { inherit pkgs; };
      # services.controlroom.agent's packages.
      agentFor = pkgs: {
        package = (rustFor pkgs).controlroomAgentd;
        piBundle = (piFor pkgs).bundle;
        nono = nonoFor pkgs;
      };
      e2eFor =
        pkgs:
        import ./nix/tests/e2e.nix {
          inherit pkgs;
          module = ./nix/module.nix;
          rust = rustFor pkgs;
          web = webFor pkgs;
          nono = nonoFor pkgs;
          agent = agentFor pkgs;
        };
      rustFor =
        pkgs:
        import ./nix/rust.nix {
          inherit pkgs crane;
          webDist = (webFor pkgs).dist;
          nono = nonoFor pkgs;
        };
    in
    {
      # services.controlroom (with services.controlroom.agent), the packages defaulting to this
      # flake's own.
      nixosModules.default =
        { lib, pkgs, ... }:
        let
          packages = self.packages.${pkgs.stdenv.hostPlatform.system};
        in
        {
          imports = [ ./nix/module.nix ];
          services.controlroom = {
            package = lib.mkDefault packages.controlroom-web;
            agent = {
              package = lib.mkDefault packages.controlroom-agentd;
              piBundle = lib.mkDefault packages.controlroom-pi;
              nono = lib.mkDefault packages.nono;
            };
          };
        };

      packages = forAllSystems (
        pkgs:
        let
          rust = rustFor pkgs;
          web = webFor pkgs;
        in
        {
          default = rust.workspace;
          inherit (rust) workspace;
          web-dist = web.dist;
          controlroom-web = rust.controlroomWeb;
          controlroom-agentd = rust.controlroomAgentd;
          # Static musl daemon binaries, deployed to execution machines over SSH.
          controlroomd-x86_64-linux = rust.controlroomdStatic.x86_64;
          controlroomd-aarch64-linux = rust.controlroomdStatic.aarch64;
          # The pinned nono, static, bundled with them for the hosts.
          nono-x86_64-linux = rust.nonoStatic.x86_64;
          nono-aarch64-linux = rust.nonoStatic.aarch64;
          # The sandbox, pinned to the version the profiles are validated against.
          nono = nonoFor pkgs;
          # pi pinned for controlroom: launcher, @controlroom/pi, OAuth helper.
          controlroom-pi = (piFor pkgs).bundle;
          # The Playwright runner of the VM test and the VM test run ($out has its logs).
          vm-e2e-runner = web.e2eVm;
        }
        // pkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          vm-e2e-run = (e2eFor pkgs).run;
          vm-e2e-driver = (e2eFor pkgs).test.driver;
        }
      );

      checks = forAllSystems (
        pkgs:
        let
          rust = rustFor pkgs;
          web = webFor pkgs;
        in
        {
          inherit (rust)
            workspace
            clippy
            fmt
            test
            ;
          controlroomd-static-x86_64 = rust.controlroomdStatic.x86_64;
          controlroomd-static-aarch64 = rust.controlroomdStatic.aarch64;
          # Static and stripped; the x86_64 one asserts `nono --version`.
          nono-static-x86_64 = rust.nonoStatic.x86_64;
          nono-static-aarch64 = rust.nonoStatic.aarch64;
          controlroom-web = rust.controlroomWeb;
          protocol-ts-drift = rust.protocolTsDrift;
          web-check = web.check;
          # @controlroom/pi's unit tests.
          extension-pi = (extensionFor pkgs).check;
          # Builds the pinned nono; its install check asserts `nono --version`.
          nono = nonoFor pkgs;
          # services.controlroom's assertions and generated settings.
          module-eval = import ./nix/tests/module-eval.nix {
            inherit pkgs;
            module = ./nix/module.nix;
          };
        }
        // (piFor pkgs).checks
        # The browser end-to-end test in NixOS VMs (needs KVM): x86_64 only.
        // pkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          # The browser end-to-end test: web server and agent daemon on the server VM, two
          # execution hosts with only controlroomd and nono.
          vm-e2e = (e2eFor pkgs).check;
          # services.controlroom in a VM: secrets, OIDC rendering, sandbox, exposure.
          vm-module = import ./nix/tests/module.nix {
            inherit pkgs;
            module = ./nix/module.nix;
            package = rust.controlroomWeb;
            agent = agentFor pkgs;
          };
        }
      );

      devShells = forAllSystems (
        pkgs:
        let
          rust = rustFor pkgs;
          web = webFor pkgs;
        in
        {
          default = rust.craneLib.devShell {
            checks = self.checks.${pkgs.stdenv.hostPlatform.system};
            packages = [
              pkgs.cargo-nextest
              pkgs.nixfmt
              web.nodejs
              web.pnpm
            ];
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
