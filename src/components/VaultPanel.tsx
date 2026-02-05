import * as React from 'react';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Shield, Plus, Trash2, Download, Key, FolderPlus, X, Eye, EyeOff, Loader2, Lock, File, Folder, Zap, ShieldCheck, ShieldAlert, ChevronDown } from 'lucide-react';
import { ArchiveEntry, AeroVaultMeta } from '../types';
import { useTranslation } from '../i18n';
import { formatDate, formatSize } from '../utils/formatters';

interface VaultPanelProps {
    onClose: () => void;
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

// Security level configuration
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

export const VaultPanel: React.FC<VaultPanelProps> = ({ onClose }) => {
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
        if (password.length < 8) { setError(t('vault.passwordTooShort') || 'Password must be at least 8 characters'); return; }
        if (password !== confirmPassword) { setError(t('vault.passwordMismatch') || 'Passwords do not match'); return; }

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
                setSuccess(t('vault.created') || 'Vault created successfully');
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
                setSuccess(t('vault.created') || 'Vault created successfully');
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

    const handleAddFiles = async () => {
        const selected = await open({ multiple: true });
        if (!selected || (Array.isArray(selected) && selected.length === 0)) return;
        const paths = Array.isArray(selected) ? selected as string[] : [selected as string];

        setLoading(true);
        setError(null);
        try {
            if (vaultSecurity?.version === 2) {
                // Add files to v2 vault
                const result = await invoke<{ added: number; total: number }>('vault_v2_add_files', {
                    vaultPath,
                    password,
                    filePaths: paths
                });
                // Re-open to refresh file list
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
                setSuccess(`${result.added} file(s) added`);
            } else {
                // Add files to v1 vault
                await invoke('vault_add_files', { vaultPath, password, filePaths: paths });
                const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
                setEntries(list);
                setSuccess(`${paths.length} file(s) added`);
            }
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleRemove = async (entryName: string) => {
        setLoading(true);
        setError(null);
        try {
            if (vaultSecurity?.version === 2) {
                // Delete from v2 vault
                const result = await invoke<{ deleted: string; remaining: number }>('vault_v2_delete_entry', {
                    vaultPath,
                    password,
                    entryName
                });
                // Re-open to refresh file list
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
                setSuccess(`Deleted ${result.deleted}`);
            } else {
                // Delete from v1 vault
                await invoke('vault_remove_file', { vaultPath, password, entryName });
                const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
                setEntries(list);
                setSuccess(`Deleted ${entryName}`);
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
            setSuccess(`Extracted ${entryName}`);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleChangePassword = async () => {
        if (newPassword.length < 8) { setError(t('vault.passwordTooShort') || 'Password must be at least 8 characters'); return; }
        if (newPassword !== confirmNewPassword) { setError(t('vault.passwordMismatch') || 'Passwords do not match'); return; }

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
            setSuccess(t('vault.passwordChanged') || 'Password changed successfully');
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
            <div className="bg-gray-800 rounded-lg shadow-2xl border border-gray-700 w-[680px] max-h-[85vh] flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
                    <div className="flex items-center gap-2">
                        <Shield size={18} className="text-emerald-400" />
                        <span className="font-medium">
                            {mode === 'browse' ? vaultName : (t('vault.title') || 'AeroVault')}
                        </span>
                        {/* Security badge in browse mode */}
                        {mode === 'browse' && currentLevelConfig && (
                            <span className={`ml-2 px-2 py-0.5 rounded text-xs font-medium ${currentLevelConfig.bgColor} bg-opacity-20 ${currentLevelConfig.color}`}>
                                <LevelIcon size={10} className="inline mr-1" />
                                {currentLevelConfig.label}
                            </span>
                        )}
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-700 rounded"><X size={18} /></button>
                </div>

                {/* Error / Success */}
                {error && <div className="px-4 py-2 bg-red-900/30 text-red-400 text-sm">{error}</div>}
                {success && <div className="px-4 py-2 bg-green-900/30 text-green-400 text-sm">{success}</div>}

                {/* Home */}
                {mode === 'home' && (
                    <div className="p-6 flex flex-col items-center gap-5">
                        {/* Security badge */}
                        <div className="relative">
                            <Shield size={56} className="text-emerald-400" />
                            <div className="absolute -bottom-1 -right-1 bg-emerald-500 rounded-full p-1">
                                <Lock size={12} className="text-white" />
                            </div>
                        </div>

                        <p className="text-gray-300 text-center text-sm max-w-md">
                            {t('vault.descriptionV2') || 'AeroVault v2 provides military-grade encryption with nonce misuse-resistant AES-256-GCM-SIV and optional cascade encryption.'}
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
                                <FolderPlus size={16} /> {t('vault.createNew') || 'Create Vault'}
                            </button>
                            <button onClick={handleOpen} className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-500 rounded text-sm font-medium">
                                <Lock size={16} /> {t('vault.openExisting') || 'Open Vault'}
                            </button>
                        </div>
                    </div>
                )}

                {/* Create */}
                {mode === 'create' && (
                    <div className="p-4 flex flex-col gap-3">
                        {/* Security Level Selector */}
                        <label className="text-sm text-gray-400">{t('vault.securityLevel') || 'Security Level'}</label>
                        <div className="relative">
                            <button
                                onClick={() => setShowLevelDropdown(!showLevelDropdown)}
                                className={`w-full flex items-center justify-between px-3 py-2.5 rounded border ${securityLevels[securityLevel].borderColor} bg-gray-900 text-left`}
                            >
                                <div className="flex items-center gap-2">
                                    {React.createElement(securityLevels[securityLevel].icon, {
                                        size: 16,
                                        className: securityLevels[securityLevel].color
                                    })}
                                    <div>
                                        <div className={`text-sm font-medium ${securityLevels[securityLevel].color}`}>
                                            {securityLevels[securityLevel].label}
                                            {securityLevel === 'advanced' && <span className="ml-2 text-xs text-emerald-300">(Recommended)</span>}
                                        </div>
                                        <div className="text-xs text-gray-500">{securityLevels[securityLevel].description}</div>
                                    </div>
                                </div>
                                <ChevronDown size={16} className="text-gray-400" />
                            </button>

                            {/* Dropdown */}
                            {showLevelDropdown && (
                                <div className="absolute z-10 mt-1 w-full bg-gray-900 border border-gray-600 rounded-lg shadow-xl overflow-hidden">
                                    {(Object.keys(securityLevels) as SecurityLevel[]).map((level) => {
                                        const config = securityLevels[level];
                                        const Icon = config.icon;
                                        const isSelected = level === securityLevel;
                                        return (
                                            <button
                                                key={level}
                                                onClick={() => { setSecurityLevel(level); setShowLevelDropdown(false); }}
                                                className={`w-full flex items-start gap-3 px-3 py-3 text-left hover:bg-gray-800 ${isSelected ? 'bg-gray-800' : ''}`}
                                            >
                                                <Icon size={18} className={`mt-0.5 ${config.color}`} />
                                                <div className="flex-1">
                                                    <div className={`text-sm font-medium ${config.color}`}>
                                                        {config.label}
                                                        {level === 'advanced' && <span className="ml-2 text-xs text-emerald-300">(Recommended)</span>}
                                                    </div>
                                                    <div className="text-xs text-gray-500 mt-0.5">{config.description}</div>
                                                    <div className="flex flex-wrap gap-1 mt-1.5">
                                                        {config.features.map((feature, i) => (
                                                            <span key={i} className="px-1.5 py-0.5 bg-gray-700 rounded text-[10px] text-gray-300">
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

                        <label className="text-sm text-gray-400 mt-2">{t('vault.description_label') || 'Description (optional)'}</label>
                        <input value={description} onChange={e => setDescription(e.target.value)}
                            className="bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm" placeholder="My secure vault" />

                        <label className="text-sm text-gray-400">{t('vault.password') || 'Password (min 8 chars)'}</label>
                        <div className="relative">
                            <input type={showPassword ? 'text' : 'password'} value={password} onChange={e => setPassword(e.target.value)}
                                className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm pr-8" />
                            <button onClick={() => setShowPassword(!showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400">
                                {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                            </button>
                        </div>

                        <label className="text-sm text-gray-400">{t('vault.confirmPassword') || 'Confirm Password'}</label>
                        <input type={showPassword ? 'text' : 'password'} value={confirmPassword} onChange={e => setConfirmPassword(e.target.value)}
                            className="bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm" />

                        <div className="flex gap-2 justify-end mt-2">
                            <button onClick={() => setMode('home')} className="px-3 py-1.5 text-sm hover:bg-gray-700 rounded">
                                {t('vault.cancel') || 'Cancel'}
                            </button>
                            <button onClick={handleCreate} disabled={loading} className={`flex items-center gap-2 px-4 py-1.5 ${securityLevels[securityLevel].bgColor} hover:opacity-90 rounded text-sm disabled:opacity-50`}>
                                {loading && <Loader2 size={14} className="animate-spin" />}
                                {t('vault.create') || 'Create'}
                            </button>
                        </div>
                    </div>
                )}

                {/* Open (password prompt) */}
                {mode === 'open' && (
                    <div className="p-4 flex flex-col gap-3">
                        <p className="text-sm text-gray-400 truncate">{vaultPath}</p>

                        {/* Show detected version and security level */}
                        {vaultSecurity && (() => {
                            const levelConfig = securityLevels[vaultSecurity.level];
                            const LevelIcon = levelConfig.icon;
                            return (
                                <div className={`flex items-center gap-2 px-3 py-2 rounded border ${levelConfig.borderColor} bg-gray-900/30`}>
                                    <LevelIcon size={16} className={levelConfig.color} />
                                    <span className={`text-sm ${levelConfig.color}`}>
                                        AeroVault v{vaultSecurity.version} ({levelConfig.label})
                                    </span>
                                </div>
                            );
                        })()}

                        <label className="text-sm text-gray-400">{t('vault.password') || 'Password'}</label>
                        <div className="relative">
                            <input type={showPassword ? 'text' : 'password'} value={password}
                                onChange={e => setPassword(e.target.value)}
                                onKeyDown={e => e.key === 'Enter' && handleUnlock()}
                                className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm pr-8" />
                            <button onClick={() => setShowPassword(!showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400">
                                {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                            </button>
                        </div>
                        <div className="flex gap-2 justify-end mt-2">
                            <button onClick={() => { resetState(); setMode('home'); }} className="px-3 py-1.5 text-sm hover:bg-gray-700 rounded">
                                {t('vault.cancel') || 'Cancel'}
                            </button>
                            <button onClick={handleUnlock} disabled={loading} className="flex items-center gap-2 px-4 py-1.5 bg-blue-600 hover:bg-blue-500 rounded text-sm disabled:opacity-50">
                                {loading && <Loader2 size={14} className="animate-spin" />}
                                {t('vault.unlock') || 'Unlock'}
                            </button>
                        </div>
                    </div>
                )}

                {/* Browse */}
                {mode === 'browse' && (
                    <>
                        {/* Toolbar */}
                        <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-700">
                            <button onClick={handleAddFiles} disabled={loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-green-700 hover:bg-green-600 rounded">
                                <Plus size={14} /> {t('vault.addFiles') || 'Add Files'}
                            </button>
                            <button onClick={() => setChangingPassword(!changingPassword)} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-700 hover:bg-gray-600 rounded">
                                <Key size={14} /> {t('vault.changePassword') || 'Change Password'}
                            </button>
                            {/* Security info badge */}
                            {currentLevelConfig && (
                                <div className={`ml-auto flex items-center gap-1.5 px-2 py-1 rounded text-xs ${currentLevelConfig.color} bg-gray-900/50`}>
                                    <LevelIcon size={12} />
                                    <span>v{vaultSecurity?.version}</span>
                                    {vaultSecurity?.cascadeMode && (
                                        <span className="flex items-center gap-0.5">
                                            <Zap size={10} /> Cascade
                                        </span>
                                    )}
                                </div>
                            )}
                        </div>

                        {/* Change password form */}
                        {changingPassword && (
                            <div className="px-4 py-3 border-b border-gray-700 flex gap-2 items-end">
                                <div className="flex-1">
                                    <label className="text-xs text-gray-400 block mb-1">{t('vault.newPassword') || 'New Password'}</label>
                                    <div className="relative">
                                        <input type={showPassword ? 'text' : 'password'} value={newPassword} onChange={e => setNewPassword(e.target.value)}
                                            className="w-full bg-gray-900 border border-gray-600 rounded px-2 py-1 text-xs pr-7" />
                                        <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-300">
                                            {showPassword ? <EyeOff size={12} /> : <Eye size={12} />}
                                        </button>
                                    </div>
                                </div>
                                <div className="flex-1">
                                    <label className="text-xs text-gray-400 block mb-1">{t('vault.confirmNew') || 'Confirm'}</label>
                                    <div className="relative">
                                        <input type={showPassword ? 'text' : 'password'} value={confirmNewPassword} onChange={e => setConfirmNewPassword(e.target.value)}
                                            className="w-full bg-gray-900 border border-gray-600 rounded px-2 py-1 text-xs pr-7" />
                                        <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-300">
                                            {showPassword ? <EyeOff size={12} /> : <Eye size={12} />}
                                        </button>
                                    </div>
                                </div>
                                <button onClick={handleChangePassword} disabled={loading} className="px-3 py-1 bg-blue-600 hover:bg-blue-500 rounded text-xs">
                                    {t('vault.apply') || 'Apply'}
                                </button>
                            </div>
                        )}

                        {/* File list */}
                        <div className="flex-1 overflow-auto">
                            {entries.length === 0 ? (
                                <div className="flex flex-col items-center justify-center py-12 text-gray-400">
                                    <Shield size={32} className="mb-2 opacity-50" />
                                    <p className="text-sm">{t('vault.empty') || 'Vault is empty. Add files to get started.'}</p>
                                </div>
                            ) : (
                                <table className="w-full">
                                    <thead className="text-xs text-gray-400 border-b border-gray-700 sticky top-0 bg-gray-800">
                                        <tr>
                                            <th className="py-2 px-3 text-left">{t('vault.fileName') || 'Name'}</th>
                                            <th className="py-2 px-3 text-right w-24">{t('vault.fileSize') || 'Size'}</th>
                                            <th className="py-2 px-3 text-right w-28">{t('vault.fileActions') || 'Actions'}</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {entries.map(entry => (
                                            <tr key={entry.name} className="hover:bg-gray-700/30 text-sm">
                                                <td className="py-1.5 px-3 flex items-center gap-2">
                                                    {entry.isDir ? <Folder size={14} className="text-yellow-400" /> : <File size={14} className="text-gray-400" />}
                                                    <span className="truncate">{entry.name}</span>
                                                </td>
                                                <td className="py-1.5 px-3 text-right text-gray-400">{formatSize(entry.size)}</td>
                                                <td className="py-1.5 px-3 text-right">
                                                    <div className="flex gap-1 justify-end">
                                                        <button onClick={() => handleExtract(entry.name)} className="p-1 hover:bg-gray-600 rounded" title="Extract">
                                                            <Download size={14} />
                                                        </button>
                                                        <button onClick={() => handleRemove(entry.name)} className="p-1 hover:bg-gray-600 rounded text-red-400" title="Remove">
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
                        <div className="px-4 py-2 border-t border-gray-700 text-xs text-gray-400 flex justify-between">
                            <span>{entries.length} {t('vault.files') || 'files'}</span>
                            {meta && <span>v{meta.version} | {t('vault.modified') || 'Modified'}: {meta.modified}</span>}
                        </div>
                    </>
                )}

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
