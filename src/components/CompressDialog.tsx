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
import { InlinePasswordGenerator } from './common/InlinePasswordGenerator';
import './CompressDialog.css';

type CompressFormat = 'zip' | '7z' | 'tar' | 'tar.gz' | 'tar.xz' | 'tar.bz2' | 'gz' | 'xz' | 'bz2';

/** 7z content-compression method. LZMA2 is the default; all are readable back. */
type SevenZMethod = 'lzma2' | 'lzma' | 'ppmd' | 'bzip2';

/** 7z-only Advanced encoder options (camelCase matches the backend SevenZAdvanced).
 *  dictionarySize and threads apply to LZMA2 only. */
export interface SevenZAdvancedOptions {
    method: SevenZMethod;
    /** LZMA2 dictionary size in bytes; omit for the encoder default. */
    dictionarySize?: number;
    /** Pack all files into one solid block (better ratio, slower random extract). */
    solid: boolean;
    /** LZMA2 compression threads; omit for single-threaded. */
    threads?: number;
}

export interface CompressOptions {
    archiveName: string;
    format: CompressFormat;
    compressionLevel: number;
    password: string | null;
    /** 7z only: also encrypt the archive header so filenames are hidden (-mhe).
     *  Meaningful only with a password; null/false keeps names readable. */
    encryptFileNames: boolean;
    /** 7z only: Advanced encoder knobs (method, dictionary, solid, threads).
     *  Undefined for every other format. */
    advanced?: SevenZAdvancedOptions;
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
    // Standalone single-stream codecs: one file, no tar wrapper (disabled unless
    // exactly one non-folder file is selected, enforced in the card render).
    { value: 'gz', label: 'GZ', supportsPassword: false, algorithm: 'Gzip', description: 'Gzip · single file' },
    { value: 'xz', label: 'XZ', supportsPassword: false, algorithm: 'XZ/LZMA2', description: 'XZ · single file' },
    { value: 'bz2', label: 'BZ2', supportsPassword: false, algorithm: 'Bzip2', description: 'Bzip2 · single file' },
];

/** Standalone single-stream codecs: valid only for exactly one non-folder file. */
const STANDALONE_FORMATS: readonly CompressFormat[] = ['gz', 'xz', 'bz2'];
const isStandaloneFormat = (f: CompressFormat): boolean => STANDALONE_FORMATS.includes(f);

/** 7z Advanced: selectable content methods (labels are technical, not translated). */
const SEVENZ_METHODS: { value: SevenZMethod; label: string }[] = [
    { value: 'lzma2', label: 'LZMA2' },
    { value: 'lzma', label: 'LZMA' },
    { value: 'ppmd', label: 'PPMd' },
    { value: 'bzip2', label: 'BZip2' },
];

/** 7z Advanced (LZMA2): dictionary-size choices in bytes; undefined = encoder default. */
const DICTIONARY_OPTIONS: { value: number | undefined; label: string }[] = [
    { value: undefined, label: '' }, // labelled from i18n "auto" at render time
    { value: 1024 * 1024, label: '1 MiB' },
    { value: 4 * 1024 * 1024, label: '4 MiB' },
    { value: 16 * 1024 * 1024, label: '16 MiB' },
    { value: 64 * 1024 * 1024, label: '64 MiB' },
];

/** 7z Advanced (LZMA2): thread-count choices; undefined = single-threaded. */
const THREAD_OPTIONS: { value: number | undefined; label: string }[] = [
    { value: undefined, label: '' }, // labelled from i18n "auto" at render time
    { value: 2, label: '2' },
    { value: 4, label: '4' },
    { value: 8, label: '8' },
];

interface LevelOption { value: number; labelKey: string; fallback: string }

// 7-Zip canonical preset mapping, shared by every compressible format. Each
// codec maps the raw value onto its own range in the backend (gzip 0-9,
// bzip2 clamped to 1-9, deflate 0-9, LZMA2 via Lzma2Options::from_level).
const SEVENZIP_LEVELS: LevelOption[] = [
    { value: 0, labelKey: 'compress.store', fallback: 'Store (no compression)' },
    { value: 1, labelKey: 'compress.fastest', fallback: 'Fastest' },
    { value: 3, labelKey: 'compress.fast', fallback: 'Fast' },
    { value: 5, labelKey: 'compress.normal', fallback: 'Normal' },
    { value: 7, labelKey: 'compress.maximum', fallback: 'Maximum' },
    { value: 9, labelKey: 'compress.ultra', fallback: 'Ultra' },
];

