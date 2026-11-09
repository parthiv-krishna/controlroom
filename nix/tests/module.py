# pyright: reportUndefinedVariable=false
# The NixOS module VM test script (see ./module.nix). The test driver defines `server`, `idp`
# and `start_all`.

import json
import re
import shlex

KEYS = "/run/keys/controlroom"
BASE = "http://127.0.0.1:8080"
HOST = "controlroom.example.com"
CLIENT_ID = "client-id-from-a-credential"
SECRETS = {
    "client-id": CLIENT_ID,
    "client-secret": "client-secret-for-tests",
}
# The highest acceptable `systemd-analyze security` exposure (0 is best, 10 worst; below 2
# systemd rates it "OK").
MAX_EXPOSURE = 2.0


def curl(path: str, *args: str) -> str:
    """A request through the expected Host; returns the status line and headers."""
    return server.succeed(f"curl -s -o /dev/null -D - -H 'Host: {HOST}' {' '.join(args)} {BASE}{path}")


def find(pattern: str, text: str) -> str:
    """The first group of `pattern`'s first match in `text`; fails if there is none."""
    match = re.search(pattern, text)
    assert match, f"{pattern!r} not found in:\n{text}"
    return match.group(1)


def status(response: str) -> int:
    return int(response.split()[1])


def write_secret(name: str, command: str) -> None:
    """Writes a root-only secret file, as sops-nix or agenix would."""
    server.succeed(f"(umask 077; mkdir -p {KEYS} && {command} > {KEYS}/{name})")


def main_pid() -> str:
    return server.succeed("systemctl show -p MainPID --value controlroom").strip()


start_all()
idp.wait_for_unit("mock-idp.service")
idp.wait_for_open_port(8000)
server.wait_for_unit("multi-user.target")

with subtest("without its master key the service doesn't start"):
    for name, value in SECRETS.items():
        write_secret(name, f"echo {shlex.quote(value)}")
    write_secret("vapid-key", "openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256")
    # systemd can't load the missing credential, so the server never runs; it keeps retrying.
    server.wait_until_succeeds("journalctl -u controlroom | grep -q 'step CREDENTIALS'")
    server.fail("systemctl is-active controlroom")

with subtest("a malformed master key is refused"):
    write_secret("master-key", "echo not-a-key")
    server.succeed("systemctl restart controlroom || true")
    server.wait_until_succeeds("journalctl -u controlroom | grep -q 'refusing to start: master key file .* invalid format'")
    server.fail("systemctl is-active controlroom")

with subtest("it starts with a master key"):
    write_secret("master-key", "openssl rand -base64 32")
    server.succeed("systemctl restart controlroom")
    server.wait_for_unit("controlroom.service")
    server.wait_for_open_port(8080)
    server.succeed(f"curl -sf {BASE}/healthz")

with subtest("the OIDC configuration renders into a sign-in redirect"):
    response = curl("/auth/session")
    assert status(response) == 401, response
    response = curl("/auth/login")
    assert status(response) == 303, response
    location = find(r"(?im)^location: (\S+)", response)
    assert location.startswith("http://idp:8000/authorize?"), location
    for part in (
        f"client_id={CLIENT_ID}",
        "redirect_uri=https%3A%2F%2Fcontrolroom.example.com%2Fauth%2Fcallback",
        "response_type=code",
        "code_challenge_method=S256",
        "groups",
    ):
        assert part in location, f"{part} missing from {location}"
    assert "__Host-controlroom_login=" in response, response

with subtest("the rendered configuration holds no secrets"):
    config = find(r"--config (\S+)", server.succeed("systemctl cat controlroom"))
    text = server.succeed(f"cat {config}")
    print(text)
    for value in SECRETS.values():
        assert value not in text, f"{value} leaked into {config}"
    assert 'client_id_file = "oidc-client-id"' in text, text
    assert server.succeed(f"stat -c %a {KEYS}/master-key").strip() == "600"

with subtest("request hardening"):
    assert status(server.succeed(f"curl -s -o /dev/null -D - -H 'Host: evil.example.com' {BASE}/")) == 421
    response = curl("/")
    assert status(response) == 200, response
    assert re.search(r"(?im)^content-security-policy: .*script-src 'self'", response), response

