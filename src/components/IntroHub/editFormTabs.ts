// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

interface EditFormTab {
    id: string;
    editingProfile?: { id: string };
    /** The form left edit mode (Cancel Editing, a protocol switch); the tab
     *  keeps `editingProfile` for its label but no longer edits that profile. */
    editSessionEnded?: boolean;
}

/**
 * The open form tab still editing `profileId`, if any. Edit switches to it so
 * an unsaved draft survives; a tab whose session ended is not an editor of the
 * profile any more, and Edit opens a fresh tab from the current profile.
 */
export function findOpenEditTab<T extends EditFormTab>(tabs: readonly T[], profileId: string): T | undefined {
    return tabs.find((ft) => ft.editingProfile?.id === profileId && !ft.editSessionEnded);
}

/** Record that the form in `tabId` left edit mode. */
export function markEditSessionEnded<T extends EditFormTab>(tabs: readonly T[], tabId: string): T[] {
    return tabs.map((ft) => (ft.id === tabId && !ft.editSessionEnded ? { ...ft, editSessionEnded: true } : ft));
}
