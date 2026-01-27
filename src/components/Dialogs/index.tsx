/**
 * Dialog components - Modal dialogs for confirmation, input, etc.
 * i18n integrated
 */

import React, { useState } from 'react';
import { useTranslation } from '../../i18n';
import { Folder, FileText, Link2, Copy, X, HardDrive, Calendar, Shield, Hash, FileType, Eye, EyeOff } from 'lucide-react';

// ============ Helper Functions ============
const formatBytes = (bytes: number | null): string => {
    if (bytes === null || bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return `${parseFloat((bytes / Math.pow(k, i)).toFixed(2))} ${sizes[i]}`;
};

const getFileExtension = (name: string): string | null => {
    const lastDot = name.lastIndexOf('.');
    if (lastDot === -1 || lastDot === 0) return null;
    return name.substring(lastDot + 1).toLowerCase();
};

const getMimeType = (name: string): string => {
    const ext = getFileExtension(name);
    if (!ext) return 'application/octet-stream';

    const mimeTypes: Record<string, string> = {
        // Images
        'jpg': 'image/jpeg', 'jpeg': 'image/jpeg', 'png': 'image/png', 'gif': 'image/gif',
        'svg': 'image/svg+xml', 'webp': 'image/webp', 'ico': 'image/x-icon', 'bmp': 'image/bmp',
        // Documents
        'pdf': 'application/pdf', 'doc': 'application/msword', 'docx': 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        'xls': 'application/vnd.ms-excel', 'xlsx': 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
        'ppt': 'application/vnd.ms-powerpoint', 'pptx': 'application/vnd.openxmlformats-officedocument.presentationml.presentation',
        'txt': 'text/plain', 'rtf': 'application/rtf', 'csv': 'text/csv',
        // Code
        'html': 'text/html', 'css': 'text/css', 'js': 'application/javascript', 'ts': 'application/typescript',
        'json': 'application/json', 'xml': 'application/xml', 'yaml': 'application/yaml', 'yml': 'application/yaml',
        'md': 'text/markdown', 'py': 'text/x-python', 'rb': 'text/x-ruby', 'php': 'application/x-php',
        'java': 'text/x-java', 'c': 'text/x-c', 'cpp': 'text/x-c++', 'h': 'text/x-c', 'rs': 'text/x-rust',
        'go': 'text/x-go', 'sh': 'application/x-sh', 'bash': 'application/x-sh',
        // Archives
        'zip': 'application/zip', 'rar': 'application/x-rar-compressed', 'tar': 'application/x-tar',
        'gz': 'application/gzip', '7z': 'application/x-7z-compressed', 'bz2': 'application/x-bzip2',
        // Media
        'mp3': 'audio/mpeg', 'wav': 'audio/wav', 'ogg': 'audio/ogg', 'flac': 'audio/flac',
        'mp4': 'video/mp4', 'webm': 'video/webm', 'avi': 'video/x-msvideo', 'mkv': 'video/x-matroska',
        // Other
        'exe': 'application/x-msdownload', 'dmg': 'application/x-apple-diskimage',
        'deb': 'application/x-deb', 'rpm': 'application/x-rpm',
    };

    return mimeTypes[ext] || 'application/octet-stream';
};

// ============ Confirm Dialog ============
interface ConfirmDialogProps {
    message: string;
    onConfirm: () => void;
    onCancel: () => void;
    confirmLabel?: string;
    confirmColor?: 'red' | 'blue' | 'green';
}

export const ConfirmDialog: React.FC<ConfirmDialogProps> = ({
    message,
    onConfirm,
    onCancel,
    confirmLabel,
    confirmColor = 'red'
}) => {
    const t = useTranslation();
    const colorMap = {
        red: 'bg-red-500 hover:bg-red-600',
        blue: 'bg-blue-500 hover:bg-blue-600',
        green: 'bg-green-500 hover:bg-green-600',
    };

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
            <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-2xl max-w-sm">
                <p className="text-gray-900 dark:text-gray-100 mb-4">{message}</p>
                <div className="flex justify-end gap-2">
                    <button
                        onClick={onCancel}
                        className="px-4 py-2 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        onClick={onConfirm}
                        className={`px-4 py-2 text-white rounded-lg ${colorMap[confirmColor]}`}
                    >
                        {confirmLabel || t('common.delete')}
                    </button>
                </div>
            </div>
        </div>
    );
};

