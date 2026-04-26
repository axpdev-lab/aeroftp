import { invoke } from '@tauri-apps/api/core';
import { ServerProfile } from '../types';
import { resolveS3Endpoint } from '../providers/registry';
import {
    SPEEDTEST_SUPPORTED_PROTOCOLS,
    SpeedTestProviderConnectionParams,
    SpeedTestResult,
    SpeedTestRunOutcome,
} from '../components/SpeedTestDialog.types';

export const SPEEDTEST_SIZES = [
    { bytes: 1024 * 1024, labelKey: 'speedTest.size1MB', expert: false },
    { bytes: 10 * 1024 * 1024, labelKey: 'speedTest.size10MB', expert: false },
    { bytes: 100 * 1024 * 1024, labelKey: 'speedTest.size100MB', expert: false },
] as const;

export const ONE_MB = 1024 * 1024;
export const ONE_GB = 1024 * ONE_MB;
export const EXPERT_THRESHOLD = 100 * ONE_MB;
export const EXPERT_MAX = ONE_GB;

export function supportsSpeedTest(server: ServerProfile): boolean {
    return (SPEEDTEST_SUPPORTED_PROTOCOLS as readonly string[]).includes(server.protocol || 'ftp');
}

export function formatBytes(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
    if (bytes >= 1024 * 1024) return `${Math.round(bytes / (1024 * 1024))} MB`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${bytes} B`;
}

export function formatDuration(ms: number): string {
    if (ms < 1000) return `${Math.round(ms)} ms`;
    return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
}

export function formatMbps(mbps: number): string {
    if (!Number.isFinite(mbps)) return '0.00';
    if (mbps >= 1000) return mbps.toFixed(0);
    if (mbps >= 100) return mbps.toFixed(1);
    return mbps.toFixed(2);
}

export function formatMBps(bytesPerSec: number): string {
    return `${(bytesPerSec / (1024 * 1024)).toFixed(2)} MB/s`;
}

async function getCredentialWithRetry(account: string, maxRetries = 3): Promise<string> {
    for (let attempt = 0; attempt < maxRetries; attempt++) {
        try {
            return await invoke<string>('get_credential', { account });
        } catch (err) {
            if (String(err).includes('STORE_NOT_READY') && attempt < maxRetries - 1) {
                await new Promise(resolve => setTimeout(resolve, 200 * (attempt + 1)));
                continue;
            }
            throw err;
        }
    }
    return '';
}

export async function buildSpeedTestConnection(server: ServerProfile): Promise<SpeedTestProviderConnectionParams> {
    const protocol = server.protocol || 'ftp';
    if (!supportsSpeedTest(server)) {
        throw new Error(`Speed Test is not available for ${protocol}`);
    }

    let password = '';
    try {
        password = await getCredentialWithRetry(`server_${server.id}`);
    } catch {
        password = '';
    }

    const options = server.options || {};
    const region = options.region || (server.providerId === 'filelu-s3' ? 'global' : 'us-east-1');
    const endpoint = options.endpoint
        || resolveS3Endpoint(server.providerId, region as string, options.accountId ? { accountId: options.accountId } : undefined)
        || (protocol === 's3' && server.host && !server.host.includes('amazonaws.com') ? server.host : null);

    return {
        protocol,
        server: server.host,
        port: server.port ?? null,
        username: server.username || '',
        password,
        initial_path: server.initialPath || null,
        bucket: options.bucket || null,
        region,
        endpoint,
        path_style: options.pathStyle ?? null,
        storage_class: options.storage_class || null,
        sse_mode: options.sse_mode || null,
        sse_kms_key_id: options.sse_kms_key_id || null,
        private_key_path: options.private_key_path || null,
        key_passphrase: options.key_passphrase || null,
        timeout: options.timeout || 30,
        tls_mode: options.tlsMode || (protocol === 'ftps' ? 'implicit' : protocol === 'ftp' ? 'explicit' : null),
        verify_cert: options.verifyCert !== undefined ? options.verifyCert : true,
    };
}

// ---------------------------------------------------------------------------
// Compare scoring + ranking (mirror of Rust compute_score)
// ---------------------------------------------------------------------------

/**
 * Compute the normalized 0-1 score for a single result.
 * Tri-state integrity (audit P1-5): skipped contributes 0.5 (neutral),
 * never the same 1.0 as a verified SHA-256 run.
 */
export function computeScore(
    downloadMbps: number,
    uploadMbps: number,
    maxDownload: number,
    maxUpload: number,
    integrityChecked: boolean,
    integrityVerified: boolean,
    cleanupOk: boolean,
): number {
    const nd = maxDownload > 0 ? Math.min(1, Math.max(0, downloadMbps / maxDownload)) : 0;
    const nu = maxUpload > 0 ? Math.min(1, Math.max(0, uploadMbps / maxUpload)) : 0;
    const ni = !integrityChecked ? 0.5 : integrityVerified ? 1 : 0;
    const nc = cleanupOk ? 1 : 0;
    return 0.45 * nd + 0.35 * nu + 0.10 * ni + 0.10 * nc;
}

export interface RankedOutcome extends SpeedTestRunOutcome {
    rank: number;
    score: number;
}

export function rankOutcomes(outcomes: SpeedTestRunOutcome[]): RankedOutcome[] {
    const successful = outcomes.filter(o => o.result);
    const maxDownload = successful.reduce((m, o) => Math.max(m, o.result!.download_mbps), 0);
    const maxUpload = successful.reduce((m, o) => Math.max(m, o.result!.upload_mbps), 0);

    const scored = outcomes.map(o => {
        if (!o.result) return { ...o, rank: 0, score: 0 };
        const r = o.result;
        return {
            ...o,
            rank: 0,
            score: computeScore(
                r.download_mbps,
                r.upload_mbps,
                maxDownload,
                maxUpload,
                r.integrity_checked,
                r.integrity_verified,
                r.temp_file_cleaned,
            ),
        };
    });

    // Sort: successful by score desc, errored last
    scored.sort((a, b) => {
        if (a.result && !b.result) return -1;
        if (!a.result && b.result) return 1;
        return b.score - a.score;
    });

    let rank = 0;
    return scored.map(o => {
        if (o.result) rank += 1;
        return { ...o, rank: o.result ? rank : 0 };
    });
}

// ---------------------------------------------------------------------------
// Methodology report
// ---------------------------------------------------------------------------

export const REPORT_SCHEMA = 'aeroftp.speedtest.v1';

export interface MethodologyContext {
    clientVersion?: string;
    sizeBytes: number;
    remoteDir?: string;
    startedAt?: number;
    finishedAt?: number;
}

function nowIso(): string {
    return new Date().toISOString();
}

function integrityLabel(r: SpeedTestResult): string {
    if (!r.integrity_checked) return 'skipped';
    return r.integrity_verified ? 'verified' : 'CORRUPTED';
}

function singleResultReportLine(o: SpeedTestRunOutcome): string {
    if (!o.result) {
        return `  - ${o.server_name || '(unknown)'}: ERROR ${o.error || 'unknown error'}`;
    }
    const r = o.result;
    const cleanup = r.temp_file_cleaned ? 'removed' : `manual: ${r.remote_path}`;
    const ttfb = r.download_ttfb_ms != null ? ` (TTFB ${r.download_ttfb_ms} ms)` : '';
    const sha = r.integrity_checked && r.upload_sha256 && r.download_sha256
        ? `      sha256(up)=${r.upload_sha256.slice(0, 16)}…  sha256(dl)=${r.download_sha256.slice(0, 16)}…`
        : `      sha256(up)=${r.upload_sha256 ? r.upload_sha256.slice(0, 16) + '…' : '(skipped)'}`;
    return [
        `  - ${o.server_name || r.protocol} (${r.protocol})`,
        `      upload   ${formatMbps(r.upload_mbps)} Mbps  (${formatMBps(r.upload_bytes_per_sec)})  in ${formatDuration(r.upload_duration_ms)}`,
        `      download ${formatMbps(r.download_mbps)} Mbps  (${formatMBps(r.download_bytes_per_sec)})  in ${formatDuration(r.download_duration_ms)}${ttfb}`,
        `      integrity ${integrityLabel(r)}    cleanup ${cleanup}`,
        sha,
    ].join('\n');
}

export function buildMethodologyReport(
    outcomes: SpeedTestRunOutcome[],
    ctx: MethodologyContext,
): string {
    const ranked = rankOutcomes(outcomes);
    const lines: string[] = [];
    lines.push('AeroFTP Speed Test');
    lines.push(`Date: ${nowIso()}`);
    if (ctx.clientVersion) lines.push(`Client: ${ctx.clientVersion}`);
    lines.push(`Test size: ${formatBytes(ctx.sizeBytes)}`);
    if (ctx.remoteDir) lines.push(`Remote directory: ${ctx.remoteDir}`);
    lines.push('Method:');
    lines.push('  Payload: high-entropy random bytes generated locally (rand 0.8 thread_rng), uncompressible.');
    lines.push('  Pipeline: local SHA-256 -> upload (streaming) -> download (streaming to local temp)');
    lines.push('            -> SHA-256 of downloaded copy -> integrity compare -> remote cleanup.');
    lines.push('  Streaming: provider on_progress callbacks; no full-file RAM buffer for the test size.');
    lines.push('  TTFB: time from download issue to first transferred byte.');
    lines.push('Results:');
    if (ranked.length === 0) {
        lines.push('  (no results)');
    } else {
        ranked.forEach((o, i) => {
            const prefix = o.result ? `[#${i + 1}]` : '[ERR]';
            lines.push(`${prefix}`);
            lines.push(singleResultReportLine(o));
        });
    }
    lines.push('Notes:');
    lines.push('  Mbps = decimal megabits per second.');
    lines.push('  MB/s = binary mebibytes per second.');
    lines.push('  Provider request and egress costs may apply, especially for object storage.');
    lines.push('  Cancel is best-effort and may wait for the current chunk to complete.');
    return lines.join('\n');
}

