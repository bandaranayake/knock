import { expect, test, vi } from "vitest";
import {
	asDiagnostics,
	copyDiagnostics,
	type Diagnostics,
	NO_CLIPBOARD,
	NO_DETAILS,
	versionLabel,
	writeToClipboard,
} from "./diagnostics";

// A reply the core could really send: several lines, blank lines and a trailing newline.
const TEXT = [
	"Knock 0.3.0-beta.1",
	"Windows 11 Pro 24H2 (build 26200.1742)",
	"",
	"Checks",
	"  Knock is connected to Claude Code: pass",
	"",
	"Displays 2",
	"",
].join("\n");
const INFO: Diagnostics = { version: "0.3.0-beta.1", text: TEXT };
const reply = () => Promise.resolve({ ...INFO });

test("the version shows when the command answers", () => {
	expect(asDiagnostics({ version: "0.3.0-beta.1", text: TEXT })).toEqual(INFO);
	expect(versionLabel(INFO)).toBe("Knock 0.3.0-beta.1");
});

test("no version line at all when the command fails or answers oddly", () => {
	// Nothing yet, or a failure: the screen shows no line, not an error or a placeholder.
	expect(versionLabel(null)).toBeNull();
	for (const bad of [
		undefined,
		null,
		"0.3.0",
		42,
		{},
		{ version: "0.3.0" },
		{ text: TEXT },
		{ version: "", text: TEXT },
		{ version: "   ", text: TEXT },
		{ version: 3, text: TEXT },
		{ version: "0.3.0", text: "" },
		{ version: "0.3.0", text: 7 },
	])
		expect(versionLabel(asDiagnostics(bad))).toBeNull();
});

const spy = () => vi.fn(async (_text: string) => {});

test("the button copies exactly the text the command returned, unchanged", async () => {
	const write = spy();
	expect(await copyDiagnostics(reply, write)).toEqual({ kind: "copied" });
	expect(write).toHaveBeenCalledTimes(1);
	// Character for character, including the blank lines and the trailing newline.
	const copied = write.mock.calls[0]?.[0];
	expect(copied).toBe(TEXT);
	expect(copied?.endsWith("\n")).toBe(true);
});

test("the diagnostics are read afresh at every copy", async () => {
	const load = vi.fn(reply);
	const write = spy();
	await copyDiagnostics(load, write);
	await copyDiagnostics(load, write);
	expect(load).toHaveBeenCalledTimes(2);
});

test("a refused clipboard says so and keeps the text to show instead", async () => {
	const write = vi.fn(() => Promise.reject(new Error("NotAllowedError")));
	expect(await copyDiagnostics(reply, write)).toEqual({
		kind: "failed",
		message: NO_CLIPBOARD,
		text: TEXT,
	});
});

test("no clipboard in the browser at all is a failure, never a silent one", async () => {
	expect(await copyDiagnostics(reply, writeToClipboard)).toEqual({
		kind: "failed",
		message: NO_CLIPBOARD,
		text: TEXT,
	});
});

test("diagnostics the core can't give: a plain message and nothing to copy", async () => {
	const write = spy();
	for (const load of [
		() => Promise.reject("Knock couldn't gather the details just now."),
		() => Promise.resolve(undefined),
		() => Promise.resolve({ version: "0.3.0" }),
	])
		expect(await copyDiagnostics(load, write)).toEqual({
			kind: "failed",
			message: NO_DETAILS,
			text: null,
		});
	expect(write).not.toHaveBeenCalled();
});
