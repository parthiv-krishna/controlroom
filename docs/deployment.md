# Deploying controlroom

controlroom has two parts:

- **The server**, one always-on NixOS machine behind your reverse proxy and identity provider,
  runs two services: the web server (`controlroom-web`), which holds the SSH keys (sealed with a
  master key), the machine list, projects and tasks, and serves the UI; and the agent daemon
  (`controlroom-agentd`), which runs every pi session (under nono) with the model credentials.
- **Execution hosts** are the machines the agents work on: their files, shells and jobs. The
  server connects to them over SSH and installs its daemon (`controlroomd`) into
  `~/.local/share/controlroom`, with the nono it sandboxes their work with. They need only bash
  and a Landlock-capable kernel: no nono, pi, bundle or model credentials.

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

### The agent daemon: `services.controlroom.agent`

Enabled with `services.controlroom` (`agent.enable`, default on). Sessions run pi through the
`controlroom-pi` bundle on this machine; its tools act on the workspaces' hosts.

Sessions need a model provider. There are two ways to configure them, and they combine.

**In the UI: Settings › Models** (recommended to start with):

- **API keys** for Anthropic, OpenAI, Google (Gemini) and OpenRouter: *Add key*. The key goes
  from the browser to the agent daemon once and is never shown again (only its last four
  characters, when and from which device it was added).
- **Subscriptions** (Anthropic Claude Pro/Max, ChatGPT/Codex): *Sign in*. pi's own sign-in runs
  on the server; open the sign-in page, then paste the URL your browser ends on (it can't reach
  the server's callback listener) or enter the device code if the provider offers one. *Sign out*
  removes it.
- **Anthropic subscription tokens** (`sk-ant-oat…`, e.g. from `claude setup-token`): paste one
  into Anthropic's key field instead of signing in. It shows as *token set …abcd* and is used
  like a sign-in (`Authorization: Bearer`, with the headers pi sends for subscriptions), but as
  is: these tokens are long-lived and nothing refreshes them. When Anthropic stops accepting it
  (expired or revoked), *Test* says *HTTP 401: token rejected*; paste a new one with *Replace
  token*. A token pasted earlier, before controlroom told tokens from keys, is recognized
  automatically. Other providers and gateways refuse these tokens. A sign-in and a pasted token
  or key for the same provider exclude each other: remove one to add the other.
- **Custom gateways** (OpenAI-compatible): a name, the base URL (`https://…/v1`), its model ids,
  the key header (`Authorization: Bearer`, `x-api-key` or `api-key`) and the key.
- **Accounts** (e.g. a personal and a work subscription): *+ Account* on a preset or a gateway
  adds a named account (lowercase letters, digits and dashes) under it, with its own key, pasted
  token or sign-in: *Add key* or *Sign in* on the account's row. What was set up before is the
  provider's first account, named *default* (*Rename* it, e.g. to *personal*); nothing needs
  re-entering. For pi, each added account is a provider of its own (`anthropic-work`, chosen when
  it is added and kept on rename) with its provider's models, so the session header's model
  picker lists every model once per account (*Model · work*), switching account is switching
  model, and the header's usage line names the account (`… $0.420 work 12.0%/200k`). Each
  account's sign-in lives in its own directory (`models/accounts/<id>/auth.json`) and is refreshed
  there only. Each row shows what the server's sessions used through it (last 24 hours, 7 days,
  all time; deleted sessions no longer count). *Remove* on an account deletes it with its key or
  sign-in (and the defaults, if they named it). The defaults can name an account.
  A provider the server's config declares keeps its server credential (read-only) and can still
  get UI accounts next to it; an account never takes a server-declared provider's id.
- **Test** sends one cheap request (e.g. the model list) with the key or token, through the
  provider's route, and shows *ok* or the HTTP status.
- **Defaults**: the provider, model and thinking level of new sessions, from the models pi
  offers with what is configured.