// ============ Input Dialog ============
interface InputDialogProps {
    title: string;
    defaultValue: string;
    onConfirm: (value: string) => void;
    onCancel: () => void;
    placeholder?: string;
    isPassword?: boolean;
}

export const InputDialog: React.FC<InputDialogProps> = ({
    title,
    defaultValue,
    onConfirm,
    onCancel,
    placeholder,
    isPassword = false
}) => {
    const t = useTranslation();
    const [value, setValue] = useState(defaultValue);
    const [showPassword, setShowPassword] = useState(false);

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
            <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-2xl w-96">
                <h3 className="text-lg font-semibold mb-4 text-gray-900 dark:text-gray-100">{title}</h3>
                <div className="relative mb-4">
                    <input
                        type={isPassword && !showPassword ? 'password' : 'text'}
                        value={value}
                        onChange={(e: React.ChangeEvent<HTMLInputElement>) => setValue(e.target.value)}
                        placeholder={placeholder}
                        className="w-full px-4 py-2 border rounded-lg dark:bg-gray-700 dark:border-gray-600 text-gray-900 dark:text-gray-100 pr-10"
                        autoFocus
                        onKeyDown={(e: React.KeyboardEvent) => e.key === 'Enter' && onConfirm(value)}
                    />
                    {isPassword && (
                        <button
                            type="button"
                            onClick={() => setShowPassword(!showPassword)}
                            className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200"
                            tabIndex={-1}
                        >
                            {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                        </button>
                    )}
                </div>
                <div className="flex justify-end gap-2">
                    <button
                        onClick={onCancel}
                        className="px-4 py-2 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        onClick={() => onConfirm(value)}
                        className="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600"
                    >
                        {t('common.ok')}
                    </button>
                </div>
            </div>
        </div>
    );
};

// ============ Sync Navigation Choice Dialog ============
interface SyncNavDialogProps {
    missingPath: string;
    isRemote: boolean;
    onCreateFolder: () => void;
    onDisableSync: () => void;
    onCancel: () => void;
}

export const SyncNavDialog: React.FC<SyncNavDialogProps> = ({
    missingPath,
    isRemote,
    onCreateFolder,
    onDisableSync,
    onCancel
}) => {
    const t = useTranslation();

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
            <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-2xl max-w-md">
                <h3 className="text-lg font-semibold mb-3 text-gray-900 dark:text-gray-100">
                    📁 {t('browser.newFolder')}
                </h3>
                <p className="text-gray-600 dark:text-gray-400 mb-2 text-sm">
                    {isRemote ? t('browser.remote') : t('browser.local')} {t('browser.path')}:
                </p>
                <p className="text-blue-500 font-mono text-sm bg-gray-100 dark:bg-gray-700 p-2 rounded mb-4 break-all">
                    {missingPath}
                </p>
                <div className="flex flex-col gap-2">
                    <button
                        onClick={onCreateFolder}
                        className="w-full px-4 py-2 bg-green-500 text-white rounded-lg hover:bg-green-600 text-left flex items-center gap-2"
                    >
                        <span>📂</span> {t('common.create')} {t('browser.newFolder')}
                    </button>
                    <button
                        onClick={onDisableSync}
                        className="w-full px-4 py-2 bg-amber-500 text-white rounded-lg hover:bg-amber-600 text-left flex items-center gap-2"
                    >
                        <span>🔗</span> {t('cloud.disable')}
                    </button>
                    <button
                        onClick={onCancel}
                        className="w-full px-4 py-2 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg text-left"
                    >
                        {t('common.cancel')}
                    </button>
                </div>
            </div>
        </div>
    );
};

// ============ Properties Dialog ============
export interface FileProperties {
    name: string;
    path: string;
    size: number | null;
    is_dir: boolean;
    modified: string | null;
    permissions?: string | null;
    isRemote: boolean;
    protocol?: string;
    // Checksum (optional, calculated on demand)
    checksum?: {
        md5?: string;
        sha256?: string;
        calculating?: boolean;
    };
}

