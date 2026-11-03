# Deploying controlroom

controlroom has two parts:

- **The web server** (`controlroom-web`) runs on one always-on NixOS machine, behind your
  reverse proxy and identity provider. It holds the SSH keys (sealed with a master key), the
  machine list, projects and tasks, and serves the UI.
- **Execution hosts** are the machines agents run on. The server connects to them over SSH and
  installs its daemon (`controlroomd`) into `~/.local/share/controlroom`. pi, the
  `controlroom-pi` bundle and nono come from the host's own Nix config (home-manager).

The examples use `controlroom.example.com` for the app, `id.example.com` for the identity
provider (Authelia here) and `alice` for the user. Adapt them.

## 1. The web server: `services.controlroom`

Add the flake and its module:

```nix
# flake.nix
inputs.controlroom.url = "git+https://git.example.com/alice/controlroom?ref=main";
# No `inputs.nixpkgs.follows`: controlroom pins the pi and nono it is validated against.
```

```nix
# the server's NixOS configuration
{ inputs, config, ... }:
{
  imports = [ inputs.controlroom.nixosModules.default ];

  services.controlroom = {
    enable = true;
    listenAddress = "127.0.0.1"; # the default; the reverse proxy terminates TLS
    port = 8080;
    publicUrl = "https://controlroom.example.com";
    masterKeyFile = config.sops.secrets."controlroom/master_key".path;
    auth.oidc = {
      issuer = "https://id.example.com";
      clientIdFile = config.sops.secrets."controlroom/oidc_client_id".path; # or clientId = "…";
      clientSecretFile = config.sops.secrets."controlroom/oidc_client_secret".path;
      allowedGroups = [ "admin" ]; # and/or allowedSubjects
    };
    push.vapidPrivateKeyFile = config.sops.secrets."controlroom/vapid_key".path;
  };
}
```

| Option | Meaning |
| --- | --- |
| `listenAddress`, `port` | Where the server listens (default `127.0.0.1:8080`). A non-loopback address requires OIDC. |
| `publicUrl` | The bare https origin browsers use. It defines the OIDC redirect URI `<publicUrl>/auth/callback`, the Host allowlist and the expected Origin. Forwarded headers are never trusted. |
| `masterKeyFile` | **Required.** Seals the SSH keys in the database. |
| `auth.mode` | `oidc` (default) or `dev-token` (loopback only; development and tests). |
| `auth.oidc.*` | `issuer`, `clientId` or `clientIdFile`, `clientSecretFile`, `allowedSubjects`, `allowedGroups`. The allow-list is re-checked on every request. |
| `push.vapidPrivateKeyFile`, `push.subject` | Web Push. The VAPID public key is derived from the private key. The subject defaults to `publicUrl`. |
| `settings` | Any other key of the TOML configuration, e.g. `settings.session_ttl_days = 14` (default 30, sliding). |
| `package` | Defaults to the flake's `controlroom-web` (the UI and both static daemons bundled). |

Secret options take paths **outside the Nix store**, such as sops-nix or agenix outputs. They
can stay root-only (`0400`): systemd reads them as root and hands them to the service with
`LoadCredential=`. The generated configuration names them by credential, so it holds no secret.

The service runs as a `DynamicUser` with `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`,
`PrivateDevices`, `PrivateUsers`, `NoNewPrivileges`, an empty capability bounding set,
`RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `SystemCallFilter=@system-service` (without
`@privileged` and `@resources`), `MemoryDenyWriteExecute` and more. `systemd-analyze security
controlroom` rates it about 1.1 ("OK"); what remains is network access, which it needs. It
restarts on failure, including while a credential file is still missing.

State lives in `/var/lib/private/controlroom` (`$STATE_DIRECTORY`): the database
`controlroom.sqlite`. On an impermanent root, persist that directory.

## 2. Secrets

Generate each secret once and store it with your secrets tool.

| Secret | Command | Notes |
| --- | --- | --- |
| Master key | `openssl rand -base64 32` | 32 random bytes; raw, hex or base64. Back it up with the database: without it, the stored SSH keys can't be unsealed and every machine has to be added again. |
| VAPID private key | `openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256` | A PKCS#8 PEM key (multi-line; YAML block scalars work). Changing it invalidates every push subscription: each device has to turn push off and on again. |
| OIDC client id | `authelia crypto rand --length 72 --charset rfc3986` | Or any fixed name with `clientId`. |
| OIDC client secret | `authelia crypto hash generate pbkdf2 --variant sha512 --random --random.length 72 --random.charset rfc3986` | Prints the random secret (`Random Password`, for controlroom) and its hash (`Digest`, for Authelia's client registration). |

`authelia` is available with `nix shell nixpkgs#authelia`, and `openssl` with
`nix shell nixpkgs#openssl`.

