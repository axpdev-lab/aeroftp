// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** DOM harness only. This is not a controller intent or an authorization boundary.
 * Match data, never interpolate profile ids or filenames into CSS selectors.
 * Refuse ambiguous addresses instead of silently acting on the first element.
 */
export function guiTarget(
    root: ParentNode,
    testId: string,
    qualifiers: Partial<Record<'data-panel' | 'data-profile-id' | 'data-session-id' |
        'data-transfer-id' | 'data-file-name' | 'data-direction' | 'data-action' | 'data-tab', string>> = {},
): HTMLElement {
    const targets = Array.from(root.querySelectorAll<HTMLElement>('[data-testid]'))
        .filter(element => element.dataset.testid === testId &&
            Object.entries(qualifiers).every(([key, value]) => element.getAttribute(key) === value) &&
            !element.closest('[hidden], [aria-hidden="true"]'));
    if (targets.length !== 1) throw new Error(`GUI target count: ${targets.length}`);
    const target = targets[0];
    if (target.closest('[data-agent="deny"]')) throw new Error('GUI target denied');
    if (target.matches(':disabled, [aria-disabled="true"]')) throw new Error('GUI target disabled');
    return target;
}
