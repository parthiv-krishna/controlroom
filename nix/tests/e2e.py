# pyright: reportUndefinedVariable=false
# The VM end-to-end test script (see ./e2e.nix, which prepends CURRENT_BUNDLE, OLD_BUNDLE,
# BROKEN_BUNDLE, VERSION, NONO_VERSION and RUNNER). The NixOS test driver defines the machines
# (server, devbox, buildbox) and start_all, and type-checks the script with them.
#
# Each scenario runs one Playwright spec (web/e2e-vm/<name>.spec.ts) on `server` in the
# background. When a spec needs something done outside the browser it reaches a *step*: it
# writes `<gates>/<step>.reached` (a JSON payload) and waits for `<step>.release`. This script
# serves the steps in order, doing what the user (or the network, or a power button) would do,
# and scripts the mock model (on the server, where pi runs) before each scenario. The agent's
# long commands wait for gate files in their workspace, which this script creates. Nothing waits
# on a fixed delay except the one negative check (an auto-upgrade must not happen while a call
# is in flight).

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
API = "/home/alice/work/api"
WEB = "/home/alice/work/web"
BUILD = "/home/alice/build"
# The nono the server uploads with controlroomd (the hosts have none of their own).
NONO = f"/home/alice/.local/share/controlroom/nono/{NONO_VERSION}/nono"

# ------------------------------------------------------------------------------- helpers


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
        self.started = time.monotonic()
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

    def release(self, name: str) -> None:
        """Lets the spec continue past step `name`."""
        server.succeed(f"touch {GATES}/{name}.release")

    def finish(self) -> None:
        server.wait_until_succeeds(f"test -e {self.status}", timeout=SPEC_TIMEOUT)
        print(server.succeed(f"cat {self.log}"))
        status = server.succeed(f"cat {self.status}").strip()
        print(f"spec {self.name}: exit {status} after {time.monotonic() - self.started:.1f} s")
        if status != "0":
            raise Exception(f"spec {self.name} failed (exit {status})")


def mock(*replies: dict[str, Any]) -> None:
    """Appends scripted replies to the mock model."""
    server.succeed(
        f"curl -sf -X POST -H 'content-type: application/json' --data {shlex.quote(json.dumps(list(replies)))} {MOCK}/replies"
    )


def mock_request(select: str = ".[-1]", **args: str) -> dict[str, Any]:
    """One model request the mock received, picked by a jq filter in the VM (default: the
    latest). All of them together grow past 100 KB, which the driver's console doesn't carry
    reliably, so only the one needed leaves the VM."""
    jq_args = " ".join(f"--arg {k} {shlex.quote(v)}" for k, v in args.items())
    return json.loads(
        server.succeed(f"curl -sf {MOCK}/requests | jq -ce {jq_args} {shlex.quote(select)}")
    )


def mock_done() -> None:
    """Every scripted reply was used."""
    state = json.loads(server.succeed(f"curl -sf {MOCK}/state"))
    assert state["remaining"] == 0, state


def text(message: dict[str, Any]) -> str:
    content = message.get("content")
    return content if isinstance(content, str) else json.dumps(content)


def tool_result(call_id: str) -> str:
    """What pi sent the model as the result of tool call `call_id` (in the latest request)."""
    for message in mock_request()["messages"]:
        if message.get("role") == "tool" and message.get("tool_call_id") == call_id:
            return text(message)
    raise Exception(f"no result of {call_id} in the last model request")


def conversation() -> list[str]:
    """The latest model request's messages, as text."""
    return [text(m) for m in mock_request()["messages"]]


def tool(call_id: str, name: str, args: dict[str, Any]) -> dict[str, Any]:
    return {"tool_call": {"id": call_id, "name": name, "args": args}}


def bash(call_id: str, label: str, command: str, shell: str | None = None) -> dict[str, Any]:
    """A bash call whose first line (its title in the timeline) is `# <label>`."""
    args: dict[str, Any] = {"command": f"# {label}\n{command}"}
    if shell:
        args["shell"] = shell
    return tool(call_id, "bash", args)


