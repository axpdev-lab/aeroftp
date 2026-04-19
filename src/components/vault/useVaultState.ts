// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Shield, ShieldCheck, ShieldAlert } from 'lucide-react';
import { ArchiveEntry, AeroVaultMeta } from '../../types';
import { useTranslation } from '../../i18n';
import { guardedUnlisten } from '../../hooks/useTauriListener';

// --- Error mapping ---

/** Map raw Rust error messages to user-friendly i18n keys */
const ERROR_PATTERNS: [RegExp, string][] = [
    [/invalid (password|hmac|key|mac)/i, 'vault.errors.wrongPassword'],
    [/wrong password/i, 'vault.errors.wrongPassword'],
    [/decryption failed/i, 'vault.errors.wrongPassword'],
    [/authentication failed/i, 'vault.errors.wrongPassword'],
    [/not (a valid |an? )?(aerovault|vault)/i, 'vault.errors.notAVault'],
    [/unsupported (vault )?version/i, 'vault.errors.unsupportedVersion'],
    [/corrupt/i, 'vault.errors.corrupted'],
    [/no such file|not found|does not exist/i, 'vault.errors.fileNotFound'],
    [/permission denied/i, 'vault.errors.permissionDenied'],
    [/already exists/i, 'vault.errors.alreadyExists'],
    [/directory (too large|exceeds)/i, 'vault.errors.directoryTooLarge'],
    [/invalid path/i, 'vault.errors.invalidPath'],
    [/disk (full|space)|no space/i, 'vault.errors.diskFull'],
];

function mapVaultError(e: unknown, t: (key: string) => string): string {
    const raw = String(e);
    for (const [pattern, key] of ERROR_PATTERNS) {
        if (pattern.test(raw)) {
            const mapped = t(key);
            if (mapped && mapped !== key) return mapped;
        }
    }
    // Fallback: strip internal details (offsets, hex, stack traces)
    const cleaned = raw
        .replace(/at offset \d+/gi, '')
        .replace(/0x[0-9a-f]+/gi, '')
        .replace(/\s{2,}/g, ' ')
        .trim();
    return cleaned || raw;
}

// --- Exported types ---

export type VaultMode = 'home' | 'create' | 'open' | 'browse';

export type SecurityLevel = 'standard' | 'advanced' | 'paranoid';

export interface VaultSecurityInfo {
    version: number;
    cascadeMode: boolean;
    level: SecurityLevel;
}

export interface IconResult {
    icon: React.ReactNode;
}

export interface IconProvider {
    getFileIcon: (name: string, size?: number) => IconResult;
    getFolderIcon: (size?: number) => IconResult;
}

export interface RecentVault {
    id: number;
    vault_path: string;
    vault_name: string;
    security_level: string;
    vault_version: number;
    cascade_mode: boolean;
    file_count: number;
    last_opened_at: number;
    created_at: number;
}

interface VaultV2Info {
    version: number;
    cascade_mode: boolean;
    chunk_size: number;
    created: string;
    modified: string;
    description: string | null;
    file_count: number;
    files: { name: string; size: number; is_dir: boolean; modified: string }[];
}

export interface FolderScanResult {
    file_count: number;
    dir_count: number;
    total_size: number;
}

export interface FolderProgress {
    current: number;
    total: number;
    current_file: string;
}

// Security level configuration — hardcoded labels (no i18n, technical terms)
export const securityLevels = {
    standard: {
        icon: Shield,
        color: 'text-blue-400',
        bgColor: 'bg-blue-600',
        borderColor: 'border-blue-500',
        label: 'Standard',
        version: 1,
        cascade: false,
        features: ['AES-256-GCM', 'Argon2id 64 MB', 'Fast encryption'],
        description: 'AES-256-GCM · Argon2id 64 MB · Fast'
    },
    advanced: {
        icon: ShieldCheck,
        color: 'text-emerald-400',
        bgColor: 'bg-emerald-600',
        borderColor: 'border-emerald-500',
        label: 'Advanced',
        version: 2,
        cascade: false,
        features: ['AES-256-GCM-SIV', 'Argon2id 128 MB', 'Encrypted filenames', 'HMAC-SHA512 header'],
        description: 'Nonce-resistant · Encrypted filenames · 128 MB KDF'
    },
    paranoid: {
        icon: ShieldAlert,
        color: 'text-purple-400',
        bgColor: 'bg-purple-600',
        borderColor: 'border-purple-500',
        label: 'Paranoid',
        version: 2,
        cascade: true,
        features: ['AES-256-GCM-SIV', 'ChaCha20-Poly1305 cascade', 'Argon2id 128 MB', 'Double encryption'],
        description: 'AES + ChaCha20 cascade · Double encryption'
    }
};

