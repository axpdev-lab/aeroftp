/**
 * Dialog components - Modal dialogs for confirmation, input, etc.
 * i18n integrated
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../../i18n';
import { Folder, FileText, Copy, X, HardDrive, Calendar, Shield, ShieldCheck, Hash, FileType, Eye, EyeOff, AlertTriangle, Info, ShieldAlert, KeyRound, Lock, Clock, Link as LinkIcon, User, Users, Loader2 } from 'lucide-react';
import { formatBytes } from '../../utils/formatters';
import { getMimeType, getFileExtension } from '../Preview/utils/fileTypes';

// ============ Alert Dialog ============
interface AlertDialogProps {
    title: string;
    message: string;
    type?: 'warning' | 'error' | 'info';
    onClose: () => void;
    actionLabel?: string;
    onAction?: () => void;
}

export const AlertDialog: React.FC<AlertDialogProps> = ({
    title,
    message,
    type = 'info',
    onClose,
    actionLabel,
    onAction,
}) => {
    const t = useTranslation();
    const iconMap = {
        warning: <AlertTriangle size={24} className="text-amber-500" />,
        error: <ShieldAlert size={24} className="text-red-500" />,
        info: <Info size={24} className="text-blue-500" />,
    };
    const accentMap = {
        warning: 'border-amber-500/30',
        error: 'border-red-500/30',
        info: 'border-blue-500/30',
    };
    const actionColorMap = {
        warning: 'bg-amber-500 hover:bg-amber-600',
        error: 'bg-red-500 hover:bg-red-600',
        info: 'bg-blue-500 hover:bg-blue-600',
    };

    return (
        <div className="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-50" role="dialog" aria-modal="true" aria-label={title} onClick={onClose}>
            <div
                className={`bg-white dark:bg-gray-800 rounded-xl shadow-2xl max-w-md w-full mx-4 border ${accentMap[type]} overflow-hidden`}
                onClick={(e) => e.stopPropagation()}
            >
                <div className="flex items-start gap-4 p-5">
                    <div className="flex-shrink-0 mt-0.5">{iconMap[type]}</div>
                    <div className="flex-1 min-w-0">
                        <h3 className="text-base font-semibold text-gray-900 dark:text-gray-100 mb-1">{title}</h3>
                        <p className="text-sm text-gray-600 dark:text-gray-400 leading-relaxed">{message}</p>
                    </div>
                </div>
                <div className="flex justify-end gap-2 px-5 py-3 bg-gray-50 dark:bg-gray-800/50 border-t border-gray-200 dark:border-gray-700">
                    <button
                        onClick={onClose}
                        className="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        {t('common.ok')}
                    </button>
                    {actionLabel && onAction && (
                        <button
                            onClick={onAction}
                            className={`px-4 py-2 text-sm text-white rounded-lg transition-colors flex items-center gap-2 ${actionColorMap[type]}`}
                        >
                            <KeyRound size={14} />
                            {actionLabel}
                        </button>
                    )}
                </div>
            </div>
        </div>
    );
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
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog" aria-modal="true" aria-label={message}>
            <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-2xl max-w-sm animate-scale-in">
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
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog" aria-modal="true" aria-label={title}>
            <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-2xl w-96 animate-scale-in">
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
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog" aria-modal="true" aria-label={missingPath}>
            <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-2xl max-w-md animate-scale-in">
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
    // Enhanced properties (optional, populated when available)
    created?: string | null;
    accessed?: string | null;
    owner?: string | null;
    group?: string | null;
    is_symlink?: boolean;
    link_target?: string | null;
    inode?: number | null;
    hard_links?: number | null;
    permissions_mode?: number | null;
    // Checksum (optional, calculated on demand)
    checksum?: {
        md5?: string;
        sha1?: string;
        sha256?: string;
        sha512?: string;
        calculating?: boolean;
    };
}

interface PropertiesDialogProps {
    file: FileProperties;
    onClose: () => void;
    onCalculateChecksum?: (algorithm: 'md5' | 'sha1' | 'sha256' | 'sha512') => void;
    onCalculateFolderSize?: () => void;
    folderSize?: { total_bytes: number; file_count: number; dir_count: number } | null;
    folderSizeCalculating?: boolean;
}

export const PropertiesDialog: React.FC<PropertiesDialogProps> = ({
    file,
    onClose,
    onCalculateChecksum,
    onCalculateFolderSize,
    folderSize,
    folderSizeCalculating = false,
}) => {
    const t = useTranslation();
    const [copiedField, setCopiedField] = useState<string | null>(null);
    const [activeTab, setActiveTab] = useState<'general' | 'permissions' | 'checksum'>('general');

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    useEffect(() => {
        document.documentElement.classList.add('modal-open');
        return () => { document.documentElement.classList.remove('modal-open'); };
    }, []);

    const copyToClipboard = (text: string, field: string) => {
        navigator.clipboard.writeText(text);
        setCopiedField(field);
        setTimeout(() => setCopiedField(null), 2000);
    };

    const formatDate = (dateStr: string | null | undefined): string => {
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

    // Also parse octal from permissions_mode if available
    const getPermissionsInfo = (): { display: string; octal?: string } => {
        if (file.permissions) return parsePermissions(file.permissions);
        if (file.permissions_mode != null) {
            const mode = file.permissions_mode & 0o777;
            const octal = mode.toString(8).padStart(3, '0');
            return parsePermissions(octal);
        }
        return { display: '—' };
    };

    const permInfo = getPermissionsInfo();

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
                        <span className="text-green-500 text-xs">{t('common.copied')}</span>
                    ) : (
                        <Copy size={14} />
                    )}
                </button>
            )}
        </div>
    );

    // Checksum row helper
    const ChecksumRow: React.FC<{ label: string; value?: string; algorithm: 'md5' | 'sha1' | 'sha256' | 'sha512'; truncate?: boolean }> =
        ({ label, value, algorithm, truncate: shouldTruncate = false }) => (
        <div className="flex items-center gap-2 mb-2">
            <span className="text-xs text-gray-500 w-16 shrink-0">{label}:</span>
            {value ? (
                <code
                    className="flex-1 text-xs font-mono bg-gray-100 dark:bg-gray-700 px-2 py-1 rounded truncate"
                    title={value}
                >
                    {shouldTruncate && value.length > 32 ? `${value.substring(0, 32)}...` : value}
                </code>
            ) : (
                <button
                    onClick={() => onCalculateChecksum?.(algorithm)}
                    disabled={file.checksum?.calculating}
                    className="text-xs text-blue-500 hover:text-blue-600 disabled:text-gray-400"
                >
                    {file.checksum?.calculating ? t('properties.calculating') : t('properties.calculate')}
                </button>
            )}
            {value && (
                <button
                    onClick={() => copyToClipboard(value, label)}
                    className="text-gray-400 hover:text-blue-500 shrink-0"
                >
                    {copiedField === label ? (
                        <span className="text-green-500 text-[10px]">{t('common.copied')}</span>
                    ) : (
                        <Copy size={12} />
                    )}
                </button>
            )}
        </div>
    );

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog" aria-modal="true" aria-label={file.name} onClick={onClose}>
            <div
                className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-[420px] max-h-[80vh] overflow-hidden animate-scale-in"
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
                                {file.is_dir ? t('properties.folder') : t('properties.file')} {' \u2022 '} {file.isRemote ? `${t('properties.remote')} (${file.protocol?.toUpperCase() || 'FTP'})` : t('properties.local')}
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

                {/* Tab Bar */}
                <div className="flex border-b border-gray-200 dark:border-gray-700">
                    {(['general', 'permissions', 'checksum'] as const).map((tab) => (
                        <button
                            key={tab}
                            onClick={() => setActiveTab(tab)}
                            className={`flex-1 px-4 py-2 text-xs font-medium transition-colors ${
                                activeTab === tab
                                    ? 'text-blue-600 dark:text-blue-400 border-b-2 border-blue-600 dark:border-blue-400'
                                    : 'text-gray-500 hover:text-gray-700 dark:hover:text-gray-300'
                            }`}
                        >
                            {tab === 'general' ? t('properties.general') : tab === 'permissions' ? t('properties.permissions') : t('properties.checksum')}
                        </button>
                    ))}
                </div>

                {/* Tab Content */}
                <div className="p-4 overflow-y-auto max-h-[calc(80vh-180px)]">

                    {/* General Tab */}
                    {activeTab === 'general' && (
                        <>
                            <PropertyRow
                                icon={<FileText size={16} />}
                                label={t('properties.name')}
                                value={file.name}
                                copyable
                            />
                            <PropertyRow
                                icon={<HardDrive size={16} />}
                                label={t('properties.path')}
                                value={file.path}
                                copyable
                                mono
                            />

                            {!file.is_dir && (
                                <>
                                    <PropertyRow
                                        icon={<Hash size={16} />}
                                        label={t('properties.size')}
                                        value={`${formatBytes(file.size)}${file.size ? ` (${file.size.toLocaleString()} bytes)` : ''}`}
                                    />
                                    <PropertyRow
                                        icon={<FileType size={16} />}
                                        label={t('properties.type')}
                                        value={`${mimeType}${extension ? ` (.${extension})` : ''}`}
                                    />
                                </>
                            )}

                            {/* Folder size */}
                            {file.is_dir && (
                                <div className="flex items-start gap-3 py-2 border-b border-gray-100 dark:border-gray-700">
                                    <div className="text-gray-400 mt-0.5"><Hash size={16} /></div>
                                    <div className="flex-1 min-w-0">
                                        <div className="text-xs text-gray-500 dark:text-gray-400">{t('properties.size')}</div>
                                        {folderSize ? (
                                            <div className="text-sm text-gray-900 dark:text-gray-100">
                                                {formatBytes(folderSize.total_bytes)} ({folderSize.file_count.toLocaleString()} {t('properties.files')}, {folderSize.dir_count.toLocaleString()} {t('properties.folders')})
                                            </div>
                                        ) : onCalculateFolderSize ? (
                                            <button
                                                onClick={onCalculateFolderSize}
                                                disabled={folderSizeCalculating}
                                                className="text-xs text-blue-500 hover:text-blue-600 disabled:text-gray-400 flex items-center gap-1"
                                            >
                                                {folderSizeCalculating ? (
                                                    <><Loader2 size={12} className="animate-spin" /> {t('properties.calculating')}</>
                                                ) : (
                                                    t('properties.calculateSize')
                                                )}
                                            </button>
                                        ) : (
                                            <span className="text-sm text-gray-500">—</span>
                                        )}
                                    </div>
                                </div>
                            )}

                            <PropertyRow
                                icon={<Calendar size={16} />}
                                label={t('properties.modified')}
                                value={formatDate(file.modified)}
                            />
                            {file.created !== undefined && (
                                <PropertyRow
                                    icon={<Calendar size={16} />}
                                    label={t('properties.created')}
                                    value={formatDate(file.created)}
                                />
                            )}
                            {file.accessed !== undefined && (
                                <PropertyRow
                                    icon={<Clock size={16} />}
                                    label={t('properties.accessed')}
                                    value={formatDate(file.accessed)}
                                />
                            )}
                            {file.is_symlink && file.link_target && (
                                <PropertyRow
                                    icon={<LinkIcon size={16} />}
                                    label={t('properties.linkTarget')}
                                    value={file.link_target}
                                    copyable
                                    mono
                                />
                            )}
                        </>
                    )}

                    {/* Permissions Tab */}
                    {activeTab === 'permissions' && (
                        <>
                            {(file.permissions || file.permissions_mode != null) ? (
                                <>
                                    <PropertyRow
                                        icon={<Shield size={16} />}
                                        label={t('properties.permissionsText')}
                                        value={permInfo.display}
                                        mono
                                    />
                                    {permInfo.octal && (
                                        <PropertyRow
                                            icon={<Hash size={16} />}
                                            label={t('properties.permissionsOctal')}
                                            value={permInfo.octal}
                                            mono
                                            copyable
                                        />
                                    )}
                                </>
                            ) : (
                                <div className="flex items-start gap-3 py-2 border-b border-gray-100 dark:border-gray-700">
                                    <div className="text-gray-400 mt-0.5"><Shield size={16} /></div>
                                    <div className="flex-1 min-w-0">
                                        <div className="text-xs text-gray-500 dark:text-gray-400">{t('properties.permissions')}</div>
                                        <div className="text-sm text-gray-500">—</div>
                                    </div>
                                </div>
                            )}

                            {(file.owner || file.group) && (
                                <>
                                    {file.owner && (
                                        <PropertyRow
                                            icon={<User size={16} />}
                                            label={t('properties.owner')}
                                            value={file.owner}
                                        />
                                    )}
                                    {file.group && (
                                        <PropertyRow
                                            icon={<Users size={16} />}
                                            label={t('properties.group')}
                                            value={file.group}
                                        />
                                    )}
                                </>
                            )}

                            {file.inode != null && (
                                <PropertyRow
                                    icon={<Hash size={16} />}
                                    label={t('properties.inode')}
                                    value={file.inode.toString()}
                                />
                            )}
                            {file.hard_links != null && (
                                <PropertyRow
                                    icon={<LinkIcon size={16} />}
                                    label={t('properties.hardLinks')}
                                    value={file.hard_links.toString()}
                                />
                            )}

                            {/* Show message when no permission data at all */}
                            {!file.permissions && file.permissions_mode == null && !file.owner && !file.group && file.inode == null && file.hard_links == null && (
                                <div className="text-sm text-gray-500 dark:text-gray-400 text-center py-4">
                                    {t('properties.notAvailable')}
                                </div>
                            )}
                        </>
                    )}

                    {/* Checksum Tab */}
                    {activeTab === 'checksum' && (
                        <>
                            {!file.is_dir && onCalculateChecksum ? (
                                <div className="space-y-1">
                                    <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-3">
                                        {t('properties.checksumVerification')}
                                    </div>
                                    <ChecksumRow label="MD5" value={file.checksum?.md5} algorithm="md5" />
                                    <ChecksumRow label="SHA-1" value={file.checksum?.sha1} algorithm="sha1" truncate />
                                    <ChecksumRow label="SHA-256" value={file.checksum?.sha256} algorithm="sha256" truncate />
                                    <ChecksumRow label="SHA-512" value={file.checksum?.sha512} algorithm="sha512" truncate />
                                </div>
                            ) : file.is_dir ? (
                                <div className="text-sm text-gray-500 dark:text-gray-400 text-center py-4">
                                    {t('properties.checksumFolderNA')}
                                </div>
                            ) : (
                                <div className="text-sm text-gray-500 dark:text-gray-400 text-center py-4">
                                    {t('properties.notAvailable')}
                                </div>
                            )}
                        </>
                    )}
                </div>

                {/* Footer */}
                <div className="flex justify-end p-4 border-t border-gray-200 dark:border-gray-700">
                    <button
                        onClick={onClose}
                        className="px-4 py-2 bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600"
                    >
                        {t('common.close')}
                    </button>
                </div>
            </div>
        </div>
    );
};

