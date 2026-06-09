// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Shield, ShieldCheck, ShieldAlert } from 'lucide-react';
import { ArchiveEntry, AeroVaultMeta } from '../../types';
import { useTranslation } from '../../i18n';
import { guardedUnlisten } from '../../hooks/useTauriListener';

/** Where Error Correction parity lives relative to the vault container. */
export type RecoveryPlacement = 'embedded' | 'detached' | 'both';

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

export function mapVaultError(e: unknown, t: (key: string) => string): string {
    const raw = String(e);
    for (const [pattern, key] of ERROR_PATTERNS) {
        if (pattern.test(raw)) {
            const mapped = t(key);
            if (mapped && mapped !== key) return mapped;
        }
    }
    // Fallback: strip internal details (offsets, hex, stack traces) and redact
    // absolute filesystem paths so raw backend paths are not surfaced to the UI
    // (CLAUDE-AV-022).
    const cleaned = raw
        .replace(/at offset \d+/gi, '')
        .replace(/0x[0-9a-f]+/gi, '')
        // Unix absolute paths and Windows drive paths -> redacted placeholder.
        .replace(/(?:[A-Za-z]:)?[\\/][^\s'":]+(?:[\\/][^\s'":]+)+/g, '<path>')
        .replace(/\s{2,}/g, ' ')
        .trim();
    return cleaned || raw;
}

// --- Exported types ---

export type VaultMode = 'home' | 'create' | 'open' | 'browse';

export type SecurityLevel = 'standard' | 'advanced' | 'paranoid' | 'experimental';

export type VaultV3CompressionProfile = 'fast' | 'balanced' | 'archive';

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

/**
 * Behind-the-scenes technical receipt emitted by the Rust vault_telemetry
 * module (shared across AeroVault v1/v2/v3). Mirrors `VaultReport` in
 * `src-tauri/src/vault_telemetry.rs`. All fields optional-tolerant: v2/v1
 * leave chunk/dedup at 0.
 */
export interface VaultReport {
    operation: string;
    vault_format: number;
    profile?: string;
    algorithms: string[];
    cdc_min?: number;
    cdc_avg?: number;
    cdc_max?: number;
    files: number;
    packed_files: number;
    packs: number;
    logical_chunks: number;
    new_physical_chunks: number;
    dedup_hits: number;
    plaintext_bytes: number;
    compressed_bytes: number;
    encrypted_bytes: number;
    compression_ratio_pct: number;
    ms_total: number;
    steps: string[];
    attribution: string;
    // P3-03 Error Correction telemetry (populated on seal for Error Correction vaults; optional for compat)
    error_correction_shards_generated?: number;
    error_correction_bytes_protected?: number;
    error_correction_overhead_pct?: number;
}

interface VaultV3Info {
    version: number;
    file_count: number;
    chunk_count: number;
    dedup_chunks: number;
    compression_level: number;
    files: { name: string; size: number; is_dir: boolean; modified: string; chunk_count: number }[];
    report?: VaultReport;
}

function mapV2InfoToEntries(info: VaultV2Info): ArchiveEntry[] {
    return info.files.map(file => ({
        name: file.name,
        size: file.size,
        compressedSize: file.size,
        isDir: file.is_dir,
        isEncrypted: true,
        modified: file.modified,
    }));
}

function mapV2InfoToMeta(info: VaultV2Info): AeroVaultMeta {
    return {
        version: info.version,
        description: info.description || null,
        created: info.created,
        modified: info.modified,
        fileCount: info.file_count,
    };
}

function mapV3InfoToEntries(info: VaultV3Info): ArchiveEntry[] {
    return info.files.map(file => ({
        name: file.name,
        size: file.size,
        compressedSize: file.size,
        isDir: file.is_dir,
        isEncrypted: true,
        modified: file.modified,
    }));
}

function mapV3InfoToMeta(info: VaultV3Info, previousMeta?: AeroVaultMeta | null): AeroVaultMeta {
    const now = new Date().toISOString();
    return {
        version: info.version,
        description: previousMeta?.description || null,
        created: previousMeta?.created || now,
        modified: now,
        fileCount: info.file_count,
    };
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

// Security level configuration: hardcoded labels (no i18n, technical terms)
export const securityLevels = {
    standard: {
        icon: Shield,
        color: 'text-blue-400',
        bgColor: 'bg-blue-600',
        borderColor: 'border-blue-500',
        label: 'Standard',
        version: 2,
        cascade: false,
        features: ['AES-256-GCM-SIV', 'Argon2id 128 MB', 'Encrypted filenames'],
        description: 'AES-256-GCM-SIV · Argon2id 128 MB · Fast'
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
    },
    experimental: {
        icon: ShieldAlert,
        color: 'text-amber-400',
        bgColor: 'bg-amber-600',
        borderColor: 'border-amber-500',
        label: 'Beta',
        version: 3,
        cascade: false,
        features: ['Gear-CDC chunking', 'Chunk deduplication', 'Zstd per chunk', 'AES-256-GCM-SIV'],
        description: 'Deduplicated archive · zstd chunks · Draft v3'
    }
};

// Error Correction (Reed-Solomon error correction) is a Phase 1 stub on top of v3 experimental.
// When enabled on create, uses vault_v3_create_with_error_correction (non-critical extension).
// Scrub/repair exposed via dedicated commands.


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

    // Behind-the-scenes technical receipt (last create/add); null when none
    lastReport: VaultReport | null;
    clearReport: () => void;

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
    compressionProfile: VaultV3CompressionProfile;
    setCompressionProfile: (profile: VaultV3CompressionProfile) => void;
    vaultSecurity: VaultSecurityInfo | null;
    setVaultSecurity: (sec: VaultSecurityInfo | null) => void;
    showLevelDropdown: boolean;
    setShowLevelDropdown: (show: boolean) => void;

    // Error Correction (Reed-Solomon error-correction) toggle for experimental/Beta vaults (P2).
    // When enabled on create, uses the dedicated with_error_correction backend (non-critical extension).
    // Enables scrub/repair actions and Error Correction badge in the UI.
    errorCorrectionEnabled: boolean;
    setErrorCorrectionEnabled: (enabled: boolean) => void;
    hasErrorCorrection: boolean;
    setHasErrorCorrection: (v: boolean) => void;
    // Where parity lives when created with Error Correction (embedded/detached/both).
    recoveryPlacement: RecoveryPlacement;
    setRecoveryPlacement: (p: RecoveryPlacement) => void;
    // QR-style Error Correction overhead level (#276), as a target overhead percentage.
    errorCorrectionPct: number;
    setErrorCorrectionPct: (p: number) => void;
    // Detected detached `.aerovault.rec` sidecar for the open vault.
    hasDetachedRecovery: boolean;
    // The detached sidecar also carries header (+ manifest locator) parity, so the
    // detached path can rebuild the 1024-byte header, not just the data blocks.
    hasDetachedHeaderRecovery: boolean;
    // Export a detached recovery file; strip the embedded parity (sidecar-aware).
    exportParity: () => Promise<void>;
    stripParity: (force: boolean) => Promise<void>;
    isExportingParity: boolean;
    isStrippingParity: boolean;

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

    // P2 Error Correction scrub/repair modals (draggable, theme-aware)
    showScrubDialog: boolean;
    setShowScrubDialog: (show: boolean) => void;
    showRepairDialog: boolean;
    setShowRepairDialog: (show: boolean) => void;
    scrubResult: any | null;  // from vault_v3_scrub
    repairResult: any | null;
    repairDryRun: boolean;
    setRepairDryRun: (v: boolean) => void;
    isRepairing: boolean;
    setScrubResult: (r: any | null) => void;
    setRepairResult: (r: any | null) => void;
    setIsRepairing: (v: boolean) => void;

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

    // P2 Error Correction actions (use the registered Tauri commands; engine shared with CLI)
    handleScrub: () => Promise<void>;
    handleRepair: () => Promise<void>;
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

    // Behind-the-scenes technical receipt of the last create/add operation
    const [lastReport, setLastReport] = useState<VaultReport | null>(null);

    // Security state
    const [securityLevel, setSecurityLevel] = useState<SecurityLevel>('advanced');
    const [compressionProfile, setCompressionProfile] = useState<VaultV3CompressionProfile>('balanced');
    const [vaultSecurity, setVaultSecurity] = useState<VaultSecurityInfo | null>(null);
    const [showLevelDropdown, setShowLevelDropdown] = useState(false);

    // Error Correction for Beta/experimental vaults (P2)
    const [errorCorrectionEnabled, setErrorCorrectionEnabled] = useState(false);
    const [hasErrorCorrection, setHasErrorCorrection] = useState(false);  // runtime detection for open vaults (via has_error_correction command)
    const [recoveryPlacement, setRecoveryPlacement] = useState<RecoveryPlacement>('embedded');
    const [errorCorrectionPct, setErrorCorrectionPct] = useState<number>(20);  // QR-style overhead level (#276); 20% == K=10/P=2
    const [hasDetachedRecovery, setHasDetachedRecovery] = useState(false);  // detached .aerovault.rec sidecar present
    const [hasDetachedHeaderRecovery, setHasDetachedHeaderRecovery] = useState(false);  // sidecar carries header (+ manifest) parity
    const [isExportingParity, setIsExportingParity] = useState(false);
    const [isStrippingParity, setIsStrippingParity] = useState(false);

    // P2 Error Correction dialogs
    const [showScrubDialog, setShowScrubDialog] = useState(false);
    const [showRepairDialog, setShowRepairDialog] = useState(false);
    const [scrubResult, setScrubResult] = useState<any | null>(null);
    const [repairResult, setRepairResult] = useState<any | null>(null);
    const [repairDryRun, setRepairDryRun] = useState(true);
    const [isRepairing, setIsRepairing] = useState(false);

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
            const isV3 = await invoke<boolean>('is_vault_v3', { path });
            if (isV3) {
                return { version: 3, cascadeMode: false, level: 'experimental' };
            }
        } catch {
            // Ignore and continue with older versions.
        }

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
            // vault_history commands may not exist yet: graceful fallback
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
            if (vaultSecurity?.version === 3) {
                await invoke('vault_v3_add_directory', {
                    vaultPath,
                    password,
                    sourceDir: initialFolderPath,
                    targetPrefix: currentDir || null,
                });
            } else {
                await invoke('vault_v2_add_directory', {
                    vaultPath,
                    password,
                    sourceDir: initialFolderPath,
                });
            }
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
            if (levelConfig.version === 3) {
                // P2: if Error Correction enabled in experimental, use the dedicated with_ecc creator (non-critical RS extension stub).
                if (securityLevel === 'experimental' && errorCorrectionEnabled) {
                    await invoke('vault_v3_create_with_error_correction', {
                        vaultPath: savePath,
                        password,
                        profile: compressionProfile,
                        placement: recoveryPlacement,
                        errorCorrectionPct: Math.min(50, Math.max(5, Math.round(errorCorrectionPct))),
                    });
                } else {
                    await invoke('vault_v3_create', {
                        vaultPath: savePath,
                        password,
                        compressionProfile,
                    });
                }
                setVaultPath(savePath);
                setVaultSecurity({ version: 3, cascadeMode: false, level: 'experimental' });

                if (initialFolderPath) {
                    setFolderProgress({ current: 0, total: folderScanResult?.file_count || 0, current_file: '' });
                    await invoke('vault_v3_add_directory', {
                        vaultPath: savePath,
                        password,
                        sourceDir: initialFolderPath,
                        targetPrefix: null,
                    });
                    const info = await invoke<VaultV3Info>('vault_v3_open', { vaultPath: savePath, password });
                    setEntries(mapV3InfoToEntries(info));
                    setMeta(mapV3InfoToMeta(info));
                    setSuccess(t('vault.created') + `: ${info.file_count} files`);
                    setFolderProgress(null);
                } else if (initialFiles?.length) {
                    const info = await invoke<VaultV3Info>('vault_v3_add_files', {
                        vaultPath: savePath,
                        password,
                        filePaths: initialFiles,
                    });
                    setEntries(mapV3InfoToEntries(info));
                    setMeta(mapV3InfoToMeta(info));
                    setLastReport(info.report ?? null);
                    setSuccess(t('vault.created') + `: ${initialFiles.length} ${initialFiles.length === 1 ? 'file' : 'files'}`);
                } else {
                    setSuccess(t('vault.created'));
                    setEntries([]);
                    setMeta({
                        version: 3,
                        description: null,
                        created: new Date().toISOString(),
                        modified: new Date().toISOString(),
                        fileCount: 0,
                    });
                }
                setMode('browse');

                const vName = savePath.split(/[\\/]/).pop() || 'Vault';
                await saveToHistory(savePath, vName, 'experimental', 3, false, initialFiles?.length || 0);
            } else if (levelConfig.version === 2) {
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
                    setEntries(mapV2InfoToEntries(info));
                    setSuccess(t('vault.created') + `: ${info.file_count} files`);
                    setMeta(mapV2InfoToMeta(info));
                    setFolderProgress(null);
                } else if (initialFiles?.length) {
                    // Auto-add selected files
                    const v2add = await invoke<{ report?: VaultReport }>('vault_v2_add_files', { vaultPath: savePath, password, filePaths: initialFiles });
                    setLastReport(v2add.report ?? null);
                    const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath: savePath, password });
                    setEntries(mapV2InfoToEntries(info));
                    setSuccess(t('vault.created') + `: ${initialFiles.length} ${initialFiles.length === 1 ? 'file' : 'files'}`);
                    setMeta(mapV2InfoToMeta(info));
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

                // Save to history: use meta.fileCount (not stale entries.length)
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
        if (vaultSecurity?.version === 3) {
            const info = await invoke<VaultV3Info>('vault_v3_open', { vaultPath, password });
            setEntries(mapV3InfoToEntries(info));
            setMeta(mapV3InfoToMeta(info, meta));
        } else if (vaultSecurity?.version === 2) {
            const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath, password });
            setEntries(mapV2InfoToEntries(info));
            setMeta(mapV2InfoToMeta(info));
        } else {
            const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
            setEntries(list);
        }
    };

    const handleUnlock = async () => {
        setLoading(true);
        setError(null);

        try {
            if (vaultSecurity?.version === 3) {
                const info = await invoke<VaultV3Info>('vault_v3_open', { vaultPath, password });
                setVaultSecurity({ version: 3, cascadeMode: false, level: 'experimental' });
                setEntries(mapV3InfoToEntries(info));
                setMeta(mapV3InfoToMeta(info, meta));
                // P2: detect Error Correction for badge and enabling scrub/repair actions in this session.
                // recovery_status reports both the embedded extension and a detached .aerovault.rec sidecar.
                try {
                    const status = await invoke<{ embedded: boolean; detached: boolean; header_parity?: boolean }>('vault_v3_recovery_status', { path: vaultPath });
                    setHasErrorCorrection(!!status.embedded);
                    setHasDetachedRecovery(!!status.detached);
                    setHasDetachedHeaderRecovery(!!status.header_parity);
                } catch { setHasErrorCorrection(false); setHasDetachedRecovery(false); setHasDetachedHeaderRecovery(false); }
                setMode('browse');

                const vName = vaultPath.split(/[\\/]/).pop() || 'Vault';
                await saveToHistory(vaultPath, vName, 'experimental', 3, false, info.file_count);
            } else if (vaultSecurity?.version === 2) {
                const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath, password });
                const secLevel: SecurityLevel = info.cascade_mode ? 'paranoid' : 'advanced';
                setVaultSecurity({ version: 2, cascadeMode: info.cascade_mode, level: secLevel });

                setEntries(mapV2InfoToEntries(info));
                setMeta(mapV2InfoToMeta(info));
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
            if (vaultSecurity?.version === 3) {
                if (currentDir) {
                    const result = await invoke<{ added: number; total: number }>('vault_v3_add_files_to_dir', {
                        vaultPath,
                        password,
                        filePaths: paths,
                        targetDir: currentDir
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.filesAdded', { count: result.added.toString() }));
                } else {
                    const info = await invoke<VaultV3Info>('vault_v3_add_files', {
                        vaultPath,
                        password,
                        filePaths: paths,
                    });
                    setEntries(mapV3InfoToEntries(info));
                    setMeta(mapV3InfoToMeta(info, meta));
                    setLastReport(info.report ?? null);
                    setSuccess(t('vault.filesAdded', { count: paths.length.toString() }));
                }
            } else if (vaultSecurity?.version === 2) {
                const result = currentDir
                    ? await invoke<{ added: number; total: number; report?: VaultReport }>('vault_v2_add_files_to_dir', {
                        vaultPath,
                        password,
                        filePaths: paths,
                        targetDir: currentDir
                    })
                    : await invoke<{ added: number; total: number; report?: VaultReport }>('vault_v2_add_files', {
                        vaultPath,
                        password,
                        filePaths: paths
                    });
                await refreshVaultEntries();
                setLastReport(result.report ?? null);
                setSuccess(t('vault.filesAdded', { count: result.added.toString() }));
            } else {
                const v1res = await invoke<{ report?: VaultReport }>('vault_add_files', { vaultPath, password, filePaths: paths });
                await refreshVaultEntries();
                setLastReport(v1res.report ?? null);
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
            if (vaultSecurity?.version === 3) {
                if (targetDir) {
                    const result = await invoke<{ added: number; total: number }>('vault_v3_add_files_to_dir', {
                        vaultPath,
                        password,
                        filePaths: paths,
                        targetDir
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.filesAdded', { count: result.added.toString() }));
                } else {
                    const info = await invoke<VaultV3Info>('vault_v3_add_files', {
                        vaultPath,
                        password,
                        filePaths: paths,
                    });
                    setEntries(mapV3InfoToEntries(info));
                    setMeta(mapV3InfoToMeta(info, meta));
                    setLastReport(info.report ?? null);
                    setSuccess(t('vault.filesAdded', { count: paths.length.toString() }));
                }
            } else if (vaultSecurity?.version === 2) {
                const result = targetDir
                    ? await invoke<{ added: number; total: number; report?: VaultReport }>('vault_v2_add_files_to_dir', {
                        vaultPath,
                        password,
                        filePaths: paths,
                        targetDir
                    })
                    : await invoke<{ added: number; total: number; report?: VaultReport }>('vault_v2_add_files', {
                        vaultPath,
                        password,
                        filePaths: paths
                    });
                await refreshVaultEntries();
                setLastReport(result.report ?? null);
                setSuccess(t('vault.filesAdded', { count: result.added.toString() }));
            } else {
                const v1res = await invoke<{ report?: VaultReport }>('vault_add_files', { vaultPath, password, filePaths: paths });
                await refreshVaultEntries();
                setLastReport(v1res.report ?? null);
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
            if (vaultSecurity?.version === 3) {
                await invoke('vault_v3_create_directory', {
                    vaultPath,
                    password,
                    dirName: fullPath
                });
            } else {
                await invoke('vault_v2_create_directory', {
                    vaultPath,
                    password,
                    dirName: fullPath
                });
            }
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
            if (vaultSecurity?.version === 3) {
                if (isDir) {
                    const result = await invoke<{ removed: number; remaining: number }>('vault_v3_delete_entries', {
                        vaultPath,
                        password,
                        entryNames: [entryName],
                        recursive: true
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.itemsDeleted', { count: result.removed.toString() }));
                } else {
                    await invoke<{ deleted: string; remaining: number }>('vault_v3_delete_entry', {
                        vaultPath,
                        password,
                        entryName
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.itemDeleted', { name: entryName.split('/').pop() || entryName }));
                }
            } else if (vaultSecurity?.version === 2) {
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
            if (vaultSecurity?.version === 3) {
                await invoke('vault_v3_extract_entry', {
                    vaultPath,
                    password,
                    entryName,
                    destPath: savePath
                });
            } else if (vaultSecurity?.version === 2) {
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
            if (vaultSecurity?.version === 3) {
                await invoke('vault_v3_change_password', {
                    vaultPath,
                    oldPassword: password,
                    newPassword
                });
            } else if (vaultSecurity?.version === 2) {
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

    // --- P2 Error Correction scrub/repair (call the Tauri commands we exposed; draggable modals in UI) ---
    // Re-aligned to hardened engine (P2-HARD + P2-09 v2): scrub {damaged, count, checked},
    // repair {repaired, damaged, dry_run}. Honest msgs + checked count surfaced.
    const handleScrub = async () => {
        if (!vaultPath) return;
        setLoading(true);
        setError(null);
        try {
            const res = await invoke<any>('vault_v3_scrub', { vaultPath, password });
            setScrubResult(res);
            setShowScrubDialog(true);
            const checked = res.checked ?? res.count ?? 0;
            const damaged = res.count ?? (res.damaged ? res.damaged.length : 0);
            setSuccess(t('vault.scrubComplete', { checked: String(checked), damaged: String(damaged) }));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleRepair = async () => {
        if (!vaultPath) return;
        setIsRepairing(true);
        setError(null);
        try {
            const res = await invoke<any>('vault_v3_repair', { vaultPath, password, dryRun: repairDryRun });
            setRepairResult(res);
            if (!repairDryRun) {
                await refreshVaultEntries();
                const repaired = res.repaired ?? 0;
                const damaged = res.damaged ?? 0;
                if (damaged === 0) {
                    setSuccess(t('vault.repairNoDamage'));
                } else if (repaired === 0) {
                    setSuccess(t('vault.repairUntouched', { damaged: String(damaged) }));
                } else {
                    // Engine is all-or-nothing: a non-zero repaired count means every
                    // damaged block verified and was persisted (repaired === damaged).
                    setSuccess(t('vault.repairSuccess', { repaired: String(repaired) }));
                }
            }
            // keep dialog open to show result (modal renders honest summary from repairResult)
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setIsRepairing(false);
        }
    };

    // --- SIDECAR: detached recovery file (.aerovault.rec) ---
    // Export writes/refreshes the sidecar from the current data; strip drops the
    // embedded parity (refused unless a sidecar exists, mirroring the engine).
    const exportParity = async () => {
        if (!vaultPath) return;
        setIsExportingParity(true);
        setError(null);
        try {
            const res = await invoke<any>('vault_v3_export_parity', { vaultPath, password });
            setHasDetachedRecovery(true);
            const protectedBytes = res?.bytes_protected ?? 0;
            if (protectedBytes === 0) {
                setSuccess(t('vault.parityExportedEmpty'));
            } else {
                setSuccess(t('vault.parityExported', { bytes: String(protectedBytes) }));
            }
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setIsExportingParity(false);
        }
    };

    const stripParity = async (force: boolean) => {
        if (!vaultPath) return;
        setIsStrippingParity(true);
        setError(null);
        try {
            const res = await invoke<any>('vault_v3_strip_parity', { vaultPath, password, force });
            setHasErrorCorrection(false);
            setSuccess(res?.sidecar_present ? t('vault.parityStripped') : t('vault.parityStrippedNoRecovery'));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setIsStrippingParity(false);
        }
    };

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
        lastReport,
        clearReport: () => setLastReport(null),
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
        compressionProfile, setCompressionProfile,
        vaultSecurity, setVaultSecurity,
        showLevelDropdown, setShowLevelDropdown,
        errorCorrectionEnabled, setErrorCorrectionEnabled,
        hasErrorCorrection, setHasErrorCorrection,
        recoveryPlacement, setRecoveryPlacement,
        errorCorrectionPct, setErrorCorrectionPct,
        hasDetachedRecovery,
        hasDetachedHeaderRecovery,
        exportParity, stripParity,
        isExportingParity, isStrippingParity,
        showScrubDialog, setShowScrubDialog,
        showRepairDialog, setShowRepairDialog,
        scrubResult, setScrubResult,
        repairResult, setRepairResult,
        repairDryRun, setRepairDryRun,
        isRepairing, setIsRepairing,
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
        handleScrub,
        handleRepair,
        handleOpenRemoteVault,
        handleSaveRemoteAndClose,
        handleCleanupRemote,
        handleCreateFromFolder,
        handleAddDirectory,
    };
}
