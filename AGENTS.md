# AGENTS.md

controlroom is a self-hosted web app for supervising pi coding-agent sessions on many Linux machines over SSH.
**The build spec is `docs/architecture.html`.** Read the sections relevant to your workstream before writing code.

## Scope decisions

- **pi only.** ACP is out of the design: no AcpDriver, no `controlroomd mcp`, no pi-acp, no ACP fixtures or fake-agent ACP mode.
  The `Driver` trait stays (PiDriver plus FakeDriver for tests).
- **The UI reimplements the functionality of the design mockup** (`controlroom-artifacts/design/mockup-src`), not its code.
  The mockup and its fake backend contain shortcuts; write clean, idiomatic code.
- **Ports from the prototype** (`../controlroom-old`) are reviewed, adapted and fixed, never copied wholesale.
  The prototype reviews (`controlroom-artifacts/reviews/*.final.md`) list the bugs that must not come back (spec §16).

## Repository layout

```text
crates/protocol      wire types, tolerant decoding, TS export (owns web/src/lib/protocol)
crates/transcript    record format, writer, reader/tail, seq index
crates/supervisor    detached per-process pipe and its socket protocol
crates/drivers       Driver trait, PiDriver, FakeDriver
crates/runtime       session runtime library (no sockets; IO via traits, hosts via the hostops bridge)
crates/sandbox       nono profiles, per-session agent dir, credential helpers, webhook types
crates/daemon        bin controlroomd (execution hosts)
crates/agentd        bin controlroom-agentd (pi sessions on the server)
crates/client-core   russh, keys, bootstrap, daemon client
crates/controller    client store, machine manager, hub, UI API, attention, broker
crates/web           bin controlroom-web (axum shell)
crates/fake-agent    scriptable fake pi-RPC agent
crates/fixture-tool  records and sanitizes pi-RPC transcripts
crates/e2e           end-to-end tests: controller, agentd and two host daemons (test-only)
web/                 Svelte 5 SPA
fixtures/            sanitized recordings (committed)
nix/                 rust.nix, web.nix (more later)
```

## Commands

- Full gate: `nix flake check -L` (check the real exit status; never pipe through `tail` without `set -o pipefail`).
  Nix only sees git-tracked files: `git add` new files first.
- Dev shell: `nix develop` (Rust toolchain, cargo-nextest, Node 22, pnpm). Inside: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt`.
- UI: `cd web && pnpm install && pnpm check && pnpm test && pnpm build`.
  After changing `web/pnpm-lock.yaml`, set the `pnpmDeps` hash in `nix/web.nix` to `""`, build `.#web-dist`, and copy the reported hash back.

## Process rules

- Work only in your assigned worktree and branch. Commit to your own branch; the supervisor rebases and merges.
- **Never `git commit -a`** and never `git add -A`/`git add .`. Stage files by name.
- **`README.md` is human-only:** never edit or commit it. Planning files stay uncommitted.
- The root `Cargo.toml` is integrator-owned. Put dependencies in your crate's `Cargo.toml`; you may add a
  `[workspace.dependencies]` entry only when several crates share it, and say so in your report.
- Don't bump the workspace version; the supervisor does it at merge (every daemon change bumps it).
- Contract changes go through `crates/protocol`. Generated TypeScript in `web/src/lib/protocol` is read-only for the UI.
- Tests are offline and deterministic: gate steps, not sleeps. Integration tests kill every process they spawn, even on failure.
- Never use `nix profile add/install`. Never modify `~/.pi/agent`, home-manager-managed files or `~/.ssh`.
  Run pi and nono experiments with isolated `HOME`/XDG dirs. **Never use nono registry packs** (`nono pull`, `ns/name` profiles).
- Keep a GC root (`nix build -o <path>`) for any binary you run as a server, and start servers only after their build finishes.
- **Privacy:** no real hostnames, usernames, domains, provider URLs, model ids or secrets in code, docs, fixtures or commit messages.
  Use `example.com`, `alice`, `devbox`. Sanitize recordings.
- American spelling. Concise, imperative commit messages scoped by crate (e.g. `transcript: add seq index`).

## Code style

- Rust 2024, `unsafe_code = "deny"` (local `#[allow]` only with a `// SAFETY:` comment), clippy clean with `-D warnings`.
- Remote paths are the `RemotePath` POSIX-string newtype, never `std::path`. Ids are ULID strings. Timestamps are unix ms.
- Every extensible enum on the wire has an `Unknown` fallback. Bounded channels. Slow work off connection loops.
- Small modules with one job each. Prefer clarity over cleverness; no dead code or speculative abstractions.
