import * as React from 'react';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Shield, Lock, Unlock, Folder, File, Download, Upload, ArrowLeft, X, Eye, EyeOff, Loader2, Key } from 'lucide-react';
import { useTranslation } from '../i18n';

interface CryptomatorBrowserProps {
    onClose: () => void;
}

interface CryptomatorEntry {
    name: string;
    isDir: boolean;
    size: number;
    dirId: string | null;
}

interface VaultInfo {
    vaultId: string;
    name: string;
    format: number;
}

interface BreadcrumbItem {
    name: string;
    dirId: string;
}

function formatSize(bytes: number): string {
    if (bytes === 0) return '—';
    const units = ['B', 'KB', 'MB', 'GB'];
    let i = 0;
    let size = bytes;
    while (size >= 1024 && i < units.length - 1) { size /= 1024; i++; }
    return `${size.toFixed(i > 0 ? 1 : 0)} ${units[i]}`;
}

export const CryptomatorBrowser: React.FC<CryptomatorBrowserProps> = ({ onClose }) => {
    const t = useTranslation();
    const [vaultInfo, setVaultInfo] = useState<VaultInfo | null>(null);
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [vaultPath, setVaultPath] = useState('');
    const [entries, setEntries] = useState<CryptomatorEntry[]>([]);
    const [breadcrumb, setBreadcrumb] = useState<BreadcrumbItem[]>([{ name: 'Root', dirId: '' }]);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);

    const currentDirId = breadcrumb[breadcrumb.length - 1].dirId;

    const handleSelectVault = async () => {
        const selected = await open({ directory: true });
        if (selected) {
            setVaultPath(selected as string);
        }
    };

    const handleUnlock = async () => {
        if (!vaultPath || !password) return;
        setLoading(true);
        setError(null);
        try {
            const info = await invoke<VaultInfo>('cryptomator_unlock', { vaultPath, password });
            setVaultInfo(info);
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: info.vaultId, dirId: '' });
            setEntries(list);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleLock = async () => {
        if (!vaultInfo) return;
        try {
            await invoke('cryptomator_lock', { vaultId: vaultInfo.vaultId });
        } catch (_) { /* ignore */ }
        setVaultInfo(null);
        setEntries([]);
        setBreadcrumb([{ name: 'Root', dirId: '' }]);
        setPassword('');
    };

    const navigateToDir = async (name: string, dirId: string) => {
        if (!vaultInfo) return;
        setLoading(true);
        setError(null);
        try {
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: vaultInfo.vaultId, dirId });
            setEntries(list);
            setBreadcrumb(prev => [...prev, { name, dirId }]);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const navigateToBreadcrumb = async (index: number) => {
        if (!vaultInfo) return;
        const target = breadcrumb[index];
        setLoading(true);
        setError(null);
        try {
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: vaultInfo.vaultId, dirId: target.dirId });
            setEntries(list);
            setBreadcrumb(prev => prev.slice(0, index + 1));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleDecrypt = async (entry: CryptomatorEntry) => {
        if (!vaultInfo) return;
        const savePath = await save({ defaultPath: entry.name });
        if (!savePath) return;

        setLoading(true);
        setError(null);
        try {
            await invoke('cryptomator_decrypt_file', {
                vaultId: vaultInfo.vaultId,
                dirId: currentDirId,
                filename: entry.name,
                outputPath: savePath,
            });
            setSuccess(`Decrypted ${entry.name}`);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleEncrypt = async () => {
        if (!vaultInfo) return;
        const selected = await open({ multiple: false });
        if (!selected) return;

        setLoading(true);
        setError(null);
        try {
            await invoke('cryptomator_encrypt_file', {
                vaultId: vaultInfo.vaultId,
                dirId: currentDirId,
                inputPath: selected as string,
            });
            // Refresh listing
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: vaultInfo.vaultId, dirId: currentDirId });
            setEntries(list);
            setSuccess('File encrypted into vault');
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
            <div className="bg-gray-800 rounded-lg shadow-2xl border border-gray-700 w-[650px] max-h-[80vh] flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
                    <div className="flex items-center gap-2">
                        <Shield size={18} className="text-emerald-400" />
                        <span className="font-medium">
                            {vaultInfo ? vaultInfo.name : (t('cryptomator.title') || 'Cryptomator Vault')}
                        </span>
                        {vaultInfo && <span className="text-xs text-gray-400">Format {vaultInfo.format}</span>}
                    </div>
                    <div className="flex items-center gap-1">
                        {vaultInfo && (
                            <button onClick={handleLock} className="flex items-center gap-1 px-2 py-1 text-xs bg-red-700 hover:bg-red-600 rounded">
                                <Lock size={12} /> {t('cryptomator.lock') || 'Lock'}
                            </button>
                        )}
                        <button onClick={onClose} className="p-1 hover:bg-gray-700 rounded"><X size={18} /></button>
                    </div>
                </div>

                {/* Error / Success */}
                {error && <div className="px-4 py-2 bg-red-900/30 text-red-400 text-sm">{error}</div>}
                {success && <div className="px-4 py-2 bg-green-900/30 text-green-400 text-sm">{success}</div>}

                {/* Unlock form */}
                {!vaultInfo && (
                    <div className="p-6 flex flex-col items-center gap-5">
                        {/* Security badge */}
                        <div className="relative">
                            <Shield size={56} className="text-emerald-400" />
                            <div className="absolute -bottom-1 -right-1 bg-emerald-500 rounded-full p-1">
                                <Lock size={12} className="text-white" />
                            </div>
                        </div>

                        <div className="text-center">
                            <p className="text-gray-300 text-sm max-w-md">
                                {t('cryptomator.description') || 'Open a Cryptomator vault (format 8) to browse and decrypt files.'}
                            </p>
                            <p className="text-gray-500 text-xs mt-1">
                                {t('cryptomator.readOnly') || 'Read-only mode — vault creation coming in v2.1'}
                            </p>
                        </div>

                        {/* Security features */}
                        <div className="grid grid-cols-2 gap-2 text-xs text-gray-400 max-w-sm">
                            <div className="flex items-center gap-2 bg-gray-800/50 rounded px-2 py-1.5">
                                <Lock size={12} className="text-emerald-400" />
                                <span>AES-GCM content</span>
                            </div>
                            <div className="flex items-center gap-2 bg-gray-800/50 rounded px-2 py-1.5">
                                <Shield size={12} className="text-emerald-400" />
                                <span>scrypt KDF</span>
                            </div>
                            <div className="flex items-center gap-2 bg-gray-800/50 rounded px-2 py-1.5">
                                <File size={12} className="text-emerald-400" />
                                <span>AES-SIV names</span>
                            </div>
                            <div className="flex items-center gap-2 bg-gray-800/50 rounded px-2 py-1.5">
                                <Key size={12} className="text-emerald-400" />
                                <span>AES Key Wrap</span>
                            </div>
                        </div>

                        <p className="text-emerald-400/70 text-xs flex items-center gap-1">
                            <Shield size={10} /> Compatible with Cryptomator app
                        </p>

                        <button onClick={handleSelectVault} className="px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm">
                            {vaultPath || (t('cryptomator.selectFolder') || 'Select Vault Folder...')}
                        </button>

                        {vaultPath && (
                            <>
                                <div className="w-full max-w-sm">
                                    <label className="text-xs text-gray-400 block mb-1">{t('cryptomator.password') || 'Vault Password'}</label>
                                    <div className="relative">
                                        <input
                                            type={showPassword ? 'text' : 'password'}
                                            value={password}
                                            onChange={e => setPassword(e.target.value)}
                                            onKeyDown={e => e.key === 'Enter' && handleUnlock()}
                                            className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm pr-8"
                                        />
                                        <button onClick={() => setShowPassword(!showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400">
                                            {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                                        </button>
                                    </div>
                                </div>
                                <button onClick={handleUnlock} disabled={loading} className="flex items-center gap-2 px-4 py-2 bg-emerald-600 hover:bg-emerald-500 rounded text-sm disabled:opacity-50">
                                    {loading ? <Loader2 size={14} className="animate-spin" /> : <Unlock size={14} />}
                                    {t('cryptomator.unlock') || 'Unlock Vault'}
                                </button>
                            </>
                        )}
                    </div>
                )}

                {/* Browsing view */}
                {vaultInfo && (
                    <>
                        {/* Breadcrumb + toolbar */}
                        <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-700">
                            {breadcrumb.length > 1 && (
                                <button onClick={() => navigateToBreadcrumb(breadcrumb.length - 2)} className="p-1 hover:bg-gray-700 rounded">
                                    <ArrowLeft size={14} />
                                </button>
                            )}
                            <div className="flex items-center gap-1 text-xs text-gray-400 flex-1 overflow-hidden">
                                {breadcrumb.map((item, i) => (
                                    <React.Fragment key={i}>
                                        {i > 0 && <span>/</span>}
                                        <button
                                            onClick={() => navigateToBreadcrumb(i)}
                                            className="hover:text-white truncate max-w-[120px]"
                                        >
                                            {item.name}
                                        </button>
                                    </React.Fragment>
                                ))}
                            </div>
                            <button onClick={handleEncrypt} disabled={loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-emerald-700 hover:bg-emerald-600 rounded">
                                <Upload size={12} /> {t('cryptomator.encrypt') || 'Encrypt File'}
                            </button>
                        </div>

                        {/* File list */}
                        <div className="flex-1 overflow-auto">
                            {loading && (
                                <div className="flex items-center justify-center py-12">
                                    <Loader2 size={24} className="animate-spin text-emerald-400" />
                                </div>
                            )}
                            {!loading && entries.length === 0 && (
                                <div className="flex items-center justify-center py-12 text-gray-400 text-sm">
                                    {t('cryptomator.empty') || 'Directory is empty'}
                                </div>
                            )}
                            {!loading && entries.length > 0 && (
                                <table className="w-full">
                                    <thead className="text-xs text-gray-400 border-b border-gray-700 sticky top-0 bg-gray-800">
                                        <tr>
                                            <th className="py-2 px-3 text-left">{t('cryptomator.name') || 'Name'}</th>
                                            <th className="py-2 px-3 text-right w-24">{t('cryptomator.size') || 'Size'}</th>
                                            <th className="py-2 px-3 text-right w-20"></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {entries.map(entry => (
                                            <tr
                                                key={entry.name}
                                                className="hover:bg-gray-700/30 text-sm cursor-pointer"
                                                onDoubleClick={() => entry.isDir && entry.dirId && navigateToDir(entry.name, entry.dirId)}
                                            >
                                                <td className="py-1.5 px-3 flex items-center gap-2">
                                                    {entry.isDir
                                                        ? <Folder size={14} className="text-yellow-400 shrink-0" />
                                                        : <File size={14} className="text-gray-400 shrink-0" />}
                                                    <span className="truncate">{entry.name}</span>
                                                </td>
                                                <td className="py-1.5 px-3 text-right text-gray-400">{entry.isDir ? '' : formatSize(entry.size)}</td>
                                                <td className="py-1.5 px-3 text-right">
                                                    {!entry.isDir && (
                                                        <button onClick={() => handleDecrypt(entry)} className="p-1 hover:bg-gray-600 rounded" title="Decrypt & Save">
                                                            <Download size={14} />
                                                        </button>
                                                    )}
                                                </td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            )}
                        </div>

                        {/* Footer */}
                        <div className="px-4 py-2 border-t border-gray-700 text-xs text-gray-400">
                            {entries.length} {t('cryptomator.items') || 'items'}
                        </div>
                    </>
                )}
            </div>
        </div>
    );
};
