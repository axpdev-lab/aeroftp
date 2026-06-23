// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useMemo, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Archive, Lock, Eye, EyeOff, X, File, Folder, Loader2, ChevronDown, ChevronUp, Shield, Check, TrendingDown, TrendingUp } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes as formatSize } from '../utils/formatters';
import { CompressionEstimateBar } from './common/CompressionEstimateBar';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useArchiveProgress } from '../hooks/useArchiveProgress';
import { useGuardedClose } from '../hooks/useGuardedClose';
import { GuardedCloseConfirm } from './GuardedCloseConfirm';
import { TransferProgressBar } from './TransferProgressBar';
import { computeCompressionRatio } from '../utils/archiveSizeReport';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import './CompressDialog.css';

type CompressFormat = 'zip' | '7z' | 'tar' | 'tar.gz' | 'tar.xz' | 'tar.bz2';

export interface CompressOptions {
    archiveName: string;
    format: CompressFormat;
    compressionLevel: number;
    password: string | null;
}

/** Real byte totals reported back by the parent after a successful compression,
 *  used to render the completion stats (ratio, bytes saved, before/after bars). */
export interface CompressResult {
    inputBytes: number;
    outputBytes: number;
}

interface CompressDialogProps {
    files: { name: string; path: string; size: number; isDir: boolean }[];
    defaultName: string;
    outputDir: string;
    /** Returns the real input/output byte totals on success so the dialog can
     *  present completion stats; throws on failure (dialog stays on the form). */
    onConfirm: (options: CompressOptions) => void | Promise<CompressResult | void>;
    onClose: () => void;
}

interface FormatOption {
    value: CompressFormat;
    label: string;
    supportsPassword: boolean;
    algorithm: string;
    description: string;
}

const FORMAT_OPTIONS: FormatOption[] = [
    { value: 'zip', label: 'ZIP', supportsPassword: true, algorithm: 'Deflate', description: 'AES-256 · Deflate' },
    { value: '7z', label: '7z', supportsPassword: true, algorithm: 'LZMA2', description: 'AES-256 · LZMA2' },
    { value: 'tar', label: 'TAR', supportsPassword: false, algorithm: 'None', description: 'Archive only' },
    { value: 'tar.gz', label: 'TAR.GZ', supportsPassword: false, algorithm: 'Gzip', description: 'Gzip' },
    { value: 'tar.xz', label: 'TAR.XZ', supportsPassword: false, algorithm: 'XZ/LZMA2', description: 'XZ · Best ratio' },
    { value: 'tar.bz2', label: 'TAR.BZ2', supportsPassword: false, algorithm: 'Bzip2', description: 'Bzip2' },
];

interface LevelOption { value: number; labelKey: string; fallback: string }

const LEVEL_OPTIONS: Record<string, LevelOption[]> = {
    zip: [
        { value: 0, labelKey: 'compress.store', fallback: 'Store (no compression)' },
        { value: 1, labelKey: 'compress.fast', fallback: 'Fast' },
        { value: 6, labelKey: 'compress.normal', fallback: 'Normal' },
        { value: 9, labelKey: 'compress.maximum', fallback: 'Maximum' },
    ],
    '7z': [
        { value: 1, labelKey: 'compress.fast', fallback: 'Fast' },
        { value: 6, labelKey: 'compress.normal', fallback: 'Normal' },
        { value: 9, labelKey: 'compress.maximum', fallback: 'Maximum' },
    ],
    tar: [],
    'tar.gz': [
        { value: 1, labelKey: 'compress.fast', fallback: 'Fast' },
        { value: 6, labelKey: 'compress.normal', fallback: 'Normal' },
        { value: 9, labelKey: 'compress.maximum', fallback: 'Maximum' },
    ],
    'tar.xz': [
        { value: 1, labelKey: 'compress.fast', fallback: 'Fast' },
        { value: 6, labelKey: 'compress.normal', fallback: 'Normal' },
        { value: 9, labelKey: 'compress.maximum', fallback: 'Maximum' },
    ],
    'tar.bz2': [
        { value: 1, labelKey: 'compress.fast', fallback: 'Fast' },
        { value: 6, labelKey: 'compress.normal', fallback: 'Normal' },
        { value: 9, labelKey: 'compress.maximum', fallback: 'Maximum' },
    ],
};