def say(reply: str) -> dict[str, Any]:
    return {"text": reply}


def wait_for_gate(root: str, name: str) -> str:
    """A command line that waits until this script opens gate `name` of the workspace."""
    return f"until [ -e {root}/.gates/{name} ]; do sleep 0.2; done"


def open_gate(machine: Any, root: str, name: str) -> None:
    as_alice(machine, f"touch {root}/.gates/{name}")


def lines(machine: Any, path: str) -> int:
    return int(machine.succeed(f"wc -l < {path}").strip())


def host_key(machine: Any) -> str:
    return machine.succeed("ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub").split()[1]


def use_bundle(path: str) -> None:
    """Restarts the web server with another bundled daemons directory (the agent daemon and its
    sessions keep running)."""
    server.succeed(f"ln -sfn {path} /run/controlroom-e2e/daemons", "systemctl restart controlroom")
    server.wait_until_succeeds(f"curl -sf {BASE_URL}/healthz")


def current_daemon(machine: Any) -> str:
    return as_alice(machine, "readlink ~/.local/share/controlroom/current").strip()


def uploaded_nono(machine: Any) -> int:
    """The inode of the nono uploaded to `machine`, after checking the current daemon uses it and
    it runs."""
    used = as_alice(machine, "readlink -f ~/.local/share/controlroom/current/nono").strip()
    assert used == NONO, used
    assert as_alice(machine, f"{NONO} --version").strip() == f"nono {NONO_VERSION}"
    return int(as_alice(machine, f"stat -c %i {NONO}").strip())


def collect_logs() -> None:
    """Copies the Playwright results, the server's and agent daemon's journals and state, the
    mock model's requests and each host's daemon state to $out."""
    server.execute(f"mkdir -p {E2E}")
    for unit in ("controlroom", "controlroom-agent", "mock-model", "push-mock"):
        server.execute(f"journalctl -u {unit} --no-pager > {E2E}/{unit}.log")
    server.execute(f"curl -sf {MOCK}/requests > {E2E}/mock-requests.json")
    server.execute(
        f"tar -C /var/lib -czf {E2E}/controlroom-agent-state.tgz --exclude=agent/auth.json controlroom-agent"
    )
    server.copy_from_machine(E2E, "server")
    for machine in (devbox, buildbox):
        machine.execute(
            "tar -C /home/alice/.local -czf /tmp/controlroom-state.tgz state/controlroom share/controlroom/current"
        )
        machine.copy_from_machine("/tmp/controlroom-state.tgz", machine.name)
    print(server.execute(f"tail -n 150 {E2E}/controlroom.log")[1])
    print(server.execute(f"tail -n 150 {E2E}/controlroom-agent.log")[1])
    for machine in (devbox, buildbox):
        print(machine.execute("tail -n 80 /home/alice/.local/state/controlroom/daemon.log")[1])


# ----------------------------------------------------------------------------- scenarios


def boot() -> None:
    start_all()
    server.wait_for_unit("controlroom-agent.service")
    server.wait_for_unit("controlroom.service")
    server.wait_for_unit("mock-model.service")
    server.wait_for_unit("push-mock.service")
    server.wait_until_succeeds(f"curl -sf {BASE_URL}/healthz")
    server.wait_until_succeeds(f"curl -sf {MOCK}/state")
    server.wait_until_succeeds("journalctl -u controlroom | grep -q 'connected to the agent daemon'")
    # The model key is the agent daemon's credential, never pi's or the web server's.
    server.fail("grep -rq sk-e2e- /var/lib/controlroom-agent/agent")
    for machine in (devbox, buildbox):
        machine.wait_for_unit("sshd.service")
        # Hosts carry no pi and no nono: the server uploads its own.
        machine.fail("su - alice -c 'command -v pi'")
        machine.fail("su - alice -c 'command -v nono'")
    # The workspaces' contents, as alice would have them.
    as_alice(
        devbox,
        f"mkdir -p {API}/src {API}/.gates && echo 'remote notes' > {API}/notes.txt"
        f" && echo 'Run the tests with make check.' > {API}/AGENTS.md",
    )
    as_alice(buildbox, f"mkdir -p {BUILD}/.gates {WEB}/.gates && echo \"const version = 'v1';\" > {WEB}/app.js")


