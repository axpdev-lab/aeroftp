import * as React from 'react';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Shield, Plus, Trash2, Download, Key, FolderPlus, X, Eye, EyeOff, Loader2, Lock, File, Folder } from 'lucide-react';
import { ArchiveEntry, AeroVaultMeta } from '../types';
import { useTranslation } from '../i18n';

interface VaultPanelProps {
    onClose: () => void;
}

type VaultMode = 'home' | 'create' | 'open' | 'browse';

function formatSize(bytes: number): string {
    if (bytes === 0) return '—';
    const units = ['B', 'KB', 'MB', 'GB'];
    let i = 0;
    let size = bytes;
    while (size >= 1024 && i < units.length - 1) { size /= 1024; i++; }
    return `${size.toFixed(i > 0 ? 1 : 0)} ${units[i]}`;
}

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
    };

    const handleCreate = async () => {
        if (password.length < 8) { setError(t('vault.passwordTooShort') || 'Password must be at least 8 characters'); return; }
        if (password !== confirmPassword) { setError(t('vault.passwordMismatch') || 'Passwords do not match'); return; }

        const savePath = await save({ defaultPath: 'vault.aerovault', filters: [{ name: 'AeroVault', extensions: ['aerovault'] }] });
        if (!savePath) return;

        setLoading(true);
        setError(null);
        try {
            await invoke('vault_create', { vaultPath: savePath, password, description: description || null });
            setVaultPath(savePath);
            setSuccess(t('vault.created') || 'Vault created successfully');
            setMode('browse');
            setEntries([]);
            const m = await invoke<AeroVaultMeta>('vault_get_meta', { vaultPath: savePath, password });
            setMeta(m);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleOpen = async () => {
        const selected = await open({ filters: [{ name: 'AeroVault', extensions: ['aerovault'] }] });
        if (!selected) return;
        setVaultPath(selected as string);
        setMode('open');
    };

    const handleUnlock = async () => {
        setLoading(true);
        setError(null);
        try {
            const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
            setEntries(list);
            const m = await invoke<AeroVaultMeta>('vault_get_meta', { vaultPath, password });
            setMeta(m);
            setMode('browse');
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
            await invoke('vault_add_files', { vaultPath, password, filePaths: paths });
            const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
            setEntries(list);
            setSuccess(`${paths.length} file(s) added`);
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
            await invoke('vault_remove_file', { vaultPath, password, entryName });
            const list = await invoke<ArchiveEntry[]>('vault_list', { vaultPath, password });
            setEntries(list);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleExtract = async (entryName: string) => {
        const savePath = await save({ defaultPath: entryName.split('/').pop() || entryName });
        if (!savePath) return;

        setLoading(true);
        try {
            await invoke('vault_extract_entry', { vaultPath, password, entryName, outputPath: savePath });
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
            await invoke('vault_change_password', { vaultPath, oldPassword: password, newPassword });
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

    const vaultName = vaultPath.split('/').pop() || vaultPath.split('\\').pop() || 'Vault';

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
            <div className="bg-gray-800 rounded-lg shadow-2xl border border-gray-700 w-[650px] max-h-[80vh] flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
                    <div className="flex items-center gap-2">
                        <Shield size={18} className="text-green-400" />
                        <span className="font-medium">
                            {mode === 'browse' ? vaultName : (t('vault.title') || 'AeroVault')}
                        </span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-700 rounded"><X size={18} /></button>
                </div>

                {/* Error / Success */}
                {error && <div className="px-4 py-2 bg-red-900/30 text-red-400 text-sm">{error}</div>}
                {success && <div className="px-4 py-2 bg-green-900/30 text-green-400 text-sm">{success}</div>}

                {/* Home */}
                {mode === 'home' && (
                    <div className="p-6 flex flex-col items-center gap-4">
                        <Shield size={48} className="text-green-400" />
                        <p className="text-gray-400 text-center text-sm max-w-md">
                            {t('vault.description') || 'AeroVault lets you create AES-256 encrypted containers to securely store your files.'}
                        </p>
                        <div className="flex gap-3">
                            <button onClick={() => { resetState(); setMode('create'); }} className="flex items-center gap-2 px-4 py-2 bg-green-600 hover:bg-green-500 rounded text-sm">
                                <FolderPlus size={16} /> {t('vault.createNew') || 'Create Vault'}
                            </button>
                            <button onClick={handleOpen} className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-500 rounded text-sm">
                                <Lock size={16} /> {t('vault.openExisting') || 'Open Vault'}
                            </button>
                        </div>
                    </div>
                )}

                {/* Create */}
                {mode === 'create' && (
                    <div className="p-4 flex flex-col gap-3">
                        <label className="text-sm text-gray-400">{t('vault.description_label') || 'Description (optional)'}</label>
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
                            <button onClick={handleCreate} disabled={loading} className="flex items-center gap-2 px-4 py-1.5 bg-green-600 hover:bg-green-500 rounded text-sm disabled:opacity-50">
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
                        </div>

                        {/* Change password form */}
                        {changingPassword && (
                            <div className="px-4 py-3 border-b border-gray-700 flex gap-2 items-end">
                                <div className="flex-1">
                                    <label className="text-xs text-gray-400 block mb-1">{t('vault.newPassword') || 'New Password'}</label>
                                    <input type="password" value={newPassword} onChange={e => setNewPassword(e.target.value)}
                                        className="w-full bg-gray-900 border border-gray-600 rounded px-2 py-1 text-xs" />
                                </div>
                                <div className="flex-1">
                                    <label className="text-xs text-gray-400 block mb-1">{t('vault.confirmNew') || 'Confirm'}</label>
                                    <input type="password" value={confirmNewPassword} onChange={e => setConfirmNewPassword(e.target.value)}
                                        className="w-full bg-gray-900 border border-gray-600 rounded px-2 py-1 text-xs" />
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
                            {meta && <span>v{meta.version} | {meta.modified}</span>}
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