// Map a UI format + level onto the backend canary codec. The backend then
// compresses a real sample with that codec to measure the true ratio.
function formatToCodec(format: CompressFormat, level: number): { codec: string; level: number } {
    switch (format) {
        case 'zip': return { codec: level === 0 ? 'store' : 'deflate', level };
        case '7z': return { codec: 'xz', level }; // 7z is LZMA2; xz estimates it well
        case 'tar': return { codec: 'store', level: 0 };
        case 'tar.gz': return { codec: 'gzip', level };
        case 'tar.xz': return { codec: 'xz', level };
        case 'tar.bz2': return { codec: 'bzip2', level };
        default: return { codec: 'store', level: 0 };
    }
}

function getExtension(format: CompressFormat): string {
    return format === 'tar.gz' ? '.tar.gz'
        : format === 'tar.xz' ? '.tar.xz'
        : format === 'tar.bz2' ? '.tar.bz2'
        : `.${format}`;
}

/**
 * Inverse "drain" bar shown beneath the live progress bar during compression.
 * It starts full and empties in step with the top progress bar: the filled
 * width is the input still to read (`total - transferred`) and the caption next
 * to it shows exactly that byte figure, so bar and number shrink together
 * (byte-true, never a fixed estimate that disagrees with the bar). The measured
 * saving is shown only at completion.
 */
const InverseDrainBar: React.FC<{ transferred: number; total: number }> = ({ transferred, total }) => {
    const remaining = Math.max(0, total - transferred);
    const filled = total > 0 ? Math.max(0, Math.min(100, (remaining / total) * 100)) : 0;
    return (
        <div className="mt-3">
            <div className="flex items-center justify-end text-[10px] mb-1" style={{ color: 'var(--compress-text-muted)' }}>
                <span className="flex items-center gap-1"><TrendingDown size={11} style={{ color: 'var(--compress-accent)' }} />{formatSize(remaining)}</span>
            </div>
            <div className="tpb-track h-3.5 rounded-full overflow-hidden" style={{ background: 'var(--compress-bg-deep)', border: '1px solid var(--compress-border)' }}>
                <div
                    className="h-full rounded-full transition-all duration-300"
                    style={{ width: `${filled}%`, background: 'linear-gradient(90deg, var(--compress-accent), var(--compress-accent-hover))' }}
                />
            </div>
        </div>
    );
};

/**
 * Completion panel: real before/after comparison after a finished compression.
 * The "After" bar animates from full down to the measured output/input ratio on
 * mount, so the user watches the file weight shrink by exactly the saved share.
 */
