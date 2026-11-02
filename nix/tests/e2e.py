# pyright: reportUndefinedVariable=false
# The VM end-to-end test script (see ./e2e.nix, which prepends CURRENT_BUNDLE, OLD_BUNDLE,
# BROKEN_BUNDLE, VERSION and RUNNER). The NixOS test driver defines the machines (server,
# devbox, buildbox) and start_all, and type-checks the script with them.
#
# Each scenario runs one Playwright spec (web/e2e-vm/<name>.spec.ts) on `server` in the
# background. When a spec needs something done outside the browser it reaches a *step*: it
# writes `<gates>/<step>.reached` (a JSON payload) and waits for `<step>.release`. This script
# serves the steps in order, doing what the user would do on the hosts, and scripts the mock
# models before each scenario. Nothing waits on a fixed delay except the one negative check
# (an auto-upgrade must not happen while a run is in flight).

import json
import shlex
import time
from datetime import timedelta
from typing import Any

E2E = "/tmp/e2e"
GATES = f"{E2E}/gates"
BASE_URL = "http://127.0.0.1:8080"
SPEC_TIMEOUT = timedelta(minutes=15)
MOCK = "http://127.0.0.1:4000/control"

# ------------------------------------------------------------------------------- helpers


def sh(*words: str) -> str:
    return " ".join(shlex.quote(w) for w in words)


def as_alice(machine: Any, command: str) -> str:
    return machine.succeed(f"su - alice -c {shlex.quote(command)}")


class Spec:
    """One Playwright spec run in the background on the server, with its steps."""

    runs = 0

    def __init__(self, name: str, **env: str) -> None:
        Spec.runs += 1
        self.name = name
        self.status = f"{E2E}/status/{name}"
        self.log = f"{E2E}/logs/{name}.log"
        settings = {
            "E2E_BASE_URL": BASE_URL,
            "E2E_TOKEN_FILE": "/var/lib/controlroom/dev-token",
            "E2E_GATES": GATES,
            "E2E_OUTPUT": f"{E2E}/results/{name}",
            "E2E_VERSION": VERSION,
            **env,
        }
        setenv = " ".join(f"--setenv={k}={shlex.quote(v)}" for k, v in settings.items())
        command = f"{RUNNER} {name}.spec.ts > {self.log} 2>&1; echo $? > {self.status}"
        server.succeed(
            f"rm -rf {GATES} {self.status} && mkdir -p {GATES} {E2E}/status {E2E}/logs",
            f"systemd-run --collect --unit=e2e-{Spec.runs}-{name} {setenv} /bin/sh -c {shlex.quote(command)}",
        )

    def step(self, name: str) -> dict[str, Any]:
        """Waits for the spec to reach step `name` and returns its payload."""
        reached = f"{GATES}/{name}.reached"
        server.wait_until_succeeds(f"test -e {reached} -o -e {self.status}", timeout=SPEC_TIMEOUT)
        if server.execute(f"test -e {reached}")[0] != 0:
            self.finish()
            raise Exception(f"{self.name} ended before step {name}")
        return json.loads(server.succeed(f"cat {reached}"))

    def release(self, name: str, payload: dict[str, Any] | None = None) -> None:
        data = shlex.quote(json.dumps(payload or {}))
        server.succeed(f"echo {data} > {GATES}/{name}.tmp && mv {GATES}/{name}.tmp {GATES}/{name}.release")

    def finish(self) -> None:
        server.wait_until_succeeds(f"test -e {self.status}", timeout=SPEC_TIMEOUT)
        print(server.succeed(f"cat {self.log}"))
        status = server.succeed(f"cat {self.status}").strip()
        if status != "0":
            raise Exception(f"spec {self.name} failed (exit {status})")


def mock(machine: Any, *replies: dict[str, Any]) -> None:
    """Appends scripted replies to the machine's mock model."""
    machine.succeed(
        f"curl -sf -X POST -H 'content-type: application/json' --data {shlex.quote(json.dumps(list(replies)))} {MOCK}/replies"
    )


def mock_state(machine: Any) -> dict[str, Any]:
    return json.loads(machine.succeed(f"curl -sf {MOCK}/state"))


def mock_requests(machine: Any) -> list[dict[str, Any]]:
    return json.loads(machine.succeed(f"curl -sf {MOCK}/requests"))


def mock_wait_gate(machine: Any, gate: str) -> None:
    machine.wait_until_succeeds(
        f"curl -sf {MOCK}/state | jq -e '.reached | index(\"{gate}\")'", timeout=timedelta(minutes=5)
    )


