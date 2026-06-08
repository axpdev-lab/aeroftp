// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * VaultReceipt: behind-the-scenes technical receipt for an AeroVault
 * create/add operation. Renders the shared `VaultReport` (Rust
 * vault_telemetry, global v1/v2/v3) as a live "mini terminal" replay of
 * the step log plus a metrics panel, with an exportable artifact.
 *
 * Wrapper-stack pipeline model: design contribution by Ehud Kirsh
 * (AeroFTP issue #162). The attribution travels inside the report and is
 * shown in the footer and included in every export.
 */

import { useEffect, useRef, useState } from 'react';
import { X, Download, Terminal } from 'lucide-react';
import type { VaultReport } from './useVaultState';

interface VaultReceiptProps {
    report: VaultReport;
    t: (key: string, params?: Record<string, string>) => string;
    onClose: () => void;
}

function fmtBytes(n: number): string {
    if (n <= 0) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
    return `${(n / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${u[i]}`;
}

function buildReceiptText(r: VaultReport): string {
    const lines: string[] = [];
    lines.push(`AeroVault technical receipt | op=${r.operation} format=v${r.vault_format}${r.profile ? ` profile=${r.profile}` : ''}`);
    lines.push(`pipeline: ${r.algorithms.join(' -> ')}`);
    if (r.cdc_min != null && r.cdc_avg != null && r.cdc_max != null) {
        lines.push(`cdc bounds: min=${r.cdc_min} avg=${r.cdc_avg} max=${r.cdc_max}`);
    }
    lines.push(`files=${r.files} (packed=${r.packed_files}, packs=${r.packs}) chunks: logical=${r.logical_chunks} new=${r.new_physical_chunks} dedup=${r.dedup_hits}`);
    lines.push(`bytes: plaintext=${r.plaintext_bytes} compressed=${r.compressed_bytes} encrypted=${r.encrypted_bytes} ratio=${r.compression_ratio_pct.toFixed(1)}%`);
    lines.push(`elapsed: ${r.ms_total} ms`);
    if (r.ecc_shards_generated != null || r.ecc_bytes_protected != null || r.ecc_overhead_pct != null) {
        lines.push(`ecc: shards=${r.ecc_shards_generated ?? '-'} protected=${r.ecc_bytes_protected ?? '-'} overhead=${r.ecc_overhead_pct != null ? r.ecc_overhead_pct.toFixed(1)+'%' : '-'}`);
    }
    lines.push('steps:');
    r.steps.forEach(s => lines.push(`  ${s}`));
    lines.push('');
    lines.push(r.attribution);
    return lines.join('\n');
}

function download(name: string, content: string, mime: string): void {
    const blob = new Blob([content], { type: mime });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function VaultReceipt({ report, t, onClose }: VaultReceiptProps): React.ReactElement {
    const [revealed, setRevealed] = useState(0);
    const termRef = useRef<HTMLDivElement>(null);

    useEffect(() => {
        setRevealed(0);
        if (!report.steps.length) return;
        const id = window.setInterval(() => {
            setRevealed(prev => {
                if (prev >= report.steps.length) {
                    window.clearInterval(id);
                    return prev;
                }
                return prev + 1;
            });
        }, 220);
        return () => window.clearInterval(id);
    }, [report]);

    useEffect(() => {
        if (termRef.current) termRef.current.scrollTop = termRef.current.scrollHeight;
    }, [revealed]);

    const ts = new Date().toISOString().replace(/[:.]/g, '-');
    const base = `aerovault-receipt-${report.operation}-${ts}`;

    const Metric = ({ label, value }: { label: string; value: string }) => (
        <div className="flex flex-col">
            <span className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">{label}</span>
            <span className="text-sm font-mono text-gray-900 dark:text-gray-100">{value}</span>
        </div>
    );

    return (
        <div className="absolute inset-0 z-20 flex items-center justify-center bg-black/50 p-4 animate-scale-in">
            <div className="w-full max-w-2xl max-h-full overflow-auto rounded-lg bg-white dark:bg-gray-800 shadow-xl border border-gray-200 dark:border-gray-700">
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <Terminal size={16} className="text-emerald-500" />
                        <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
                            {t('vault.receipt.title')}
                        </h3>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
                        aria-label={t('common.close')}
                    >
                        <X size={16} />
                    </button>
                </div>

                <div className="p-4 space-y-4">
                    <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
                        <Metric label={t('vault.receipt.operation')} value={`${report.operation} (v${report.vault_format})`} />
                        <Metric label={t('vault.receipt.profile')} value={report.profile ?? '-'} />
                        <Metric label={t('vault.receipt.files')} value={`${report.files} (${report.packed_files} packed)`} />
                        <Metric label={t('vault.receipt.elapsed')} value={`${report.ms_total} ms`} />
                        <Metric label={t('vault.receipt.chunks')} value={`${report.logical_chunks}L / ${report.new_physical_chunks}N / ${report.dedup_hits}D`} />
                        <Metric label={t('vault.receipt.plaintext')} value={fmtBytes(report.plaintext_bytes)} />
                        <Metric label={t('vault.receipt.encrypted')} value={fmtBytes(report.encrypted_bytes)} />
                        <Metric label={t('vault.receipt.ratio')} value={report.compressed_bytes > 0 ? `${report.compression_ratio_pct.toFixed(1)}%` : '-'} />
                        {/* P3-03: ECC fields (only when present for ECC-enabled v3+ vaults) */}
                        {report.ecc_shards_generated != null && (
                            <Metric label={t('vault.receipt.eccShards') || 'ECC shards'} value={String(report.ecc_shards_generated)} />
                        )}
                        {report.ecc_bytes_protected != null && (
                            <Metric label={t('vault.receipt.eccProtected') || 'ECC protected'} value={fmtBytes(report.ecc_bytes_protected)} />
                        )}
                        {report.ecc_overhead_pct != null && (
                            <Metric label={t('vault.receipt.eccOverhead') || 'ECC overhead'} value={`${report.ecc_overhead_pct.toFixed(1)}%`} />
                        )}
                    </div>

                    <div>
                        <span className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {t('vault.receipt.pipeline')}
                        </span>
                        <div className="mt-1 text-xs font-mono text-gray-700 dark:text-gray-300 break-words">
                            {report.algorithms.join(' → ')}
                        </div>
                    </div>

                    <div
                        ref={termRef}
                        className="rounded bg-gray-950 text-emerald-400 font-mono text-xs p-3 h-44 overflow-auto border border-gray-800"
                    >
                        {report.steps.slice(0, revealed).map((s, i) => (
                            <div key={i}>
                                <span className="text-gray-600">{'>'}</span> {s}
                            </div>
                        ))}
                        {revealed < report.steps.length && (
                            <span className="inline-block w-2 h-3 bg-emerald-400 animate-pulse" />
                        )}
                    </div>

                    <div className="flex flex-wrap items-center gap-2">
                        <button
                            onClick={() => download(`${base}.txt`, buildReceiptText(report), 'text/plain')}
                            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded text-xs font-medium bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-800 dark:text-gray-100"
                        >
                            <Download size={13} /> {t('vault.receipt.exportTxt')}
                        </button>
                        <button
                            onClick={() => download(`${base}.json`, JSON.stringify(report, null, 2), 'application/json')}
                            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded text-xs font-medium bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-800 dark:text-gray-100"
                        >
                            <Download size={13} /> {t('vault.receipt.exportJson')}
                        </button>
                    </div>

                    <p className="text-[10px] leading-relaxed text-gray-500 dark:text-gray-400 border-t border-gray-200 dark:border-gray-700 pt-3">
                        {report.attribution}
                    </p>
                </div>
            </div>
        </div>
    );
}
