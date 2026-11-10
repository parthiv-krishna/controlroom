# nono, pinned to the version controlroom's sandbox profiles are validated against
# (spec §11, §14). Built from the upstream tag, independent of the nixpkgs revision, so a
# nixpkgs bump never changes the sandbox. Bump it only together with
# `controlroom_sandbox::probe::PINNED_NONO_VERSION` (and check ./nono-musl-o-path.patch and ./nono-musl-o-path-access.patch,
# which the static build for the hosts in ./rust.nix applies), and re-run the validation scripts
# (crates/sandbox/scripts/validate-nono.sh, crates/agentd/scripts/acceptance-pi-nono.sh, and the
# ignored `controlroomd --test host nono` tests with CR_TEST_NONO set to the static build).
{ pkgs }:
pkgs.rustPlatform.buildRustPackage (finalAttrs: {
  pname = "nono";
  version = "0.80.0";

  src = pkgs.fetchFromGitHub {
    owner = "nolabs-ai";
    repo = "nono";
    tag = "v${finalAttrs.version}";
    hash = "sha256-VaHeLMUZOxPjIeBSf37vWfm9fChskJYJgD4I9pKbgyo=";
  };
  cargoHash = "sha256-c8PL9lA0Jocse+rir5kTt5I6yGq5z7XN0pcWPrk2DGE=";

  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.dbus ];

  # Upstream's suite needs a real host (Landlock outside the build sandbox, /usr/bin, git,
  # TLS bundles); controlroom validates the pinned binary with its own host tests instead.
  doCheck = false;

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    [ "$($out/bin/nono --version)" = "nono ${finalAttrs.version}" ]
    runHook postInstallCheck
  '';

  meta = {
    description = "Kernel-enforced sandbox for AI agents (pinned for controlroom)";
    homepage = "https://github.com/nolabs-ai/nono";
    license = pkgs.lib.licenses.asl20;
    mainProgram = "nono";
    platforms = pkgs.lib.platforms.linux;
  };
})