def mock_release(machine: Any, gate: str) -> None:
    machine.succeed(f"curl -sf -X POST {MOCK}/release/{gate}")


def mock_done(machine: Any) -> None:
    """Every scripted reply was used."""
    state = mock_state(machine)
    assert state["remaining"] == 0, state


def last_message(request: dict[str, Any]) -> str:
    """What pi sent last in a model request (a prompt or a tool result), as text."""
    return json.dumps(request["messages"][-1]["content"])


def host_key(machine: Any) -> str:
    return machine.succeed("ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub").split()[1]


def use_bundle(path: str) -> None:
    """Restarts the server with another bundled daemons directory."""
    server.succeed(f"ln -sfn {path} /run/controlroom-e2e/daemons", "systemctl restart controlroom-web")
    server.wait_until_succeeds(f"curl -sf {BASE_URL}/healthz")


def current_daemon(machine: Any) -> str:
    return as_alice(machine, "readlink ~/.local/share/controlroom/current").strip()


def agent_pids(machine: Any) -> list[str]:
    """pi processes of alice's sessions (pi names its process `pi`)."""
    out = machine.execute("pgrep -u alice -x pi")[1]
    return out.split()


def collect_logs() -> None:
    """Copies the Playwright results and the server, daemon and mock logs to $out."""
    server.execute(f"journalctl -u controlroom-web --no-pager > {E2E}/server.log")
    server.copy_from_machine(E2E, "server")
    for machine in (devbox, buildbox):
        machine.execute("journalctl -u mock-model --no-pager > /tmp/mock-model.log")
        machine.execute(f"curl -sf {MOCK}/requests > /tmp/mock-requests.json")
        machine.execute(
            "tar -C /home/alice/.local -czf /tmp/controlroom-state.tgz state/controlroom share/controlroom/current"
        )
        for path in ("/tmp/mock-model.log", "/tmp/mock-requests.json", "/tmp/controlroom-state.tgz"):
            machine.copy_from_machine(path, machine.name)
    print(server.execute(f"tail -n 200 {E2E}/server.log")[1])
    for machine in (devbox, buildbox):
        print(machine.execute("tail -n 100 /home/alice/.local/state/controlroom/daemon.log")[1])


# ----------------------------------------------------------------------------- scenarios


def boot() -> None:
    start_all()
    server.wait_for_unit("controlroom-web.service")
    server.wait_until_succeeds(f"curl -sf {BASE_URL}/healthz")
    for machine in (devbox, buildbox):
        machine.wait_for_unit("sshd.service")
        machine.wait_for_unit("alice-config.service")
        machine.wait_for_unit("mock-model.service")
        machine.wait_until_succeeds(f"curl -sf {MOCK}/state")


def add_devbox() -> None:
    """Add a machine: host key, permission denied → install command → install, doctor."""
    spec = Spec("add-machine", E2E_HOST="devbox", E2E_HOST_KEY=host_key(devbox), E2E_KEY_FIRST="")
    command = spec.step("install-key")["command"]
    assert "authorized_keys" in command, command
    as_alice(devbox, command)
    spec.release("install-key")
    spec.finish()
    assert current_daemon(devbox) == f"versions/{VERSION}"


def sandboxed_session() -> None:
    """A strict (nono) pi session streams the mock model's reply, then settles idle and unread."""
    mock(devbox, {"gated_text": {"before": "Hello from", "gate": "stream", "after": " the mock model."}})
    spec = Spec("session")
    spec.step("streaming")
    mock_wait_gate(devbox, "stream")
    # pi runs under nono.
    devbox.succeed("pgrep -u alice -f '^[^ ]*nono run'")
    mock_release(devbox, "stream")
    spec.release("streaming")
    spec.finish()
    mock_done(devbox)
    assert "Say hello" in last_message(mock_requests(devbox)[-1])


def start_fresh() -> None:
    """The agent dies mid-turn; start fresh with the previous conversation; the next run works."""
    mock(devbox, {"gated_text": {"before": "Thinking it over", "gate": "crash", "after": " never sent."}})
    spec = Spec("start-fresh")
    spec.step("crash")
    mock_wait_gate(devbox, "crash")
    pids = agent_pids(devbox)
    assert len(pids) == 1, devbox.succeed("ps -eo pid,user,args")
    devbox.succeed(f"kill -KILL {pids[0]}")
    spec.release("crash")

    spec.step("fresh")
    history = as_alice(devbox, "ls ~/.local/state/controlroom/sessions/*/home/history-*.md").split()
    assert len(history) == 1, history
    content = as_alice(devbox, f"cat {history[0]}")
    assert "Say hello" in content and "Hello from the mock model." in content, content
    mock(
        devbox,
        {"tool_call": {"id": "call_read_history", "name": "read", "args": {"path": history[0]}}},
        {"text": "I read the earlier conversation."},
    )
    spec.release("fresh")
    spec.finish()
    mock_done(devbox)
    requests = mock_requests(devbox)
    prompt, tool_result = last_message(requests[-2]), last_message(requests[-1])
    assert history[0] in prompt and "Earlier conversation" in prompt, prompt
    assert "Hello from the mock model." in tool_result, f"pi couldn't read the history: {tool_result}"


