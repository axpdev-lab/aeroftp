import * as React from 'react';
import { useState, useMemo, useEffect } from 'react';
import { Archive, Lock, Eye, EyeOff, X, File, Folder, Loader2 } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes as formatSize } from '../utils/formatters';

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

const FORMAT_OPTIONS: { value: CompressFormat; label: string; supportsPassword: boolean }[] = [
    { value: 'zip', label: 'ZIP', supportsPassword: true },
    { value: '7z', label: '7z', supportsPassword: true },
    { value: 'tar', label: 'TAR', supportsPassword: false },
    { value: 'tar.gz', label: 'TAR.GZ', supportsPassword: false },
    { value: 'tar.xz', label: 'TAR.XZ', supportsPassword: false },
    { value: 'tar.bz2', label: 'TAR.BZ2', supportsPassword: false },
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

    const fullOutputPath = useMemo(() => {
        const ext = getExtension(format);
        const name = archiveName.replace(/\.(zip|7z|tar|tar\.gz|tar\.xz|tar\.bz2|tgz|txz|tbz2)$/i, '');
        return `${outputDir}/${name}${ext}`;
    }, [archiveName, format, outputDir]);

    const handleFormatChange = (newFormat: CompressFormat) => {
        setFormat(newFormat);
        // Reset compression level to default for new format
        const newLevels = LEVEL_OPTIONS[newFormat] || [];
        const hasCurrentLevel = newLevels.some(l => l.value === compressionLevel);
        if (!hasCurrentLevel && newLevels.length > 0) {
            const normal = newLevels.find(l => l.value === 6);
            setCompressionLevel(normal ? 6 : newLevels[0].value);
        }
        // Clear password if format doesn't support it
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
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" role="dialog" aria-modal="true" aria-label="Compress Files">
            <div className="bg-gray-800 rounded-lg shadow-2xl border border-gray-700 w-[480px] flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
                    <div className="flex items-center gap-2">
                        <Archive size={18} className="text-blue-400" />
                        <span className="font-medium">{t('compress.title') || 'Compress Files'}</span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-700 rounded" title={t('common.close')}><X size={18} /></button>
                </div>

                <div className="p-4 flex flex-col gap-3">
                    {/* File info */}
                    <div className="flex items-center gap-3 text-xs text-gray-400 bg-gray-900/50 rounded px-3 py-2">
                        <div className="flex items-center gap-1">
                            <File size={12} />
                            <span>{fileCount} {t('compress.files') || 'files'}</span>
                        </div>
                        {folderCount > 0 && (
                            <div className="flex items-center gap-1">
                                <Folder size={12} />
                                <span>{folderCount} {t('compress.folders') || 'folders'}</span>
                            </div>
                        )}
                        <span className="ml-auto">{formatSize(totalSize)}</span>
                    </div>

                    {/* Archive name */}
                    <div>
                        <label className="text-xs text-gray-400 block mb-1">{t('compress.archiveName') || 'Archive Name'}</label>
                        <div className="flex gap-2">
                            <input
                                type="text"
                                value={archiveName}
                                onChange={e => setArchiveName(e.target.value)}
                                className="flex-1 bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm"
                            />
                            <span className="flex items-center text-xs text-gray-500">{getExtension(format)}</span>
                        </div>
                    </div>

                    {/* Format */}
                    <div>
                        <label className="text-xs text-gray-400 block mb-1">{t('compress.format') || 'Format'}</label>
                        <div className="flex gap-1 flex-wrap">
                            {FORMAT_OPTIONS.map(opt => (
                                <button
                                    key={opt.value}
                                    onClick={() => handleFormatChange(opt.value)}
                                    className={`px-2.5 py-1 text-xs rounded border ${
                                        format === opt.value
                                            ? 'bg-blue-600 border-blue-500 text-white'
                                            : 'bg-gray-900 border-gray-600 text-gray-300 hover:bg-gray-700'
                                    }`}
                                >
                                    {opt.label}
                                </button>
                            ))}
                        </div>
                    </div>

                    {/* Compression level */}
                    {levels.length > 0 && (
                        <div>
                            <label className="text-xs text-gray-400 block mb-1">{t('compress.level') || 'Compression Level'}</label>
                            <div className="flex gap-1">
                                {levels.map(lvl => (
                                    <button
                                        key={lvl.value}
                                        onClick={() => setCompressionLevel(lvl.value)}
                                        className={`px-2.5 py-1 text-xs rounded border ${
                                            compressionLevel === lvl.value
                                                ? 'bg-blue-600 border-blue-500 text-white'
                                                : 'bg-gray-900 border-gray-600 text-gray-300 hover:bg-gray-700'
                                        }`}
                                    >
                                        {t(lvl.labelKey) || lvl.fallback}
                                    </button>
                                ))}
                            </div>
                        </div>
                    )}

                    {/* Password (ZIP/7z only) */}
                    {formatInfo.supportsPassword && (
                        <div>
                            <label className="text-xs text-gray-400 block mb-1">
                                <div className="flex items-center gap-1">
                                    <Lock size={11} />
                                    {t('compress.password') || 'Password (optional, AES-256)'}
                                </div>
                            </label>
                            <div className="relative">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    value={password}
                                    onChange={e => setPassword(e.target.value)}
                                    placeholder={t('compress.passwordHint') || 'Leave empty for no encryption'}
                                    className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm pr-8"
                                />
                                <button
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400"
                                >
                                    {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                                </button>
                            </div>
                        </div>
                    )}

                    {/* Output path preview */}
                    <div className="text-xs text-gray-500 truncate" title={fullOutputPath}>
                        {fullOutputPath}
                    </div>
                </div>

                {/* Footer */}
                <div className="flex justify-end gap-2 px-4 py-3 border-t border-gray-700">
                    <button onClick={onClose} className="px-3 py-1.5 text-sm hover:bg-gray-700 rounded">
                        {t('common.cancel') || 'Cancel'}
                    </button>
                    <button
                        onClick={handleConfirm}
                        disabled={!archiveName.trim() || compressing}
                        className="flex items-center gap-2 px-4 py-1.5 bg-blue-600 hover:bg-blue-500 rounded text-sm disabled:opacity-50"
                    >
                        {compressing ? <Loader2 size={14} className="animate-spin" /> : <Archive size={14} />}
                        {t('compress.compress') || 'Compress'} ({FORMAT_OPTIONS.find(f => f.value === format)?.label})
                    </button>
                </div>
            </div>
        </div>
    );
};
