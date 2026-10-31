/**
 * Test extension loaded (with `-e`) while recording pi RPC fixtures.
 *
 * - `/fixture-dialogs`: select, confirm, input and editor dialogs, then
 *   notify, setStatus, setWidget, setTitle and appendEntry.
 * - `/fixture-timeout`: a select dialog with a short timeout that the
 *   recorder never answers.
 * - `fixture_nested` tool: runs `read` through `ctx.executeTool`, so the
 *   nested call carries `parentToolCallId`.
 * - bash commands containing `fixture-guard` ask for permission with a
 *   select dialog before they run.
 */

import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

const nestedTool = defineTool({
	name: "fixture_nested",
	label: "Fixture nested",
	description: "Reads a file through a nested tool call.",
	parameters: Type.Object({
		path: Type.String({ description: "File to read" }),
	}),
	async execute(_toolCallId, params, signal, onUpdate, ctx) {
		const outcome = await ctx.executeTool("read", { path: params.path }, { signal, onUpdate });
		const text = outcome.result.content
			.filter((part) => part.type === "text")
			.map((part) => part.text)
			.join("");
		return {
			content: [{ type: "text", text: `nested read: ${text}` }],
			details: { nested: true },
		};
	},
});

export default function (pi: ExtensionAPI) {
	pi.registerTool(nestedTool);

	pi.on("session_start", async (_event, ctx) => {
		ctx.ui.setStatus("fixture", "ready");
	});

	pi.on("tool_call", async (event, ctx) => {
		if (event.toolName !== "bash") return undefined;
		const command = String(event.input.command ?? "");
		if (!command.includes("fixture-guard")) return undefined;
		const choice = await ctx.ui.select(`Allow: ${command}?`, ["Allow", "Block"]);
		if (choice !== "Allow") return { block: true, reason: "Blocked by user" };
		return undefined;
	});

	pi.registerCommand("fixture-dialogs", {
		description: "Exercise every RPC extension UI method",
		handler: async (_args, ctx) => {
			const choice = await ctx.ui.select("Pick a color", ["red", "green", "blue"]);
			const confirmed = await ctx.ui.confirm("Proceed?", "This is a fixture dialog.");
			const name = await ctx.ui.input("Your name", "type a name");
			const text = await ctx.ui.editor("Edit notes", "line 1\nline 2");
			ctx.ui.notify(
				`select=${choice ?? "<none>"} confirm=${confirmed} input=${name ?? "<none>"} editor=${text ?? "<none>"}`,
				"info",
			);
			ctx.ui.setStatus("fixture", "dialogs done");
			ctx.ui.setWidget("fixture", ["fixture widget", `color: ${choice ?? "none"}`]);
			ctx.ui.setTitle("fixture title");
			pi.appendEntry("fixture.answers", { choice, confirmed, name });
		},
	});

	pi.registerCommand("fixture-timeout", {
		description: "A select dialog that times out",
		handler: async (_args, ctx) => {
			const choice = await ctx.ui.select("Quick!", ["yes", "no"], { timeout: 300 });
			ctx.ui.notify(`timed out with ${choice ?? "<none>"}`, "warning");
		},
	});
}
