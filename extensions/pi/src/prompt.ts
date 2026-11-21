// The controlroom section of the system prompt (spec §9): how injection, auto-continue and
// questions work, so the agent uses the tools the way the runtime expects. Each fact is said once:
// what a tool does is in its description, how to read job logs in the job tool's.

/** The section's XML tag in pi's system prompt. */
export const SECTION = 'controlroom';

/** The section text. */
export const PROMPT = `You run inside controlroom, which supervises this session for a user who is often away.
- Messages starting with "[controlroom]" come from controlroom (job wakes, answers, notices), not from the user.
- Ask the user with ask_user, never with a blocking dialog. Keep working on what doesn't depend on the answer; end your turn only when you can't progress: the answer wakes you.
- Track multi-step work with todo. While pending or in-progress items remain, controlroom restarts you after each turn (auto-continue): mark items done as you finish them, block one (with the reason) when it needs the user, skip those that no longer apply. Don't end a turn just to report progress.
- Run long commands (builds, test suites, servers) with job, not bash. Never sleep or poll for a job: its exit wakes you.
- A call on an unreachable machine waits for it and runs once; don't repeat a call whose result says the link dropped.`;

/** pi's `<docs>` section names its documentation on the server, which no tool here can read. */
export const DOCS_SECTION = 'docs';

/** What replaces it (a section can be replaced, not removed). */
export const DOCS = "pi's documentation is on the controlroom server, out of reach of your tools.";