const LEVEL_OPTIONS: Record<string, LevelOption[]> = {
    zip: SEVENZIP_LEVELS,
    // 7z LZMA2 has no real "store" mode; from_level(0) still compresses.
    '7z': SEVENZIP_LEVELS.filter(l => l.value !== 0),
    tar: [],
    // gzip/xz level 0 means no compression: pointless for a tar wrapper.
    'tar.gz': SEVENZIP_LEVELS.filter(l => l.value !== 0),
    'tar.xz': SEVENZIP_LEVELS.filter(l => l.value !== 0),
    // bzip2 has no level 0; the backend clamps to 1-9.
    'tar.bz2': SEVENZIP_LEVELS.filter(l => l.value !== 0),
    // Standalone single-stream codecs mirror their tar.* counterparts (no store).
    gz: SEVENZIP_LEVELS.filter(l => l.value !== 0),
    xz: SEVENZIP_LEVELS.filter(l => l.value !== 0),
    bz2: SEVENZIP_LEVELS.filter(l => l.value !== 0),
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
        case 'gz': return { codec: 'gzip', level };
        case 'xz': return { codec: 'xz', level };
        case 'bz2': return { codec: 'bzip2', level };
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
    const [compressionLevel, setCompressionLevel] = useState(5);
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    // 7z "Encrypt file names" (-mhe), opt-in like 7-Zip. Only applied for 7z with
    // a password; reset whenever those preconditions drop so it can't silently
    // ride along on a zip or an unencrypted archive.
    const [encryptFileNames, setEncryptFileNames] = useState(false);
    // 7z Advanced (collapsed by default): content method + LZMA2 dictionary/threads
    // + solid block. Only sent for 7z; solid is off by default (Q5).
    const [showAdvanced, setShowAdvanced] = useState(false);
    const [advMethod, setAdvMethod] = useState<SevenZMethod>('lzma2');
    const [advDictionary, setAdvDictionary] = useState<number | undefined>(undefined);
    const [advThreads, setAdvThreads] = useState<number | undefined>(undefined);
    const [advSolid, setAdvSolid] = useState(false);
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

    // Standalone gz/xz/bz2 hold exactly one file (single stream, no tar wrapper),
    // so they are offered only for a lone non-folder selection; otherwise their
    // cards are disabled with a "single file only" hint.
    const standaloneBlocked = files.length !== 1 || !!files[0]?.isDir;

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
        const name = archiveName.replace(/\.(zip|7z|tar|tar\.gz|tar\.xz|tar\.bz2|tgz|txz|tbz2|gz|xz|bz2)$/i, '');
        return `${outputDir}/${name}${ext}`;
    }, [archiveName, format, outputDir]);

    const handleFormatChange = (newFormat: CompressFormat) => {
        setFormat(newFormat);
        const newLevels = LEVEL_OPTIONS[newFormat] || [];
        const hasCurrentLevel = newLevels.some(l => l.value === compressionLevel);
        if (!hasCurrentLevel && newLevels.length > 0) {
            const normal = newLevels.find(l => l.value === 5);
            setCompressionLevel(normal ? 5 : newLevels[0].value);
        }
        if (!FORMAT_OPTIONS.find(f => f.value === newFormat)?.supportsPassword) {
            setPassword('');
            setConfirmPassword('');
        }
        // Filename encryption is a 7z-only feature; clear it on any other format.
        if (newFormat !== '7z') {
            setEncryptFileNames(false);
        }
    };

    const handleConfirm = async () => {
        setResult(null);
        setCompressing(true);
        try {
            const res = await onConfirm({
                archiveName: archiveName.replace(/\.(zip|7z|tar|tar\.gz|tar\.xz|tar\.bz2|tgz|txz|tbz2|gz|xz|bz2)$/i, ''),
                format,
                compressionLevel,
                password: formatInfo.supportsPassword && password ? password : null,
                // Only a 7z with an actual password can hide filenames.
                encryptFileNames: format === '7z' && !!password && encryptFileNames,
                // Advanced knobs are 7z only; dictionary/threads apply to LZMA2.
                advanced: format === '7z' ? {
                    method: advMethod,
                    dictionarySize: advMethod === 'lzma2' ? advDictionary : undefined,
                    solid: advSolid,
                    threads: advMethod === 'lzma2' ? advThreads : undefined,
                } : undefined,
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
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60" role="dialog" aria-modal="true" aria-label="Compress Files" onClick={(e) => {
            // Once a password has been typed (encrypted archive) the backdrop is
            // inert, so a stray click outside cannot discard it and force retyping.
            // The X is the only way out then. Other states still close on click.
            if (e.target === e.currentTarget && !password) guarded.requestBackdropClose();
        }}>
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
                            {FORMAT_OPTIONS.map(opt => {
                                // A standalone codec needs exactly one file: block its card
                                // (with a hint) whenever the selection isn't a lone file.
                                const blocked = isStandaloneFormat(opt.value) && standaloneBlocked;
                                return (
                                <button
                                    key={opt.value}
                                    onClick={() => handleFormatChange(opt.value)}
                                    disabled={compressing || blocked}
                                    title={blocked ? (t('compress.singleFileOnly') || 'Single file only') : undefined}
                                    className={`compress-format-card ${format === opt.value ? 'active' : ''} ${blocked ? 'blocked' : ''} rounded-lg px-3 py-2.5 text-left transition-all`}
                                >
                                    <div className="flex items-center gap-1.5">
                                        <span className="text-sm font-semibold">{opt.label}</span>
                                        {opt.supportsPassword && <Lock size={10} style={{ color: 'var(--compress-accent)' }} />}
                                    </div>
                                    <div className="text-[10px] mt-0.5" style={{ color: 'var(--compress-text-muted)' }}>
                                        {opt.description}
                                    </div>
                                </button>
                                );
                            })}
                        </div>
                    </div>

                    {/* ── Compression level ─────────────────────────── */}
                    {levels.length > 0 && (
                        <div>
                            <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                                {t('compress.level') || 'Compression Level'}
                            </label>
                            <div className="flex flex-wrap gap-1.5">
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

                    {/* ── 7z Advanced (collapsed, 7z only) ──────────── */}
                    {format === '7z' && (
                        <div className="rounded-lg" style={{ background: 'var(--compress-bg-deep)', border: '1px solid var(--compress-border)' }}>
                            <button
                                type="button"
                                onClick={() => setShowAdvanced(v => !v)}
                                disabled={compressing}
                                className="w-full flex items-center gap-2 px-3.5 py-2.5 text-xs font-medium"
                                style={{ color: 'var(--compress-text-secondary)' }}
                            >
                                <span className="flex-1 text-left">{t('compress.advanced') || 'Advanced'}</span>
                                {showAdvanced
                                    ? <ChevronUp size={14} style={{ color: 'var(--compress-text-muted)' }} />
                                    : <ChevronDown size={14} style={{ color: 'var(--compress-text-muted)' }} />}
                            </button>
                            {showAdvanced && (
                                <div className="border-t px-3.5 py-3 flex flex-col gap-3" style={{ borderColor: 'var(--compress-border)' }}>
                                    {/* Content method */}
                                    <div>
                                        <label className="text-[10px] font-medium block mb-1" style={{ color: 'var(--compress-text-muted)' }}>
                                            {t('compress.method') || 'Method'}
                                        </label>
                                        <div className="flex flex-wrap gap-1.5">
                                            {SEVENZ_METHODS.map(m => (
                                                <button
                                                    key={m.value}
                                                    onClick={() => setAdvMethod(m.value)}
                                                    disabled={compressing}
                                                    className={`compress-format-card ${advMethod === m.value ? 'active' : ''} rounded-lg px-3 py-1.5 text-xs transition-all`}
                                                >
                                                    {m.label}
                                                </button>
                                            ))}
                                        </div>
                                    </div>

                                    {/* Dictionary size + threads (LZMA2 only) */}
                                    {advMethod === 'lzma2' && (
                                        <div className="flex gap-3">
                                            <div className="flex-1">
                                                <label className="text-[10px] font-medium block mb-1" style={{ color: 'var(--compress-text-muted)' }}>
                                                    {t('compress.dictionary') || 'Dictionary size'}
                                                </label>
                                                <select
                                                    value={advDictionary ?? ''}
                                                    disabled={compressing}
                                                    onChange={e => setAdvDictionary(e.target.value === '' ? undefined : Number(e.target.value))}
                                                    className="w-full rounded-lg px-2 py-1.5 text-xs outline-none"
                                                    style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                                >
                                                    {DICTIONARY_OPTIONS.map(o => (
                                                        <option key={o.label || 'auto'} value={o.value ?? ''}>
                                                            {o.value === undefined ? (t('compress.auto') || 'Auto') : o.label}
                                                        </option>
                                                    ))}
                                                </select>
                                            </div>
                                            <div className="flex-1">
                                                <label className="text-[10px] font-medium block mb-1" style={{ color: 'var(--compress-text-muted)' }}>
                                                    {t('compress.threads') || 'Threads'}
                                                </label>
                                                <select
                                                    value={advThreads ?? ''}
                                                    disabled={compressing}
                                                    onChange={e => setAdvThreads(e.target.value === '' ? undefined : Number(e.target.value))}
                                                    className="w-full rounded-lg px-2 py-1.5 text-xs outline-none"
                                                    style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                                >
                                                    {THREAD_OPTIONS.map(o => (
                                                        <option key={o.label || 'auto'} value={o.value ?? ''}>
                                                            {o.value === undefined ? (t('compress.auto') || 'Auto') : o.label}
                                                        </option>
                                                    ))}
                                                </select>
                                            </div>
                                        </div>
                                    )}

                                    {/* Solid block (off by default) */}
                                    <label className="flex items-start gap-2 cursor-pointer select-none">
                                        <input
                                            type="checkbox"
                                            checked={advSolid}
                                            disabled={compressing}
                                            onChange={e => setAdvSolid(e.target.checked)}
                                            className="mt-0.5"
                                            style={{ accentColor: 'var(--compress-accent)' }}
                                        />
                                        <span>
                                            <span className="text-xs font-medium block" style={{ color: 'var(--compress-text-secondary)' }}>
                                                {t('compress.solid') || 'Solid block'}
                                            </span>
                                            <span className="text-[10px] block" style={{ color: 'var(--compress-text-muted)' }}>
                                                {t('compress.solidHint') || 'Better ratio for many small files, slower random extraction'}
                                            </span>
                                        </span>
                                    </label>
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
                                    className="w-full rounded-lg px-3 py-2 text-sm pr-16 outline-none transition-colors"
                                    style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                    onFocus={e => (e.currentTarget.style.borderColor = 'var(--compress-accent)')}
                                    onBlur={e => (e.currentTarget.style.borderColor = 'var(--compress-input-border)')}
                                />
                                <InlinePasswordGenerator
                                    onGenerated={value => { setPassword(value); setConfirmPassword(value); }}
                                    disabled={compressing}
                                    className="absolute right-8 top-1/2 -translate-y-1/2"
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
                            {/* 7z "Encrypt file names" (-mhe), opt-in like 7-Zip: only
                                offered for 7z once a password is set. */}
                            {format === '7z' && password && (
                                <label className="mt-2.5 flex items-start gap-2 cursor-pointer select-none">
                                    <input
                                        type="checkbox"
                                        checked={encryptFileNames}
                                        disabled={compressing}
                                        onChange={e => setEncryptFileNames(e.target.checked)}
                                        className="mt-0.5 accent-current"
                                        style={{ accentColor: 'var(--compress-accent)' }}
                                    />
                                    <span>
                                        <span className="text-xs font-medium block" style={{ color: 'var(--compress-text-secondary)' }}>
                                            {t('compress.encryptFileNames')}
                                        </span>
                                        <span className="text-[10px] block" style={{ color: 'var(--compress-text-muted)' }}>
                                            {t('compress.encryptFileNamesHint')}
                                        </span>
                                    </span>
                                </label>
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