def add_machine(machine: Any, key_first: bool) -> None:
    """Add a machine: host key, then the key install command (after "permission denied", or up
    front), then the server installs the host daemon and shows its doctor."""
    spec = Spec(
        "add-machine",
        E2E_HOST=machine.name,
        E2E_HOST_KEY=host_key(machine),
        E2E_KEY_FIRST="1" if key_first else "",
    )
    command = spec.step("install-key")["command"]
    assert "authorized_keys" in command, command
    as_alice(machine, command)
    spec.release("install-key")
    spec.finish()


def auto_upgrade() -> None:
    """buildbox (added with the old bundled daemon) is upgraded only once its call is done; the
    call survives a web server restart; the shell survives the upgrade with its state."""
    mock(
        bash(
            "call_build",
            "make-it",
            f"echo make-it >> ran.txt; export STAGE=built; {wait_for_gate(BUILD, 'build')}; echo built",
        ),
        say("Built."),
        bash("call_stage", "stage", 'echo "stage=$STAGE"'),
        say("Still here."),
    )
    spec = Spec("upgrade")
    spec.step("restart")
    buildbox.wait_until_succeeds(f"test -s {BUILD}/ran.txt")
    supervisors = buildbox.succeed("pgrep -u alice -f 'controlroomd supervise'").split()
    nono = uploaded_nono(buildbox)
    use_bundle(CURRENT_BUNDLE)
    spec.release("restart")

    # The server sees the update but must not upgrade while the call is in flight: a negative
    # check over several of the controller's quiet periods (5 s).
    spec.step("update-available")
    time.sleep(20)
    assert current_daemon(buildbox) == f"versions/{VERSION}-old"
    open_gate(buildbox, BUILD, "build")
    spec.release("update-available")

    spec.step("upgraded")
    assert current_daemon(buildbox) == f"versions/{VERSION}"
    buildbox.fail(f"pgrep -u alice -f 'versions/{VERSION}-old/controlroomd serve'")
    assert buildbox.succeed("pgrep -u alice -f 'controlroomd supervise'").split() == supervisors, "the shell kept running"
    assert uploaded_nono(buildbox) == nono, "the upgrade kept nono (the same version): no upload"
    spec.release("upgraded")
    spec.finish()
    mock_done()
    assert lines(buildbox, f"{BUILD}/ran.txt") == 1, "the command ran once across the web server restart"
    assert "built" in tool_result("call_build")
    assert "stage=built" in tool_result("call_stage"), "the shell kept its variables across the upgrade"


def workspaces() -> None:
    """A session on both machines: files, named shells and sandboxes on each."""
    mock(
        tool("call_read", "read", {"path": "devbox:notes.txt"}),
        tool("call_edit", "edit", {"path": "buildbox:app.js", "edits": [{"oldText": "'v1'", "newText": "'v2'"}]}),
        bash("call_cd", "cd src", "cd src && export GREETING=hi && pwd"),
        bash(
            "call_greet",
            "greet",
            'echo "$GREETING from $(pwd)"; (echo x > ~/planted.txt) 2>/dev/null || echo confined',
        ),
        bash("call_where", "where", "hostname; cat app.js", shell="buildbox"),
        say("Both machines work."),
    )
    spec = Spec("workspaces")
    spec.finish()
    mock_done()
    # pi ran on the server under nono; the shells ran on the hosts under the uploaded nono.
    server.succeed("pgrep -u controlroom-agent -f '^[^ ]*nono run'")
    for machine in (devbox, buildbox):
        machine.succeed(f"pgrep -u alice -f '^{NONO} run'")
    first = mock_request(
        "first(.[] | select(.messages | tostring | contains($prompt)))",
        prompt="Look around both machines",
    )
    system = text(first["messages"][0])
    assert "devbox:~/work/api/AGENTS.md" in system and "make check" in system, system
    assert "<workspaces>" in system, system
    assert "remote notes" in tool_result("call_read")
    assert "'v2'" in buildbox.succeed(f"cat {WEB}/app.js")
    assert f"{API}/src" in tool_result("call_cd")
    greet = tool_result("call_greet")
    assert f"hi from {API}/src" in greet and "confined" in greet, greet
    devbox.fail("test -e /home/alice/planted.txt")
    where = tool_result("call_where")
    assert "buildbox" in where and "'v2'" in where, where


