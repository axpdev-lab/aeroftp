import * as React from 'react';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Shield, Plus, Trash2, Download, Key, FolderPlus, X, Eye, EyeOff, Loader2, Lock, File, Folder, Zap, ShieldCheck, ShieldAlert, ChevronDown, ChevronRight, ArrowLeft } from 'lucide-react';
import { VaultIcon } from './icons/VaultIcon';
import { ArchiveEntry, AeroVaultMeta } from '../types';
import { useTranslation } from '../i18n';
import { formatDate, formatSize } from '../utils/formatters';

interface VaultPanelProps {
    onClose: () => void;
    isConnected?: boolean;
}

type VaultMode = 'home' | 'create' | 'open' | 'browse';

// Security levels for vault creation
type SecurityLevel = 'standard' | 'advanced' | 'paranoid';

interface VaultSecurityInfo {
    version: number;
    cascadeMode: boolean;
    level: SecurityLevel;
}

// v2 vault info from backend
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

// Security level configuration — hardcoded labels (no i18n, technical terms)
const securityLevels = {
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

export const VaultPanel: React.FC<VaultPanelProps> = ({ onClose, isConnected = false }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<VaultMode>('home');
    const [vaultPath, setVaultPath] = useState('');
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

    // Remote vault state
    const [remoteVaultPath, setRemoteVaultPath] = useState('');
    const [remoteLocalPath, setRemoteLocalPath] = useState('');
    const [remoteLoading, setRemoteLoading] = useState(false);
    const [showRemoteInput, setShowRemoteInput] = useState(false);

    // New state for unified vault support
    const [securityLevel, setSecurityLevel] = useState<SecurityLevel>('advanced');
    const [vaultSecurity, setVaultSecurity] = useState<VaultSecurityInfo | null>(null);
    const [showLevelDropdown, setShowLevelDropdown] = useState(false);

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
    };

    const detectVaultVersion = async (path: string): Promise<VaultSecurityInfo> => {
        try {
            // First try to peek v2 header (reads cascade_mode without password)
            const peek = await invoke<{ version: number; cascade_mode: boolean; security_level: string }>('vault_v2_peek', { path });
            const level: SecurityLevel = peek.cascade_mode ? 'paranoid' : 'advanced';
            return { version: 2, cascadeMode: peek.cascade_mode, level };
        } catch {
            // Not v2 or error - check if v1
            try {
                const isV2 = await invoke<boolean>('is_vault_v2', { path });
                if (isV2) {
                    return { version: 2, cascadeMode: false, level: 'advanced' };
                }
            } catch { /* ignore */ }
            return { version: 1, cascadeMode: false, level: 'standard' };
        }
    };

    const handleCreate = async () => {
        if (password.length < 8) { setError(t('vault.passwordTooShort')); return; }
        if (password !== confirmPassword) { setError(t('vault.passwordMismatch')); return; }

        const savePath = await save({ defaultPath: 'vault.aerovault', filters: [{ name: 'AeroVault', extensions: ['aerovault'] }] });
        if (!savePath) return;

        setLoading(true);
        setError(null);

        const levelConfig = securityLevels[securityLevel];

        try {
            if (levelConfig.version === 2) {
                // Create v2 vault
                await invoke('vault_v2_create', {
                    vaultPath: savePath,
                    password,
                    description: description || null,
                    cascadeMode: levelConfig.cascade
                });
                setVaultPath(savePath);
                setVaultSecurity({ version: 2, cascadeMode: levelConfig.cascade, level: securityLevel });
                setSuccess(t('vault.created'));
                setMode('browse');
                setEntries([]);
                setMeta({
                    version: 2,
                    description: description || null,
                    created: new Date().toISOString(),
                    modified: new Date().toISOString(),
                    fileCount: 0
                });
            } else {
                // Create v1 vault (legacy)
                await invoke('vault_create', { vaultPath: savePath, password, description: description || null });
                setVaultPath(savePath);
                setVaultSecurity({ version: 1, cascadeMode: false, level: 'standard' });
                setSuccess(t('vault.created'));
                setMode('browse');
                setEntries([]);
                const m = await invoke<AeroVaultMeta>('vault_get_meta', { vaultPath: savePath, password });
                setMeta(m);
            }
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleOpen = async () => {
        const selected = await open({ filters: [{ name: 'AeroVault', extensions: ['aerovault'] }] });
        if (!selected) return;
        const path = selected as string;
        setVaultPath(path);

        // Detect version before showing password prompt
        const security = await detectVaultVersion(path);
        setVaultSecurity(security);
        setMode('open');
    };

    // Remote Vault: download from server, open locally
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
            setError(String(e));
        } finally {
            setRemoteLoading(false);
        }
    };

    // Remote Vault: upload changes back to server and cleanup
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
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    // Remote Vault: cleanup without uploading
    const handleCleanupRemote = async () => {
        if (!remoteLocalPath) return;
        try {
            await invoke('vault_v2_cleanup_temp', { localPath: remoteLocalPath });
        } catch { /* best-effort cleanup */ }
        setRemoteLocalPath('');
        setRemoteVaultPath('');
    };

    const handleUnlock = async () => {
        setLoading(true);
        setError(null);

        try {
            if (vaultSecurity?.version === 2) {
                // Open v2 vault
                const info = await invoke<VaultV2Info>('vault_v2_open', { vaultPath, password });
                const secLevel: SecurityLevel = info.cascade_mode ? 'paranoid' : 'advanced';
                setVaultSecurity({ version: 2, cascadeMode: info.cascade_mode, level: secLevel });

                // Convert v2 files to ArchiveEntry format
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
            } else {
                // Open v1 vault (legacy)
                const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
                setEntries(list);
                const m = await invoke<AeroVaultMeta>('vault_get_meta', { vaultPath, password });
                setMeta(m);
                setMode('browse');
            }
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
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

    const handleAddFiles = async () => {
        const selected = await open({ multiple: true });
        if (!selected || (Array.isArray(selected) && selected.length === 0)) return;
        const paths = Array.isArray(selected) ? selected as string[] : [selected as string];

        setLoading(true);
        setError(null);
        try {
            if (vaultSecurity?.version === 2) {
                // Add files to v2 vault (to current directory if browsing a subdirectory)
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
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

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
            setError(String(e));
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
                    // Use recursive delete for directories
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
            setError(String(e));
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
            setError(String(e));
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
                // Change password for v2 vault
                await invoke('vault_v2_change_password', {
                    vaultPath,
                    oldPassword: password,
                    newPassword
                });
            } else {
                // Change password for v1 vault
                await invoke('vault_change_password', { vaultPath, oldPassword: password, newPassword });
            }
            setPassword(newPassword);
            setChangingPassword(false);
            setNewPassword('');
            setConfirmNewPassword('');
            setSuccess(t('vault.passwordChanged'));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const vaultName = vaultPath.split(/[\\/]/).pop() || 'Vault';
    const currentLevelConfig = vaultSecurity ? securityLevels[vaultSecurity.level] : null;
    const LevelIcon = currentLevelConfig?.icon || Shield;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-[680px] max-h-[85vh] flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <VaultIcon size={18} className="text-emerald-400" />
                        <span className="font-medium">
                            {mode === 'browse' ? vaultName : t('vault.title')}
                        </span>
                        {/* Security badge in browse mode */}
                        {mode === 'browse' && currentLevelConfig && (
                            <span className={`ml-2 px-2 py-0.5 rounded text-xs font-medium ${currentLevelConfig.bgColor} bg-opacity-20 ${currentLevelConfig.color}`}>
                                <LevelIcon size={10} className="inline mr-1" />
                                {currentLevelConfig.label}
                            </span>
                        )}
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded"><X size={18} /></button>
                </div>

                {/* Error / Success */}
                {error && <div className="px-4 py-2 bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400 text-sm">{error}</div>}
                {success && <div className="px-4 py-2 bg-green-100 dark:bg-green-900/30 text-green-600 dark:text-green-400 text-sm">{success}</div>}

                {/* Home */}
                {mode === 'home' && (
                    <div className="p-6 flex flex-col items-center gap-5">
                        {/* Security badge */}
                        <div className="relative">
                            <VaultIcon size={56} className="text-emerald-400" />
                            <div className="absolute -bottom-1 -right-1 bg-emerald-500 rounded-full p-1">
                                <Lock size={12} className="text-white" />
                            </div>
                        </div>

                        <p className="text-gray-600 dark:text-gray-300 text-center text-sm max-w-md">
                            {t('vault.descriptionV2')}
                        </p>

                        {/* Security levels preview */}
                        <div className="flex gap-2 text-xs">
                            {Object.entries(securityLevels).map(([key, config]) => {
                                const Icon = config.icon;
                                return (
                                    <div key={key} className={`flex items-center gap-1.5 px-2 py-1 rounded border ${config.borderColor} bg-opacity-10`}>
                                        <Icon size={12} className={config.color} />
                                        <span className={config.color}>{config.label}</span>
                                    </div>
                                );
                            })}
                        </div>

                        <div className="flex gap-3 mt-1">
                            <button onClick={() => { resetState(); setMode('create'); }} className="flex items-center gap-2 px-4 py-2 bg-emerald-600 hover:bg-emerald-500 rounded text-sm font-medium">
                                <FolderPlus size={16} /> {t('vault.createNew')}
                            </button>
                            <button onClick={handleOpen} className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-500 rounded text-sm font-medium">
                                <Lock size={16} /> {t('vault.openExisting')}
                            </button>
                        </div>

                        {/* Remote Vault — only when connected to a server */}
                        {isConnected && (
                            <div className="w-full max-w-md mt-2 space-y-2">
                                {!showRemoteInput ? (
                                    <button
                                        onClick={() => setShowRemoteInput(true)}
                                        className="flex items-center gap-2 px-4 py-2 rounded text-sm font-medium
                                            bg-purple-600/20 text-purple-400 hover:bg-purple-600/30 transition-colors w-full justify-center"
                                    >
                                        <Download size={16} />
                                        {t('vault.remote.open')}
                                    </button>
                                ) : (
                                    <div className="p-3 rounded-lg border border-purple-500/30 bg-purple-500/5 space-y-2">
                                        <p className="text-xs text-purple-400">{t('vault.remote.title')}</p>
                                        <input
                                            type="text"
                                            value={remoteVaultPath}
                                            onChange={e => setRemoteVaultPath(e.target.value)}
                                            placeholder="/path/to/vault.aerovault"
                                            className="w-full px-3 py-1.5 rounded text-sm bg-gray-800 border border-gray-600 text-white placeholder:text-gray-500"
                                        />
                                        <div className="flex gap-2">
                                            <button
                                                onClick={() => { setShowRemoteInput(false); setRemoteVaultPath(''); }}
                                                className="flex-1 py-1.5 rounded text-xs bg-gray-700 text-gray-300"
                                            >
                                                {t('security.totp.back')}
                                            </button>
                                            <button
                                                onClick={handleOpenRemoteVault}
                                                disabled={remoteLoading || !remoteVaultPath.endsWith('.aerovault')}
                                                className="flex-1 py-1.5 rounded text-xs bg-purple-600 text-white disabled:opacity-50 flex items-center justify-center gap-1"
                                            >
                                                {remoteLoading ? <Loader2 size={12} className="animate-spin" /> : <Download size={12} />}
                                                {remoteLoading ? t('vault.remote.downloading') : t('vault.remote.open')}
                                            </button>
                                        </div>
                                    </div>
                                )}
                                {!isConnected && (
                                    <p className="text-xs text-gray-500 text-center">{t('vault.remote.noConnection')}</p>
                                )}
                            </div>
                        )}
                    </div>
                )}

                {/* Create */}
                {mode === 'create' && (
                    <div className="p-4 flex flex-col gap-3">
                        {/* Security Level Selector */}
                        <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.securityLevel')}</label>
                        <div className="relative">
                            <button
                                onClick={() => setShowLevelDropdown(!showLevelDropdown)}
                                className={`w-full flex items-center justify-between px-3 py-2.5 rounded border ${securityLevels[securityLevel].borderColor} bg-gray-50 dark:bg-gray-900 text-left`}
                            >
                                <div className="flex items-center gap-2">
                                    {React.createElement(securityLevels[securityLevel].icon, {
                                        size: 16,
                                        className: securityLevels[securityLevel].color
                                    })}
                                    <div>
                                        <div className={`text-sm font-medium ${securityLevels[securityLevel].color}`}>
                                            {securityLevels[securityLevel].label}
                                            {securityLevel === 'advanced' && <span className="ml-2 text-xs text-emerald-300">({t('vault.securityRecommended')})</span>}
                                        </div>
                                        <div className="text-xs text-gray-500">{securityLevels[securityLevel].description}</div>
                                    </div>
                                </div>
                                <ChevronDown size={16} className="text-gray-500 dark:text-gray-400" />
                            </button>

                            {/* Dropdown */}
                            {showLevelDropdown && (
                                <div className="absolute z-10 mt-1 w-full bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-600 rounded-lg shadow-xl overflow-hidden">
                                    {(Object.keys(securityLevels) as SecurityLevel[]).map((level) => {
                                        const config = securityLevels[level];
                                        const Icon = config.icon;
                                        const isSelected = level === securityLevel;
                                        return (
                                            <button
                                                key={level}
                                                onClick={() => { setSecurityLevel(level); setShowLevelDropdown(false); }}
                                                className={`w-full flex items-start gap-3 px-3 py-3 text-left hover:bg-gray-100 dark:hover:bg-gray-800 ${isSelected ? 'bg-gray-100 dark:bg-gray-800' : ''}`}
                                            >
                                                <Icon size={18} className={`mt-0.5 ${config.color}`} />
                                                <div className="flex-1">
                                                    <div className={`text-sm font-medium ${config.color}`}>
                                                        {config.label}
                                                        {level === 'advanced' && <span className="ml-2 text-xs text-emerald-300">({t('vault.securityRecommended')})</span>}
                                                    </div>
                                                    <div className="text-xs text-gray-500 mt-0.5">{config.description}</div>
                                                    <div className="flex flex-wrap gap-1 mt-1.5">
                                                        {config.features.map((feature, i) => (
                                                            <span key={i} className="px-1.5 py-0.5 bg-gray-200 dark:bg-gray-700 rounded text-[10px] text-gray-600 dark:text-gray-300">
                                                                {feature}
                                                            </span>
                                                        ))}
                                                    </div>
                                                </div>
                                            </button>
                                        );
                                    })}
                                </div>
                            )}
                        </div>

                        <label className="text-sm text-gray-500 dark:text-gray-400 mt-2">{t('vault.description_label')}</label>
                        <input value={description} onChange={e => setDescription(e.target.value)}
                            className="bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm" placeholder="My secure vault" />

                        <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.password')}</label>
                        <div className="relative">
                            <input type={showPassword ? 'text' : 'password'} value={password} onChange={e => setPassword(e.target.value)}
                                className="w-full bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm pr-8" />
                            <button onClick={() => setShowPassword(!showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400">
                                {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                            </button>
                        </div>

                        <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.confirmPassword')}</label>
                        <input type={showPassword ? 'text' : 'password'} value={confirmPassword} onChange={e => setConfirmPassword(e.target.value)}
                            className="bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm" />

                        <div className="flex gap-2 justify-end mt-2">
                            <button onClick={() => setMode('home')} className="px-3 py-1.5 text-sm hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                                {t('vault.cancel')}
                            </button>
                            <button onClick={handleCreate} disabled={loading} className={`flex items-center gap-2 px-4 py-1.5 ${securityLevels[securityLevel].bgColor} hover:opacity-90 rounded text-sm disabled:opacity-50`}>
                                {loading && <Loader2 size={14} className="animate-spin" />}
                                {t('vault.create')}
                            </button>
                        </div>
                    </div>
                )}

                {/* Open (password prompt) */}
                {mode === 'open' && (
                    <div className="p-4 flex flex-col gap-3">
                        <p className="text-sm text-gray-500 dark:text-gray-400 truncate">{vaultPath}</p>

                        {/* Show detected version and security level */}
                        {vaultSecurity && (() => {
                            const levelConfig = securityLevels[vaultSecurity.level];
                            const LevelIcon = levelConfig.icon;
                            return (
                                <div className={`flex items-center gap-2 px-3 py-2 rounded border ${levelConfig.borderColor} bg-gray-100/50 dark:bg-gray-900/30`}>
                                    <LevelIcon size={16} className={levelConfig.color} />
                                    <span className={`text-sm ${levelConfig.color}`}>
                                        AeroVault v{vaultSecurity.version} ({levelConfig.label})
                                    </span>
                                </div>
                            );
                        })()}

                        <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.password')}</label>
                        <div className="relative">
                            <input type={showPassword ? 'text' : 'password'} value={password}
                                onChange={e => setPassword(e.target.value)}
                                onKeyDown={e => e.key === 'Enter' && handleUnlock()}
                                className="w-full bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm pr-8" />
                            <button onClick={() => setShowPassword(!showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400">
                                {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                            </button>
                        </div>
                        <div className="flex gap-2 justify-end mt-2">
                            <button onClick={() => { resetState(); setMode('home'); }} className="px-3 py-1.5 text-sm hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                                {t('vault.cancel')}
                            </button>
                            <button onClick={handleUnlock} disabled={loading} className="flex items-center gap-2 px-4 py-1.5 bg-blue-600 hover:bg-blue-500 rounded text-sm disabled:opacity-50">
                                {loading && <Loader2 size={14} className="animate-spin" />}
                                {t('vault.unlock')}
                            </button>
                        </div>
                    </div>
                )}

                {/* Browse */}
                {mode === 'browse' && (() => {
                    // Filter entries for the current directory
                    const prefix = currentDir ? `${currentDir}/` : '';
                    const visibleEntries = entries.filter(entry => {
                        if (!prefix) {
                            // Root: show entries without "/" or top-level dirs
                            return !entry.name.includes('/');
                        }
                        // Inside a dir: show entries that start with prefix and have no further "/"
                        if (!entry.name.startsWith(prefix)) return false;
                        const rest = entry.name.slice(prefix.length);
                        return rest.length > 0 && !rest.includes('/');
                    });

                    // Sort: directories first, then files
                    const sortedEntries = [...visibleEntries].sort((a, b) => {
                        if (a.isDir && !b.isDir) return -1;
                        if (!a.isDir && b.isDir) return 1;
                        return a.name.localeCompare(b.name);
                    });

                    // Breadcrumb parts
                    const breadcrumbParts = currentDir ? currentDir.split('/') : [];

                    // Display name: just the last segment of the path
                    const displayName = (fullName: string) => fullName.split('/').pop() || fullName;

                    return (
                    <>
                        {/* Toolbar */}
                        <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                            <button onClick={handleAddFiles} disabled={loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-green-700 hover:bg-green-600 rounded">
                                <Plus size={14} /> {t('vault.addFiles')}
                            </button>
                            {vaultSecurity?.version === 2 && (
                                <button onClick={() => { setShowNewDirDialog(true); setNewDirName(''); }} disabled={loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-yellow-700 hover:bg-yellow-600 rounded">
                                    <FolderPlus size={14} /> {t('vault.newFolder')}
                                </button>
                            )}
                            <button onClick={() => setChangingPassword(!changingPassword)} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded">
                                <Key size={14} /> {t('vault.changePassword')}
                            </button>
                            {/* Remote vault: Save & Close */}
                            {remoteLocalPath && (
                                <button
                                    onClick={handleSaveRemoteAndClose}
                                    disabled={loading}
                                    className="flex items-center gap-1 px-2 py-1 text-xs bg-purple-600 hover:bg-purple-500 rounded text-white"
                                >
                                    <Download size={14} /> {t('vault.remote.saveAndClose')}
                                </button>
                            )}
                            {currentLevelConfig && (
                                <div className={`ml-auto flex items-center gap-1.5 px-2 py-1 rounded text-xs ${currentLevelConfig.color} bg-gray-100/50 dark:bg-gray-900/50`}>
                                    <LevelIcon size={12} />
                                    <span>v{vaultSecurity?.version}</span>
                                    {vaultSecurity?.cascadeMode && (
                                        <span className="flex items-center gap-0.5">
                                            <Zap size={10} /> {t('vault.cascade')}
                                        </span>
                                    )}
                                </div>
                            )}
                        </div>

                        {/* New folder dialog */}
                        {showNewDirDialog && (
                            <div className="px-4 py-2 border-b border-gray-200 dark:border-gray-700 flex gap-2 items-center">
                                <FolderPlus size={14} className="text-yellow-400 shrink-0" />
                                <input
                                    autoFocus
                                    value={newDirName}
                                    onChange={e => setNewDirName(e.target.value)}
                                    onKeyDown={e => { if (e.key === 'Enter') handleCreateDirectory(); if (e.key === 'Escape') setShowNewDirDialog(false); }}
                                    placeholder={t('vault.folderName')}
                                    className="flex-1 bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs"
                                />
                                <button onClick={handleCreateDirectory} disabled={loading || !newDirName.trim()} className="px-2 py-1 bg-yellow-700 hover:bg-yellow-600 rounded text-xs disabled:opacity-50">
                                    {t('vault.create')}
                                </button>
                                <button onClick={() => setShowNewDirDialog(false)} className="px-2 py-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded text-xs">
                                    {t('vault.cancel')}
                                </button>
                            </div>
                        )}

                        {/* Breadcrumb navigation */}
                        {currentDir && (
                            <div className="flex items-center gap-1 px-4 py-1.5 border-b border-gray-200 dark:border-gray-700 text-xs">
                                <button onClick={() => setCurrentDir('')} className="hover:text-blue-400 text-gray-500 dark:text-gray-400 flex items-center gap-0.5">
                                    <ArrowLeft size={12} />
                                    <VaultIcon size={12} className="text-emerald-400" />
                                </button>
                                <ChevronRight size={10} className="text-gray-500" />
                                {breadcrumbParts.map((part, idx) => {
                                    const path = breadcrumbParts.slice(0, idx + 1).join('/');
                                    const isLast = idx === breadcrumbParts.length - 1;
                                    return (
                                        <React.Fragment key={path}>
                                            {isLast ? (
                                                <span className="text-gray-800 dark:text-gray-200 font-medium">{part}</span>
                                            ) : (
                                                <>
                                                    <button onClick={() => setCurrentDir(path)} className="hover:text-blue-400 text-gray-500 dark:text-gray-400">
                                                        {part}
                                                    </button>
                                                    <ChevronRight size={10} className="text-gray-500" />
                                                </>
                                            )}
                                        </React.Fragment>
                                    );
                                })}
                            </div>
                        )}

                        {/* Change password form */}
                        {changingPassword && (
                            <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-700 flex gap-2 items-end">
                                <div className="flex-1">
                                    <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">{t('vault.newPassword')}</label>
                                    <div className="relative">
                                        <input type={showPassword ? 'text' : 'password'} value={newPassword} onChange={e => setNewPassword(e.target.value)}
                                            className="w-full bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs pr-7" />
                                        <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300">
                                            {showPassword ? <EyeOff size={12} /> : <Eye size={12} />}
                                        </button>
                                    </div>
                                </div>
                                <div className="flex-1">
                                    <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">{t('vault.confirmNew')}</label>
                                    <div className="relative">
                                        <input type={showPassword ? 'text' : 'password'} value={confirmNewPassword} onChange={e => setConfirmNewPassword(e.target.value)}
                                            className="w-full bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs pr-7" />
                                        <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300">
                                            {showPassword ? <EyeOff size={12} /> : <Eye size={12} />}
                                        </button>
                                    </div>
                                </div>
                                <button onClick={handleChangePassword} disabled={loading} className="px-3 py-1 bg-blue-600 hover:bg-blue-500 rounded text-xs">
                                    {t('vault.apply')}
                                </button>
                            </div>
                        )}

                        {/* File list */}
                        <div className="flex-1 overflow-auto">
                            {sortedEntries.length === 0 ? (
                                <div className="flex flex-col items-center justify-center py-12 text-gray-500 dark:text-gray-400">
                                    <VaultIcon size={32} className="mb-2 opacity-50" />
                                    <p className="text-sm">{currentDir ? t('vault.dirEmpty') : t('vault.empty')}</p>
                                </div>
                            ) : (
                                <table className="w-full">
                                    <thead className="text-xs text-gray-500 dark:text-gray-400 border-b border-gray-200 dark:border-gray-700 sticky top-0 bg-white dark:bg-gray-800">
                                        <tr>
                                            <th className="py-2 px-3 text-left">{t('vault.fileName')}</th>
                                            <th className="py-2 px-3 text-right w-24">{t('vault.fileSize')}</th>
                                            <th className="py-2 px-3 text-right w-28">{t('vault.fileActions')}</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {sortedEntries.map(entry => (
                                            <tr key={entry.name} className="hover:bg-gray-100/50 dark:hover:bg-gray-700/30 text-sm">
                                                <td className="py-1.5 px-3">
                                                    <div
                                                        className={`flex items-center gap-2 ${entry.isDir ? 'cursor-pointer' : ''}`}
                                                        onDoubleClick={() => { if (entry.isDir) setCurrentDir(entry.name); }}
                                                    >
                                                        {entry.isDir ? <Folder size={14} className="text-yellow-400 shrink-0" /> : <File size={14} className="text-gray-500 dark:text-gray-400 shrink-0" />}
                                                        <span className="truncate">{displayName(entry.name)}</span>
                                                    </div>
                                                </td>
                                                <td className="py-1.5 px-3 text-right text-gray-500 dark:text-gray-400">{entry.isDir ? '' : formatSize(entry.size)}</td>
                                                <td className="py-1.5 px-3 text-right">
                                                    <div className="flex gap-1 justify-end">
                                                        {!entry.isDir && (
                                                            <button onClick={() => handleExtract(entry.name)} className="p-1 hover:bg-gray-200 dark:hover:bg-gray-600 rounded" title={t('vault.extract')}>
                                                                <Download size={14} />
                                                            </button>
                                                        )}
                                                        <button onClick={() => handleRemove(entry.name, entry.isDir)} className="p-1 hover:bg-gray-200 dark:hover:bg-gray-600 rounded text-red-400" title={t('vault.remove')}>
                                                            <Trash2 size={14} />
                                                        </button>
                                                    </div>
                                                </td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            )}
                        </div>

                        {/* Footer */}
                        <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-700 text-xs text-gray-500 dark:text-gray-400 flex justify-between">
                            <span>{sortedEntries.length} {t('vault.items')}{currentDir ? ` in /${currentDir}` : ''}</span>
                            {meta && <span>v{meta.version} | {entries.length} {t('vault.totalItems')}</span>}
                        </div>
                    </>
                    );
                })()}

                {/* Loading overlay */}
                {loading && mode === 'browse' && (
                    <div className="absolute inset-0 bg-black/30 flex items-center justify-center rounded-lg">
                        <Loader2 size={24} className="animate-spin text-blue-400" />
                    </div>
                )}
            </div>
        </div>
    );
};
