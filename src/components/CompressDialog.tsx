import * as React from 'react';
import { useState, useMemo, useEffect } from 'react';
import { Archive, Lock, Eye, EyeOff, X, File, Folder, Loader2, ChevronDown, ChevronUp, Shield } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes as formatSize } from '../utils/formatters';
import './CompressDialog.css';

type CompressFormat = 'zip' | '7z' | 'tar' | 'tar.gz' | 'tar.xz' | 'tar.bz2';

export interface CompressOptions {
    archiveName: string;
    format: CompressFormat;
    compressionLevel: number;
    password: string | null;
}

interface CompressDialogProps {
    files: { name: string; path: string; size: number; isDir: boolean }[];
    defaultName: string;
    outputDir: string;
    onConfirm: (options: CompressOptions) => void;
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

// Estimated compression ratio by format+level (approximate, for display only)
function getEstimatedRatio(format: CompressFormat, level: number): string | null {
    if (format === 'tar') return null; // no compression
    if (level === 0) return null; // store mode
    const ratios: Record<string, Record<number, string>> = {
        zip: { 1: '~70%', 6: '~55%', 9: '~50%' },
        '7z': { 1: '~55%', 6: '~40%', 9: '~35%' },
        'tar.gz': { 1: '~65%', 6: '~50%', 9: '~45%' },
        'tar.xz': { 1: '~50%', 6: '~35%', 9: '~30%' },
        'tar.bz2': { 1: '~60%', 6: '~45%', 9: '~40%' },
    };
    return ratios[format]?.[level] || null;
}

function getExtension(format: CompressFormat): string {
    return format === 'tar.gz' ? '.tar.gz'
        : format === 'tar.xz' ? '.tar.xz'
        : format === 'tar.bz2' ? '.tar.bz2'
        : `.${format}`;
}

export const CompressDialog: React.FC<CompressDialogProps> = ({ files, defaultName, outputDir, onConfirm, onClose }) => {
    const t = useTranslation();
    const [format, setFormat] = useState<CompressFormat>('zip');
    const [archiveName, setArchiveName] = useState(defaultName);
    const [compressionLevel, setCompressionLevel] = useState(6);
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [compressing, setCompressing] = useState(false);
    const [showFileList, setShowFileList] = useState(false);

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
    const estimatedRatio = getEstimatedRatio(format, compressionLevel);

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
        }
    };

    const handleConfirm = async () => {
        setCompressing(true);
        try {
            await onConfirm({
                archiveName: archiveName.replace(/\.(zip|7z|tar|tar\.gz|tar\.xz|tar\.bz2|tgz|txz|tbz2)$/i, ''),
                format,
                compressionLevel,
                password: formatInfo.supportsPassword && password ? password : null,
            });
        } finally {
            setCompressing(false);
        }
    };

    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60" role="dialog" aria-modal="true" aria-label="Compress Files" onClick={(e) => { if (e.target === e.currentTarget && !compressing) onClose(); }}>
            <div className="compress-dialog rounded-xl shadow-2xl w-[600px] max-h-[90vh] flex flex-col animate-scale-in"
                style={{ background: 'var(--compress-bg)', border: '1px solid var(--compress-border)', color: 'var(--compress-text)' }}>

                {/* Header */}
                <div className="flex items-center justify-between px-5 py-3.5 border-b" style={{ borderColor: 'var(--compress-border)' }}>
                    <div className="flex items-center gap-2.5">
                        <Archive size={20} style={{ color: 'var(--compress-accent)' }} />
                        <span className="font-semibold text-base">{t('compress.title') || 'Compress Files'}</span>
                    </div>
                    <button onClick={onClose} disabled={compressing} className="p-1.5 rounded-md transition-colors" style={{ color: 'var(--compress-text-secondary)' }}
                        onMouseEnter={e => (e.currentTarget.style.background = 'var(--compress-bg-hover)')}
                        onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                        title={t('common.close')}>
                        <X size={18} />
                    </button>
                </div>

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
                            {estimatedRatio && (
                                <div className="text-[10px] mt-1.5 flex items-center gap-1" style={{ color: 'var(--compress-text-muted)' }}>
                                    <Archive size={10} />
                                    {t('compress.estimatedSize') || 'Estimated'}: {estimatedRatio} ({formatSize(Math.round(totalSize * parseInt(estimatedRatio.replace(/[^0-9]/g, '')) / 100))})
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
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2.5 top-1/2 -translate-y-1/2 transition-colors"
                                    style={{ color: 'var(--compress-text-muted)' }}
                                >
                                    {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                                </button>
                            </div>
                        </div>
                    )}

                    {/* ── Output path preview ──────────────────────── */}
                    <div className="text-xs truncate font-mono" title={fullOutputPath} style={{ color: 'var(--compress-text-muted)' }}>
                        {fullOutputPath}
                    </div>
                </div>

                {/* ── Footer / Progress ─────────────────────────── */}
                {compressing ? (
                    <div className="px-5 py-4 border-t" style={{ borderColor: 'var(--compress-border)' }}>
                        <div className="flex items-center gap-3 mb-2">
                            <Loader2 size={16} className="animate-spin" style={{ color: 'var(--compress-accent)' }} />
                            <span className="text-sm font-medium">{t('compress.compressing') || 'Compressing...'}</span>
                            <span className="text-xs ml-auto" style={{ color: 'var(--compress-text-muted)' }}>{formatInfo.label}</span>
                        </div>
                        <div className="compress-progress-bar">
                            <div className="bar" />
                        </div>
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
                            disabled={!archiveName.trim()}
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
        </div>
    );
};