Changes apply to new sessions and to restarts of existing ones; running agents keep what they
started with. They are stored in `/var/lib/controlroom-agent/models/` (`keys.json` is mode
0600, `accounts/` 0700, readable by the agent user only) and logged by both services with the
device that made them, never with a key. An added account's models are pi's own model list of
its provider, fetched when Settings › Models is opened or changed (kept in
`models/definitions.json`); after a pi upgrade, open Settings › Models once to refresh it. Only the presets and gateways you add get network routes from the UI, so
the sandbox's allow-list stays deliberate. Sessions at every sandbox level, *off* included, get
them (pi always runs under nono on the server).

**Declaratively, in Nix** (the alternative, or for what the UI shouldn't change):

```nix
services.controlroom.agent = {
  # sandbox.toml: one credential route per model provider.
  sandbox.routes = [
    {
      provider = "gateway";
      upstream = "https://llm.example.com";
      allow = [ "POST /v1/chat/completions" ];
    }
  ];
  # pi's models.json and settings.json in the agent dir (or manage them by hand: null).
  pi.models.providers.gateway = {
    baseUrl = "https://llm.example.com/v1";
    api = "openai-completions";
    apiKey = "!cat $CREDENTIALS_DIRECTORY/gateway"; # resolved outside the sandbox only
    models = [ { id = "some-model"; name = "Some model"; } ];
  };
  pi.settings = {
    defaultProvider = "gateway";
    defaultModel = "some-model";
  };
  credentials.gateway = config.sops.secrets."controlroom/gateway_key".path;
};
```

| Option | Meaning |
| --- | --- |
| `credentials` | Model API keys: name → a root-only file outside the store, loaded with `LoadCredential=`. `models.json` names them as `"!cat $CREDENTIALS_DIRECTORY/<name>"`; only nono's credential helper (`controlroom-agentd credential`, outside the sandbox) reads them, and pi only ever sees a placeholder. |
| `sandbox` | `sandbox.toml` (`crates/agentd/src/config/sandbox.rs`): `routes`, `keep`, `oauth_claims`, …; `nono`, `oauth_helper` and `nono_tmp` default to the module's. |
| `agents` | `agents.toml`; by default sessions run the bundle's `controlroom-pi` (add a keep-list with `agents.agents.pi.command = "controlroom-pi --keep …"`). |
| `pi.models`, `pi.settings` | pi's `models.json` and `settings.json`, installed into the agent dir at every start (`null`: left alone). Settings › Models keeps its own files, so these copies never wipe it. |
| `extraPackages` | More programs on the agent user's PATH. |
| `package`, `piBundle`, `nono` | Default to the flake's `controlroom-agentd`, `controlroom-pi` and pinned `nono`. |

How the two combine: a provider is the **server's** when `sandbox.routes` routes it,
`pi.models` defines it or pi's `auth.json` holds a key for it. The UI shows server providers
read-only ("from server config"), refuses keys and gateways under their names, and if one was
added before the server took the name, the server's wins and the UI shows the clash. The defaults
set in the UI win over `pi.settings`' `defaultProvider`, `defaultModel` and
`defaultThinkingLevel` (each new session gets them; the file itself is never rewritten). A
subscription signed in from the UI can use a route declared in `sandbox.routes` (for a provider
pi can sign in to that isn't a preset).

It runs as the static user `controlroom-agent` with its state in `/var/lib/controlroom-agent`
(sessions, transcripts and pi's agent dir, including `auth.json`; persist and back it up with
the web server's state) and its configuration in `/etc/controlroom-agent`. `KillMode=process`
keeps running sessions alive across restarts and upgrades of the daemon. There is no
`MemoryDenyWriteExecute` (pi is Node) and no system call filter of its own: nono confines each
pi with Landlock to its scratch directory, its agent socket and the model routes. The daemon's
socket `/run/controlroom-agent/agentd.sock` is shared with the web server through the
`controlroom-agent` group, and `services.controlroom` points `agentd_socket` at it.

pi always runs under nono, at every sandbox level (level off unsandboxes only the hosts), on
the session's own agent dir: a sign-in in the daemon's agent dir (`controlroom-agentd login`
below) reaches sessions only through a credential route, i.e. a Settings › Models preset or
account, or a `sandbox.routes` entry. The daemon removes provider keys (`OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, …: every variable pi reads a key from) from its environment at startup, so
put keys in Settings › Models or the agent dir's `models.json` (e.g. `!cat
$CREDENTIALS_DIRECTORY/<name>`), never in the unit's environment. A session's profile decides
which of these accounts its agent sees.

To sign in to a subscription from a terminal instead of the UI (pi asks for `/login <provider>`):

```sh
sudo -u controlroom-agent -i controlroom-agentd login <provider>
```

The agent daemon's CLI (`login`, `doctor`, `ctl`) run as `controlroom-agent` finds the daemon's
directories through `/etc/controlroom-agent/agentd.toml`, which the module installs; flags
(`--state-dir`, `--config-dir`, `--socket`) still override it. An unreadable or invalid file, or
one that disagrees with the service's directories, is an error rather than a fallback to
`~/.local/state`.

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

Hosts need a Linux kernel with Landlock enabled (ABI 4 or later to confine TCP; the doctor
reports it), bash, and SSH access. They need **no nono** of their own: the server uploads a
static build of the pinned nono with the daemon, verifies it by running it, and the daemon uses
it by default. On the host:

```text
~/.local/share/controlroom/
  versions/<version>/controlroomd       the daemon (one directory per version)
  versions/<version>/nono -> ../../nono/<nono version>-<sha>/nono
  nono/<nono version>-<sha>/nono        keyed by version and content (12 hex digits of its
                                        SHA-256): uploaded only when the host lacks that build
  current -> versions/<version>
```

Optionally, through home-manager:

- `~/.config/controlroom/sandbox.toml` (`crates/daemon/src/config/sandbox.rs`); `nono = "…"`
  there overrides the bundled nono with a program on the login shell's PATH or a path (the
  doctor warns if its version isn't the validated one);
- persist `~/.local/state/controlroom` and `~/.local/share/controlroom` on an impermanent home.

Then, in the UI, open **Settings › Machines › Add machine**: enter the SSH target, trust the host
key after comparing it with `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the host, and
install controlroom's public key into the host's `~/.ssh/authorized_keys` with the command the
dialog shows. The server installs nono and the daemon and runs `controlroomd doctor`, which shows
the nono in use ("bundled" or "configured in sandbox.toml", with its version) and the Landlock
ABI.

Sandboxed shells on a host reach only what **Settings › Sandbox** grants. Two things that commonly
surprise:

- **Unix sockets** need a socket grant (the "Sockets" row): nono can't ask about a socket
  connection, so an ungranted one fails at once ("Permission denied" or "Operation not
  permitted"), e.g. `dbus-send --system` without the D-Bus system bus. systemd 254 and later
  (`systemctl`, `busctl`, `loginctl`) can't use the bus from the sandbox even when it is granted
  (spec §11.6).
- **Paths nono always protects** (`~/.ssh`, `~/.bash_history`, `~/.bashrc`, `~/.config/gcloud`, …)
  can be granted one by one, but no directory containing one (`~`, `~/.config`, `~/.local/share`):
  nono would refuse to start, so Settings refuses them with the reason.

## 6. Profiles

A **profile** separates agents, not people: work, personal and a private assistant can share one
controlroom without their agents ever meeting. Each profile owns its machine entries, the model
accounts its agents may use, its projects, tasks and sessions, and its sandbox defaults; contact
never crosses profiles. Names, colors, privacy and accounts are all yours to choose
(`docs/design/profiles-tenants.md`).

**First run.** A fresh server has no profile. The first thing that needs one (adding a machine,
creating a project, a task or a session) asks for it in a small "New profile" dialog, then carries
on. Create more from the profile picker in Settings › Machines, Models or Sandbox (＋). Per
profile:

- **Machines:** a machine entry (host and user) belongs to one profile. The same host appears in
  two profiles as two entries under two users (`alice@devbox` in Work, `agent@devbox` as
  `devbox-agent` in Personal); names are unique across profiles. Sharing an entry with another
  profile ("Share with…") is an explicit choice, never for a private profile, and a shared entry
  never runs at level off.
- **Models:** "Accounts in Work: Any / Only the checked ones" in Settings › Models. Accounts
  (keys, sign-ins, gateways) stay global; a profile only picks which ones its agents may use.
- **Private:** pushes name only the profile and the attention kind, its entries can't be shared,
  and its accounts must be listed (never Any).
- **Moving** a machine, a project (with its tasks) or a session to another profile is one action
  with a preview ("Move to profile…"); a moved live agent restarts.

The agent daemon and the controller enforce all of this; the UI only shows the current profile.
pi's own sandbox on the server is always on, whatever a session's level.

### Dedicated users on homelab hosts

The uid is the boundary between profiles on a host. For a profile that should not touch your own
files (a personal assistant, say), give it a user of its own on each host, e.g. `assistant` on
`homelab`:

- create the user in the host's NixOS configuration and put **controlroom's public key**
  (Settings › Machines › SSH keys) in its `~/.ssh/authorized_keys`, or run the install command
  the add-machine dialog shows, as that user;
- add the entry in the profile (Settings › Machines, with the profile picked) as
  `assistant@homelab`;
- set its sandbox level to **off** (the profile's or the machine's defaults in Settings ›
  Sandbox): the user exists only for agents, so the uid and the host's firewall are the boundary,
  and nono would only get in the way. The warning names the user ("the agent can do anything
  assistant can on homelab"), which is exactly the point.

**The network boundary is the host's firewall, in the host's NixOS configuration**: controlroom
configures no firewall, resolver or proxy. A recipe that holds up (validated in a VM by the
profiles spike): nftables rules by socket owner (`meta skuid`) that let the assistant reach only a
local allow-list proxy, which runs as a system service under its own user and resolves names
itself.

```nix
# The assistant (uid 1102) reaches only the proxy on 127.0.0.1:3128; other users are untouched.
networking.nftables = {
  enable = true;
  tables.agent-egress = {
    family = "inet";
    content = ''
      chain output {
        type filter hook output priority filter; policy accept;
        meta skuid 1102 jump assistant
      }
      chain refuse {
        meta l4proto tcp reject with tcp reset
        reject with icmpx admin-prohibited
      }
      chain assistant {
        ct state established,related accept
        oif "lo" ip daddr 127.0.0.1 tcp dport 3128 accept
        goto refuse
      }
    '';
  };
};
```

- **The proxy:** any forward proxy with a domain allow-list that answers `CONNECT` (for example
  tinyproxy with a default-deny filter, or squid), as its own system user on `127.0.0.1:3128`.
  Point the user's tools at it with `HTTPS_PROXY`/`HTTP_PROXY` in its login environment (e.g. its
  home-manager `home.sessionVariables`). Tools that ignore the variables fail closed. Never let
  the agent's own uid run or configure the proxy: its egress would then count as the agent's.
- **No DNS for the user:** the proxy resolves names, and the rules above refuse port 53, so DNS
  can't carry data out either.
- **`reject`, not `drop`:** a dropped connection hangs a tool until its own timeout (bash's
  `/dev/tcp` waited 135 s in the spike); a reject fails it at once.
- **`ct state established,related accept`** keeps SSH working: the user's `sshd-session` replies
  as that user.
- **Daemons that act for users bypass per-uid rules.** Two leaks the spike showed, and their
  fixes:
  - **nscd** resolves names as its own user, so the user's lookups reach the resolver: disable it
    on such hosts (`services.nscd.enable = false;` with `system.nssModules = lib.mkForce [ ];`),
    so lookups leave from the caller's uid and the rules apply;
  - **the nix daemon** fetches fixed-output derivations as its build users, so a URL the agent
    chooses reaches the network: leave the user out of `nix.settings.allowed-users` (it then has
    no nix daemon).

  `systemd-resolved` and container engines act for users the same way; keep them off such hosts
  or out of the user's reach.
- **Stopping `nftables.service` fails open.** A backstop for processes of the user's login
  sessions (everything started over SSH): `systemd.slices."user-1102".sliceConfig = {
  IPAddressDeny = "any"; IPAddressAllow = "localhost"; };`. It doesn't cover processes started
  outside a login session.

### Work machines

On a machine where you can't create users or change the firewall (no root), the entry is your
own account and the agent runs as you. Keep the sandbox on (**strict**, or **relaxed** for your
git identity): nono confines the agent's shells, jobs and file tools to the workspace and the
grants of Settings › Sandbox, and its network to the allowed domains. Such machines usually
belong to one profile; sharing one entry between profiles keeps the sandbox on by design, but
both profiles' agents then run as the same user.

## 7. Phone: the installed app and push

1. Open the public URL in Safari and sign in.
2. Share › **Add to Home Screen**, then open controlroom from the Home Screen icon and sign in
   there too: the installed app has its own cookies.
3. **Settings › Notifications › Enable on this device** (iOS asks for permission only after a
   tap inside the installed app).

Notifications only carry metadata (session, machine, state; for a private profile only the
profile's name and the state); tapping one opens the session. When
the server can't be reached from the phone, the app shows "can't reach server" and retries.

## 8. Checks after deploying

```sh
systemctl status controlroom controlroom-agent
curl -fsS http://127.0.0.1:8080/healthz
systemd-analyze security controlroom
journalctl -u controlroom -u controlroom-agent -e
```

## 9. Upgrades and rollback

- The server is upgraded with the system. When it bundles a newer `controlroomd`, it upgrades
  each machine's daemon automatically once the machine is idle. It runs the new binary first and
  rolls back if the daemon doesn't answer; running agents are not interrupted. nono is uploaded
  only when the host lacks that build (checked by version and SHA-256), so a nono rebuilt with
  the same version (a new patch in `nix/`) reaches each host with its next daemon upgrade, by
  itself; each daemon version keeps the nono it came with, so a rollback restores both, and
  builds no remaining daemon version uses are pruned.
- To roll the server back: `nixos-rebuild switch --rollback` (or boot the previous generation).
  Daemons are never downgraded: hosts that were already upgraded keep the newer daemon (the
  previous ones stay under `~/.local/share/controlroom/versions/`).
- The database has no migrations yet: a server version with a different schema refuses to start
  ("delete the database"). Moving to it means removing `controlroom.sqlite` and adding the
  machines again; the daemons and their sessions on the hosts are unaffected.
- **The profiles release starts fresh** (`docs/design/profiles-tenants.md`). Both stores changed
  schema with no upgrade: an older `controlroom.sqlite` or `agent.sqlite` is refused at startup
  ("delete the database file to start over", "delete the agent daemon's state to start over").
  Before upgrading:
  - delete or close every session in the UI, so that no agent runs (supervisors outlive the
    service; otherwise stop them with `pkill -u controlroom-agent -f 'controlroom-agentd
    supervise'`);
  - stop both services;
  - delete `/var/lib/private/controlroom/controlroom.sqlite` and, from
    `/var/lib/controlroom-agent`, `agent.sqlite` (with its `-wal` and `-shm` files),
    `sessions/` and `run/`. That deletes every session and transcript; keep `agent/` and
    `models/`.

  After the upgrade:

  - create the first profile (setup asks for one);
  - add the machines again. The new database generates a new controlroom SSH key, so authorize
    it on every host (the add-machine flow shows the install command); remove the old key from
    `~/.ssh/authorized_keys` there;
  - recreate projects, tasks, schedules and the sandbox defaults (Settings › Sandbox), and import
    additional SSH keys again;
  - devices sign in again and re-enable push.

  Model accounts and keys (Settings › Models) live in the agent daemon's agent dir and stay.
