// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Dialog components - Modal dialogs for confirmation, input, etc.
 * i18n integrated
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../../i18n';
import { Folder, FileText, Copy, X, HardDrive, Calendar, Shield, ShieldCheck, Hash, FileType, Eye, EyeOff, AlertTriangle, Info, ShieldAlert, KeyRound, Lock, Clock, Link as LinkIcon, User, Users, Loader2, Files as FilesIcon } from 'lucide-react';
import { formatBytes } from '../../utils/formatters';
import { formatArchiveCipher } from '../../utils/archiveCipher';
import { getMimeType, getFileExtension } from '../Preview/utils/fileTypes';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { PROFILES_CHANGED_EVENT } from '../../utils/serverProfileStore';
import { dispatchMasterPasswordChanged } from '../../utils/masterPasswordEvents';
import { PasswordStrengthBar } from '../vault/PasswordStrengthBar';
import { PasswordMatchHint } from '../common/PasswordMatchHint';

// ============ Alert Dialog ============
interface AlertDialogProps {
    title: string;
    message: string;
    type?: 'warning' | 'error' | 'info';
    onClose: () => void;
    actionLabel?: string;
    onAction?: () => void;
    actionIcon?: React.ReactNode;
}

export const AlertDialog: React.FC<AlertDialogProps> = ({
    title,
    message,
    type = 'info',
    onClose,
    actionLabel,
    onAction,
    actionIcon,
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
                className={`bg-white dark:bg-gray-800 rounded-lg shadow-2xl max-w-md w-full mx-4 border ${accentMap[type]} overflow-hidden`}
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
                        {actionLabel && onAction ? t('common.cancel') : t('common.ok')}
                    </button>
                    {actionLabel && onAction && (
                        <button
                            onClick={onAction}
                            className={`px-4 py-2 text-sm text-white rounded-lg transition-colors flex items-center gap-2 ${actionColorMap[type]}`}
                        >
                            {actionIcon ?? <KeyRound size={14} />}
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
            <div className="bg-white dark:bg-gray-800 rounded-lg p-6 shadow-2xl max-w-sm animate-scale-in">
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
    description?: string;
}

export const InputDialog: React.FC<InputDialogProps> = ({
    title,
    defaultValue,
    onConfirm,
    onCancel,
    placeholder,
    isPassword = false,
    description,
}) => {
    const t = useTranslation();
    const [value, setValue] = useState(defaultValue);
    const [showPassword, setShowPassword] = useState(false);

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog" aria-modal="true" aria-label={title}>
            <div className="bg-white dark:bg-gray-800 rounded-lg p-6 shadow-2xl w-96 animate-scale-in">
                <h3 className={`text-lg font-semibold ${description ? 'mb-2' : 'mb-4'} text-gray-900 dark:text-gray-100`}>{title}</h3>
                {description && (
                    <p className="mb-4 text-sm text-gray-600 dark:text-gray-400">{description}</p>
                )}
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

// ============ Archive Password Dialog (encrypted zip / 7z / rar) ============
// A dedicated draggable mini-modal for unlocking an encrypted general archive.
// Unlike the generic InputDialog it owns the verify lifecycle: a wrong password
// stays IN the same modal with an inline error (no close/reopen flash), the
// Decrypt button shows a spinner while verifying, and the title bar names the
// archive plus its real format / detected cipher. AeroVault uses its own dialog.
interface ArchivePasswordDialogProps {
    /** Archive file name, shown in the draggable title bar. */
    archiveName: string;
    /** Short format tag for the badge, e.g. "zip", "7z", "rar". */
    format: string;
    /** Real detected cipher for the badge, e.g. "AES-256", "ZipCrypto". Absent
     *  until detection resolves (the badge simply does not render meanwhile). */
    cipher?: string;
    /** Runs the decrypt/extract. Resolve = success (dialog closes); throw an
     *  Error whose message is shown inline so the user retries without a flash. */
    onSubmit: (password: string) => Promise<void>;
    onClose: () => void;
}

export const ArchivePasswordDialog: React.FC<ArchivePasswordDialogProps> = ({ archiveName, format, cipher, onSubmit, onClose }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [verifying, setVerifying] = useState(false);
    const [error, setError] = useState('');

    const handleSubmit = async (e: React.FormEvent) => {
        e.preventDefault();
        if (!password || verifying) return;
        setVerifying(true);
        setError('');
        try {
            await onSubmit(password);
            onClose();
        } catch (err) {
            // Stay mounted: surface the error inline and let the user retry. No
            // close/reopen flash; the input keeps focus for the next attempt.
            setError(err instanceof Error ? err.message : String(err));
            setVerifying(false);
        }
    };

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[18vh] bg-black/50 backdrop-blur-sm"
            onClick={(e) => { if (e.target === e.currentTarget && !verifying) onClose(); }}
        >
            <form
                {...modalDrag.panelProps}
                onSubmit={handleSubmit}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-full max-w-sm mx-4 overflow-hidden animate-scale-in"
            >
                {/* Draggable title bar: names the archive being unlocked + close. */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center gap-2 px-3 py-2 border-b border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/60 cursor-grab active:cursor-grabbing"
                >
                    <Lock size={14} className="text-sky-500 shrink-0 pointer-events-none" />
                    <span className="text-xs font-semibold text-gray-700 dark:text-gray-200 truncate pointer-events-none" title={archiveName}>{archiveName}</span>
                    <button type="button" onClick={onClose} disabled={verifying} aria-label={t('common.close')} className="ml-auto shrink-0 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 p-0.5 rounded disabled:opacity-40">
                        <X size={15} />
                    </button>
                </div>

                <div className="p-4 space-y-3">
                    {/* Real format + detected-cipher badges (cipher is filled once
                        backend detection resolves; weak ZipCrypto reads amber). */}
                    <div className="flex items-center gap-1.5">
                        <span className="px-1.5 py-0.5 rounded text-[10px] font-semibold uppercase tracking-wide bg-gray-200 dark:bg-gray-700 text-gray-600 dark:text-gray-300">{format}</span>
                        {cipher && (() => {
                            const c = formatArchiveCipher(cipher);
                            return (
                                <span className={`px-1.5 py-0.5 rounded text-[10px] font-semibold tracking-wide ${c.strong ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300' : 'bg-amber-500/15 text-amber-700 dark:text-amber-300'}`} title={c.strong ? undefined : t('contextMenu.zipCryptoLegacy')}>{c.label}</span>
                            );
                        })()}
                    </div>
                    <p className="text-xs text-gray-600 dark:text-gray-400 flex items-center gap-1.5">
                        <KeyRound size={13} className="shrink-0" /> {t('contextMenu.enterArchivePassword')}
                    </p>
                    <div className="relative">
                        <input
                            type={showPassword ? 'text' : 'password'}
                            value={password}
                            autoFocus
                            disabled={verifying}
                            onChange={(e) => { setPassword(e.target.value); if (error) setError(''); }}
                            placeholder={t('contextMenu.enterArchivePassword')}
                            className={`w-full px-3 py-2 pr-9 text-sm rounded-md border bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 outline-none focus:ring-2 ${error ? 'border-red-500 dark:border-red-500 focus:ring-red-500' : 'border-gray-300 dark:border-gray-600 focus:ring-sky-500'}`}
                            aria-invalid={error ? true : undefined}
                        />
                        <button type="button" onClick={() => setShowPassword((v) => !v)} tabIndex={-1} aria-label={showPassword ? t('extractWindow.hidePassword') : t('extractWindow.showPassword')} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200">
                            {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                        </button>
                    </div>
                    {error && <p className="text-xs text-red-500" role="alert">{error}</p>}
                    <div className="flex justify-end gap-2 pt-1">
                        <button type="button" onClick={onClose} disabled={verifying} className="px-3 py-1.5 text-sm rounded-md text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40">
                            {t('common.cancel')}
                        </button>
                        <button type="submit" disabled={!password || verifying} className="px-3 py-1.5 text-sm rounded-md bg-sky-500 text-white hover:bg-sky-600 disabled:opacity-50 flex items-center gap-1.5">
                            {verifying && <Loader2 size={14} className="animate-spin" />}
                            {t('contextMenu.decrypt')}
                        </button>
                    </div>
                </div>
            </form>
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
            <div className="bg-white dark:bg-gray-800 rounded-lg p-6 shadow-2xl max-w-md animate-scale-in">
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
    is_readonly?: boolean | null;
    is_hidden?: boolean | null;
    // Checksum (optional, calculated on demand)
    checksum?: {
        md5?: string;
        sha1?: string;
        sha256?: string;
        sha512?: string;
        blake3?: string;
        // Server-only digests: present only when the backend exposes
        // them (OneDrive QuickXorHash, Dropbox content_hash). Never
        // computed locally.
        quickxor?: string;
        dropbox?: string;
        calculating?: boolean;
    };
}

// Label/description/icon for an OpenDrive privacy level (#252). Shared by the
// single- and multi-file Properties dialogs so the editor wording stays in one
// place. Reuses the already-translated `properties.privacy*` keys.
const privacyLevelMeta = (
    t: ReturnType<typeof useTranslation>,
    token: 'public' | 'private' | 'hidden',
): { label: string; description: string; icon: React.ReactNode } => {
    if (token === 'public') {
        return { label: t('properties.privacyPublic') || 'Public', description: t('properties.privacyPublicDesc') || 'Anyone with the link can access this item.', icon: <Eye size={16} /> };
    }
    if (token === 'hidden') {
        return { label: t('properties.privacyHidden') || 'Hidden', description: t('properties.privacyHiddenDesc') || 'Accessible by direct link only; not searchable.', icon: <EyeOff size={16} /> };
    }
    return { label: t('properties.privacyPrivate') || 'Private', description: t('properties.privacyPrivateDesc') || 'Only the account owner can access this item.', icon: <Lock size={16} /> };
};

interface PropertiesDialogProps {
    file: FileProperties;
    onClose: () => void;
    onCalculateChecksum?: (algorithm: 'md5' | 'sha1' | 'sha256' | 'sha512' | 'blake3') => void;
    onCalculateFolderSize?: () => void;
    folderSize?: { total_bytes: number; file_count: number; dir_count: number } | null;
    folderSizeCalculating?: boolean;
    /** Tab to open on first render (#252: deep-link to Permissions). */
    initialTab?: 'general' | 'permissions' | 'checksum';
    /** OpenDrive (#252): when provided, the Permissions tab renders an editable
     *  privacy chooser (Private/Public/Hidden) that calls this to apply. */
    onPrivacyChange?: (level: 'public' | 'private' | 'hidden') => void | Promise<void>;
}

export const PropertiesDialog: React.FC<PropertiesDialogProps> = ({
    file,
    onClose,
    onCalculateChecksum,
    onCalculateFolderSize,
    folderSize,
    folderSizeCalculating = false,
    initialTab,
    onPrivacyChange,
}) => {
    const t = useTranslation();
    const [copiedField, setCopiedField] = useState<string | null>(null);
    const [activeTab, setActiveTab] = useState<'general' | 'permissions' | 'checksum'>(initialTab || 'general');
    const [selectedPrivacy, setSelectedPrivacy] = useState<'public' | 'private' | 'hidden'>(
        (file.permissions || '').trim().toLowerCase() === 'public' ? 'public' :
        (file.permissions || '').trim().toLowerCase() === 'hidden' ? 'hidden' : 'private'
    );
    const [applyingPrivacy, setApplyingPrivacy] = useState(false);

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
        if (!dateStr) return '-';
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
        if (!perms) return { display: '-' };

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
        return { display: '-' };
    };

    const permInfo = getPermissionsInfo();

    // Privacy-token rendering for providers that surface a per-item
    // visibility level instead of Unix mode bits (OpenDrive, 4shared,
    // FileLu). The provider populates `permissions` with one of
    // `public` | `private` | `hidden`; we render a labeled row with a
    // human-readable description instead of mangling the token through
    // the Unix permissions parser.
    const getPrivacyInfo = (): { token: 'public' | 'private' | 'hidden'; label: string; description: string; icon: React.ReactNode } | null => {
        const raw = (file.permissions || '').trim().toLowerCase();
        if (raw === 'public') {
            return {
                token: 'public',
                label: t('properties.privacyPublic') || 'Public',
                description: t('properties.privacyPublicDesc') || 'Anyone with the link can access this item.',
                icon: <Eye size={16} />,
            };
        }
        if (raw === 'private') {
            return {
                token: 'private',
                label: t('properties.privacyPrivate') || 'Private',
                description: t('properties.privacyPrivateDesc') || 'Only the account owner can access this item.',
                icon: <Lock size={16} />,
            };
        }
        if (raw === 'hidden') {
            return {
                token: 'hidden',
                label: t('properties.privacyHidden') || 'Hidden',
                description: t('properties.privacyHiddenDesc') || 'Accessible by direct link only; not searchable.',
                icon: <EyeOff size={16} />,
            };
        }
        return null;
    };

    const privacyInfo = getPrivacyInfo();

    // The editor needs metadata for every option, not just the current one.
    const privacyMetaFor = (token: 'public' | 'private' | 'hidden') => privacyLevelMeta(t, token);

    const applyPrivacy = async () => {
        if (!onPrivacyChange) return;
        setApplyingPrivacy(true);
        try {
            await onPrivacyChange(selectedPrivacy);
        } finally {
            setApplyingPrivacy(false);
        }
    };

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

    // Checksum row helper. Hash is rendered with `break-all` so the full digest
    // is readable without truncation; the dialog widens (see modal class) to
    // accommodate SHA-512 (128 hex) and BLAKE3 (64 hex) on a single line where
    // possible, wrapping cleanly otherwise.
    // `serverOnly` rows (quickxor, dropbox) cannot be computed locally:
    // they exist only when the backend already returned them, so no
    // "Calculate" action is offered, just the value plus an honest note.
    const ChecksumRow: React.FC<{
        label: string;
        value?: string;
        algorithm?: 'md5' | 'sha1' | 'sha256' | 'sha512' | 'blake3';
        serverOnly?: boolean;
    }> = ({ label, value, algorithm, serverOnly }) => (
        <div className="flex items-start gap-2 mb-2">
            <span className="text-xs text-gray-500 w-20 shrink-0 pt-1">{label}:</span>
            {value ? (
                <code
                    className="flex-1 text-xs font-mono bg-gray-100 dark:bg-gray-700 px-2 py-1 rounded break-all leading-relaxed"
                    title={value}
                >
                    {value}
                </code>
            ) : serverOnly ? (
                <span className="flex-1 text-xs text-gray-400 italic pt-1">
                    {t('properties.checksumServerOnly')}
                </span>
            ) : (
                <button
                    onClick={() => algorithm && onCalculateChecksum?.(algorithm)}
                    disabled={file.checksum?.calculating}
                    className="text-xs text-blue-500 hover:text-blue-600 disabled:text-gray-400"
                >
                    {file.checksum?.calculating ? t('properties.calculating') : t('properties.calculate')}
                </button>
            )}
            {value && (
                <button
                    onClick={() => copyToClipboard(value, label)}
                    className="text-gray-400 hover:text-blue-500 shrink-0 pt-1"
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
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[560px] max-w-[92vw] max-h-[85vh] overflow-hidden animate-scale-in"
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
                                            <div className="flex items-center gap-2">
                                                <div className="text-sm text-gray-900 dark:text-gray-100">
                                                    {formatBytes(folderSize.total_bytes)} ({folderSize.file_count.toLocaleString()} {t('properties.files')}, {folderSize.dir_count.toLocaleString()} {t('properties.folders')})
                                                </div>
                                                {folderSizeCalculating && (
                                                    <Loader2 size={12} className="animate-spin text-blue-500 shrink-0" />
                                                )}
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
                                            <span className="text-sm text-gray-500">{'\u2014'}</span>
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
                            {onPrivacyChange ? (
                                <div className="space-y-2">
                                    <div className="text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-1">
                                        {t('properties.visibility') || 'Visibility'}
                                    </div>
                                    {(['private', 'public', 'hidden'] as const).map((lvl) => {
                                        const meta = privacyMetaFor(lvl);
                                        const isCurrent = privacyInfo?.token === lvl;
                                        return (
                                            <label
                                                key={lvl}
                                                className={`flex items-start gap-2 p-2 rounded-lg border cursor-pointer transition-colors ${selectedPrivacy === lvl ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/20' : 'border-gray-200 dark:border-gray-700 hover:border-gray-300 dark:hover:border-gray-600'}`}
                                            >
                                                <input
                                                    type="radio"
                                                    name="opendrive-privacy"
                                                    checked={selectedPrivacy === lvl}
                                                    onChange={() => setSelectedPrivacy(lvl)}
                                                    className="mt-1"
                                                />
                                                <div className="flex-1 min-w-0">
                                                    <div className="flex items-center gap-1.5 text-sm font-medium text-gray-900 dark:text-gray-100">
                                                        {meta.icon}
                                                        {meta.label}
                                                        {isCurrent && <span className="text-xs font-normal text-gray-400">({t('common.current') || 'current'})</span>}
                                                    </div>
                                                    <div className="text-xs text-gray-500 dark:text-gray-400">{meta.description}</div>
                                                </div>
                                            </label>
                                        );
                                    })}
                                    <button
                                        onClick={applyPrivacy}
                                        disabled={applyingPrivacy || (privacyInfo?.token === selectedPrivacy)}
                                        className="mt-1 w-full py-2 rounded-lg text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:bg-gray-300 dark:disabled:bg-gray-600 disabled:cursor-not-allowed flex items-center justify-center gap-2 transition-colors"
                                    >
                                        {applyingPrivacy && <Loader2 size={14} className="animate-spin" />}
                                        {t('properties.applyPrivacy') || 'Apply'}
                                    </button>
                                </div>
                            ) : privacyInfo ? (
                                <>
                                    <PropertyRow
                                        icon={privacyInfo.icon}
                                        label={t('properties.visibility') || 'Visibility'}
                                        value={privacyInfo.label}
                                    />
                                    <div className="text-xs text-gray-500 dark:text-gray-400 pl-7 pb-3">
                                        {privacyInfo.description}
                                    </div>
                                </>
                            ) : null}

                            {!privacyInfo && (file.permissions || file.permissions_mode != null) && (
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
                            )}

                            {file.is_readonly != null && (
                                <PropertyRow
                                    icon={<Lock size={16} />}
                                    label={t('properties.readOnly')}
                                    value={file.is_readonly ? t('common.yes') : t('common.no')}
                                />
                            )}
                            {file.is_hidden != null && (
                                <PropertyRow
                                    icon={file.is_hidden ? <EyeOff size={16} /> : <Eye size={16} />}
                                    label={t('properties.hidden')}
                                    value={file.is_hidden ? t('common.yes') : t('common.no')}
                                />
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
                            {!onPrivacyChange && !privacyInfo && !file.permissions && file.permissions_mode == null && !file.owner && !file.group && file.inode == null && file.hard_links == null && file.is_readonly == null && file.is_hidden == null && (
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
                                    <ChecksumRow label="SHA-1" value={file.checksum?.sha1} algorithm="sha1" />
                                    <ChecksumRow label="SHA-256" value={file.checksum?.sha256} algorithm="sha256" />
                                    <ChecksumRow label="SHA-512" value={file.checksum?.sha512} algorithm="sha512" />
                                    <ChecksumRow label="BLAKE3" value={file.checksum?.blake3} algorithm="blake3" />
                                    {/* Server-only digests: shown only when the
                                        backend (OneDrive, Dropbox) actually
                                        returned them. No local fallback. */}
                                    {file.checksum?.quickxor && (
                                        <ChecksumRow label="QuickXor" value={file.checksum.quickxor} serverOnly />
                                    )}
                                    {file.checksum?.dropbox && (
                                        <ChecksumRow label="Dropbox" value={file.checksum.dropbox} serverOnly />
                                    )}
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

// ============ Multi-File Properties Dialog ============
//
// Aggregate Properties view for a multi-file selection (Windows-style).
// Shows total size, kind breakdown, common parent path, oldest/newest mtime,
// and a "Mixed" indicator for permissions when not uniform across the set.

export interface MultiFileProperties {
    files: FileProperties[];
    isRemote: boolean;
    protocol?: string;
}

interface MultiFilePropertiesDialogProps {
    selection: MultiFileProperties;
    onClose: () => void;
    /** OpenDrive (#252): when provided, renders a privacy chooser that applies
     *  the chosen level to every selected item. */
    onPrivacyChange?: (level: 'public' | 'private' | 'hidden') => void | Promise<void>;
}

const computeCommonParent = (paths: string[]): string => {
    if (paths.length === 0) return '';
    const sep = paths[0].includes('\\') && !paths[0].includes('/') ? '\\' : '/';
    const splitPaths = paths.map(p => p.split(sep));
    // Drop the trailing filename component before comparing.
    const minLen = Math.min(...splitPaths.map(s => Math.max(s.length - 1, 0)));
    const out: string[] = [];
    for (let i = 0; i < minLen; i++) {
        const seg = splitPaths[0][i];
        if (splitPaths.every(s => s[i] === seg)) out.push(seg);
        else break;
    }
    const joined = out.join(sep);
    if (joined.length > 0) return joined;
    // POSIX absolute path with no shared prefix beyond root.
    if (paths.every(p => p.startsWith('/'))) return '/';
    return '';
};

export const MultiFilePropertiesDialog: React.FC<MultiFilePropertiesDialogProps> = ({
    selection,
    onClose,
    onPrivacyChange,
}) => {
    const t = useTranslation();
    const { files, isRemote, protocol } = selection;

    // OpenDrive (#252) privacy editor state. Default to the common level when
    // the whole selection shares one, otherwise Private (max privacy).
    const initialPrivacy = ((): 'public' | 'private' | 'hidden' => {
        const set = new Set(files.map(f => (f.permissions || '').trim().toLowerCase()).filter(Boolean));
        const only = set.size === 1 ? Array.from(set)[0] : null;
        return only === 'public' || only === 'hidden' ? only : 'private';
    })();
    const [selectedPrivacy, setSelectedPrivacy] = useState<'public' | 'private' | 'hidden'>(initialPrivacy);
    const [applyingPrivacy, setApplyingPrivacy] = useState(false);
    const applyPrivacyToAll = async () => {
        if (!onPrivacyChange) return;
        setApplyingPrivacy(true);
        try { await onPrivacyChange(selectedPrivacy); } finally { setApplyingPrivacy(false); }
    };

    useEffect(() => {
        document.documentElement.classList.add('modal-open');
        return () => { document.documentElement.classList.remove('modal-open'); };
    }, []);

    useEffect(() => {
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        window.addEventListener('keydown', onKey);
        return () => window.removeEventListener('keydown', onKey);
    }, [onClose]);

    const fileCount = files.filter(f => !f.is_dir).length;
    const folderCount = files.filter(f => f.is_dir).length;
    const knownSizes = files.filter(f => !f.is_dir && typeof f.size === 'number');
    const totalSize = knownSizes.reduce((acc, f) => acc + (f.size || 0), 0);
    const sizesUnknown = files.some(f => !f.is_dir && (f.size == null));

    const commonParent = computeCommonParent(files.map(f => f.path));

    const mtimes = files
        .map(f => (f.modified ? new Date(f.modified).getTime() : NaN))
        .filter(t => !Number.isNaN(t));
    const oldestMtime = mtimes.length ? new Date(Math.min(...mtimes)) : null;
    const newestMtime = mtimes.length ? new Date(Math.max(...mtimes)) : null;

    const formatDate = (date: Date | null): string => {
        if (!date) return '-';
        try { return date.toLocaleString(); } catch { return '-'; }
    };

    // Permission strings: collapse to a single value if uniform, else "Mixed".
    const permsSet = new Set(files.map(f => f.permissions || '').filter(Boolean));
    const permsUniform = permsSet.size === 1 ? Array.from(permsSet)[0] : null;
    const permsMixed = permsSet.size > 1;
    const privacyUniform: 'public' | 'private' | 'hidden' | null =
        permsUniform === 'public' || permsUniform === 'private' || permsUniform === 'hidden' ? permsUniform : null;

    // Read-only / hidden mixed-state (only meaningful when set on every entry).
    const aggregateBool = (pick: (f: FileProperties) => boolean | null | undefined): 'all' | 'none' | 'some' | 'unknown' => {
        const known = files.filter(f => pick(f) != null);
        if (known.length === 0) return 'unknown';
        const yes = known.filter(f => pick(f) === true).length;
        if (yes === known.length) return 'all';
        if (yes === 0) return 'none';
        return 'some';
    };
    const readonlyState = aggregateBool(f => f.is_readonly);
    const hiddenState = aggregateBool(f => f.is_hidden);

    const tristateLabel = (state: 'all' | 'none' | 'some' | 'unknown'): string | null => {
        if (state === 'all') return t('common.yes');
        if (state === 'none') return t('common.no');
        if (state === 'some') return t('properties.mixed');
        return null;
    };

    const Row: React.FC<{ icon: React.ReactNode; label: string; value: React.ReactNode; mono?: boolean }> =
        ({ icon, label, value, mono = false }) => (
        <div className="flex items-start gap-3 py-2 border-b border-gray-100 dark:border-gray-700 last:border-0">
            <div className="text-gray-400 mt-0.5">{icon}</div>
            <div className="flex-1 min-w-0">
                <div className="text-xs text-gray-500 dark:text-gray-400">{label}</div>
                <div className={`text-sm text-gray-900 dark:text-gray-100 break-all ${mono ? 'font-mono' : ''}`}>
                    {value}
                </div>
            </div>
        </div>
    );

    return (
        <div
            className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
            role="dialog"
            aria-modal="true"
            aria-label={t('properties.multipleSelected', { count: files.length })}
            onClick={onClose}
        >
            <div
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[560px] max-w-[92vw] max-h-[85vh] overflow-hidden animate-scale-in"
                onClick={(e) => e.stopPropagation()}
            >
                <div className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3">
                        <FilesIcon size={24} className="text-blue-500" />
                        <div>
                            <h3 className="font-semibold text-gray-900 dark:text-gray-100">
                                {t('properties.multipleSelected', { count: files.length })}
                            </h3>
                            <span className="text-xs text-gray-500">
                                {isRemote ? `${t('properties.remote')} (${protocol?.toUpperCase() || 'FTP'})` : t('properties.local')}
                            </span>
                        </div>
                    </div>
                    <button
                        onClick={onClose}
                        className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 p-1"
                        aria-label={t('common.close')}
                    >
                        <X size={20} />
                    </button>
                </div>

                <div className="p-4 overflow-y-auto max-h-[calc(85vh-140px)]">
                    <Row
                        icon={<FilesIcon size={16} />}
                        label={t('properties.contains')}
                        value={`${fileCount.toLocaleString()} ${t('properties.files')}, ${folderCount.toLocaleString()} ${t('properties.folders')}`}
                    />
                    <Row
                        icon={<Hash size={16} />}
                        label={t('properties.totalSize')}
                        value={
                            knownSizes.length === 0 && folderCount > 0 ? (
                                <span className="text-gray-500">{t('properties.foldersOnly')}</span>
                            ) : (
                                <>
                                    {formatBytes(totalSize)}
                                    <span className="text-gray-500"> ({totalSize.toLocaleString()} bytes)</span>
                                    {sizesUnknown && (
                                        <span className="ml-2 text-xs text-amber-600 dark:text-amber-400">
                                            {t('properties.sizesPartial')}
                                        </span>
                                    )}
                                </>
                            )
                        }
                    />
                    {commonParent && (
                        <Row
                            icon={<Folder size={16} />}
                            label={t('properties.location')}
                            value={commonParent}
                            mono
                        />
                    )}
                    {oldestMtime && newestMtime && (
                        oldestMtime.getTime() === newestMtime.getTime() ? (
                            <Row
                                icon={<Calendar size={16} />}
                                label={t('properties.modified')}
                                value={formatDate(oldestMtime)}
                            />
                        ) : (
                            <>
                                <Row
                                    icon={<Calendar size={16} />}
                                    label={t('properties.oldestModified')}
                                    value={formatDate(oldestMtime)}
                                />
                                <Row
                                    icon={<Calendar size={16} />}
                                    label={t('properties.newestModified')}
                                    value={formatDate(newestMtime)}
                                />
                            </>
                        )
                    )}
                    {(permsUniform || permsMixed) && (
                        <Row
                            icon={<Shield size={16} />}
                            label={t('properties.permissionsText')}
                            value={permsMixed ? t('properties.mixed') : (permsUniform || '')}
                            mono={!permsMixed}
                        />
                    )}
                    {tristateLabel(readonlyState) && (
                        <Row
                            icon={<Lock size={16} />}
                            label={t('properties.readOnly')}
                            value={tristateLabel(readonlyState)!}
                        />
                    )}
                    {tristateLabel(hiddenState) && (
                        <Row
                            icon={hiddenState === 'all' ? <EyeOff size={16} /> : <Eye size={16} />}
                            label={t('properties.hidden')}
                            value={tristateLabel(hiddenState)!}
                        />
                    )}

                    {/* OpenDrive (#252): apply a privacy level to the whole selection */}
                    {onPrivacyChange && (
                        <div className="mt-4 pt-3 border-t border-gray-200 dark:border-gray-700">
                            <div className="flex items-center justify-between mb-2">
                                <div className="text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide">
                                    {t('properties.visibility') || 'Visibility'}
                                </div>
                                <div className="text-xs text-gray-400">
                                    {permsMixed ? t('properties.mixed') : (privacyUniform ? privacyLevelMeta(t, privacyUniform).label : '')}
                                </div>
                            </div>
                            <div className="space-y-2">
                                {(['private', 'public', 'hidden'] as const).map((lvl) => {
                                    const meta = privacyLevelMeta(t, lvl);
                                    return (
                                        <label
                                            key={lvl}
                                            className={`flex items-start gap-2 p-2 rounded-lg border cursor-pointer transition-colors ${selectedPrivacy === lvl ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/20' : 'border-gray-200 dark:border-gray-700 hover:border-gray-300 dark:hover:border-gray-600'}`}
                                        >
                                            <input
                                                type="radio"
                                                name="opendrive-privacy-multi"
                                                checked={selectedPrivacy === lvl}
                                                onChange={() => setSelectedPrivacy(lvl)}
                                                className="mt-1"
                                            />
                                            <div className="flex-1 min-w-0">
                                                <div className="flex items-center gap-1.5 text-sm font-medium text-gray-900 dark:text-gray-100">
                                                    {meta.icon}
                                                    {meta.label}
                                                </div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400">{meta.description}</div>
                                            </div>
                                        </label>
                                    );
                                })}
                            </div>
                            <button
                                onClick={applyPrivacyToAll}
                                disabled={applyingPrivacy}
                                className="mt-2 w-full py-2 rounded-lg text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:bg-gray-300 dark:disabled:bg-gray-600 disabled:cursor-not-allowed flex items-center justify-center gap-2 transition-colors"
                            >
                                {applyingPrivacy && <Loader2 size={14} className="animate-spin" />}
                                {t('properties.applyPrivacyAll', { count: files.length })}
                            </button>
                        </div>
                    )}

                    <div className="mt-4">
                        <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">
                            {t('properties.selectedItems')}
                        </div>
                        <div className="border border-gray-200 dark:border-gray-700 rounded max-h-44 overflow-y-auto">
                            {files.map((f, idx) => (
                                <div
                                    key={`${f.path}-${idx}`}
                                    className="flex items-center gap-2 px-3 py-1.5 text-xs border-b border-gray-100 dark:border-gray-700 last:border-0"
                                >
                                    {f.is_dir ? (
                                        <Folder size={12} className="text-yellow-500 shrink-0" />
                                    ) : (
                                        <FileText size={12} className="text-blue-500 shrink-0" />
                                    )}
                                    <span className="truncate text-gray-700 dark:text-gray-300" title={f.path}>{f.name}</span>
                                    {!f.is_dir && f.size != null && (
                                        <span className="ml-auto text-gray-500 shrink-0">{formatBytes(f.size)}</span>
                                    )}
                                </div>
                            ))}
                        </div>
                    </div>
                </div>

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
    bootstrapMode?: boolean;
}

export const MasterPasswordSetupDialog: React.FC<MasterPasswordSetupDialogProps> = ({ onComplete, onClose, bootstrapMode = false }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
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
            if (bootstrapMode) {
                await invoke('bootstrap_master_credential_store', {
                    password,
                    timeoutSeconds: timeoutMinutes * 60,
                });
            } else {
                await invoke('enable_master_password', {
                    password,
                    timeoutSeconds: timeoutMinutes * 60,
                });
            }
            // Vault is now (re-)initialized; notify UserDropdown so the avatar
            // appears even if it mounted before CredentialStore was ready.
            try { window.dispatchEvent(new CustomEvent(PROFILES_CHANGED_EVENT)); } catch { /* best effort */ }
            dispatchMasterPasswordChanged({
                enabled: true,
                isLocked: false,
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
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-full max-w-md mx-4 overflow-hidden animate-scale-in"
            >
                {/* Header: drag moves this modal, not the native app window. */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center gap-2 px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <Shield size={18} className="text-emerald-500 dark:text-emerald-400 pointer-events-none" />
                    <span className="font-medium text-gray-900 dark:text-gray-100 pointer-events-none">{t('masterPassword.setupTitle')}</span>
                    <span className="text-xs text-gray-400 ml-auto pointer-events-none">{bootstrapMode ? 'Keyring unavailable: bootstrap master mode' : t('masterPassword.setupDescription')}</span>
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
                        <div className="mt-2">
                            <PasswordStrengthBar password={password} />
                        </div>
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
                        <PasswordMatchHint password={password} confirm={confirmPassword} />
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