const CompletionStats: React.FC<{ result: CompressResult; t: (k: string) => string }> = ({ result, t }) => {
    const { inputBytes, outputBytes, savedBytes, savedPercent } = computeCompressionRatio(result.inputBytes, result.outputBytes);
    const pct = Math.round(savedPercent);
    // Three honest outcomes. A sub-1% delta in either direction is reported as
    // "incompressible" rather than a confusing "+0% / Increased 148 B": the file
    // was already compressed and there is essentially no space to reclaim.
    const outcome: 'saved' | 'grew' | 'incompressible' = pct >= 1 ? 'saved' : pct <= -1 ? 'grew' : 'incompressible';
    // Both bars are scaled against the larger of the two so they stay
    // proportional in either direction: the bigger size fills the track, the
    // smaller one is visibly shorter (a grown archive reads longer, not equal).
    const maxBytes = Math.max(inputBytes, outputBytes, 1);
    const beforeWidth = (inputBytes / maxBytes) * 100;
    const afterTarget = (outputBytes / maxBytes) * 100;
    // Animate the "After" bar from the "Before" length to its real length, so
    // the change (shrink or grow) is shown as motion.
    const [settled, setSettled] = useState(false);
    useEffect(() => {
        const h = requestAnimationFrame(() => setSettled(true));
        return () => cancelAnimationFrame(h);
    }, []);
    const afterWidth = settled ? afterTarget : beforeWidth;
    const afterFill = outcome === 'saved' ? 'linear-gradient(90deg,#10b981,#22c55e)'
        : outcome === 'grew' ? 'linear-gradient(90deg,#f97316,#ef4444)'
        : 'var(--compress-text-muted)';
    const badgeColor = outcome === 'saved' ? 'text-green-400' : outcome === 'grew' ? 'text-orange-400' : 'text-gray-400';
    return (
        <div className="px-5 py-4 border-t" style={{ borderColor: 'var(--compress-border)' }}>
            <div className="flex items-center gap-2 mb-3">
                <span className="flex items-center justify-center w-6 h-6 rounded-full" style={{ background: 'rgba(34,197,94,0.15)' }}>
                    <Check size={14} style={{ color: '#22c55e' }} />
                </span>
                <span className="text-sm font-semibold">{t('compress.complete') || 'Compression complete'}</span>
                <span className={`ml-auto text-sm font-bold flex items-center gap-1 ${badgeColor}`}>
                    {outcome === 'saved' && <><TrendingDown size={14} />-{pct}%</>}
                    {outcome === 'grew' && <><TrendingUp size={14} />+{Math.abs(pct)}%</>}
                    {outcome === 'incompressible' && <>≈0%</>}
                </span>
            </div>

            {/* Before bar (full = original) */}
            <div className="flex items-center gap-2 mb-1.5">
                <span className="text-[10px] w-12 shrink-0" style={{ color: 'var(--compress-text-muted)' }}>{t('compress.before') || 'Before'}</span>
                <div className="tpb-track h-3 rounded-full overflow-hidden flex-1" style={{ background: 'var(--compress-bg-deep)' }}>
                    <div className="h-full rounded-full" style={{ width: `${beforeWidth}%`, background: 'var(--compress-text-muted)' }} />
                </div>
                <span className="text-[10px] w-16 text-right shrink-0">{formatSize(inputBytes)}</span>
            </div>
            {/* After bar (drains to output/input ratio) */}
            <div className="flex items-center gap-2">
                <span className="text-[10px] w-12 shrink-0" style={{ color: 'var(--compress-text-muted)' }}>{t('compress.after') || 'After'}</span>
                <div className="tpb-track h-3 rounded-full overflow-hidden flex-1" style={{ background: 'var(--compress-bg-deep)' }}>
                    <div
                        className="h-full rounded-full transition-all duration-700 ease-out"
                        style={{ width: `${afterWidth}%`, background: afterFill }}
                    />
                </div>
                <span className="text-[10px] w-16 text-right shrink-0">{formatSize(outputBytes)}</span>
            </div>

            <div className="mt-3 text-center text-xs" style={{ color: 'var(--compress-text-secondary)' }}>
                {outcome === 'saved' && `${t('compress.saved') || 'Saved'} ${formatSize(savedBytes)}`}
                {outcome === 'grew' && `${t('compress.increased') || 'Increased'} ${formatSize(Math.abs(savedBytes))}`}
                {outcome === 'incompressible' && (t('compress.incompressible') || 'Already compressed, no space to save')}
            </div>
        </div>
    );
};