with subtest("the process runs sandboxed"):
    pid = main_pid()
    proc = server.succeed(f"cat /proc/{pid}/status")
    print(proc)
    assert re.search(r"(?m)^CapEff:\s+0+$", proc), "no capabilities"
    assert re.search(r"(?m)^CapBnd:\s+0+$", proc), "an empty bounding set"
    assert re.search(r"(?m)^NoNewPrivs:\s+1$", proc), "no new privileges"
    assert re.search(r"(?m)^Seccomp:\s+2$", proc), "a seccomp filter"
    # A dynamic user, named after the unit.
    user = server.succeed(f"stat -c %U /proc/{pid}").strip()
    assert user == "controlroom", user
    # The state directory is private (systemd may idmap it, so its owner on the host varies).
    state = server.succeed("stat -c %a /var/lib/private /var/lib/private/controlroom").split()
    assert state == ["700", "700"], state
    server.succeed("test -s /var/lib/private/controlroom/controlroom.sqlite")
    # MemoryDenyWriteExecute and the rest are in force, and the server runs with them.
    for setting in ("MemoryDenyWriteExecute=yes", "ProtectSystem=strict", "ProtectHome=yes", "PrivateTmp=yes"):
        server.succeed(f"systemctl show controlroom | grep -qx {setting}")

with subtest("the state survives a restart"):
    before = server.succeed("stat -c %i /var/lib/private/controlroom/controlroom.sqlite")
    server.succeed("systemctl restart controlroom")
    server.wait_for_open_port(8080)
    assert status(curl("/auth/session")) == 401
    assert server.succeed("stat -c %i /var/lib/private/controlroom/controlroom.sqlite") == before

with subtest("systemd-analyze security"):
    report = server.succeed("systemd-analyze security --no-pager controlroom.service")
    print(report)
    exposure = float(find(r"Overall exposure level for controlroom\.service: (\d+\.\d+)", report))
    assert exposure <= MAX_EXPOSURE, f"exposure {exposure} > {MAX_EXPOSURE}"

with subtest("the agent daemon runs as its own user and serves the web server"):
    server.wait_for_unit("controlroom-agent.service")
    socket = "/run/controlroom-agent/agentd.sock"
    server.wait_until_succeeds(f"test -S {socket}")
    owner = server.succeed(f"stat -c '%U %G %a' {socket} /run/controlroom-agent").split("\n")
    assert owner[0] == "controlroom-agent controlroom-agent 660", owner
    assert owner[1] == "controlroom-agent controlroom-agent 750", owner
    # The web server (a dynamic user in the group) connects; nobody else may.
    server.wait_until_succeeds("journalctl -u controlroom | grep -q 'connected to the agent daemon'")
    server.fail(f"su -s /bin/sh nobody -c 'test -w {socket}'")
    pid = server.succeed("systemctl show -p MainPID --value controlroom-agent").strip()
    assert server.succeed(f"stat -c %U /proc/{pid}").strip() == "controlroom-agent"
    proc = server.succeed(f"cat /proc/{pid}/status")
    assert re.search(r"(?m)^CapEff:\s+0+$", proc), "no capabilities"
    assert re.search(r"(?m)^NoNewPrivs:\s+1$", proc), "no new privileges"
    for setting in ("KillMode=process", "MemoryDenyWriteExecute=no", "ProtectSystem=strict", "ProtectHome=yes"):
        server.succeed(f"systemctl show controlroom-agent | grep -qx {setting}")
    # The daemon's CLI, the bundle and nono are on the agent user's login PATH; its doctor
    # (as an admin would run it, without flags) finds the daemon's directories through
    # /etc/controlroom-agent/agentd.toml, nono and Landlock.
    print(server.succeed("su -l controlroom-agent -c 'command -v controlroom-agentd controlroom-pi controlroom-pi-models nono'"))
    doctor = server.succeed("su -l controlroom-agent -c 'controlroom-agentd doctor'")
    report = json.loads(doctor)
    assert report["nono"]["status"] == "ok" and report["landlock_abi"], doctor
    assert [a["status"] for a in report["agents"]] == ["ok"], doctor
    assert report["agents_config"] == "/etc/controlroom-agent/agents.toml", doctor
    print(server.succeed("systemd-analyze security --no-pager controlroom-agent.service | tail -n 1"))
    server.succeed("test -d /var/lib/controlroom-agent -a -d /var/tmp/controlroom-agent")
    # A restart of the agent daemon: the web server reconnects.
    server.succeed("systemctl restart controlroom-agent")
    server.wait_until_succeeds("journalctl -u controlroom | grep -q 'agent daemon connection lost'")
    server.wait_until_succeeds("test $(journalctl -u controlroom | grep -c 'connected to the agent daemon') -ge 2")
