// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open, save } from '@tauri-apps/plugin-dialog';
import { writeTextFile } from '@tauri-apps/plugin-fs';
import { Shield, ShieldCheck, ShieldAlert } from 'lucide-react';
import { ArchiveEntry, AeroVaultMeta } from '../../types';
import { formatSize } from '../../utils/formatters';
import { useTranslation } from '../../i18n';
import { guardedUnlisten } from '../../hooks/useTauriListener';
import { splitPathsByType } from './pathStaging';

/** Where Error Correction parity lives relative to the vault container. */
export type RecoveryPlacement = 'embedded' | 'detached' | 'both';

/** Byte-level progress of a vault compress+encrypt operation (AeroProgress). */
export interface VaultProgress {
    percentage: number;
    transferred: number;
    total: number;
}

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

export type VaultContainerKind = 'vault' | 'zip';

export type SecurityLevel = 'standard' | 'advanced' | 'paranoid' | 'experimental';

export type VaultV3CompressionProfile = 'fast' | 'balanced' | 'archive';

export interface VaultSecurityInfo {
    version: number;
    cascadeMode: boolean;
    level: SecurityLevel;
    plaintext?: boolean;
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
    /** Total elapsed time in seconds, one decimal (Ehud #2). */
    time_elapsed: number;
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

function mapV3InfoToEntries(info: VaultV3Info, isEncrypted = true): ArchiveEntry[] {
    return info.files.map(file => ({
        name: file.name,
        size: file.size,
        compressedSize: file.size,
        isDir: file.is_dir,
        isEncrypted,
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
        label: 'Archive',
        version: 3,
        cascade: false,
        features: ['Gear-CDC chunking', 'Chunk deduplication', 'Zstd per chunk', 'AES-256-GCM-SIV'],
        description: 'Deduplicated archive · zstd chunks · v3'
    }
};

// Error Correction (Reed-Solomon) extends a v3 vault with recoverable parity.
// When enabled on create, uses vault_v3_create_with_error_correction (non-critical extension).
// Scrub/repair exposed via dedicated commands.


// --- Hook props & return type ---

export interface UseVaultStateProps {
    initialMode?: VaultMode;
    initialContainerKind?: VaultContainerKind;
    initialPath?: string;
    initialFiles?: string[];
    initialFolderPath?: string;
    isConnected?: boolean;
    onClose: () => void;
    // Optional activity-log sink: mutating vault ops that do not produce a receipt
    // (e.g. Change Mode) report through this so they appear in the Activity Log.
    onActivityLog?: (message: string, details?: string) => void;
}

export interface VaultState {
    // Mode
    mode: VaultMode;
    setMode: (mode: VaultMode) => void;

    // Core state
    containerKind: VaultContainerKind;
    setContainerKind: (kind: VaultContainerKind) => void;
    isPlaintextZip: boolean;
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
    /** Byte-level progress of the current compress+encrypt op (AeroProgress). */
    vaultProgress: VaultProgress | null;
    error: string | null;
    setError: (err: string | null) => void;
    success: string | null;
    setSuccess: (msg: string | null) => void;

    // Behind-the-scenes technical receipt (last create/add); null when none
    lastReport: VaultReport | null;
    clearReport: () => void;

    // Input-vs-output sizes of the last AeroVault Zip create (compression report)
    zipReport: { inputBytes: number; outputBytes: number } | null;

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

    // Change Mode (Ehud #2): re-pack a vault under a new security level (v2<->v3 /
    // cascade), compression profile and/or Error Correction, keeping the same
    // password. The repack extracts the source in its own format and recreates it in
    // the target format, so it covers both same-format tweaks and full cross-format
    // moves.
    changingMode: boolean;
    setChangingMode: (changing: boolean) => void;
    newModeSecurityLevel: SecurityLevel;
    setNewModeSecurityLevel: (l: SecurityLevel) => void;
    newModeProfile: VaultV3CompressionProfile;
    setNewModeProfile: (p: VaultV3CompressionProfile) => void;
    newModeErrorCorrection: boolean;
    setNewModeErrorCorrection: (v: boolean) => void;
    handleChangeMode: () => Promise<void>;

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
    // Detected detached `.aerocorrect` sidecar for the open vault.
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

    // Initial props passthrough (now backed by the staged-files list so the
    // create screen reflects drag&drop / mixed-picker additions; Ehud #2/#8)
    initialFiles?: string[];
    stagedFiles: string[];
    stagedDirs: string[];
    removeStagedFile: (p: string) => void;
    removeStagedDir: (p: string) => void;
    handleStageDrop: (paths: string[]) => Promise<void>;

    // Functions
    resetState: () => void;
    detectVaultVersion: (path: string) => Promise<VaultSecurityInfo>;
    handleCreate: () => Promise<void>;
    handleOpen: () => Promise<void>;
    handleUnlock: () => Promise<void>;
    refreshVaultEntries: () => Promise<void>;
    handleAddFiles: () => Promise<void>;
    handleAddFolder: () => Promise<void>;
    handleDropFiles: (paths: string[]) => Promise<void>;
    handleCreateDirectory: () => Promise<void>;
    handleRemove: (entryName: string, isDir: boolean) => Promise<void>;
    handleExtract: (entryName: string) => Promise<void>;
    handleExtractAll: () => Promise<void>;
    handleSaveAll: (target: 'folder' | 'zip' | 'aerozip') => Promise<void>;
    handleExportVaultReport: (format: 'txt' | 'json') => Promise<void>;
    handleCopyVaultReport: () => Promise<void>;
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
    const { initialMode, initialContainerKind, initialPath, initialFiles, initialFolderPath, onClose, onActivityLog } = props;
    const t = useTranslation();

    // Core state
    const [mode, setMode] = useState<VaultMode>(initialMode || (initialPath ? 'open' : initialFiles?.length ? 'create' : 'home'));
    const [containerKind, setContainerKind] = useState<VaultContainerKind>(initialContainerKind || 'vault');
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
    const [changingMode, setChangingMode] = useState(false);
    const [newModeSecurityLevel, setNewModeSecurityLevel] = useState<SecurityLevel>('experimental');
    const [newModeProfile, setNewModeProfile] = useState<VaultV3CompressionProfile>('balanced');
    const [newModeErrorCorrection, setNewModeErrorCorrection] = useState(false);
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
    // Byte-level progress of the current compress+encrypt op (AeroProgress).
    const [vaultProgress, setVaultProgress] = useState<VaultProgress | null>(null);
    // Final input-vs-output sizes of the last AeroVault Zip create, for the
    // post-create compression report (parity with the standard CompressDialog).
    const [zipReport, setZipReport] = useState<{ inputBytes: number; outputBytes: number } | null>(null);

    // Security state
    const [securityLevel, setSecurityLevel] = useState<SecurityLevel>('advanced');
    const [compressionProfile, setCompressionProfile] = useState<VaultV3CompressionProfile>(
        initialContainerKind === 'zip' ? 'archive' : 'balanced',
    );
    const [vaultSecurity, setVaultSecurity] = useState<VaultSecurityInfo | null>(null);
    const [showLevelDropdown, setShowLevelDropdown] = useState(false);

    // Error Correction for Beta/experimental vaults (P2)
    const [errorCorrectionEnabled, setErrorCorrectionEnabled] = useState(false);
    const [hasErrorCorrection, setHasErrorCorrection] = useState(false);  // runtime detection for open vaults (via has_error_correction command)
    const [recoveryPlacement, setRecoveryPlacement] = useState<RecoveryPlacement>('embedded');
    const [errorCorrectionPct, setErrorCorrectionPct] = useState<number>(20);  // QR-style overhead level (#276); 20% == K=10/P=2
    const [hasDetachedRecovery, setHasDetachedRecovery] = useState(false);  // detached .aerocorrect sidecar present
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

    // Staged contents for the create screen (Ehud #2 drag&drop + #8 mixed
    // file/folder picker). Seeded from the `initialFiles` prop (selection that
    // opened the panel); drops and the picker append files here and folders to
    // stagedDirs. handleCreate adds files via vault_*_add_files and folders via
    // vault_*_add_directory after the container is created.
    const [stagedFiles, setStagedFiles] = useState<string[]>(initialFiles || []);
    const [stagedDirs, setStagedDirs] = useState<string[]>([]);

    // Recent vaults (NEW)
    const [recentVaults, setRecentVaults] = useState<RecentVault[]>([]);

    // Folder encryption (NEW)
    const [folderScanResult, setFolderScanResult] = useState<FolderScanResult | null>(null);
    const [folderProgress, setFolderProgress] = useState<FolderProgress | null>(null);
    const isPlaintextZip = containerKind === 'zip' || vaultSecurity?.plaintext === true;

    const resetState = () => {
        setContainerKind('vault');
        setPassword('');
        setConfirmPassword('');
        setDescription('');
        setError(null);
        setSuccess(null);
        setZipReport(null);
        setEntries([]);
        setMeta(null);
        setChangingPassword(false);
        setNewPassword('');
        setConfirmNewPassword('');
        setVaultSecurity(null);
        setCompressionProfile('balanced');
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
            const isZipArchive = await invoke<boolean>('aerovz_is_archive', { path });
            if (isZipArchive) {
                return { version: 3, cascadeMode: false, level: 'experimental', plaintext: true };
            }
        } catch {
            // Header sniffing is best-effort; encrypted vault detection follows.
        }

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
        if (!initialFolderPath || !vaultPath || (!password && !isPlaintextZip)) return;
        setLoading(true);
        setError(null);
        try {
            if (isPlaintextZip) {
                await invoke('aerovz_add_directory', {
                    vaultPath,
                    sourceDir: initialFolderPath,
                    targetPrefix: currentDir || null,
                });
            } else if (vaultSecurity?.version === 3) {
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
        const creatingZip = isPlaintextZip;
        if (!creatingZip) {
            if (password.length < 8) { setError(t('vault.passwordTooShort')); return; }
            if (password !== confirmPassword) { setError(t('vault.passwordMismatch')); return; }
        }

        const defaultName = (() => {
            const ext = creatingZip ? 'aerozip' : 'aerovault';
            const fallback = creatingZip ? 'zip' : 'vault';
            if (initialFolderPath) {
                const name = initialFolderPath.split('/').pop() || fallback;
                return `${name}.${ext}`;
            }
            if (stagedFiles.length === 1 && stagedDirs.length === 0) {
                const name = stagedFiles[0].split('/').pop()?.replace(/\.[^.]+$/, '') || fallback;
                return `${name}.${ext}`;
            }
            if (stagedDirs.length === 1 && stagedFiles.length === 0) {
                const name = stagedDirs[0].replace(/\/+$/, '').split('/').pop() || fallback;
                return `${name}.${ext}`;
            }
            if (stagedFiles.length > 1 || stagedDirs.length > 0) {
                const first = stagedFiles[0] || stagedDirs[0];
                const parent = first.split('/').slice(0, -1).pop() || fallback;
                return `${parent}.${ext}`;
            }
            if (description) return `${description.replace(/[^a-zA-Z0-9_-]/g, '_')}.${ext}`;
            return `${fallback}.${ext}`;
        })();

        const savePath = await save({
            defaultPath: defaultName,
            filters: creatingZip
                ? [{ name: 'AeroVault Zip', extensions: ['aerozip'] }]
                : [{ name: 'AeroVault', extensions: ['aerovault'] }],
        });
        if (!savePath) return;

        setLoading(true);
        setError(null);

        // Re-validate file-vs-dir at create time. Staging classifies each path
        // when it is added (handleStageDrop -> splitPathsByType); if a directory
        // was ever staged as a file, routing it to vault_*_add_files fails with
        // EISDIR ("is a directory"). Re-splitting here is authoritative and cheap,
        // so the create path is robust regardless of how/when items were staged.
        const { files: effFiles, dirs: effDirs } = (stagedFiles.length || stagedDirs.length)
            ? await splitPathsByType([...stagedFiles, ...stagedDirs])
            : { files: stagedFiles, dirs: stagedDirs };

        const levelConfig = securityLevels[securityLevel];

        try {
            if (creatingZip) {
                await invoke('aerovz_create_archive', {
                    vaultPath: savePath,
                    compressionProfile,
                    errorCorrectionPct: errorCorrectionEnabled
                        ? Math.min(50, Math.max(5, Math.round(errorCorrectionPct)))
                        : 0,
                });
                setContainerKind('zip');
                setVaultPath(savePath);
                setVaultSecurity({ version: 3, cascadeMode: false, level: 'experimental', plaintext: true });

                if (initialFolderPath) {
                    setFolderProgress({ current: 0, total: folderScanResult?.file_count || 0, current_file: '' });
                    await invoke('aerovz_add_directory', {
                        vaultPath: savePath,
                        sourceDir: initialFolderPath,
                        targetPrefix: null,
                    });
                    setFolderProgress(null);
                } else {
                    if (effFiles.length) {
                        await invoke<VaultV3Info>('aerovz_add_files', {
                            vaultPath: savePath,
                            filePaths: effFiles,
                        });
                    }
                    for (const dir of effDirs) {
                        await invoke('aerovz_add_directory', {
                            vaultPath: savePath,
                            sourceDir: dir,
                            targetPrefix: null,
                        });
                    }
                    setLastReport(null);
                }

                const info = await invoke<VaultV3Info>('aerovz_open_archive', { vaultPath: savePath });
                setEntries(mapV3InfoToEntries(info, false));
                setMeta(mapV3InfoToMeta(info));
                setHasErrorCorrection(errorCorrectionEnabled);
                setHasDetachedRecovery(false);
                setHasDetachedHeaderRecovery(false);
                setSuccess(t('vault.zipCreated') + `: ${info.file_count} ${info.file_count === 1 ? 'file' : 'files'}`);
                // Compression report: original payload (sum of file sizes) vs the
                // produced .aerozip container size. Stat is best-effort; on failure
                // the report is simply omitted (no fake ratio).
                try {
                    const inputBytes = info.files.reduce((sum, f) => sum + (f.is_dir ? 0 : f.size), 0);
                    const props = await invoke<{ size: number }>('get_file_properties', { path: savePath });
                    const outputBytes = props?.size ?? 0;
                    setZipReport(inputBytes > 0 && outputBytes > 0 ? { inputBytes, outputBytes } : null);
                } catch {
                    setZipReport(null);
                }
                setMode('browse');

                const vName = savePath.split(/[\\/]/).pop() || 'AeroVault Zip';
                await saveToHistory(savePath, vName, 'aerovault-zip', 3, false, info.file_count);
            } else if (levelConfig.version === 3) {
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
                } else if (effFiles.length || effDirs.length) {
                    let addInfo: VaultV3Info | null = null;
                    if (effFiles.length) {
                        addInfo = await invoke<VaultV3Info>('vault_v3_add_files', {
                            vaultPath: savePath,
                            password,
                            filePaths: effFiles,
                        });
                    }
                    for (const dir of effDirs) {
                        await invoke('vault_v3_add_directory', {
                            vaultPath: savePath,
                            password,
                            sourceDir: dir,
                            targetPrefix: null,
                        });
                    }
                    const info = await invoke<VaultV3Info>('vault_v3_open', { vaultPath: savePath, password });
                    setEntries(mapV3InfoToEntries(info));
                    setMeta(mapV3InfoToMeta(info));
                    setLastReport((addInfo?.report ?? info.report) ?? null);
                    setSuccess(t('vault.created') + `: ${info.file_count} ${info.file_count === 1 ? 'file' : 'files'}`);
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
                await saveToHistory(savePath, vName, 'experimental', 3, false, effFiles.length || 0);
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
                } else if (effFiles.length || effDirs.length) {
                    // Auto-add staged files, then each staged folder (tree preserved)
                    let v2report: VaultReport | null = null;
                    if (effFiles.length) {
                        const v2add = await invoke<{ report?: VaultReport }>('vault_v2_add_files', { vaultPath: savePath, password, filePaths: effFiles });
                        v2report = v2add.report ?? null;
                    }
                    for (const dir of effDirs) {
                        await invoke('vault_v2_add_directory', { vaultPath: savePath, password, sourceDir: dir });
                    }
                    setLastReport(v2report);
                    const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath: savePath, password });
                    setEntries(mapV2InfoToEntries(info));
                    setSuccess(t('vault.created') + `: ${info.file_count} ${info.file_count === 1 ? 'file' : 'files'}`);
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
                const actualCount = initialFolderPath ? (folderScanResult?.file_count || 0) : (effFiles.length || 0);
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
        const selected = await open({ filters: [{ name: 'AeroVault / AeroVault Zip', extensions: ['aerovault', 'aerozip'] }] });
        if (!selected) return;
        const path = selected as string;
        setVaultPath(path);

        const security = await detectVaultVersion(path);
        if (path.toLowerCase().endsWith('.aerozip') && !security.plaintext) {
            setError(t('vault.zipEncryptedRejected'));
            return;
        }
        setContainerKind(security.plaintext ? 'zip' : 'vault');
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

    // versionOverride: read the container as this format instead of the (possibly
    // stale) vaultSecurity.version. Needed right after a cross-format Change Mode
    // repack, where the on-disk format already changed but the React state has not
    // yet been updated, so the default reader would open a v3 file as v2 and fail
    // with "Not a valid AeroVault file".
    const refreshVaultEntries = async (versionOverride?: number) => {
        const version = versionOverride ?? vaultSecurity?.version;
        if (isPlaintextZip && versionOverride == null) {
            const info = await invoke<VaultV3Info>('aerovz_open_archive', { vaultPath });
            setEntries(mapV3InfoToEntries(info, false));
            setMeta(mapV3InfoToMeta(info, meta));
        } else if (version === 3) {
            const info = await invoke<VaultV3Info>('vault_v3_open', { vaultPath, password });
            setEntries(mapV3InfoToEntries(info));
            setMeta(mapV3InfoToMeta(info, meta));
        } else if (version === 2) {
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
            if (isPlaintextZip) {
                const info = await invoke<VaultV3Info>('aerovz_open_archive', { vaultPath });
                setContainerKind('zip');
                setVaultSecurity({ version: 3, cascadeMode: false, level: 'experimental', plaintext: true });
                setEntries(mapV3InfoToEntries(info, false));
                setMeta(mapV3InfoToMeta(info, meta));
                try {
                    const status = await invoke<{ embedded: boolean; detached: boolean; header_parity?: boolean }>('aerovz_recovery_status', { path: vaultPath });
                    setHasErrorCorrection(!!status.embedded);
                    setHasDetachedRecovery(!!status.detached);
                    setHasDetachedHeaderRecovery(!!status.header_parity);
                } catch {
                    setHasErrorCorrection(false);
                    setHasDetachedRecovery(false);
                    setHasDetachedHeaderRecovery(false);
                }
                setMode('browse');

                const vName = vaultPath.split(/[\\/]/).pop() || 'AeroVault Zip';
                await saveToHistory(vaultPath, vName, 'aerovault-zip', 3, false, info.file_count);
            } else if (vaultSecurity?.version === 3) {
                const info = await invoke<VaultV3Info>('vault_v3_open', { vaultPath, password });
                setVaultSecurity({ version: 3, cascadeMode: false, level: 'experimental' });
                setEntries(mapV3InfoToEntries(info));
                setMeta(mapV3InfoToMeta(info, meta));
                // P2: detect Error Correction for badge and enabling scrub/repair actions in this session.
                // recovery_status reports both the embedded extension and a detached .aerocorrect sidecar.
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
            if (isPlaintextZip) {
                if (currentDir) {
                    const result = await invoke<{ added: number; total: number }>('aerovz_add_files_to_dir', {
                        vaultPath,
                        filePaths: paths,
                        targetDir: currentDir
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.filesAdded', { count: result.added.toString() }));
                } else {
                    const info = await invoke<VaultV3Info>('aerovz_add_files', {
                        vaultPath,
                        filePaths: paths,
                    });
                    setEntries(mapV3InfoToEntries(info, false));
                    setMeta(mapV3InfoToMeta(info, meta));
                    setLastReport(null);
                    setSuccess(t('vault.filesAdded', { count: paths.length.toString() }));
                }
            } else if (vaultSecurity?.version === 3) {
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

    // Ehud #2: first-class "add folder" via a native folder picker (previously a
    // folder could only be added by drag-and-drop). Adds the whole tree under the
    // current directory, preserving structure.
    const handleAddFolder = async () => {
        const selected = await open({ directory: true });
        if (!selected || typeof selected !== 'string') return;

        setLoading(true);
        setError(null);
        try {
            if (isPlaintextZip) {
                await invoke('aerovz_add_directory', {
                    vaultPath,
                    sourceDir: selected,
                    targetPrefix: currentDir || null,
                });
            } else if (vaultSecurity?.version === 3) {
                await invoke('vault_v3_add_directory', {
                    vaultPath,
                    password,
                    sourceDir: selected,
                    targetPrefix: currentDir || null,
                });
            } else if (vaultSecurity?.version === 2) {
                await invoke('vault_v2_add_directory', {
                    vaultPath,
                    password,
                    sourceDir: selected,
                });
            } else {
                setError(t('vault.addFolderUnsupported'));
                return;
            }
            await refreshVaultEntries();
            setSuccess(t('vault.folderAdded'));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleDropFiles = useCallback(async (paths: string[]) => {
        if (!paths.length || !vaultPath || (!password && !isPlaintextZip) || loading) return;

        setLoading(true);
        setError(null);
        try {
            const targetDir = dragTargetDir || currentDir;
            // Ehud #2/#8: a drop (or a mixed pick) can include folders; route the
            // files through add_files and each folder through add_directory so a
            // dropped directory keeps its tree instead of erroring as a "file".
            const { files, dirs } = await splitPathsByType(paths);
            if (!files.length && !dirs.length) return;
            const totalCount = files.length + dirs.length;

            if (isPlaintextZip) {
                if (files.length) {
                    if (targetDir) {
                        await invoke('aerovz_add_files_to_dir', { vaultPath, filePaths: files, targetDir });
                    } else {
                        await invoke<VaultV3Info>('aerovz_add_files', { vaultPath, filePaths: files });
                        setLastReport(null);
                    }
                }
                for (const dir of dirs) {
                    await invoke('aerovz_add_directory', { vaultPath, sourceDir: dir, targetPrefix: targetDir || null });
                }
                await refreshVaultEntries();
                setSuccess(t('vault.filesAdded', { count: totalCount.toString() }));
            } else if (vaultSecurity?.version === 3) {
                if (files.length) {
                    if (targetDir) {
                        await invoke('vault_v3_add_files_to_dir', { vaultPath, password, filePaths: files, targetDir });
                    } else {
                        const info = await invoke<VaultV3Info>('vault_v3_add_files', { vaultPath, password, filePaths: files });
                        setLastReport(info.report ?? null);
                    }
                }
                for (const dir of dirs) {
                    await invoke('vault_v3_add_directory', { vaultPath, password, sourceDir: dir, targetPrefix: targetDir || null });
                }
                await refreshVaultEntries();
                setSuccess(t('vault.filesAdded', { count: totalCount.toString() }));
            } else if (vaultSecurity?.version === 2) {
                if (files.length) {
                    const result = targetDir
                        ? await invoke<{ added: number; total: number; report?: VaultReport }>('vault_v2_add_files_to_dir', {
                            vaultPath, password, filePaths: files, targetDir
                        })
                        : await invoke<{ added: number; total: number; report?: VaultReport }>('vault_v2_add_files', {
                            vaultPath, password, filePaths: files
                        });
                    setLastReport(result.report ?? null);
                }
                for (const dir of dirs) {
                    await invoke('vault_v2_add_directory', { vaultPath, password, sourceDir: dir });
                }
                await refreshVaultEntries();
                setSuccess(t('vault.filesAdded', { count: totalCount.toString() }));
            } else {
                // v1 has no directory support: add files, surface folders as unsupported.
                if (files.length) {
                    const v1res = await invoke<{ report?: VaultReport }>('vault_add_files', { vaultPath, password, filePaths: files });
                    setLastReport(v1res.report ?? null);
                }
                await refreshVaultEntries();
                if (dirs.length) setError(t('vault.addFolderUnsupported'));
                else setSuccess(t('vault.filesAdded', { count: files.length.toString() }));
            }
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
            setDragTargetDir(null);
        }
    }, [vaultPath, password, currentDir, dragTargetDir, vaultSecurity, isPlaintextZip, loading, t]);

    // Ehud #2: OS drag&drop onto the CREATE screen stages files/folders into the
    // not-yet-created vault (handleCreate adds them after sealing the container).
    const handleStageDrop = useCallback(async (paths: string[]) => {
        if (!paths.length) return;
        const { files, dirs } = await splitPathsByType(paths);
        if (files.length) setStagedFiles(prev => Array.from(new Set([...prev, ...files])));
        if (dirs.length) setStagedDirs(prev => Array.from(new Set([...prev, ...dirs])));
    }, []);

    const removeStagedFile = useCallback((p: string) => setStagedFiles(prev => prev.filter(x => x !== p)), []);
    const removeStagedDir = useCallback((p: string) => setStagedDirs(prev => prev.filter(x => x !== p)), []);

    const handleCreateDirectory = async () => {
        const trimmed = newDirName.trim();
        if (!trimmed) return;

        setLoading(true);
        setError(null);
        try {
            const fullPath = currentDir ? `${currentDir}/${trimmed}` : trimmed;
            if (isPlaintextZip) {
                await invoke('aerovz_create_directory', {
                    vaultPath,
                    dirName: fullPath
                });
            } else if (vaultSecurity?.version === 3) {
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
            if (isPlaintextZip) {
                if (isDir) {
                    const result = await invoke<{ removed: number; remaining: number }>('aerovz_delete_entries', {
                        vaultPath,
                        entryNames: [entryName],
                        recursive: true
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.itemsDeleted', { count: result.removed.toString() }));
                } else {
                    await invoke<{ deleted: string; remaining: number }>('aerovz_delete_entry', {
                        vaultPath,
                        entryName
                    });
                    await refreshVaultEntries();
                    setSuccess(t('vault.itemDeleted', { name: entryName.split('/').pop() || entryName }));
                }
            } else if (vaultSecurity?.version === 3) {
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
            if (isPlaintextZip) {
                await invoke('aerovz_extract_entry', {
                    vaultPath,
                    entryName,
                    destPath: savePath
                });
            } else if (vaultSecurity?.version === 3) {
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

    // Ehud #2: one-click "extract all" to a chosen folder (v3 recursive backend).
    const handleExtractAll = async () => {
        const destPath = await open({ directory: true });
        if (!destPath || typeof destPath !== 'string') return;

        setLoading(true);
        try {
            let count: number;
            if (isPlaintextZip) {
                count = await invoke<number>('aerovz_extract_all', { vaultPath, destPath });
            } else if (vaultSecurity?.version === 2) {
                // v2 returns { extracted, dest } (the crate names the param `destDir`).
                const res = await invoke<{ extracted: number }>('vault_v2_extract_all', {
                    vaultPath,
                    password,
                    destDir: destPath,
                });
                count = res.extracted;
            } else {
                count = await invoke<number>('vault_v3_extract_all', { vaultPath, password, destPath });
            }
            setSuccess(t('vault.extractedAll', { count: String(count), path: destPath }));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    // AeroMount Save-All (#322, Ehud idea #1): export the whole decrypted tree in
    // one shot. Folder reuses the existing extract-all; Zip / Archive stream the
    // contents into a single plaintext .zip / .aerozip via the ReadableVault seam.
    // SECURITY: this writes PLAINTEXT to the chosen path; the SaveAllMenu confirms
    // that intent (with an extra "not encrypted" note for the .zip target) first.
    const handleSaveAll = async (target: 'folder' | 'zip' | 'aerozip') => {
        if (target === 'folder') { await handleExtractAll(); return; }
        const base = (vaultPath.split(/[\\/]/).pop() || 'vault').replace(/\.(aerovault|aerozip)$/i, '');
        const destPath = await save({
            defaultPath: target === 'zip' ? `${base}.zip` : `${base}.aerozip`,
            filters: target === 'zip'
                ? [{ name: 'Zip', extensions: ['zip'] }]
                : [{ name: 'AeroZip', extensions: ['aerozip'] }],
        });
        if (!destPath) return;

        setLoading(true);
        try {
            const report = isPlaintextZip
                ? await invoke<{ files: number; dirs: number; skipped: string[] }>('aerovz_save_all', { vaultPath, destPath, target })
                : vaultSecurity?.version === 2
                    ? await invoke<{ files: number; dirs: number; skipped: string[] }>('vault_v2_save_all', { vaultPath, password, destPath, target })
                    : await invoke<{ files: number; dirs: number; skipped: string[] }>('vault_v3_save_all', { vaultPath, password, destPath, target });
            const skippedNote = report.skipped.length ? ` ${t('saveAll.skipped', { count: String(report.skipped.length) })}` : '';
            setSuccess(`${t('saveAll.done', { count: String(report.files), path: destPath })}${skippedNote}`);
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    // Ehud #2: a complete technical report of the open vault — metadata,
    // encryption pipeline, error-correction state, chunk-level stats (re-fetched
    // from the backend, richer than the UI-mapped entries) and the full file
    // list. Shared by Export .txt/.json (save dialog) and Copy (clipboard).
    const profileForLevel = (lvl: number): string => (lvl === 3 ? 'fast' : lvl === 15 || lvl === 19 ? 'archive' : 'balanced');

    const buildVaultReport = async (format: 'txt' | 'json'): Promise<string> => {
        const vaultName = (vaultPath.split(/[\\/]/).pop() || 'vault').replace(/\.(aerovault|aerozip)$/i, '');
        const generatedAt = new Date().toISOString();
        const levelConfig = vaultSecurity ? securityLevels[vaultSecurity.level] : null;
        const algorithms = isPlaintextZip
            ? ['Plaintext content', 'zstd per chunk', 'BLAKE3 integrity', 'Reed-Solomon recovery']
            : (levelConfig?.features ?? []);

        // Re-fetch full technical info (chunk_count, dedup, compression level,
        // per-file chunk counts) which the UI entries/meta mapping discards.
        let v3tech: VaultV3Info | null = null;
        let v2tech: VaultV2Info | null = null;
        let fileList: { name: string; size: number; is_dir: boolean; modified: string; chunk_count?: number }[] =
            entries.map(e => ({ name: e.name, size: e.size, is_dir: e.isDir, modified: e.modified || '' }));
        try {
            if (isPlaintextZip) {
                v3tech = await invoke<VaultV3Info>('aerovz_open_archive', { vaultPath });
                fileList = v3tech.files.map(f => ({ name: f.name, size: f.size, is_dir: f.is_dir, modified: f.modified, chunk_count: f.chunk_count }));
            } else if (vaultSecurity?.version === 3) {
                v3tech = await invoke<VaultV3Info>('vault_v3_open', { vaultPath, password });
                fileList = v3tech.files.map(f => ({ name: f.name, size: f.size, is_dir: f.is_dir, modified: f.modified, chunk_count: f.chunk_count }));
            } else if (vaultSecurity?.version === 2) {
                v2tech = await invoke<VaultV2Info>('vault_v2_open', { vaultPath, password });
                fileList = v2tech.files.map(f => ({ name: f.name, size: f.size, is_dir: f.is_dir, modified: f.modified }));
            }
        } catch {
            // best-effort: keep the in-state fallback file list
        }

        const fileEntries = fileList.filter(f => !f.is_dir);
        const dirCount = fileList.length - fileEntries.length;
        const totalSize = fileEntries.reduce((acc, f) => acc + (f.size || 0), 0);
        const compressionProfileLabel = v3tech ? profileForLevel(v3tech.compression_level) : compressionProfile;

        if (format === 'json') {
            return JSON.stringify({
                aerovault_report: {
                    generated_at: generatedAt,
                    tool: 'AeroFTP',
                    vault: {
                        name: vaultName,
                        path: vaultPath,
                        format_version: meta?.version ?? vaultSecurity?.version ?? null,
                        security_level: isPlaintextZip ? 'aerovault-zip' : (vaultSecurity?.level ?? null),
                        cascade_mode: vaultSecurity?.cascadeMode ?? false,
                        description: meta?.description ?? null,
                        created: meta?.created ?? null,
                        modified: meta?.modified ?? null,
                        confidential: !isPlaintextZip,
                        encryption_pipeline: algorithms,
                        compression_profile: compressionProfileLabel,
                        error_correction: {
                            embedded: hasErrorCorrection,
                            detached: hasDetachedRecovery,
                            detached_header_protected: hasDetachedHeaderRecovery,
                        },
                    },
                    technical: v3tech
                        ? {
                            file_count: v3tech.file_count,
                            chunk_count: v3tech.chunk_count,
                            dedup_chunks: v3tech.dedup_chunks,
                            compression_level: v3tech.compression_level,
                        }
                        : v2tech
                            ? { file_count: v2tech.file_count, chunk_size: v2tech.chunk_size, cascade_mode: v2tech.cascade_mode }
                            : null,
                    summary: {
                        total_items: fileList.length,
                        files: fileEntries.length,
                        directories: dirCount,
                        total_size_bytes: totalSize,
                    },
                    files: fileList,
                    last_operation_receipt: lastReport ?? null,
                },
            }, null, 2);
        }

        // Human-readable text report.
        const L: string[] = [];
        L.push('AeroVault — Vault Report');
        L.push('========================');
        L.push(`Generated:          ${generatedAt}`);
        L.push('');
        L.push('VAULT');
        L.push(`  Name:             ${vaultName}`);
        L.push(`  Path:             ${vaultPath}`);
        L.push(`  Format:           v${meta?.version ?? vaultSecurity?.version ?? '?'}`);
        L.push(`  Security level:   ${isPlaintextZip ? 'AeroVault Zip (not confidential)' : `${vaultSecurity?.level ?? '?'}${vaultSecurity?.cascadeMode ? ' (cascade)' : ''}`}`);
        if (meta?.description) L.push(`  Description:      ${meta.description}`);
        if (meta?.created) L.push(`  Created:          ${meta.created}`);
        if (meta?.modified) L.push(`  Modified:         ${meta.modified}`);
        if (algorithms.length) L.push(`${isPlaintextZip ? '  Processing:       ' : '  Encryption:       '}${algorithms.join(' · ')}`);
        L.push(`  Compression:      ${compressionProfileLabel}${v3tech ? ` (zstd -${v3tech.compression_level})` : ''}`);
        const ecState = [hasErrorCorrection ? 'embedded' : null, hasDetachedRecovery ? 'detached' : null].filter(Boolean).join(' + ') || 'none';
        L.push(`  Error correction: ${ecState}${hasDetachedHeaderRecovery ? ' (header protected)' : ''}`);
        L.push('');
        L.push('TECHNICAL');
        if (v3tech) {
            L.push(`  Files:            ${v3tech.file_count}`);
            L.push(`  Chunks:           ${v3tech.chunk_count} (${v3tech.dedup_chunks} dedup)`);
            L.push(`  zstd level:       ${v3tech.compression_level}`);
        } else if (v2tech) {
            L.push(`  Files:            ${v2tech.file_count}`);
            L.push(`  Chunk size:       ${formatSize(v2tech.chunk_size)}`);
            L.push(`  Cascade mode:     ${v2tech.cascade_mode}`);
        } else {
            L.push('  (chunk-level stats unavailable)');
        }
        L.push('');
        L.push('SUMMARY');
        L.push(`  Items:            ${fileList.length} (${fileEntries.length} files, ${dirCount} folders)`);
        L.push(`  Total size:       ${formatSize(totalSize)}`);
        L.push('');
        L.push(`FILES (${fileList.length})`);
        for (const f of [...fileList].sort((a, b) => a.name.localeCompare(b.name))) {
            const tag = f.is_dir ? 'D' : 'F';
            const size = f.is_dir ? '' : `  (${formatSize(f.size || 0)})`;
            const chunks = f.chunk_count != null ? `  [${f.chunk_count} chunk${f.chunk_count === 1 ? '' : 's'}]` : '';
            const mod = f.modified ? `  ${f.modified}` : '';
            L.push(`  [${tag}] ${f.name}${size}${chunks}${mod}`);
        }
        if (lastReport) {
            L.push('');
            L.push('LAST OPERATION RECEIPT');
            L.push(`  Operation:        ${lastReport.operation} (v${lastReport.vault_format})`);
            L.push(`  Pipeline:         ${lastReport.algorithms.join(' → ')}`);
            L.push(`  Profile:          ${lastReport.profile ?? '-'}`);
            L.push(`  Files / packed:   ${lastReport.files} / ${lastReport.packed_files}`);
            L.push(`  Chunks L/N/D:     ${lastReport.logical_chunks} / ${lastReport.new_physical_chunks} / ${lastReport.dedup_hits}`);
            if (lastReport.cdc_avg) L.push(`  CDC min/avg/max:  ${lastReport.cdc_min} / ${lastReport.cdc_avg} / ${lastReport.cdc_max}`);
            L.push(`  Plaintext:        ${formatSize(lastReport.plaintext_bytes)}`);
            L.push(`  Compressed:       ${formatSize(lastReport.compressed_bytes)}`);
            L.push(`  Encrypted:        ${formatSize(lastReport.encrypted_bytes)}`);
            if (lastReport.compressed_bytes > 0) L.push(`  Compression:      ${lastReport.compression_ratio_pct.toFixed(1)}%`);
            if (lastReport.error_correction_shards_generated != null) {
                L.push(`  EC shards:        ${lastReport.error_correction_shards_generated}`);
                L.push(`  EC protected:     ${formatSize(lastReport.error_correction_bytes_protected ?? 0)}`);
                L.push(`  EC overhead:      ${(lastReport.error_correction_overhead_pct ?? 0).toFixed(1)}%`);
            }
            L.push(`  Elapsed:          ${lastReport.time_elapsed.toFixed(1)} s`);
        }
        L.push('');
        L.push('Generated by AeroFTP · AeroVault');
        return L.join('\n');
    };

    const handleExportVaultReport = async (format: 'txt' | 'json') => {
        setLoading(true);
        setError(null);
        try {
            const content = await buildVaultReport(format);
            const vaultName = (vaultPath.split(/[\\/]/).pop() || 'vault').replace(/\.(aerovault|aerozip)$/i, '');
            const filePath = await save({
                defaultPath: `${vaultName}-report.${format}`,
                filters: [{ name: `AeroVault report (.${format})`, extensions: [format] }],
            });
            if (!filePath) return;
            await writeTextFile(filePath, content);
            setSuccess(t('vault.reportExported', { path: filePath }));
        } catch (e) {
            setError(mapVaultError(e, t));
        } finally {
            setLoading(false);
        }
    };

    const handleCopyVaultReport = async () => {
        setError(null);
        try {
            const content = await buildVaultReport('txt');
            await navigator.clipboard.writeText(content);
            setSuccess(t('vault.reportCopied'));
        } catch (e) {
            setError(mapVaultError(e, t));
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

    // Change Mode (Ehud #2): re-pack a v3 vault under a new compression profile + EC,
    // keeping the same password. The backend repack (decrypt -> recreate -> re-add ->
    // atomic swap) is the same engine the full v2<->v3 security-level change will reuse.
    const handleChangeMode = async () => {
        if (!vaultPath || !vaultSecurity) return;
        const targetConfig = securityLevels[newModeSecurityLevel];
        const targetIsV3 = targetConfig.version === 3;
        // Error Correction lives only in v3; the backend ignores it for a v2 target,
        // so never request it when moving down to a v2 level.
        const ec = targetIsV3 && newModeErrorCorrection;
        setLoading(true);
        setError(null);
        try {
            const repack = await invoke<{ files?: number; format?: number }>('vault_v3_change_mode', {
                vaultPath,
                password,
                targetSecurityLevel: newModeSecurityLevel,
                compressionProfile: targetIsV3 ? newModeProfile : null,
                errorCorrection: ec,
            });
            // The container was rewritten in place (possibly across formats); reload
            // entries/meta using the TARGET format (vaultSecurity state is still the
            // old version at this point) and reflect the new security/EC shape.
            await refreshVaultEntries(targetConfig.version);
            setErrorCorrectionEnabled(ec);
            setHasErrorCorrection(ec);
            setHasDetachedRecovery(false);
            if (targetIsV3) setCompressionProfile(newModeProfile);
            setVaultSecurity({
                version: targetConfig.version,
                cascadeMode: targetConfig.cascade,
                level: newModeSecurityLevel,
            });
            // Recent Vaults history was written at create time with the OLD level/format;
            // refresh it so the entry reflects the new mode/format/file count after the
            // repack (otherwise the history badge keeps showing e.g. "Standard / 2 files").
            const vName = vaultPath.split(/[\\/]/).pop() || 'Vault';
            await saveToHistory(vaultPath, vName, newModeSecurityLevel, targetConfig.version, targetConfig.cascade, repack?.files ?? 0);
            // Change Mode produces no receipt, so it would otherwise be invisible in the
            // Activity Log; report it explicitly through the optional sink.
            onActivityLog?.(
                t('vault.modeChanged'),
                `${vName} -> ${newModeSecurityLevel} (v${targetConfig.version}), ${repack?.files ?? 0} file(s)`,
            );
            setChangingMode(false);
            setSuccess(t('vault.modeChanged'));
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
            detectVaultVersion(initialPath).then((security) => {
                if (initialPath.toLowerCase().endsWith('.aerozip') && !security.plaintext) {
                    setError(t('vault.zipEncryptedRejected'));
                    setContainerKind('vault');
                    setVaultSecurity(security);
                    return;
                }
                setContainerKind(security.plaintext ? 'zip' : 'vault');
                setVaultSecurity(security);
            }).catch(() => {});
        }
    }, [initialPath, t, vaultSecurity]);

    // Plaintext Zip (.aerozip) needs no password: skip the open/confirm screen
    // and browse immediately, so a double-click (or the "Open with AeroVault Zip"
    // menu / Recent Vaults) lands straight on the contents. Encrypted containers
    // still show the password prompt. The ref records the path we auto-opened so
    // a failed unlock (error, mode stays 'open') does not retry in a loop.
    // handleUnlock is intentionally not in the deps (recreated each render).
    const autoOpenedZipRef = useRef<string | null>(null);
    useEffect(() => {
        if (mode === 'open' && isPlaintextZip && vaultPath && !loading
            && entries.length === 0 && autoOpenedZipRef.current !== vaultPath) {
            autoOpenedZipRef.current = vaultPath;
            void handleUnlock();
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [mode, isPlaintextZip, vaultPath, loading, entries.length]);

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

    // Ehud #2: OS file drag-and-drop onto the create screen (stages into the
    // new vault). Separate from the browse listener so each only fires in its
    // own mode.
    useEffect(() => {
        if (mode !== 'create') return;

        const webview = getCurrentWebview();
        return guardedUnlisten(webview.onDragDropEvent((event) => {
            if (event.payload.type === 'over' || event.payload.type === 'enter') {
                setDragOver(true);
            } else if (event.payload.type === 'drop') {
                setDragOver(false);
                if (event.payload.paths.length > 0) {
                    handleStageDrop(event.payload.paths);
                }
            } else if (event.payload.type === 'leave') {
                setDragOver(false);
            }
        }));
    }, [mode, handleStageDrop]);

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

    // Byte-level vault progress (compress + encrypt) for every add/create op.
    useEffect(() => {
        return guardedUnlisten(listen<VaultProgress>('vault_progress', (event) => {
            setVaultProgress(event.payload);
        }));
    }, []);

    // Clear the progress bar once the operation finishes.
    useEffect(() => {
        if (!loading) setVaultProgress(null);
    }, [loading]);

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
            const res = isPlaintextZip
                ? await invoke<any>('aerovz_scrub', { vaultPath })
                : await invoke<any>('vault_v3_scrub', { vaultPath, password });
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
            const res = isPlaintextZip
                ? await invoke<any>('aerovz_repair', { vaultPath, dryRun: repairDryRun })
                : await invoke<any>('vault_v3_repair', { vaultPath, password, dryRun: repairDryRun });
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

    // --- SIDECAR: detached recovery file (.aerocorrect) ---
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
        containerKind, setContainerKind,
        isPlaintextZip,
        vaultPath, setVaultPath,
        password, setPassword,
        confirmPassword, setConfirmPassword,
        description, setDescription,
        showPassword, setShowPassword,
        loading,
        vaultProgress,
        error, setError,
        success, setSuccess,
        lastReport,
        clearReport: () => setLastReport(null),
        zipReport,
        entries,
        meta,
        currentDir, setCurrentDir,
        newDirName, setNewDirName,
        showNewDirDialog, setShowNewDirDialog,
        changingPassword, setChangingPassword,
        changingMode, setChangingMode,
        newModeSecurityLevel, setNewModeSecurityLevel,
        newModeProfile, setNewModeProfile,
        newModeErrorCorrection, setNewModeErrorCorrection,
        handleChangeMode,
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
        initialFiles: stagedFiles,
        stagedFiles,
        stagedDirs,
        removeStagedFile,
        removeStagedDir,
        handleStageDrop,
        resetState,
        detectVaultVersion,
        handleCreate,
        handleOpen,
        handleUnlock,
        refreshVaultEntries,
        handleAddFiles,
        handleAddFolder,
        handleDropFiles,
        handleCreateDirectory,
        handleRemove,
        handleExtract,
        handleExtractAll,
        handleSaveAll,
        handleExportVaultReport,
        handleCopyVaultReport,
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