export interface JsonReport {
    schema: string;
    client_version?: string;
    size_bytes: number;
    remote_dir?: string;
    started_at?: number;
    finished_at?: number;
    generated_at: string;
    results: Array<{
        rank: number;
        server_name: string | null;
        protocol: string | null;
        score: number;
        result: SpeedTestResult | null;
        error: string | null;
    }>;
}

export function buildJsonReport(
    outcomes: SpeedTestRunOutcome[],
    ctx: MethodologyContext,
): JsonReport {
    const ranked = rankOutcomes(outcomes);
    return {
        schema: REPORT_SCHEMA,
        client_version: ctx.clientVersion,
        size_bytes: ctx.sizeBytes,
        remote_dir: ctx.remoteDir,
        started_at: ctx.startedAt,
        finished_at: ctx.finishedAt,
        generated_at: nowIso(),
        results: ranked.map(o => ({
            rank: o.rank,
            server_name: o.server_name,
            protocol: o.result?.protocol || null,
            score: o.score,
            result: o.result,
            error: o.error,
        })),
    };
}

function triggerDownload(blob: Blob, filename: string) {
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    setTimeout(() => URL.revokeObjectURL(url), 1500);
}

export function downloadJsonReport(json: JsonReport, filename = 'aeroftp-speedtest-report.json') {
    triggerDownload(
        new Blob([JSON.stringify(json, null, 2)], { type: 'application/json' }),
        filename,
    );
}