interface PropertiesDialogProps {
    file: FileProperties;
    onClose: () => void;
    onCalculateChecksum?: (algorithm: 'md5' | 'sha256') => void;
}

export const PropertiesDialog: React.FC<PropertiesDialogProps> = ({
    file,
    onClose,
    onCalculateChecksum
}) => {
    const t = useTranslation();
    const [copiedField, setCopiedField] = useState<string | null>(null);

    const copyToClipboard = (text: string, field: string) => {
        navigator.clipboard.writeText(text);
        setCopiedField(field);
        setTimeout(() => setCopiedField(null), 2000);
    };

    const formatDate = (dateStr: string | null): string => {
        if (!dateStr) return '—';
        try {
            const date = new Date(dateStr);
            return date.toLocaleString();
        } catch {
            return dateStr;
        }
    };

    const extension = file.is_dir ? null : getFileExtension(file.name);
    const mimeType = file.is_dir ? 'inode/directory' : getMimeType(file.name);

    // Permission string parser (e.g., "drwxr-xr-x" or "0755")
    const parsePermissions = (perms: string | null | undefined): { display: string; octal?: string } => {
        if (!perms) return { display: '—' };

        // If it's already in rwx format
        if (perms.match(/^[d\-l][rwx\-]{9}$/)) {
            // Calculate octal from rwx
            const toOctal = (r: string, w: string, x: string) =>
                (r === 'r' ? 4 : 0) + (w === 'w' ? 2 : 0) + (x === 'x' ? 1 : 0);
            const owner = toOctal(perms[1], perms[2], perms[3]);
            const group = toOctal(perms[4], perms[5], perms[6]);
            const other = toOctal(perms[7], perms[8], perms[9]);
            return { display: perms, octal: `${owner}${group}${other}` };
        }

        // If it's octal format (e.g., "755")
        if (perms.match(/^[0-7]{3,4}$/)) {
            const octal = perms.length === 4 ? perms.slice(1) : perms;
            const toRwx = (n: number) =>
                (n & 4 ? 'r' : '-') + (n & 2 ? 'w' : '-') + (n & 1 ? 'x' : '-');
            const rwx = file.is_dir ? 'd' : '-';
            const display = rwx + toRwx(parseInt(octal[0])) + toRwx(parseInt(octal[1])) + toRwx(parseInt(octal[2]));
            return { display, octal };
        }

        return { display: perms };
    };

    const permInfo = parsePermissions(file.permissions);

    const PropertyRow: React.FC<{ icon: React.ReactNode; label: string; value: string; copyable?: boolean; mono?: boolean }> =
        ({ icon, label, value, copyable = false, mono = false }) => (
        <div className="flex items-start gap-3 py-2 border-b border-gray-100 dark:border-gray-700 last:border-0">
            <div className="text-gray-400 mt-0.5">{icon}</div>
            <div className="flex-1 min-w-0">
                <div className="text-xs text-gray-500 dark:text-gray-400">{label}</div>
                <div className={`text-sm text-gray-900 dark:text-gray-100 break-all ${mono ? 'font-mono' : ''}`}>
                    {value}
                </div>
            </div>
            {copyable && (
                <button
                    onClick={() => copyToClipboard(value, label)}
                    className="text-gray-400 hover:text-blue-500 transition-colors p-1"
                    title="Copy"
                >
                    {copiedField === label ? (
                        <span className="text-green-500 text-xs">Copied!</span>
                    ) : (
                        <Copy size={14} />
                    )}
                </button>
            )}
        </div>
    );

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
            <div
                className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-[420px] max-h-[80vh] overflow-hidden"
                onClick={(e) => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3">
                        {file.is_dir ? (
                            <Folder size={24} className="text-yellow-500" />
                        ) : (
                            <FileText size={24} className="text-blue-500" />
                        )}
                        <div>
                            <h3 className="font-semibold text-gray-900 dark:text-gray-100 truncate max-w-[280px]" title={file.name}>
                                {file.name}
                            </h3>
                            <span className="text-xs text-gray-500">
                                {file.is_dir ? 'Folder' : 'File'} • {file.isRemote ? `Remote (${file.protocol?.toUpperCase() || 'FTP'})` : 'Local'}
                            </span>
                        </div>
                    </div>
                    <button
                        onClick={onClose}
                        className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 p-1"
                    >
                        <X size={20} />
                    </button>
                </div>

                {/* Properties List */}
                <div className="p-4 overflow-y-auto max-h-[calc(80vh-120px)]">
                    {/* Basic Info */}
                    <PropertyRow
                        icon={<FileText size={16} />}
                        label="Name"
                        value={file.name}
                        copyable
                    />
                    <PropertyRow
                        icon={<HardDrive size={16} />}
                        label="Path"
                        value={file.path}
                        copyable
                        mono
                    />

                    {!file.is_dir && (
                        <>
                            <PropertyRow
                                icon={<Hash size={16} />}
                                label="Size"
                                value={`${formatBytes(file.size)}${file.size ? ` (${file.size.toLocaleString()} bytes)` : ''}`}
                            />
                            <PropertyRow
                                icon={<FileType size={16} />}
                                label="Type"
                                value={`${mimeType}${extension ? ` (.${extension})` : ''}`}
                            />
                        </>
                    )}

                    <PropertyRow
                        icon={<Calendar size={16} />}
                        label="Modified"
                        value={formatDate(file.modified)}
                    />

                    {/* Permissions (remote files only) */}
                    {file.isRemote && file.permissions && (
                        <PropertyRow
                            icon={<Shield size={16} />}
                            label="Permissions"
                            value={`${permInfo.display}${permInfo.octal ? ` (${permInfo.octal})` : ''}`}
                            mono
                        />
                    )}

                    {/* Checksum Section (files only) */}
                    {!file.is_dir && onCalculateChecksum && (
                        <div className="mt-4 pt-4 border-t border-gray-200 dark:border-gray-700">
                            <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">
                                Checksum Verification
                            </div>

                            {/* MD5 */}
                            <div className="flex items-center gap-2 mb-2">
                                <span className="text-xs text-gray-500 w-16">MD5:</span>
                                {file.checksum?.md5 ? (
                                    <code className="flex-1 text-xs font-mono bg-gray-100 dark:bg-gray-700 px-2 py-1 rounded truncate">
                                        {file.checksum.md5}
                                    </code>
                                ) : (
                                    <button
                                        onClick={() => onCalculateChecksum('md5')}
                                        disabled={file.checksum?.calculating}
                                        className="text-xs text-blue-500 hover:text-blue-600 disabled:text-gray-400"
                                    >
                                        {file.checksum?.calculating ? 'Calculating...' : 'Calculate'}
                                    </button>
                                )}
                                {file.checksum?.md5 && (
                                    <button
                                        onClick={() => copyToClipboard(file.checksum!.md5!, 'MD5')}
                                        className="text-gray-400 hover:text-blue-500"
                                    >
                                        <Copy size={12} />
                                    </button>
                                )}
                            </div>

                            {/* SHA-256 */}
                            <div className="flex items-center gap-2">
                                <span className="text-xs text-gray-500 w-16">SHA-256:</span>
                                {file.checksum?.sha256 ? (
                                    <code className="flex-1 text-xs font-mono bg-gray-100 dark:bg-gray-700 px-2 py-1 rounded truncate" title={file.checksum.sha256}>
                                        {file.checksum.sha256.substring(0, 32)}...
                                    </code>
                                ) : (
                                    <button
                                        onClick={() => onCalculateChecksum('sha256')}
                                        disabled={file.checksum?.calculating}
                                        className="text-xs text-blue-500 hover:text-blue-600 disabled:text-gray-400"
                                    >
                                        {file.checksum?.calculating ? 'Calculating...' : 'Calculate'}
                                    </button>
                                )}
                                {file.checksum?.sha256 && (
                                    <button
                                        onClick={() => copyToClipboard(file.checksum!.sha256!, 'SHA-256')}
                                        className="text-gray-400 hover:text-blue-500"
                                    >
                                        <Copy size={12} />
                                    </button>
                                )}
                            </div>
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className="flex justify-end p-4 border-t border-gray-200 dark:border-gray-700">
                    <button
                        onClick={onClose}
                        className="px-4 py-2 bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600"
                    >
                        Close
                    </button>
                </div>
            </div>
        </div>
    );
};
