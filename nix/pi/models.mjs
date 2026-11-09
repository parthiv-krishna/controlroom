// controlroom-pi-models: Settings › Models asks pi itself (spec §11.3).
//
//   controlroom-pi-models list   --agent-dir DIR                prints {providers, models}
//   controlroom-pi-models logout --agent-dir DIR --provider ID  signs out (under pi's lock)
//   controlroom-pi-models login  --agent-dir DIR --provider ID  signs in, over JSON lines
//
// The agent daemon runs this host-side. `list` reads DIR's config (the agent daemon builds one
// with placeholder credentials) and prints every provider pi knows, whether pi can sign in to
// it, and the models of the providers that have credentials. Nothing is written but auth.json,
// by `login` and `logout`, through pi's own locked store.
//
// `login` drives pi's login flow for one provider. It prints `{"step": STEP}` lines (the
// protocol's LoginStep: choose, open_url, device_code, input, progress, done, failed) and reads
// `{"input": TEXT}` and `{"cancel": true}` lines. An input answers pi's pending prompt (an
// option, an answer, or the pasted code or redirect URL for its manual-code prompt); with no
// prompt pending, a pasted loopback redirect URL is delivered to pi's callback listener on this
// machine (where the browser can't reach it). Errors never contain token material.
// Exit status: 0 done, 1 failed or cancelled, 2 usage.

import { randomUUID } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { parseArgs } from "node:util";

import { InMemoryModelsStore } from "@earendil-works/pi-ai";
import { ModelRuntime, readStoredCredential } from "@earendil-works/pi-coding-agent";

function fail(message, status = 1) {
  process.stderr.write(`controlroom-pi-models: ${message}\n`);
  process.exit(status);
}

/** An error and its causes as one line (pi's messages name endpoints, never tokens). */
function describe(error) {
  let text = "";
  for (let e = error, depth = 0; e && depth < 4; e = e.cause, depth += 1) {
    const message = e instanceof Error ? e.message : String(e);
    if (!text.includes(message)) text = text ? `${text}: ${message}` : message;
  }
  return text.slice(0, 500);
}

const [mode, ...rest] = process.argv.slice(2);
let options;
try {
  ({ values: options } = parseArgs({
    args: rest,
    options: { "agent-dir": { type: "string" }, provider: { type: "string" } },
    strict: true,
  }));
} catch (error) {
  fail(error.message, 2);
}
const agentDir = options["agent-dir"];
const provider = options.provider;
const needsProvider = mode === "login" || mode === "logout";
if (!["list", "login", "logout"].includes(mode) || !agentDir?.startsWith("/") || needsProvider !== !!provider) {
  fail("usage: list|login|logout --agent-dir ABSOLUTE_DIR [--provider ID]", 2);
}

const authPath = join(agentDir, "auth.json");
mkdirSync(agentDir, { recursive: true, mode: 0o700 });
const runtime = await ModelRuntime.create({
  authPath,
  modelsPath: join(agentDir, "models.json"),
  // No catalog refresh, no models-store.json.
  modelsStore: new InMemoryModelsStore(),
  refreshOnCreate: false,
}).catch((error) => fail(`loading pi's models: ${describe(error)}`));

if (mode === "list") {
  const providers = runtime.getProviders().map((p) => ({ id: p.id, name: p.name, oauth: !!p.auth?.oauth }));
  const available = await runtime.getAvailable().catch((error) => fail(`listing models: ${describe(error)}`));
  const models = available.map((m) => ({ provider: m.provider, id: m.id, name: m.name ?? m.id, reasoning: !!m.reasoning }));
  process.stdout.write(JSON.stringify({ providers, models }));
  process.exit(0);
}

if (mode === "logout") {
  if (readStoredCredential(provider, authPath)?.type !== "oauth") fail(`not signed in to ${provider}`);
  await runtime.logout(provider).catch((error) => fail(`signing out of ${provider}: ${describe(error)}`));
  process.exit(0);
}

// ---------------------------------------------------------------------------------------------
// login

const step = (s) => process.stdout.write(`${JSON.stringify({ step: s })}\n`);
const abort = new AbortController();
/** pi's pending prompt: {type, resolve, reject}. */
let pending = null;
/** A queued input that arrived before pi asked. */
const queued = [];

function ask(prompt) {
  if (prompt.signal?.aborted) return Promise.reject(new Error("Login cancelled"));
  return new Promise((resolve, reject) => {
    pending = { type: prompt.type, resolve, reject };
    prompt.signal?.addEventListener("abort", () => {
      if (pending?.resolve === resolve) pending = null;
      reject(new Error("Login cancelled"));
    });
    switch (prompt.type) {
      case "select":
        step({ kind: "choose", message: prompt.message, options: prompt.options.map((o) => ({ id: o.id, label: o.label })) });
        break;
      case "manual_code":
        // Shown with the sign-in page (open_url), whose paste field answers it.
        break;
      case "text":
        step({ kind: "input", message: prompt.message, placeholder: prompt.placeholder ?? null });
        break;
      default:
        pending = null;
        reject(new Error(`pi asked for a ${prompt.type}, which a sign-in from the UI can't answer`));
    }
    if (pending && queued.length > 0) answer(queued.shift());
  });
}

/** A loopback URL (pi's callback listener on this machine). */
function loopback(text) {
  try {
    const url = new URL(text.trim());
    return url.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname) ? url : null;
  } catch {
    return null;
  }
}

async function answer(text) {
  if (pending) {
    const { resolve } = pending;
    pending = null;
    resolve(text);
    return;
  }
  const url = loopback(text);
  if (!url) {
    queued.push(text);
    return;
  }
  // The browser ended on pi's loopback callback: deliver it here.
  step({ kind: "progress", message: "Completing sign-in" });
  try {
    url.hostname = "127.0.0.1";
    const response = await fetch(url, { signal: AbortSignal.timeout(30_000) });
    if (!response.ok) step({ kind: "failed", message: `the sign-in callback answered ${response.status}` });
  } catch (error) {
    step({ kind: "failed", message: `delivering the redirect URL: ${describe(error)}` });
  }
}

function deviceId() {
  const path = join(agentDir, "controlroom-device-id");
  try {
    return readFileSync(path, "utf8").trim();
  } catch {
    const id = randomUUID();
    writeFileSync(path, id, { mode: 0o600 });
    return id;
  }
}

createInterface({ input: process.stdin }).on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.cancel) abort.abort();
  else if (typeof message.input === "string") void answer(message.input);
}).on("close", () => abort.abort());

const known = runtime.getProvider(provider);
if (!known?.auth?.oauth) {
  step({ kind: "failed", message: `pi can't sign in to ${provider}` });
  process.exit(1);
}
step({ kind: "progress", message: `Starting ${known.auth.oauth.name ?? provider} sign-in` });
try {
  await runtime.login(
    provider,
    "oauth",
    {
      signal: abort.signal,
      prompt: ask,
      notify: (event) => {
        if (event.type === "auth_url") step({ kind: "open_url", url: event.url, instructions: event.instructions ?? null });
        else if (event.type === "device_code") step({ kind: "device_code", user_code: event.userCode, verification_uri: event.verificationUri });
        else if (event.type === "progress" || event.type === "info") step({ kind: "progress", message: event.message });
      },
    },
    { getDeviceId: deviceId, agentName: "controlroom" },
  );
} catch (error) {
  step({ kind: "failed", message: abort.signal.aborted ? "cancelled" : describe(error) });
  process.exit(1);
}
const credential = readStoredCredential(provider, authPath);
step({ kind: "done", account: typeof credential?.email === "string" ? credential.email : null });
process.exit(0);
