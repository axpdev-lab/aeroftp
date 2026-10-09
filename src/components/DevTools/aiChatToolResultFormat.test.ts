// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { formatToolResult } from './aiChatUtils';

// The shapes below are the ones the backend tools return (ai_core::remote_tools
// and the community benchmark report), trimmed to the keys the formatter reads.

const benchmarkReport = {
    schema_version: 1,
    report_id: 'rid-1',
    level: 'quick',
    access: 'WebDAV',
    service: 'Custom',
    results: [
        { protocol: 'webdav', operation: 'upload', payload_size_bytes: 10485760, runs: 1, throughput_mbps: { p50: 2014.5 }, latency_ms: { p50: 41.6 }, errors: { transient: 0, fatal: 0 }, raw_runs: [] },
        { protocol: 'webdav', operation: 'download', payload_size_bytes: 10485760, runs: 1, throughput_mbps: { p50: 13906.2 }, latency_ms: { p50: 6.0 }, errors: { transient: 0, fatal: 0 }, raw_runs: [] },
    ],
    summary: { total_runs: 2, total_bytes_transferred: 20971520, total_duration_ms: 1200, errors: [] },
};

describe('formatToolResult on the tools AeroAgent gained from the MCP surface', () => {
    it('gives the model the benchmark numbers, not the search-hit printout', () => {
        const out = formatToolResult('aeroftp_benchmark', benchmarkReport);
        expect(out).not.toContain('undefined');
        expect(out).toContain('| upload | 10 MiB | 1 | 2014.5 |');
        expect(out).toContain('| download | 10 MiB | 1 | 13906.2 |');
        expect(out).toContain('Custom over WebDAV');
    });

    it('lists the notes and errors of a run', () => {
        const out = formatToolResult('aeroftp_benchmark', {
            ...benchmarkReport,
            summary: { ...benchmarkReport.summary, errors: ['benchmark cancelled before the end; the results cover the runs completed until then'] },
        });
        expect(out).toContain('benchmark cancelled before the end');
    });

    it('carries the report notes, so a bridge is not reported as the service', () => {
        const out = formatToolResult('aeroftp_benchmark', { ...benchmarkReport, notes: ['these figures measure the local Filen Desktop bridge and its cache, not the Filen servers'] });
        expect(out).toContain('**Note:** these figures measure the local Filen Desktop bridge');
    });

    it('prints the flat remote_tree by path', () => {
        const out = formatToolResult('remote_tree', {
            root: '/data', count: 2, truncated: false,
            entries: [{ path: '/data/a', depth: 1, is_dir: true }, { path: '/data/a/b.txt', depth: 2, is_dir: false, size: 12 }],
        });
        expect(out).not.toContain('undefined');
        expect(out).toContain('/data/a/b.txt (12 bytes)');
    });

    it('does not read a trash listing as a file listing', () => {
        const out = formatToolResult('remote_trash', {
            prefix: '', count: 1,
            entries: [{ key: 'k1', display_key: 'docs/a.txt', version_id: 'v1', is_delete_marker: true, is_latest: true, size: 0 }],
        });
        expect(out).not.toContain('undefined');
        expect(out).toContain('docs/a.txt');
    });

    it('states a truncated remote_head with the size it reports', () => {
        const out = formatToolResult('remote_head', { content: 'line 1\nline 2', truncated: true, total_size: 4096, lines_returned: 2, total_lines: 200 });
        expect(out).not.toContain('undefined');
        expect(out).toContain('4096 bytes total');
    });

    it('does not print a results array without names as search hits, for any tool', () => {
        const out = formatToolResult('some_plugin_tool', { results: [{ score: 0.9 }, { score: 0.4 }] });
        expect(out).not.toContain('undefined');
        expect(out).toContain('"score": 0.9');
    });

    it('still prints local_search hits as before', () => {
        const out = formatToolResult('local_search', { results: [{ name: 'a.txt', is_dir: false, size: 3 }], truncated: false });
        expect(out).toContain('  a.txt (3 bytes)');
    });
});