export const CompressDialog: React.FC<CompressDialogProps> = ({ files, defaultName, outputDir, onConfirm, onClose }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [format, setFormat] = useState<CompressFormat>('zip');
    const [archiveName, setArchiveName] = useState(defaultName);
    const [compressionLevel, setCompressionLevel] = useState(6);
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [compressing, setCompressing] = useState(false);
    const [showFileList, setShowFileList] = useState(false);
    // Measured result of a finished compression; drives the completion stats panel.
    const [result, setResult] = useState<CompressResult | null>(null);
    // Real byte-level progress (>=10MB ops only); null for small/instant compressions.
    const progress = useArchiveProgress(compressing);
    // Lock the modal while compressing: inert backdrop + confirm on X, so a stray
    // click can't abandon an in-flight big-file compression (same pattern as AeroSync,
    // CrossProfile, AeroVault).
    const guarded = useGuardedClose({ guard: compressing ? 'busy' : null, onClose });

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    useEffect(() => {
        document.documentElement.classList.add('modal-open');
        return () => { document.documentElement.classList.remove('modal-open'); };
    }, []);

    const formatInfo = FORMAT_OPTIONS.find(f => f.value === format)!;
    const levels = LEVEL_OPTIONS[format] || [];

    const fileCount = files.filter(f => !f.isDir).length;
    const folderCount = files.filter(f => f.isDir).length;
    const totalSize = files.reduce((sum, f) => sum + f.size, 0);

    // Real (canary) compression-size estimate: recomputed, debounced, whenever
    // the input, format or level changes. The backend compresses a bounded
    // sample with the actual codec and extrapolates, so this reflects measured
    // behaviour rather than the old per-format ratio guess.
    const [estimate, setEstimate] = useState<{ original: number; estimated: number; exact: boolean } | null>(null);
    const [estimateLoading, setEstimateLoading] = useState(false);
    useEffect(() => {
        if (totalSize <= 0) { setEstimate(null); return; }
        const { codec, level } = formatToCodec(format, compressionLevel);
        let cancelled = false;
        setEstimateLoading(true);
        const handle = setTimeout(async () => {
            try {
                const paths = files.map(f => f.path);
                const r = await invoke<{ input_bytes: number; estimated_bytes: number; exact: boolean }>(
                    'estimate_compressed_size', { paths, codec, level },
                );
                if (!cancelled) setEstimate({ original: r.input_bytes, estimated: r.estimated_bytes, exact: r.exact });
            } catch {
                if (!cancelled) setEstimate(null);
            } finally {
                if (!cancelled) setEstimateLoading(false);
            }
        }, 250);
        return () => { cancelled = true; clearTimeout(handle); };
    }, [files, format, compressionLevel, totalSize]);

    const fullOutputPath = useMemo(() => {
        const ext = getExtension(format);
        const name = archiveName.replace(/\.(zip|7z|tar|tar\.gz|tar\.xz|tar\.bz2|tgz|txz|tbz2)$/i, '');
        return `${outputDir}/${name}${ext}`;
    }, [archiveName, format, outputDir]);

    const handleFormatChange = (newFormat: CompressFormat) => {
        setFormat(newFormat);
        const newLevels = LEVEL_OPTIONS[newFormat] || [];
        const hasCurrentLevel = newLevels.some(l => l.value === compressionLevel);
        if (!hasCurrentLevel && newLevels.length > 0) {
            const normal = newLevels.find(l => l.value === 6);
            setCompressionLevel(normal ? 6 : newLevels[0].value);
        }
        if (!FORMAT_OPTIONS.find(f => f.value === newFormat)?.supportsPassword) {
            setPassword('');
            setConfirmPassword('');
        }
    };

    const handleConfirm = async () => {
        setResult(null);
        setCompressing(true);
        try {
            const res = await onConfirm({
                archiveName: archiveName.replace(/\.(zip|7z|tar|tar\.gz|tar\.xz|tar\.bz2|tgz|txz|tbz2)$/i, ''),
                format,
                compressionLevel,
                password: formatInfo.supportsPassword && password ? password : null,
            });
            // On success the parent returns the real byte totals: switch to the
            // completion stats view. A 0 output means the size could not be read,
            // so we close rather than render a misleading "100% saved". On failure
            // the parent throws (already toasted + logged) and we stay on the form.
            if (res && typeof res.outputBytes === 'number' && res.outputBytes > 0) {
                setResult(res);
            } else {
                onClose();
            }
        } catch {
            /* parent surfaced the error; remain on the form for a retry */
        } finally {
            setCompressing(false);
        }
    };

    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60" role="dialog" aria-modal="true" aria-label="Compress Files" onClick={(e) => { if (e.target === e.currentTarget) guarded.requestBackdropClose(); }}>
            <div
                {...modalDrag.panelProps}
                className="compress-dialog rounded-lg shadow-2xl w-[600px] max-h-[90vh] flex flex-col animate-scale-in"
                style={{ ...modalDrag.panelProps.style, background: 'var(--compress-bg)', border: '1px solid var(--compress-border)', color: 'var(--compress-text)' }}>

                {/* Header */}
                <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-3.5 border-b cursor-grab active:cursor-grabbing" style={{ borderColor: 'var(--compress-border)' }}>
                    <div className="flex items-center gap-2.5">
                        <Archive size={20} style={{ color: 'var(--compress-accent)' }} />
                        <span className="font-semibold text-base">{t('compress.title') || 'Compress Files'}</span>
                    </div>
                    <button onClick={guarded.requestClose} className="p-1.5 rounded-lg transition-colors" style={{ color: 'var(--compress-text-secondary)' }}
                        onMouseEnter={e => (e.currentTarget.style.background = 'var(--compress-bg-hover)')}
                        onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                        title={t('common.close')}>
                        <X size={18} />
                    </button>
                </div>

                {!result && (
                <div className="p-5 flex flex-col gap-4 overflow-y-auto">

                    {/* ── File summary + expandable list ────────────── */}
                    <div className="rounded-lg" style={{ background: 'var(--compress-bg-deep)', border: '1px solid var(--compress-border)' }}>
                        <button
                            type="button"
                            className="w-full flex items-center gap-3 px-3.5 py-2.5 text-sm"
                            onClick={() => setShowFileList(!showFileList)}
                        >
                            <div className="flex items-center gap-3 flex-1 min-w-0">
                                <div className="flex items-center gap-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                                    <File size={14} />
                                    <span>{fileCount} {t('compress.files') || 'file'}</span>
                                </div>
                                {folderCount > 0 && (
                                    <div className="flex items-center gap-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                                        <Folder size={14} />
                                        <span>{folderCount} {t('compress.folders') || 'folders'}</span>
                                    </div>
                                )}
                            </div>
                            <span className="text-xs font-medium" style={{ color: 'var(--compress-text-secondary)' }}>{formatSize(totalSize)}</span>
                            {showFileList ? <ChevronUp size={14} style={{ color: 'var(--compress-text-muted)' }} /> : <ChevronDown size={14} style={{ color: 'var(--compress-text-muted)' }} />}
                        </button>
                        {showFileList && (
                            <div className="border-t max-h-[150px] overflow-y-auto" style={{ borderColor: 'var(--compress-border)' }}>
                                {files.map((f, i) => (
                                    <div key={i} className="flex items-center gap-2 px-3.5 py-1.5 text-xs" style={{ color: 'var(--compress-text-secondary)' }}>
                                        {f.isDir ? <Folder size={12} className="text-yellow-400 shrink-0" /> : <File size={12} className="shrink-0" style={{ color: 'var(--compress-text-muted)' }} />}
                                        <span className="truncate flex-1">{f.name}</span>
                                        {!f.isDir && <span style={{ color: 'var(--compress-text-muted)' }}>{formatSize(f.size)}</span>}
                                    </div>
                                ))}
                            </div>
                        )}
                    </div>

                    {/* ── Archive name ──────────────────────────────── */}
                    <div>
                        <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                            {t('compress.archiveName') || 'Archive Name'}
                        </label>
                        <div className="flex gap-2 items-center">
                            <input
                                type="text"
                                value={archiveName}
                                onChange={e => setArchiveName(e.target.value)}
                                disabled={compressing}
                                className="flex-1 rounded-lg px-3 py-2 text-sm outline-none transition-colors"
                                style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                onFocus={e => (e.currentTarget.style.borderColor = 'var(--compress-accent)')}
                                onBlur={e => (e.currentTarget.style.borderColor = 'var(--compress-input-border)')}
                            />
                            <span className="text-xs font-mono whitespace-nowrap" style={{ color: 'var(--compress-text-muted)' }}>{getExtension(format)}</span>
                        </div>
                    </div>

                    {/* ── Format cards (3x2 grid) ──────────────────── */}
                    <div>
                        <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                            {t('compress.format') || 'Format'}
                        </label>
                        <div className="grid grid-cols-3 gap-2">
                            {FORMAT_OPTIONS.map(opt => (
                                <button
                                    key={opt.value}
                                    onClick={() => handleFormatChange(opt.value)}
                                    disabled={compressing}
                                    className={`compress-format-card ${format === opt.value ? 'active' : ''} rounded-lg px-3 py-2.5 text-left transition-all`}
                                >
                                    <div className="flex items-center gap-1.5">
                                        <span className="text-sm font-semibold">{opt.label}</span>
                                        {opt.supportsPassword && <Lock size={10} style={{ color: 'var(--compress-accent)' }} />}
                                    </div>
                                    <div className="text-[10px] mt-0.5" style={{ color: 'var(--compress-text-muted)' }}>
                                        {opt.description}
                                    </div>
                                </button>
                            ))}
                        </div>
                    </div>

                    {/* ── Compression level ─────────────────────────── */}
                    {levels.length > 0 && (
                        <div>
                            <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                                {t('compress.level') || 'Compression Level'}
                            </label>
                            <div className="flex gap-1.5">
                                {levels.map(lvl => (
                                    <button
                                        key={lvl.value}
                                        onClick={() => setCompressionLevel(lvl.value)}
                                        disabled={compressing}
                                        className={`compress-format-card ${compressionLevel === lvl.value ? 'active' : ''} rounded-lg px-3 py-1.5 text-xs transition-all`}
                                    >
                                        {t(lvl.labelKey) || lvl.fallback}
                                    </button>
                                ))}
                            </div>
                            {totalSize > 0 && format !== 'tar' && (estimate || estimateLoading) && (
                                <div className="mt-2">
                                    <CompressionEstimateBar
                                        originalBytes={estimate?.original ?? totalSize}
                                        estimatedBytes={estimate?.estimated ?? totalSize}
                                        exact={estimate?.exact ?? false}
                                        loading={estimateLoading && !estimate}
                                    />
                                </div>
                            )}
                        </div>
                    )}

                    {/* ── Password (ZIP/7z only) ───────────────────── */}
                    {formatInfo.supportsPassword && (
                        <div>
                            <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                                <div className="flex items-center gap-1.5">
                                    <Shield size={12} style={{ color: 'var(--compress-accent)' }} />
                                    {t('compress.password') || 'Password (optional, AES-256)'}
                                </div>
                            </label>
                            <div className="relative">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    value={password}
                                    onChange={e => setPassword(e.target.value)}
                                    disabled={compressing}
                                    placeholder={t('compress.passwordHint') || 'Leave empty for no encryption'}
                                    className="w-full rounded-lg px-3 py-2 text-sm pr-9 outline-none transition-colors"
                                    style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                    onFocus={e => (e.currentTarget.style.borderColor = 'var(--compress-accent)')}
                                    onBlur={e => (e.currentTarget.style.borderColor = 'var(--compress-input-border)')}
                                />
                                <button
                                    type="button"
                                    tabIndex={-1}
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2.5 top-1/2 -translate-y-1/2 transition-colors"
                                    style={{ color: 'var(--compress-text-muted)' }}
                                >
                                    {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                                </button>
                            </div>
                            {password && <div className="mt-1.5"><PasswordStrengthBar password={password} /></div>}
                            {password && (
                                <div className="relative mt-2">
                                    <input
                                        type={showPassword ? 'text' : 'password'}
                                        value={confirmPassword}
                                        onChange={e => setConfirmPassword(e.target.value)}
                                        disabled={compressing}
                                        placeholder={t('password.confirmPlaceholder')}
                                        aria-label={t('password.confirm')}
                                        className="w-full rounded-lg px-3 py-2 text-sm pr-9 outline-none transition-colors"
                                        style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                        onFocus={e => (e.currentTarget.style.borderColor = 'var(--compress-accent)')}
                                        onBlur={e => (e.currentTarget.style.borderColor = 'var(--compress-input-border)')}
                                    />
                                    <button
                                        type="button"
                                        tabIndex={-1}
                                        onClick={() => setShowPassword(!showPassword)}
                                        className="absolute right-2.5 top-1/2 -translate-y-1/2 transition-colors"
                                        style={{ color: 'var(--compress-text-muted)' }}
                                    >
                                        {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                                    </button>
                                    <PasswordMatchHint password={password} confirm={confirmPassword} />
                                </div>
                            )}
                        </div>
                    )}

                    {/* ── Output path preview ──────────────────────── */}
                    <div className="text-xs truncate font-mono" title={fullOutputPath} style={{ color: 'var(--compress-text-muted)' }}>
                        {fullOutputPath}
                    </div>
                </div>
                )}

                {/* ── Footer / Progress / Completion ────────────── */}
                {result ? (
                    <>
                        <CompletionStats result={result} t={t} />
                        <div className="flex justify-end px-5 py-3 border-t" style={{ borderColor: 'var(--compress-border)' }}>
                            <button
                                onClick={onClose}
                                className="flex items-center gap-2 px-5 py-2 rounded-lg text-sm font-medium text-white transition-colors"
                                style={{ background: 'var(--compress-accent)' }}
                                onMouseEnter={e => (e.currentTarget.style.background = 'var(--compress-accent-hover)')}
                                onMouseLeave={e => (e.currentTarget.style.background = 'var(--compress-accent)')}
                            >
                                <Check size={15} />
                                {t('compress.done') || 'Done'}
                            </button>
                        </div>
                    </>
                ) : compressing ? (
                    <div className="px-5 py-4 border-t" style={{ borderColor: 'var(--compress-border)' }}>
                        <div className="flex items-center gap-3 mb-2">
                            <Loader2 size={16} className="animate-spin" style={{ color: 'var(--compress-accent)' }} />
                            <span className="text-sm font-medium">{t('compress.compressing') || 'Compressing...'}</span>
                            <span className="text-xs ml-auto" style={{ color: 'var(--compress-text-muted)' }}>{formatInfo.label}</span>
                        </div>
                        {/* Real byte-true bar appears only for >=10MB ops; small ones are
                            instant and show just the spinner above (no fake bar). The
                            lower inverse bar drains in step, picturing the file shrinking. */}
                        {progress && (
                            <>
                                <TransferProgressBar
                                    percentage={progress.percentage}
                                    transferredBytes={progress.transferred}
                                    totalBytes={progress.total}
                                    speedBps={progress.speedBps}
                                    etaSeconds={progress.etaSeconds}
                                    variant={progress.indeterminate ? 'indeterminate' : 'gradient'}
                                    size="lg"
                                />
                                {!progress.indeterminate && (
                                    <InverseDrainBar transferred={progress.transferred} total={progress.total} />
                                )}
                            </>
                        )}
                    </div>
                ) : (
                    <div className="flex justify-end gap-2.5 px-5 py-3.5 border-t" style={{ borderColor: 'var(--compress-border)' }}>
                        <button
                            onClick={onClose}
                            className="px-4 py-2 text-sm rounded-lg transition-colors"
                            style={{ color: 'var(--compress-text-secondary)' }}
                            onMouseEnter={e => (e.currentTarget.style.background = 'var(--compress-bg-hover)')}
                            onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                        >
                            {t('common.cancel') || 'Cancel'}
                        </button>
                        <button
                            onClick={handleConfirm}
                            disabled={!archiveName.trim() || (!!password && confirmPassword !== password)}
                            className="flex items-center gap-2 px-5 py-2 rounded-lg text-sm font-medium text-white transition-colors disabled:opacity-50"
                            style={{ background: 'var(--compress-accent)' }}
                            onMouseEnter={e => { if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--compress-accent-hover)'; }}
                            onMouseLeave={e => (e.currentTarget.style.background = 'var(--compress-accent)')}
                        >
                            <Archive size={15} />
                            {t('compress.compress') || 'Compress'} ({formatInfo.label})
                        </button>
                    </div>
                )}
            </div>
            {guarded.confirmOpen && guarded.confirmKind && (
                <GuardedCloseConfirm
                    kind={guarded.confirmKind}
                    onKeep={guarded.cancelConfirm}
                    onConfirm={guarded.confirmAndClose}
                />
            )}
        </div>
    );
};
