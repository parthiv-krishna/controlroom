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
      rustFor =
        pkgs:
        import ./nix/rust.nix {
          inherit pkgs crane;
          webDist = (webFor pkgs).dist;
        };
    in
    {
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
          # Static musl daemon binaries, deployed to execution machines over SSH.
          controlroomd-x86_64-linux = rust.controlroomdStatic.x86_64;
          controlroomd-aarch64-linux = rust.controlroomdStatic.aarch64;
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
          controlroom-web = rust.controlroomWeb;
          web-check = web.check;
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
