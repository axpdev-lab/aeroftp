// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from 'vitest';

const sources = import.meta.glob('../App.tsx', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/**
 * The File Versions dialog downloads, restores and purges on the active
 * provider. Like the Nextcloud trash manager it records the session and the
 * connection generation it was opened on, renders only on that session, and
 * every command carries that generation, which the backend re-checks under
 * the provider lock (a purge cannot be undone).
 */
describe('File Versions dialog session binding', () => {
    const src = Object.values(sources)[0] ?? '';

    it('records the session and the connection generation on open and renders only on that session', () => {
        expect(src).toMatch(/if \(!backendServesLiveTab\(\)\) \{ notServing\(\); return; \}\s*const target = snapshotQueuedTarget\(\);\s*const sessionId = activeSessionId;\s*const seq = \+\+versionsOpenSeqRef\.current;/);
        expect(src).toMatch(/generation = await invoke<number>\('provider_connection_generation'\);[\s\S]{0,300}if \(seq !== versionsOpenSeqRef\.current\) return;\s*if \(queuedTargetProblem\(target\)\) \{ notServing\(\); return; \}\s*setVersionsDialog\(\{ path: file\.path, name: file\.name, sessionId, generation, guiTarget \}\);/);
        expect(src).toMatch(/\{versionsDialog && versionsDialog\.sessionId === activeSessionId && \(\s*<FileVersionsDialog\s+key=\{`\$\{versionsDialog\.generation\}:\$\{versionsDialog\.guiTarget\}`\}\s+generation=\{versionsDialog\.generation\}\s+guiTarget=\{versionsDialog\.guiTarget\}/);
        // The remote target is captured with the generation: an overlay applied, cleared or locked since refuses every command.
        expect(src).toMatch(/generation = await invoke<number>\('provider_connection_generation'\);\s*guiTarget = await captureGuiTarget\(\(\) => \{\}\);/);
        expect(src).not.toMatch(/setVersionsDialog\(\{ path: file\.path, name: file\.name \}\)/);
    });
});