// --- Hook props & return type ---

export interface UseVaultStateProps {
    initialMode?: VaultMode;
    initialPath?: string;
    initialFiles?: string[];
    initialFolderPath?: string;
    isConnected?: boolean;
    onClose: () => void;
}

export interface VaultState {
    // Mode
    mode: VaultMode;
    setMode: (mode: VaultMode) => void;

    // Core state
    vaultPath: string;
    setVaultPath: (path: string) => void;
    password: string;
    setPassword: (pw: string) => void;
    confirmPassword: string;
    setConfirmPassword: (pw: string) => void;
    description: string;
    setDescription: (desc: string) => void;
    showPassword: boolean;
    setShowPassword: (show: boolean) => void;

    // Loading / feedback
    loading: boolean;
    error: string | null;
    setError: (err: string | null) => void;
    success: string | null;
    setSuccess: (msg: string | null) => void;

    // Entries
    entries: ArchiveEntry[];
    meta: AeroVaultMeta | null;

    // Directory navigation
    currentDir: string;
    setCurrentDir: (dir: string) => void;
    newDirName: string;
    setNewDirName: (name: string) => void;
    showNewDirDialog: boolean;
    setShowNewDirDialog: (show: boolean) => void;

    // Change password
    changingPassword: boolean;
    setChangingPassword: (changing: boolean) => void;
    newPassword: string;
    setNewPassword: (pw: string) => void;
    confirmNewPassword: string;
    setConfirmNewPassword: (pw: string) => void;

    // Remote vault
    remoteVaultPath: string;
    setRemoteVaultPath: (path: string) => void;
    remoteLocalPath: string;
    remoteLoading: boolean;
    showRemoteInput: boolean;
    setShowRemoteInput: (show: boolean) => void;

    // Security
    securityLevel: SecurityLevel;
    setSecurityLevel: (level: SecurityLevel) => void;
    vaultSecurity: VaultSecurityInfo | null;
    setVaultSecurity: (sec: VaultSecurityInfo | null) => void;
    showLevelDropdown: boolean;
    setShowLevelDropdown: (show: boolean) => void;

    // Drag-and-drop
    dragOver: boolean;
    setDragOver: (over: boolean) => void;
    dragTargetDir: string | null;
    setDragTargetDir: (dir: string | null) => void;

    // Sync
    showSyncDialog: boolean;
    setShowSyncDialog: (show: boolean) => void;

    // Recent vaults (NEW)
    recentVaults: RecentVault[];
    loadRecentVaults: () => Promise<void>;
    removeFromHistory: (vaultPath: string) => Promise<void>;
    clearHistory: () => Promise<void>;

    // Folder encryption (NEW)
    folderScanResult: FolderScanResult | null;
    folderProgress: FolderProgress | null;
    initialFolderPath?: string;

    // Initial props passthrough
    initialFiles?: string[];

    // Functions
    resetState: () => void;
    detectVaultVersion: (path: string) => Promise<VaultSecurityInfo>;
    handleCreate: () => Promise<void>;
    handleOpen: () => Promise<void>;
    handleUnlock: () => Promise<void>;
    refreshVaultEntries: () => Promise<void>;
    handleAddFiles: () => Promise<void>;
    handleDropFiles: (paths: string[]) => Promise<void>;
    handleCreateDirectory: () => Promise<void>;
    handleRemove: (entryName: string, isDir: boolean) => Promise<void>;
    handleExtract: (entryName: string) => Promise<void>;
    handleChangePassword: () => Promise<void>;
    handleOpenRemoteVault: () => Promise<void>;
    handleSaveRemoteAndClose: () => Promise<void>;
    handleCleanupRemote: () => Promise<void>;
    handleCreateFromFolder: (folderPath: string) => Promise<void>;
    handleAddDirectory: () => Promise<void>;
}

// --- Hook implementation ---

