// controlroom-pi-oauth: prints a fresh access token of one OAuth provider (spec §11.3, §20).
//
//   controlroom-pi-oauth --agent-dir DIR --provider ID [--valid-for-secs N]
//
// controlroomd runs this host-side, for nono's credential capture; sandboxed pi only ever sees
// a synthetic token. The token is refreshed by pi itself: `Models.getAuth` with a minimum
// validity runs pi-ai's `refreshStoredOAuthCredential`, which refreshes under the auth.json
// file lock and persists the rotated tokens before releasing it. This process therefore
// coordinates with the user's own pi processes, and is the only refresher of the session
// credentials. The token goes to stdout without a newline; errors go to stderr and never
// contain token material. Exit status: 0 printed, 1 failed, 2 usage.

import { join } from "node:path";
import { parseArgs } from "node:util";

import { InMemoryModelsStore } from "@earendil-works/pi-ai";
import { ModelRuntime, readStoredCredential } from "@earendil-works/pi-coding-agent";

/** Upper bound for the whole run: pi waits up to 30 s for the lock and 15 s for a refresh. */
const DEADLINE_MS = 50_000;

function fail(message, status = 1) {
  process.stderr.write(`controlroom-pi-oauth: ${message}\n`);
  process.exit(status);
}

/**
 * An error and its causes as one line. pi's messages name the provider and the endpoint's
 * error, never token values.
 */
function describe(error) {
  let text = "";
  for (let e = error, depth = 0; e && depth < 4; e = e.cause, depth += 1) {
    const message = e instanceof Error ? e.message : String(e);
    if (!text.includes(message)) text = text ? `${text}: ${message}` : message;
  }
  return text;
}

let options;
try {
  ({ values: options } = parseArgs({
    options: {
      "agent-dir": { type: "string" },
      provider: { type: "string" },
      "valid-for-secs": { type: "string", default: "600" },
    },
    strict: true,
  }));
} catch (error) {
  fail(error.message, 2);
}
const agentDir = options["agent-dir"];
const provider = options.provider;
const validForSecs = Number(options["valid-for-secs"]);
if (!agentDir?.startsWith("/") || !provider || !Number.isInteger(validForSecs) || validForSecs < 0) {
  fail("usage: --agent-dir ABSOLUTE_DIR --provider ID [--valid-for-secs N]", 2);
}

const authPath = join(agentDir, "auth.json");
if (readStoredCredential(provider, authPath)?.type !== "oauth") {
  fail(`provider ${provider} has no OAuth credential in ${authPath}`);
}

const validForMs = validForSecs * 1000;
try {
  const runtime = await ModelRuntime.create({
    authPath,
    modelsPath: join(agentDir, "models.json"),
    // Nothing but auth.json is written: no catalog refresh, no models-store.json.
    modelsStore: new InMemoryModelsStore(),
    refreshOnCreate: false,
  });
  const auth = await runtime.getAuth(provider, {
    minOAuthValidityMs: validForMs,
    signal: AbortSignal.timeout(DEADLINE_MS),
  });
  if (!auth) fail(`provider ${provider} is not an OAuth provider pi knows`);
} catch (error) {
  fail(`refreshing ${provider} failed: ${describe(error)}`);
}

// Read back what pi persisted (getAuth's result may be provider-shaped, e.g. extra headers).
const credential = readStoredCredential(provider, authPath);
if (credential?.type !== "oauth" || typeof credential.access !== "string" || !credential.access) {
  fail(`provider ${provider} lost its OAuth credential while refreshing`);
}
if (credential.expires < Date.now() + validForMs) {
  fail(`the ${provider} token expires in less than ${validForSecs} s`);
}
process.stdout.write(credential.access);
