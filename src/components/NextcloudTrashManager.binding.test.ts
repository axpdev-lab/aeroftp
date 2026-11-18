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
 * connection: it is shown only on the session it was opened on, and its
 * commands carry the connection generation it was opened on, which the
 * backend re-checks under the provider lock (a session switched back to that
 * is still reconnecting, or another sessionless connection, has another).
 */
describe('Nextcloud trash manager session binding', () => {
    const src = Object.values(sources)[0] ?? '';

    it('records the session and the connection generation on open and renders only on that session', () => {
        // Read only while the backend serves the live tab; the target clicked
        // from is taken first and must still be the live one, served, after
        // the call (queuedTargetProblem): a sessionless connection that took
        // the slot meanwhile has the same null session id, not the same target.
        expect(src).toMatch(/if \(!backendServesLiveTab\(\)\) \{ notServing\(\); return; \}[^\n]*\n(?:\s*\/\/[^\n]*\n)*\s*const target = snapshotQueuedTarget\(\);\s*const sessionId = activeSessionId;\s*const seq = \+\+nextcloudTrashOpenSeqRef\.current;/);
        expect(src).toMatch(/if \(seq !== nextcloudTrashOpenSeqRef\.current\) return;\s*(?:\/\/[^\n]*\n\s*)*if \(queuedTargetProblem\(target\)\) \{ notServing\(\); return; \}\s*setNextcloudTrashOwner/);
        expect(src).toMatch(/const queuedTargetProblem = useLatestCallback\(\(queuedFor: QueuedTarget\): string \| null => \{[\s\S]{0,900}if \(!backendServesLiveTab\(\)\) return[\s\S]{0,200}isSameConnection\(liveConnectionParams, queuedFor\.params\)/);
        expect(src).toMatch(/generation = await invoke<number>\('provider_connection_generation'\);[\s\S]{0,900}setNextcloudTrashOwner\(\{ sessionId, generation \}\);\s*setShowNextcloudTrash\(true\);/);
        expect(src).toMatch(/\{showNextcloudTrash && nextcloudTrashOwner && nextcloudTrashOwner\.sessionId === activeSessionId && \(\s*<NextcloudTrashManager\s+key=\{nextcloudTrashOwner\.generation\}\s+generation=\{nextcloudTrashOwner\.generation\}/);
    });
});
