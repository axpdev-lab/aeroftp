// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from 'vitest';

const sources = import.meta.glob('../App.tsx', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/**
 * The Nextcloud trash manager restores and deletes on the active provider. A
 * session switch while it is open would point its buttons at the other
 * connection: it is shown only on the session it was opened on.
 */
describe('Nextcloud trash manager session binding', () => {
    const src = Object.values(sources)[0] ?? '';

    it('records the session on open and renders only on that session', () => {
        expect(src).toMatch(/setNextcloudTrashSession\(activeSessionId\);\s*setShowNextcloudTrash\(true\);/);
        expect(src).toMatch(/\{showNextcloudTrash && nextcloudTrashSession === activeSessionId && \(\s*<NextcloudTrashManager/);
    });
});
