import { describe, expect, it } from 'vitest';
import { formatToolResult } from './aiChatUtils';

/** A `sync_preview` result whose comparable files all match. */
function preview(extra: Record<string, unknown>) {
    return {
        local_path: '/home/u',
        remote_path: '/backup',
        local_files: 2,
        remote_files: 2,
        identical: 2,
        only_local: [],
        only_remote: [],
        size_different: [],
        protected_skipped: 0,
        not_compared: [],
        synced: false,
        ...extra,
    };
}

describe('formatToolResult for sync_preview', () => {
    it('names the entries it could not compare and says the comparison is incomplete', () => {
        const out = formatToolResult('sync_preview', preview({ not_compared: ['/etc/passwd', '../up'] }));
        expect(out).toContain('**Not compared** (2)');
        expect(out).toContain('/etc/passwd');
        expect(out).toContain('../up');
        expect(out).toContain('The comparison is incomplete');
        expect(out).not.toContain('in sync');
    });

    it('caps a long list of names with the count of the rest', () => {
        const names = Array.from({ length: 25 }, (_, i) => `/abs/n${i}`);
        const out = formatToolResult('sync_preview', preview({ not_compared: names }));
        expect(out).toContain('/abs/n19');
        expect(out).not.toContain('/abs/n20');
        expect(out).toContain('... +5 more');
    });

    it('reports the protected entries left out', () => {
        const out = formatToolResult('sync_preview', preview({ protected_skipped: 3 }));
        expect(out).toContain('**Protected entries left out:** 3');
        expect(out).toContain('The comparison is incomplete');
    });

    it('still reads as in sync when nothing was left out', () => {
        const out = formatToolResult('sync_preview', preview({ synced: true }));
        expect(out).toContain('Folders are in sync');
        expect(out).not.toContain('incomplete');
    });
});
