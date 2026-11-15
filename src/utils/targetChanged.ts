// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/** How a changed-target refusal of the backend starts (`TARGET_CHANGED_MARKER` in Rust). */
export const TARGET_CHANGED_MARKER = 'CONNECTION_CHANGED:';

/** Words an operation or the window puts in front of an error: `upload failed: `, `Error: `. */
const WRAPPERS_ONLY = /^\s*(?:[A-Za-z0-9 ]+: )*$/;

/**
 * Whether a rendered error is the backend's refusal for a changed target.
 * Twin of `is_target_changed_text` (src-tauri/src/providers/types.rs); both
 * run src/shared/target-changed-cases.json.
 *
 * The position of the marker decides, and nothing else: it counts where the
 * message starts, or behind wrappers only (`upload failed:
 * CONNECTION_CHANGED: ...`). A marker later in the text is part of something
 * the error names, such as a remote path, and is not the refusal. No path is
 * taken out of the text first: a file can be named after the marker or after
 * the first words of the refusal, and removing it removed the refusal's own
 * start. A marker that stands where a refusal starts is read as the refusal
 * whatever the file is called.
 */
export const isTargetChangedRefusal = (error: unknown): boolean => {
    const text = String(error);
    const at = text.indexOf(TARGET_CHANGED_MARKER);
    return at >= 0 && WRAPPERS_ONLY.test(text.slice(0, at));
};
