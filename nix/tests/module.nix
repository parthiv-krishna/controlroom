# The NixOS module (../module.nix) in a VM: `server` runs services.controlroom in OIDC mode
# behind an https public URL, with the secrets as root-only files; `idp` serves a static OIDC
# discovery document. The script (./module.py) checks that the service refuses to start without
# a usable master key, that the OIDC configuration renders into a working sign-in redirect
# (client id from a credential, the callback URI, PKCE, the groups scope), the request
# hardening, the systemd sandbox of the running process, and the `systemd-analyze security`
# exposure. SSH to execution hosts and Web Push under the same module run in ./e2e.nix.
#
#   nix build -L .#checks.x86_64-linux.vm-module
{
  pkgs,
  module,
  package,
}:
let
  idpPort = 8000;
  issuer = "http://idp:${toString idpPort}";

  # Enough of a discovery document for the sign-in redirect; nothing signs tokens here.
  discovery = pkgs.writeText "openid-configuration" (
    builtins.toJSON {
      inherit issuer;
      authorization_endpoint = "${issuer}/authorize";
      token_endpoint = "${issuer}/token";
      userinfo_endpoint = "${issuer}/userinfo";
      jwks_uri = "${issuer}/jwks.json";
      response_types_supported = [ "code" ];
      subject_types_supported = [ "public" ];
      id_token_signing_alg_values_supported = [ "RS256" ];
    }
  );
  idpRoot = pkgs.runCommand "mock-idp-root" { } ''
    mkdir -p $out/.well-known
    cp ${discovery} $out/.well-known/openid-configuration
    echo '{"keys":[]}' > $out/jwks.json
  '';
  # Serves idpRoot as JSON (the discovery document has no file extension).
  idpServer = pkgs.writers.writePython3 "mock-idp" { flakeIgnore = [ "E501" ]; } ''
    import functools
    import http.server

    handler = functools.partial(
        http.server.SimpleHTTPRequestHandler, directory="${idpRoot}"
    )
    handler.func.extensions_map = {"": "application/json"}
    http.server.ThreadingHTTPServer(("", ${toString idpPort}), handler).serve_forever()
  '';
in
pkgs.testers.runNixOSTest {
  name = "controlroom-module";

  nodes = {
    idp = {
      networking.firewall.allowedTCPPorts = [ idpPort ];
      systemd.services.mock-idp = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          ExecStart = idpServer;
          DynamicUser = true;
        };
      };
    };

    server = {
      imports = [ module ];
      environment.systemPackages = [
        pkgs.curl
        pkgs.openssl
      ];
      services.controlroom = {
        enable = true;
        inherit package;
        publicUrl = "https://controlroom.example.com";
        # The script creates these, the master key last.
        masterKeyFile = "/run/keys/controlroom/master-key";
        auth.oidc = {
          inherit issuer;
          clientIdFile = "/run/keys/controlroom/client-id";
          clientSecretFile = "/run/keys/controlroom/client-secret";
          allowedGroups = [ "admin" ];
        };
        push.vapidPrivateKeyFile = "/run/keys/controlroom/vapid-key";
      };
    };
  };

  testScript = builtins.readFile ./module.py;
}