export function useVaultState(props: UseVaultStateProps): VaultState {
    const { initialMode, initialPath, initialFiles, initialFolderPath, onClose } = props;
    const t = useTranslation();

    // Core state
    const [mode, setMode] = useState<VaultMode>(initialMode || (initialPath ? 'open' : initialFiles?.length ? 'create' : 'home'));
    const [vaultPath, setVaultPath] = useState(initialPath || '');
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [description, setDescription] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [entries, setEntries] = useState<ArchiveEntry[]>([]);
    const [meta, setMeta] = useState<AeroVaultMeta | null>(null);
    const [changingPassword, setChangingPassword] = useState(false);
    const [newPassword, setNewPassword] = useState('');
    const [confirmNewPassword, setConfirmNewPassword] = useState('');

    // Directory navigation state
    const [currentDir, setCurrentDir] = useState('');
    const [newDirName, setNewDirName] = useState('');
    const [showNewDirDialog, setShowNewDirDialog] = useState(false);

    // Vault sync state
    const [showSyncDialog, setShowSyncDialog] = useState(false);

    // Remote vault state
    const [remoteVaultPath, setRemoteVaultPath] = useState('');
    const [remoteLocalPath, setRemoteLocalPath] = useState('');
    const [remoteLoading, setRemoteLoading] = useState(false);
    const [showRemoteInput, setShowRemoteInput] = useState(false);

    // Security state
    const [securityLevel, setSecurityLevel] = useState<SecurityLevel>('advanced');
    const [vaultSecurity, setVaultSecurity] = useState<VaultSecurityInfo | null>(null);
    const [showLevelDropdown, setShowLevelDropdown] = useState(false);

    // Drag-and-drop state
    const [dragOver, setDragOver] = useState(false);
    const [dragTargetDir, setDragTargetDir] = useState<string | null>(null);

    // Recent vaults (NEW)
    const [recentVaults, setRecentVaults] = useState<RecentVault[]>([]);

    // Folder encryption (NEW)
    const [folderScanResult, setFolderScanResult] = useState<FolderScanResult | null>(null);
    const [folderProgress, setFolderProgress] = useState<FolderProgress | null>(null);

    const resetState = () => {
        setPassword('');
        setConfirmPassword('');
        setDescription('');
        setError(null);
        setSuccess(null);
        setEntries([]);
        setMeta(null);
        setChangingPassword(false);
        setNewPassword('');
        setConfirmNewPassword('');
        setVaultSecurity(null);
        setCurrentDir('');
        setNewDirName('');
        setShowNewDirDialog(false);
        setDragOver(false);
        setDragTargetDir(null);
        setFolderScanResult(null);
        setFolderProgress(null);
    };

    const detectVaultVersion = async (path: string): Promise<VaultSecurityInfo> => {
        try {
            const peek = await invoke<{ version: number; cascade_mode: boolean; security_level: string }>('vault_v2_peek', { path });
            const level: SecurityLevel = peek.cascade_mode ? 'paranoid' : 'advanced';
            return { version: 2, cascadeMode: peek.cascade_mode, level };
        } catch {
            try {
                const isV2 = await invoke<boolean>('is_vault_v2', { path });
                if (isV2) {
                    return { version: 2, cascadeMode: false, level: 'advanced' };
                }
            } catch { /* ignore */ }
            return { version: 1, cascadeMode: false, level: 'standard' };
        }
    };

    // --- Recent vaults ---

    const loadRecentVaults = async () => {
        try {
            const list = await invoke<RecentVault[]>('vault_history_list');
            setRecentVaults(list);
        } catch {
            // vault_history commands may not exist yet — graceful fallback
            setRecentVaults([]);
        }
    };

    const saveToHistory = async (vPath: string, vName: string, sLevel: string, vVersion: number, cascadeMode: boolean, fileCount: number) => {
        try {
            await invoke('vault_history_save', {
                vaultPath: vPath,
                vaultName: vName,
                securityLevel: sLevel,
                vaultVersion: vVersion,
                cascadeMode,
                fileCount,
            });
            await loadRecentVaults();
        } catch { /* best-effort */ }
    };

    const removeFromHistory = async (vPath: string) => {
        try {
            await invoke('vault_history_remove', { vaultPath: vPath });
            await loadRecentVaults();
        } catch { /* best-effort */ }
    };

    const clearHistory = async () => {
        try {
            await invoke('vault_history_clear');
            setRecentVaults([]);
        } catch { /* best-effort */ }
    };

    // --- Folder encryption ---

    const handleCreateFromFolder = async (folderPath: string) => {
        setFolderScanResult(null);
        try {
            const result = await invoke<FolderScanResult>('vault_v2_scan_directory', { sourceDir: folderPath });
            setFolderScanResult(result);
        } catch (e) {
            setError(mapVaultError(e, t));
        }
    };

    const handleAddDirectory = async () => {
        if (!initialFolderPath || !vaultPath || !password) return;
        setLoading(true);
        setError(null);
        try {
            await invoke('vault_v2_add_directory', {
                vaultPath,
                password,
                sourceDir: initialFolderPath,
            });
            await refreshVaultEntries();
            setSuccess(t('vault.filesAdded', { count: String(folderScanResult?.file_count || 0) }));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    // --- Core vault operations ---

    const handleCreate = async () => {
        if (password.length < 8) { setError(t('vault.passwordTooShort')); return; }
        if (password !== confirmPassword) { setError(t('vault.passwordMismatch')); return; }

        const defaultName = (() => {
            if (initialFolderPath) {
                const name = initialFolderPath.split('/').pop() || 'vault';
                return `${name}.aerovault`;
            }
            if (initialFiles?.length === 1) {
                const name = initialFiles[0].split('/').pop()?.replace(/\.[^.]+$/, '') || 'vault';
                return `${name}.aerovault`;
            }
            if (initialFiles && initialFiles.length > 1) {
                const parent = initialFiles[0].split('/').slice(0, -1).pop() || 'archive';
                return `${parent}.aerovault`;
            }
            if (description) return `${description.replace(/[^a-zA-Z0-9_-]/g, '_')}.aerovault`;
            return 'vault.aerovault';
        })();

        const savePath = await save({ defaultPath: defaultName, filters: [{ name: 'AeroVault', extensions: ['aerovault'] }] });
        if (!savePath) return;

        setLoading(true);
        setError(null);

        const levelConfig = securityLevels[securityLevel];

        try {
            if (levelConfig.version === 2) {
                await invoke('vault_v2_create', {
                    vaultPath: savePath,
                    password,
                    description: description || null,
                    cascadeMode: levelConfig.cascade
                });
                setVaultPath(savePath);
                setVaultSecurity({ version: 2, cascadeMode: levelConfig.cascade, level: securityLevel });

                // Auto-add folder contents
                if (initialFolderPath) {
                    setFolderProgress({ current: 0, total: folderScanResult?.file_count || 0, current_file: '' });
                    await invoke('vault_v2_add_directory', {
                        vaultPath: savePath,
                        password,
                        sourceDir: initialFolderPath,
                    });
                    const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath: savePath, password });
                    const fileEntries: ArchiveEntry[] = info.files.map(f => ({
                        name: f.name,
                        size: f.size,
                        compressedSize: f.size,
                        isDir: f.is_dir,
                        isEncrypted: true,
                        modified: f.modified
                    }));
                    setEntries(fileEntries);
                    setSuccess(t('vault.created') + ` — ${info.file_count} files`);
                    setMeta({
                        version: info.version,
                        description: description || null,
                        created: info.created || new Date().toISOString(),
                        modified: info.modified || new Date().toISOString(),
                        fileCount: info.file_count
                    });
                    setFolderProgress(null);
                } else if (initialFiles?.length) {
                    // Auto-add selected files
                    await invoke('vault_v2_add_files', { vaultPath: savePath, password, filePaths: initialFiles });
                    const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath: savePath, password });
                    const fileEntries: ArchiveEntry[] = info.files.map(f => ({
                        name: f.name,
                        size: f.size,
                        compressedSize: f.size,
                        isDir: f.is_dir,
                        isEncrypted: true,
                        modified: f.modified
                    }));
                    setEntries(fileEntries);
                    setSuccess(t('vault.created') + ` — ${initialFiles.length} ${initialFiles.length === 1 ? 'file' : 'files'}`);
                    setMeta({
                        version: info.version,
                        description: description || null,
                        created: info.created || new Date().toISOString(),
                        modified: info.modified || new Date().toISOString(),
                        fileCount: info.file_count
                    });
                } else {
                    setSuccess(t('vault.created'));
                    setEntries([]);
                    setMeta({
                        version: 2,
                        description: description || null,
                        created: new Date().toISOString(),
                        modified: new Date().toISOString(),
                        fileCount: 0
                    });
                }
                setMode('browse');

                // Save to history — use meta.fileCount (not stale entries.length)
                const vName = savePath.split(/[\\/]/).pop() || 'Vault';
                const actualCount = initialFolderPath ? (folderScanResult?.file_count || 0) : (initialFiles?.length || 0);
                await saveToHistory(savePath, vName, securityLevel, 2, levelConfig.cascade, actualCount);
            } else {
                await invoke('vault_create', { vaultPath: savePath, password, description: description || null });
                setVaultPath(savePath);
                setVaultSecurity({ version: 1, cascadeMode: false, level: 'standard' });
                setSuccess(t('vault.created'));
                setMode('browse');
                setEntries([]);
                const m = await invoke<AeroVaultMeta>('vault_get_meta', { vaultPath: savePath, password });
                setMeta(m);

                // Save to history
                const vName = savePath.split(/[\\/]/).pop() || 'Vault';
                await saveToHistory(savePath, vName, 'standard', 1, false, 0);
            }
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleOpen = async () => {
        const selected = await open({ filters: [{ name: 'AeroVault', extensions: ['aerovault'] }] });
        if (!selected) return;
        const path = selected as string;
        setVaultPath(path);

        const security = await detectVaultVersion(path);
        setVaultSecurity(security);
        setMode('open');
    };

    const handleOpenRemoteVault = async () => {
        if (!remoteVaultPath.trim() || !remoteVaultPath.endsWith('.aerovault')) {
            setError(t('vault.remote.open') + ': .aerovault');
            return;
        }
        setRemoteLoading(true);
        setError(null);
        try {
            const localPath = await invoke<string>('vault_v2_download_remote', { remotePath: remoteVaultPath });
            setRemoteLocalPath(localPath);
            setVaultPath(localPath);
            const security = await detectVaultVersion(localPath);
            setVaultSecurity(security);
            setShowRemoteInput(false);
            setMode('open');
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setRemoteLoading(false);
        }
    };

    const handleSaveRemoteAndClose = async () => {
        if (!remoteLocalPath || !remoteVaultPath) return;
        setLoading(true);
        setError(null);
        try {
            await invoke('vault_v2_upload_remote', { localPath: remoteLocalPath, remotePath: remoteVaultPath });
            await invoke('vault_v2_cleanup_temp', { localPath: remoteLocalPath });
            setRemoteLocalPath('');
            setRemoteVaultPath('');
            setSuccess(t('vault.remote.saveAndClose'));
            resetState();
            setMode('home');
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleCleanupRemote = async () => {
        if (!remoteLocalPath) return;
        try {
            await invoke('vault_v2_cleanup_temp', { localPath: remoteLocalPath });
        } catch { /* best-effort cleanup */ }
        setRemoteLocalPath('');
        setRemoteVaultPath('');
    };

    const refreshVaultEntries = async () => {
        if (vaultSecurity?.version === 2) {
            const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath, password });
            const fileEntries: ArchiveEntry[] = info.files.map(f => ({
                name: f.name,
                size: f.size,
                compressedSize: f.size,
                isDir: f.is_dir,
                isEncrypted: true,
                modified: f.modified
            }));
            setEntries(fileEntries);
            setMeta({
                version: info.version,
                description: info.description || null,
                created: info.created,
                modified: info.modified,
                fileCount: info.file_count
            });
        } else {
            const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
            setEntries(list);
        }
    };

    const handleUnlock = async () => {
        setLoading(true);
        setError(null);

        try {
            if (vaultSecurity?.version === 2) {
                const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath, password });
                const secLevel: SecurityLevel = info.cascade_mode ? 'paranoid' : 'advanced';
                setVaultSecurity({ version: 2, cascadeMode: info.cascade_mode, level: secLevel });

                const fileEntries: ArchiveEntry[] = info.files.map(f => ({
                    name: f.name,
                    size: f.size,
                    compressedSize: f.size,
                    isDir: f.is_dir,
                    isEncrypted: true,
                    modified: f.modified
                }));
                setEntries(fileEntries);
                setMeta({
                    version: info.version,
                    description: info.description || null,
                    created: info.created,
                    modified: info.modified,
                    fileCount: info.file_count
                });
                setMode('browse');

                // Save to history
                const vName = vaultPath.split(/[\\/]/).pop() || 'Vault';
                await saveToHistory(vaultPath, vName, secLevel, 2, info.cascade_mode, info.file_count);
            } else {
                const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
                setEntries(list);
                const m = await invoke<AeroVaultMeta>('vault_get_meta', { vaultPath, password });
                setMeta(m);
                setMode('browse');

                // Save to history
                const vName = vaultPath.split(/[\\/]/).pop() || 'Vault';
                await saveToHistory(vaultPath, vName, 'standard', 1, false, list.length);
            }
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleAddFiles = async () => {
        const selected = await open({ multiple: true });
        if (!selected || (Array.isArray(selected) && selected.length === 0)) return;
        const paths = Array.isArray(selected) ? selected as string[] : [selected as string];

        setLoading(true);
        setError(null);
        try {
            if (vaultSecurity?.version === 2) {
                const result = currentDir
                    ? await invoke<{ added: number; total: number }>('vault_v2_add_files_to_dir', {
                        vaultPath,
                        password,
                        filePaths: paths,
                        targetDir: currentDir
                    })
                    : await invoke<{ added: number; total: number }>('vault_v2_add_files', {
                        vaultPath,
                        password,
                        filePaths: paths
                    });
                await refreshVaultEntries();
                setSuccess(t('vault.filesAdded', { count: result.added.toString() }));
            } else {
                await invoke('vault_add_files', { vaultPath, password, filePaths: paths });
                await refreshVaultEntries();
                setSuccess(t('vault.filesAdded', { count: paths.length.toString() }));
            }
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleDropFiles = useCallback(async (paths: string[]) => {
        if (!paths.length || !vaultPath || !password || loading) return;

        setLoading(true);
        setError(null);
        try {
            const targetDir = dragTargetDir || currentDir;
            if (vaultSecurity?.version === 2) {
                const result = targetDir
                    ? await invoke<{ added: number; total: number }>('vault_v2_add_files_to_dir', {
                        vaultPath,
                        password,
                        filePaths: paths,
                        targetDir
                    })
                    : await invoke<{ added: number; total: number }>('vault_v2_add_files', {
                        vaultPath,
                        password,
                        filePaths: paths
                    });
                await refreshVaultEntries();
                setSuccess(t('vault.filesAdded', { count: result.added.toString() }));
            } else {
                await invoke('vault_add_files', { vaultPath, password, filePaths: paths });
                await refreshVaultEntries();
                setSuccess(t('vault.filesAdded', { count: paths.length.toString() }));
            }
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
            setDragTargetDir(null);
        }
    }, [vaultPath, password, currentDir, dragTargetDir, vaultSecurity, loading, t]);

    const handleCreateDirectory = async () => {
        const trimmed = newDirName.trim();
        if (!trimmed) return;

        setLoading(true);
        setError(null);
        try {
            const fullPath = currentDir ? `${currentDir}/${trimmed}` : trimmed;
            await invoke('vault_v2_create_directory', {
                vaultPath,
                password,
                dirName: fullPath
            });
            await refreshVaultEntries();
            setSuccess(t('vault.directoryCreated', { name: trimmed }));
            setShowNewDirDialog(false);
            setNewDirName('');
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleRemove = async (entryName: string, isDir: boolean) => {
        setLoading(true);
        setError(null);
        try {
            if (vaultSecurity?.version === 2) {
                if (isDir) {
                    const result = await invoke<{ deleted: string[]; remaining: number; removed_count: number }>('vault_v2_delete_entries', {
                        vaultPath,
                        password,
                        entryNames: [entryName],
                        recursive: true
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.itemsDeleted', { count: result.removed_count.toString() }));
                } else {
                    await invoke<{ deleted: string; remaining: number }>('vault_v2_delete_entry', {
                        vaultPath,
                        password,
                        entryName
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.itemDeleted', { name: entryName.split('/').pop() || entryName }));
                }
            } else {
                await invoke('vault_remove_file', { vaultPath, password, entryName });
                await refreshVaultEntries();
                setSuccess(t('vault.itemDeleted', { name: entryName }));
            }
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleExtract = async (entryName: string) => {
        const savePath = await save({ defaultPath: entryName.split(/[\\/]/).pop() || entryName });
        if (!savePath) return;

        setLoading(true);
        try {
            if (vaultSecurity?.version === 2) {
                await invoke('vault_v2_extract_entry', {
                    vaultPath,
                    password,
                    entryName,
                    destPath: savePath
                });
            } else {
                await invoke('vault_extract_entry', { vaultPath, password, entryName, outputPath: savePath });
            }
            setSuccess(t('vault.extracted', { name: entryName }));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleChangePassword = async () => {
        if (newPassword.length < 8) { setError(t('vault.passwordTooShort')); return; }
        if (newPassword !== confirmNewPassword) { setError(t('vault.passwordMismatch')); return; }

        setLoading(true);
        setError(null);
        try {
            if (vaultSecurity?.version === 2) {
                await invoke('vault_v2_change_password', {
                    vaultPath,
                    oldPassword: password,
                    newPassword
                });
            } else {
                await invoke('vault_change_password', { vaultPath, oldPassword: password, newPassword });
            }
            setPassword(newPassword);
            setChangingPassword(false);
            setNewPassword('');
            setConfirmNewPassword('');
            setSuccess(t('vault.passwordChanged'));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    // --- Effects ---

    // Auto-detect vault version when opened via context menu (initialPath)
    useEffect(() => {
        if (initialPath && !vaultSecurity) {
            detectVaultVersion(initialPath).then(setVaultSecurity).catch(() => {});
        }
    }, [initialPath]);

    // Listen for OS file drag-and-drop events via Tauri webview API
    useEffect(() => {
        if (mode !== 'browse') return;

        const webview = getCurrentWebview();
        return guardedUnlisten(webview.onDragDropEvent((event) => {
            if (event.payload.type === 'over' || event.payload.type === 'enter') {
                setDragOver(true);
            } else if (event.payload.type === 'drop') {
                setDragOver(false);
                const paths = event.payload.paths;
                if (paths.length > 0) {
                    handleDropFiles(paths);
                }
            } else if (event.payload.type === 'leave') {
                setDragOver(false);
                setDragTargetDir(null);
            }
        }));
    }, [mode, handleDropFiles]);

    // Load recent vaults on mount
    useEffect(() => {
        loadRecentVaults();
    }, []);

    // Listen to vault-add-progress events for folder progress
    useEffect(() => {
        if (!initialFolderPath) return;

        const webview = getCurrentWebview();
        return guardedUnlisten(webview.listen<FolderProgress>('vault-add-progress', (event) => {
            setFolderProgress(event.payload);
        }));
    }, [initialFolderPath]);

    // Scan folder on mount if initialFolderPath is provided
    useEffect(() => {
        if (initialFolderPath) {
            handleCreateFromFolder(initialFolderPath);
        }
    }, [initialFolderPath]);

    // Clear sensitive state on unmount (AVP-001: password must not persist)
    useEffect(() => {
        return () => {
            setPassword('');
            setNewPassword('');
            setConfirmNewPassword('');
            setConfirmPassword('');
        };
    }, []);

    return {
        mode, setMode,
        vaultPath, setVaultPath,
        password, setPassword,
        confirmPassword, setConfirmPassword,
        description, setDescription,
        showPassword, setShowPassword,
        loading,
        error, setError,
        success, setSuccess,
        entries,
        meta,
        currentDir, setCurrentDir,
        newDirName, setNewDirName,
        showNewDirDialog, setShowNewDirDialog,
        changingPassword, setChangingPassword,
        newPassword, setNewPassword,
        confirmNewPassword, setConfirmNewPassword,
        remoteVaultPath, setRemoteVaultPath,
        remoteLocalPath,
        remoteLoading,
        showRemoteInput, setShowRemoteInput,
        securityLevel, setSecurityLevel,
        vaultSecurity, setVaultSecurity,
        showLevelDropdown, setShowLevelDropdown,
        dragOver, setDragOver,
        dragTargetDir, setDragTargetDir,
        showSyncDialog, setShowSyncDialog,
        recentVaults,
        loadRecentVaults,
        removeFromHistory,
        clearHistory,
        folderScanResult,
        folderProgress,
        initialFolderPath,
        initialFiles,
        resetState,
        detectVaultVersion,
        handleCreate,
        handleOpen,
        handleUnlock,
        refreshVaultEntries,
        handleAddFiles,
        handleDropFiles,
        handleCreateDirectory,
        handleRemove,
        handleExtract,
        handleChangePassword,
        handleOpenRemoteVault,
        handleSaveRemoteAndClose,
        handleCleanupRemote,
        handleCreateFromFolder,
        handleAddDirectory,
    };
}