def approvals() -> None:
    """git push needs an approval: allowed once in one tab, denied with a reason in the other."""
    as_alice(
        devbox,
        "mkdir -p ~/push && cd ~/push && git init -q --bare remote.git && git init -q repo && cd repo"
        " && git -c user.email=alice@example.com -c user.name=alice commit -q --allow-empty -m init"
        " && git remote add origin ../remote.git",
    )

    def push(branch: str) -> dict[str, Any]:
        command = f"cd repo && git push origin HEAD:{branch} 2>&1; echo exit=$?"
        return {"tool_call": {"id": f"call_push_{branch}", "name": "bash", "args": {"command": command}}}

    mock(devbox, push("allowed"), {"text": "Pushed."}, push("denied"), {"text": "The push was denied."})

    def remote_has(branch: str) -> bool:
        return devbox.execute(f"su - alice -c 'git -C ~/push/remote.git rev-parse -q --verify refs/heads/{branch}'")[0] == 0

    spec = Spec("approvals")
    spec.finish()
    mock_done(devbox)
    assert remote_has("allowed"), "the allowed push reached the remote"
    assert not remote_has("denied"), "the denied push must not reach the remote"
    denied = last_message(mock_requests(devbox)[-1])
    assert "not this branch" in denied and "exit=126" in denied, denied


def auto_upgrade() -> None:
    """An older daemon is upgraded only once idle; the session survives."""
    use_bundle(OLD_BUNDLE)
    mock(
        buildbox,
        {"gated_text": {"before": "Building", "gate": "build", "after": " done."}},
        {"text": "Still here."},
    )
    spec = Spec("add-machine", E2E_HOST="buildbox", E2E_HOST_KEY=host_key(buildbox), E2E_KEY_FIRST="1")
    as_alice(buildbox, spec.step("install-key")["command"])
    spec.release("install-key")
    spec.finish()
    assert current_daemon(buildbox) == f"versions/{VERSION}-old"

    spec = Spec("upgrade")
    spec.step("restart")
    mock_wait_gate(buildbox, "build")
    supervisors = buildbox.succeed("pgrep -u alice -f ' supervise '").split()
    use_bundle(CURRENT_BUNDLE)
    spec.release("restart")

    # The server sees the update but must not upgrade while the run is in flight: a negative
    # check over several of the controller's quiet periods (5 s).
    spec.step("update-available")
    time.sleep(20)
    assert current_daemon(buildbox) == f"versions/{VERSION}-old"
    mock_release(buildbox, "build")
    spec.release("update-available")

    spec.step("upgraded")
    assert current_daemon(buildbox) == f"versions/{VERSION}"
    buildbox.fail(f"pgrep -u alice -f 'versions/{VERSION}-old/controlroomd serve'")
    assert buildbox.succeed("pgrep -u alice -f ' supervise '").split() == supervisors, "the agent kept running"
    spec.release("upgraded")
    spec.finish()
    mock_done(buildbox)


def unreachable() -> None:
    """A machine without network shows as unreachable with its stale state; the other one keeps
    working; it reconnects once the network is back."""
    mock(devbox, {"text": "Devbox still works."})
    spec = Spec("unreachable")
    spec.step("unplug")
    buildbox.block()
    spec.release("unplug")
    spec.step("plug")
    buildbox.unblock()
    spec.release("plug")
    spec.finish()
    mock_done(devbox)


def rollback() -> None:
    """A daemon that can't serve is rolled back; the machine stays usable."""
    mock(buildbox, {"text": "Rolled back fine."})
    use_bundle(BROKEN_BUNDLE)
    Spec("rollback").finish()
    for machine in (devbox, buildbox):
        assert current_daemon(machine) == f"versions/{VERSION}"
    mock_done(buildbox)


try:
    boot()
    add_devbox()
    sandboxed_session()
    start_fresh()
    approvals()
    auto_upgrade()
    unreachable()
    rollback()
except Exception:
    collect_logs()
    raise