def overlap() -> None:
    """A second session on devbox:~/work/api is warned about (no model call)."""
    Spec("overlap").finish()


def push_subscribe() -> None:
    """A device (iPhone) turns Web Push on; its subscription points at the mock push service."""
    Spec("push").finish()


def approvals() -> None:
    """git push in devbox's sandbox needs an approval: allowed once in one tab, denied with a
    reason in the other."""
    as_alice(
        devbox,
        "mkdir -p ~/push && cd ~/push && git init -q --bare remote.git && git init -q repo && cd repo"
        " && git -c user.email=alice@example.com -c user.name=alice commit -q --allow-empty -m init"
        " && git remote add origin ../remote.git",
    )

    def push(branch: str) -> dict[str, Any]:
        command = f"cd ~/push/repo && git push origin HEAD:{branch} 2>&1; echo exit=$?"
        return bash(f"call_push_{branch}", f"push {branch}", command)

    mock(push("allowed"), say("Pushed."), push("denied"), say("The push was denied."))

    def remote_has(branch: str) -> bool:
        return devbox.execute(f"su - alice -c 'git -C ~/push/remote.git rev-parse -q --verify refs/heads/{branch}'")[0] == 0

    Spec("approvals").finish()
    mock_done()
    assert remote_has("allowed"), "the allowed push reached the remote"
    assert not remote_has("denied"), "the denied push must not reach the remote"
    denied = tool_result("call_push_denied")
    assert "not this branch" in denied and "exit=0" not in denied, denied


def web_push() -> None:
    """The approval was pushed to the iPhone: the hardened server resolved push.example.com,
    passed its SSRF guard, and completed TLS up to rejecting the mock push service's certificate
    (only the public web PKI is trusted), with which the push fails."""
    server.wait_until_succeeds(
        "journalctl -u push-mock | grep -Eq 'TLS handshake for push.example.com: .*(CERTIFICATE|UNKNOWN_CA)'",
        timeout=timedelta(minutes=2),
    )
    server.succeed("journalctl -u controlroom | grep -q 'push failed'")


def terminal_input() -> None:
    """A command waits for input; the user types it in the shell's terminal tab."""
    mock(bash("call_ask", "ask", "read -r -p 'Name? ' name && echo \"hello $name\""), say("Greeted."))
    Spec("terminal-input").finish()
    mock_done()
    assert "hello alice" in tool_result("call_ask")


def schedules() -> None:
    """A schedule's "Run now" sends its message to pi through the server's scheduler."""
    mock(say("Inbox is empty."))
    Spec("schedules").finish()
    mock_done()
    assert "Check inbox/ and report" in conversation()[-1]


def blip() -> None:
    """devbox's network goes away mid-command (within the grace) and comes back."""
    mock(
        bash(
            "call_slow",
            "slow",
            f"echo slow >> {API}/ran.txt; echo tick 1; {wait_for_gate(API, 'blip')}; echo tick 2",
        ),
        say("Slow one done."),
    )
    spec = Spec("blip")
    spec.step("cut")
    devbox.wait_until_succeeds(f"test -s {API}/ran.txt")
    devbox.block()
    spec.release("cut")
    spec.step("restore")
    # The command finishes while devbox is cut off; its result waits in devbox's journal.
    open_gate(devbox, API, "blip")
    devbox.unblock()
    spec.release("restore")
    spec.finish()
    mock_done()
    assert lines(devbox, f"{API}/ran.txt") == 1, "the command ran once"
    result = tool_result("call_slow")
    assert "tick 1" in result and "tick 2" in result, result
    assert "dropped" in result, f"the agent is told about the link drop: {result}"