## 3. Reverse proxy

Proxy the public name to the loopback listener. WebSockets (`/api/ws`) need nothing special.
For example, with Caddy:

```caddyfile
controlroom.example.com {
 reverse_proxy 127.0.0.1:8080
}
```

- The app is served at `/`; a path prefix is not supported.
- The Host must be `publicUrl`'s host. Requests for any other name (for example a second,
  internal name for the same proxy) are refused with `421`. `/healthz` is exempt, for probes.

## 4. Identity provider (Authelia)

Register a confidential client with PKCE:

```yaml
identity_providers:
  oidc:
    clients:
      - client_id: '<client id>'
        client_name: 'controlroom'
        client_secret: '<the pbkdf2 digest>'
        public: false
        authorization_policy: 'one_factor'
        require_pkce: true
        pkce_challenge_method: 'S256'
        redirect_uris:
          - 'https://controlroom.example.com/auth/callback'
        scopes: ['openid', 'profile', 'email', 'groups']
        response_types: ['code']
        grant_types: ['authorization_code']
        token_endpoint_auth_method: 'client_secret_basic'
        userinfo_signed_response_alg: 'none'
```

Prefer `allowedGroups`: Authelia's `sub` is an opaque identifier you only learn after a first
sign-in. The `groups` scope is requested whenever `allowedGroups` is set.

## 5. Execution hosts

On each host, through home-manager:

- install the `controlroom-pi` package (`inputs.controlroom.packages.<system>.controlroom-pi`) and
  nono;
- write `~/.config/controlroom/agents.toml`, with the pi agent's command pointing at the
  `controlroom-pi` launcher and its keep-list;
- write `~/.config/controlroom/sandbox.toml` (credential routes, `exec_dirs` including the
  bundle's `libexec/controlroom-pi/tools`, `oauth_helper`);
- persist `~/.local/state/controlroom` and `~/.local/share/controlroom` on an impermanent home.

Then, in the UI, open **Settings › Machines › Add machine**: enter the SSH target, trust the host
key after comparing it with `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the host, and
install controlroom's public key into the host's `~/.ssh/authorized_keys` with the command the
dialog shows. The server installs the daemon and runs `controlroomd doctor`.

## 6. Phone: the installed app and push

1. Open the public URL in Safari and sign in.
2. Share › **Add to Home Screen**, then open controlroom from the Home Screen icon and sign in
   there too: the installed app has its own cookies.
3. **Settings › Notifications › Enable on this device** (iOS asks for permission only after a
   tap inside the installed app).

Notifications only carry metadata (session, machine, state); tapping one opens the session. When
the server can't be reached from the phone, the app shows "can't reach server" and retries.

## 7. Checks after deploying

```sh
systemctl status controlroom
curl -fsS http://127.0.0.1:8080/healthz
systemd-analyze security controlroom
journalctl -u controlroom -e
```

## 8. Upgrades and rollback

- The server is upgraded with the system. When it bundles a newer `controlroomd`, it upgrades
  each machine's daemon automatically once the machine is idle. It runs the new binary first and
  rolls back if the daemon doesn't answer; running agents are not interrupted.
- To roll the server back: `nixos-rebuild switch --rollback` (or boot the previous generation).
  Daemons are never downgraded: hosts that were already upgraded keep the newer daemon (the
  previous ones stay under `~/.local/share/controlroom/versions/`).
- The database has no migrations yet: a server version with a different schema refuses to start
  ("delete the database"). Moving to it means removing `controlroom.sqlite` and adding the
  machines again; the daemons and their sessions on the hosts are unaffected.