/**
 * CSV-safe cell encoding mirroring the CLI helper. Always quotes the cell,
 * doubles internal quotes, and prefixes a single quote when the cell starts
 * with a spreadsheet-formula trigger character (= + - @ tab cr) — protects
 * against CSV-injection attacks in Excel/Numbers.
 */
function csvEscape(value: unknown): string {
    if (value == null) return '';
    let s = String(value);
    const trigger = /^[=+\-@\t\r]/.test(s);
    s = s.replace(/"/g, '""');
    return trigger ? `"'${s}"` : `"${s}"`;
}

/**
 * Escape pipe and newline characters so the value remains a single Markdown
 * table cell. Backslashes are also escaped to keep the output literal.
 */
function mdCellEscape(value: unknown): string {
    if (value == null) return '';
    return String(value)
        .replace(/\\/g, '\\\\')
        .replace(/\|/g, '\\|')
        .replace(/[\r\n]+/g, ' ');
}

export function buildCsvReport(outcomes: SpeedTestRunOutcome[]): string {
    const ranked = rankOutcomes(outcomes);
    const header = [
        'rank', 'server_name', 'protocol', 'size_bytes',
        'upload_mbps', 'download_mbps',
        'upload_bps', 'download_bps',
        'upload_ms', 'download_ms', 'download_ttfb_ms',
        'integrity', 'cleanup_ok', 'score',
        'upload_sha256', 'download_sha256', 'error',
    ].join(',');
    const rows = ranked.map(o => {
        const r = o.result;
        const integrity = !r ? '' : !r.integrity_checked ? 'skipped' : r.integrity_verified ? 'verified' : 'corrupted';
        return [
            o.rank,
            csvEscape(o.server_name),
            csvEscape(r?.protocol),
            r?.size_bytes ?? '',
            r ? formatMbps(r.upload_mbps) : '',
            r ? formatMbps(r.download_mbps) : '',
            r?.upload_bytes_per_sec.toFixed(2) ?? '',
            r?.download_bytes_per_sec.toFixed(2) ?? '',
            r?.upload_duration_ms ?? '',
            r?.download_duration_ms ?? '',
            r?.download_ttfb_ms ?? '',
            integrity,
            r?.temp_file_cleaned ? 1 : 0,
            (o.score * 100).toFixed(1),
            r?.upload_sha256 ?? '',
            r?.download_sha256 ?? '',
            csvEscape(o.error),
        ].join(',');
    });
    return [header, ...rows].join('\n');
}

export function downloadCsvReport(outcomes: SpeedTestRunOutcome[], filename = 'aeroftp-speedtest-report.csv') {
    triggerDownload(new Blob([buildCsvReport(outcomes)], { type: 'text/csv' }), filename);
}

export function buildMarkdownReport(
    outcomes: SpeedTestRunOutcome[],
    ctx: MethodologyContext,
): string {
    const ranked = rankOutcomes(outcomes);
    const lines: string[] = [];
    lines.push('# AeroFTP Speed Test');
    lines.push('');
    lines.push(`- **Date:** ${nowIso()}`);
    if (ctx.clientVersion) lines.push(`- **Client:** ${ctx.clientVersion}`);
    lines.push(`- **Test size:** ${formatBytes(ctx.sizeBytes)}`);
    if (ctx.remoteDir) lines.push(`- **Remote directory:** \`${ctx.remoteDir}\``);
    lines.push('');
    lines.push('## Method');
    lines.push('');
    lines.push('High-entropy random uncompressible payload (generated locally), streaming upload');
    lines.push('then streaming download into a local temp file, SHA-256 compare end-to-end, then remote cleanup.');
    lines.push('');
    lines.push('## Results');
    lines.push('');
    lines.push('| # | Server | Protocol | Down (Mbps) | Up (Mbps) | TTFB (ms) | Integrity | Cleanup | Score |');
    lines.push('|---:|---|---|---:|---:|---:|:---:|:---:|---:|');
    ranked.forEach(o => {
        if (!o.result) {
            lines.push(
                `| — | ${mdCellEscape(o.server_name || '?')} | — | — | — | — | — | — | error: ${mdCellEscape(o.error || 'unknown')} |`
            );
            return;
        }
        const r = o.result;
        const integ = !r.integrity_checked ? '—' : r.integrity_verified ? '✓' : '✗';
        lines.push(
            `| ${o.rank} | ${mdCellEscape(o.server_name || '?')} | ${mdCellEscape(r.protocol.toUpperCase())} | ${formatMbps(r.download_mbps)} | ${formatMbps(r.upload_mbps)} | ${r.download_ttfb_ms ?? '—'} | ${integ} | ${r.temp_file_cleaned ? '✓' : '✗'} | ${(o.score * 100).toFixed(0)} |`
        );
    });
    lines.push('');
    lines.push('## Notes');
    lines.push('');
    lines.push('- Mbps = decimal megabits/sec, MB/s = binary mebibytes/sec.');
    lines.push('- Score weighting: 0.45 download, 0.35 upload, 0.10 integrity, 0.10 cleanup.');
    lines.push('- Provider request and egress costs may apply, especially for object storage.');
    return lines.join('\n');
}

export function downloadMarkdownReport(
    outcomes: SpeedTestRunOutcome[],
    ctx: MethodologyContext,
    filename = 'aeroftp-speedtest-report.md',
) {
    triggerDownload(
        new Blob([buildMarkdownReport(outcomes, ctx)], { type: 'text/markdown' }),
        filename,
    );
}

export function singleResultAsOutcome(
    result: SpeedTestResult,
): SpeedTestRunOutcome {
    return {
        test_id: result.test_id,
        server_name: result.server_name,
        result,
        error: null,
    };
}

// ---------------------------------------------------------------------------
// Host hashing (privacy: store hash, not host)
// ---------------------------------------------------------------------------

export async function hashHost(host: string): Promise<string> {
    if (!host) return '';
    try {
        const buf = new TextEncoder().encode(host.toLowerCase());
        const digest = await crypto.subtle.digest('SHA-256', buf);
        const arr = Array.from(new Uint8Array(digest));
        return arr.map(b => b.toString(16).padStart(2, '0')).join('').slice(0, 32);
    } catch {
        return '';
    }
}