// ============ Master Password Setup Dialog ============

interface MasterPasswordSetupDialogProps {
    onComplete: () => void;
    onClose: () => void;
}

export const MasterPasswordSetupDialog: React.FC<MasterPasswordSetupDialogProps> = ({ onComplete, onClose }) => {
    const t = useTranslation();
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [timeoutMinutes, setTimeoutMinutes] = useState(5);
    const [error, setError] = useState('');
    const [isLoading, setIsLoading] = useState(false);

    const handleSubmit = async (e: React.FormEvent) => {
        e.preventDefault();
        setError('');

        if (password.length < 8) {
            setError(t('masterPassword.tooShort'));
            return;
        }
        if (password !== confirmPassword) {
            setError(t('masterPassword.mismatch'));
            return;
        }

        setIsLoading(true);
        try {
            await invoke('enable_master_password', {
                password,
                timeoutSeconds: timeoutMinutes * 60,
            });
            onComplete();
        } catch (err) {
            setError(String(err));
        } finally {
            setIsLoading(false);
        }
    };

    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm" onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-full max-w-md mx-4 overflow-hidden animate-scale-in">
                {/* Header — matches LockScreen / AeroVault style */}
                <div className="flex items-center gap-2 px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <Shield size={18} className="text-emerald-500 dark:text-emerald-400" />
                    <span className="font-medium text-gray-900 dark:text-gray-100">{t('masterPassword.setupTitle')}</span>
                    <span className="text-xs text-gray-400 ml-auto">{t('masterPassword.setupDescription')}</span>
                    <button onClick={onClose} className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 p-1 ml-2">
                        <X size={16} />
                    </button>
                </div>

                {/* Shield icon */}
                <div className="flex justify-center pt-5 pb-2">
                    <div className="relative">
                        <Shield size={48} className="text-emerald-500 dark:text-emerald-400" />
                        <div className="absolute -bottom-1 -right-1 bg-emerald-500 rounded-full p-1">
                            <Lock size={10} className="text-white" />
                        </div>
                    </div>
                </div>

                {/* Form */}
                <form onSubmit={handleSubmit} className="p-5 pt-3 space-y-4">
                    {error && (
                        <div className="flex items-center gap-2 p-3 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg text-red-600 dark:text-red-400 text-sm">
                            <AlertTriangle size={16} />
                            {error}
                        </div>
                    )}

                    {/* Password */}
                    <div>
                        <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                            {t('masterPassword.password')}
                        </label>
                        <div className="relative">
                            <input
                                type={showPassword ? 'text' : 'password'}
                                value={password}
                                onChange={e => setPassword(e.target.value)}
                                className="w-full px-4 py-2.5 pr-10 bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg focus:ring-2 focus:ring-emerald-500 focus:border-transparent text-gray-900 dark:text-gray-100"
                                placeholder="••••••••"
                                autoFocus
                                disabled={isLoading}
                            />
                            <button
                                type="button"
                                onClick={() => setShowPassword(!showPassword)}
                                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                                tabIndex={-1}
                            >
                                {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                            </button>
                        </div>
                        <p className="mt-1 text-xs text-gray-500">{t('masterPassword.minLength')}</p>
                    </div>

                    {/* Confirm Password */}
                    <div>
                        <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                            {t('masterPassword.confirmPassword')}
                        </label>
                        <div className="relative">
                            <input
                                type={showPassword ? 'text' : 'password'}
                                value={confirmPassword}
                                onChange={e => setConfirmPassword(e.target.value)}
                                className="w-full px-4 py-2.5 pr-10 bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg focus:ring-2 focus:ring-emerald-500 focus:border-transparent text-gray-900 dark:text-gray-100"
                                placeholder="••••••••"
                                disabled={isLoading}
                            />
                            <button
                                type="button"
                                onClick={() => setShowPassword(!showPassword)}
                                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                                tabIndex={-1}
                            >
                                {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                            </button>
                        </div>
                    </div>

                    {/* Auto-lock Timeout */}
                    <div>
                        <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1 flex items-center gap-2">
                            <Clock size={14} />
                            {t('masterPassword.autoLockTimeout')}
                        </label>
                        <div className="flex items-center gap-3">
                            <input
                                type="range"
                                min={1}
                                max={60}
                                value={timeoutMinutes}
                                onChange={e => setTimeoutMinutes(parseInt(e.target.value))}
                                className="flex-1 accent-emerald-500"
                                disabled={isLoading}
                            />
                            <span className="text-sm font-medium w-16 text-right text-gray-700 dark:text-gray-300">
                                {timeoutMinutes} min
                            </span>
                        </div>
                    </div>

                    {/* Security info */}
                    <div className="flex items-start gap-2 p-3 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg text-blue-700 dark:text-blue-300 text-xs">
                        <Info size={14} className="mt-0.5 flex-shrink-0" />
                        <p>{t('masterPassword.setupInfo')}</p>
                    </div>

                    {/* Submit button with encrypting animation */}
                    <button
                        type="submit"
                        disabled={!password || !confirmPassword || isLoading}
                        className="w-full py-3 bg-emerald-600 hover:bg-emerald-500 disabled:bg-gray-400 dark:disabled:bg-gray-600 disabled:cursor-not-allowed text-white font-medium rounded-lg transition-colors flex items-center justify-center gap-2"
                    >
                        {isLoading ? (
                            <>
                                <svg className="h-5 w-5" viewBox="0 0 100 100" fill="currentColor">
                                    <path d="M31.6,3.5C5.9,13.6-6.6,42.7,3.5,68.4c10.1,25.7,39.2,38.3,64.9,28.1l-3.1-7.9c-21.3,8.4-45.4-2-53.8-23.3c-8.4-21.3,2-45.4,23.3-53.8L31.6,3.5z">
                                        <animateTransform attributeName="transform" type="rotate" dur="2s" from="0 50 50" to="360 50 50" repeatCount="indefinite" />
                                    </path>
                                    <path d="M42.3,39.6c5.7-4.3,13.9-3.1,18.1,2.7c4.3,5.7,3.1,13.9-2.7,18.1l4.1,5.5c8.8-6.5,10.6-19,4.1-27.7c-6.5-8.8-19-10.6-27.7-4.1L42.3,39.6z">
                                        <animateTransform attributeName="transform" type="rotate" dur="1s" from="0 50 50" to="-360 50 50" repeatCount="indefinite" />
                                    </path>
                                    <path d="M82,35.7C74.1,18,53.4,10.1,35.7,18S10.1,46.6,18,64.3l7.6-3.4c-6-13.5,0-29.3,13.5-35.3s29.3,0,35.3,13.5L82,35.7z">
                                        <animateTransform attributeName="transform" type="rotate" dur="2s" from="0 50 50" to="360 50 50" repeatCount="indefinite" />
                                    </path>
                                </svg>
                                <span className="transition-opacity duration-200">{t('settings.encrypting')}</span>
                            </>
                        ) : (
                            <>
                                <ShieldCheck size={20} />
                                {t('masterPassword.enable')}
                            </>
                        )}
                    </button>

                    {/* Cancel link */}
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={isLoading}
                        className="w-full text-center text-sm text-gray-500 hover:text-gray-700 dark:hover:text-gray-300 disabled:opacity-50"
                    >
                        {t('common.cancel')}
                    </button>
                </form>

                {/* Footer */}
                <div className="px-5 pb-4">
                    <p className="text-xs text-center text-gray-400">
                        {t('lockScreen.securityNote')}
                    </p>
                </div>
            </div>
        </div>
    );
};