def grace() -> None:
    """An outage past the grace with a job and a command running on devbox."""
    mock(
        tool(
            "call_job",
            "job",
            {
                "action": "run",
                "name": "build",
                "at": "devbox:~/work/api",
                "command": f"echo compiling; {wait_for_gate(API, 'job')}; echo linked",
            },
        ),
        bash("call_late", "late", f"echo late >> {API}/late.txt; {wait_for_gate(API, 'late')}; echo \"late-$((1+1))\""),
        say("devbox is away; I'll wait."),
    )
    spec = Spec("grace")
    spec.step("cut")
    devbox.wait_until_succeeds(f"test -s {API}/late.txt")
    devbox.block()
    spec.release("cut")

    spec.step("restore")
    mock_done()
    lost = tool_result("call_late")
    assert "devbox" in lost and "unreachable" in lost, lost
    # The job and the command finish while devbox is cut off.
    open_gate(devbox, API, "job")
    open_gate(devbox, API, "late")
    devbox.wait_until_succeeds(f"grep -q linked {API}/.controlroom/jobs/*.log")
    devbox.unblock()
    spec.release("restore")

    # After the reconnect the late result and the job's exit wake the agent (in one run or
    # two; the mock answers "ok" to anything unscripted).
    spec.step("woken")
    server.wait_until_succeeds(
        f"curl -sf {MOCK}/requests | jq -e '.[-1].messages | tostring | (contains(\"late-2\") and contains(\"exited with code 0\"))'",
        timeout=timedelta(minutes=3),
    )
    spec.release("woken")
    spec.finish()
    seen = "\n".join(conversation())
    assert seen.count("late-2") == 1, f"the late result is injected once: {seen}"
    assert seen.count("exited with code 0") == 1, f"one exit wake: {seen}"
    assert lines(devbox, f"{API}/late.txt") == 1, "the command ran once"


def reboot() -> None:
    """buildbox reboots mid-command: the call is lost, the shell is reset."""
    mock(
        bash("call_prepare", "prepare", "mkdir -p sub && cd sub && export STAGE=prepared && echo ready", shell="buildbox"),
        bash("call_wait", "wait", f"echo waiting >> {WEB}/waited.txt; {wait_for_gate(WEB, 'reboot')}", shell="buildbox"),
        say("buildbox went away mid-command."),
        bash("call_check", "check", 'echo "stage=${STAGE:-unset}"; pwd', shell="buildbox"),
        say("The shell was reset."),
    )
    spec = Spec("reboot")
    spec.step("reboot")
    buildbox.wait_until_succeeds(f"test -s {WEB}/waited.txt")
    buildbox.shutdown()
    buildbox.start()
    buildbox.wait_for_unit("sshd.service")
    spec.release("reboot")
    spec.finish()
    mock_done()
    lost = tool_result("call_wait")
    assert "restarted while this call ran" in lost and "result is lost" in lost, lost
    check = tool_result("call_check")
    assert "stage=unset" in check and f"{WEB}/sub" in check, check
    assert "was reopened before this command (the host restarted" in check, f"the agent is told the shell was reset: {check}"
    assert lines(buildbox, f"{WEB}/waited.txt") == 1, "the lost command is never run again"


def rollback() -> None:
    """A host daemon that can't serve is rolled back; the machines stay usable."""
    mock(say("Rolled back fine."))
    use_bundle(BROKEN_BUNDLE)
    Spec("rollback").finish()
    for machine in (devbox, buildbox):
        assert current_daemon(machine) == f"versions/{VERSION}"
    mock_done()


try:
    boot()
    add_machine(devbox, key_first=False)
    assert current_daemon(devbox) == f"versions/{VERSION}"
    uploaded_nono(devbox)
    use_bundle(OLD_BUNDLE)
    add_machine(buildbox, key_first=True)
    assert current_daemon(buildbox) == f"versions/{VERSION}-old"
    uploaded_nono(buildbox)
    auto_upgrade()
    workspaces()
    overlap()
    push_subscribe()
    approvals()
    web_push()
    terminal_input()
    schedules()
    blip()
    grace()
    reboot()
    rollback()
except Exception:
    collect_logs()
    raise
