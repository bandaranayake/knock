// Copy diagnostics and the version line in Settings (SPEC.md §8.5 `get_diagnostics()`,
// §11 rule 8, ADR 0038). The core makes the text; the UI never builds, parses, filters or
// adds to it. Nothing is ever sent anywhere: the button only puts the text on the clipboard.

/** What `get_diagnostics()` answers (§8.5). */
export interface Diagnostics {
	version: string;
	text: string;
}

/**
 * The core's reply, or null when it is missing or the wrong shape. Null means Settings
 * shows no version line at all, rather than an error or a placeholder.
 */
export function asDiagnostics(reply: unknown): Diagnostics | null {
	if (!reply || typeof reply !== "object") return null;
	const { version, text } = reply as Partial<Diagnostics>;
	if (typeof version !== "string" || version.trim() === "") return null;
	if (typeof text !== "string" || text === "") return null;
	return { version, text };
}

/** The line a user reads, in plain words: "Knock 0.3.0-beta.1". Null when unknown. */
export function versionLabel(info: Diagnostics | null): string | null {
	return info ? `Knock ${info.version}` : null;
}

export type CopyState =
	| { kind: "idle" }
	| { kind: "copied" }
	/** `text` is the core's text when there is some to fall back on, else null. */
	| { kind: "failed"; message: string; text: string | null };

export const COPIED = "Copied. Paste it into your bug report.";
export const NO_DETAILS =
	"Knock couldn't gather the details just now. Please try again.";
export const NO_CLIPBOARD =
	"Windows didn't let Knock use the clipboard. The details are below: select them and press Ctrl+C to copy them yourself.";

export type Write = (text: string) => Promise<void>;

/** The browser clipboard (§13.2: no new dependency). Throws when there is none. */
export async function writeToClipboard(text: string): Promise<void> {
	const clipboard = globalThis.navigator?.clipboard;
	if (typeof clipboard?.writeText !== "function")
		throw new Error("no clipboard");
	await clipboard.writeText(text);
}

/**
 * Reads the diagnostics afresh (the check results and counts change) and copies the
 * core's text exactly as it came. Never silent: every failure comes back as a message.
 */
export async function copyDiagnostics(
	load: () => Promise<unknown>,
	write: Write = writeToClipboard,
): Promise<CopyState> {
	let info: Diagnostics | null = null;
	try {
		info = asDiagnostics(await load());
	} catch {
		info = null;
	}
	if (!info) return { kind: "failed", message: NO_DETAILS, text: null };
	try {
		await write(info.text);
	} catch {
		return { kind: "failed", message: NO_CLIPBOARD, text: info.text };
	}
	return { kind: "copied" };
}
