import * as React from 'react';
import { useState, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Upload, Download, Shield, AlertCircle, CheckCircle2, X, Eye, EyeOff, Lock, Server } from 'lucide-react';
import { ServerProfile } from '../types';
import { useTranslation } from '../i18n';

interface ExportImportDialogProps {
    servers: ServerProfile[];
    onImport: (servers: ServerProfile[]) => void;
    onClose: () => void;
}

interface ImportedServer {
    id: string;
    name: string;
    host: string;
    port: number;
    username: string;
    protocol?: string;
    initialPath?: string;
    localInitialPath?: string;
    color?: string;
    lastConnected?: string;
    options?: Record<string, unknown>;
    providerId?: string;
    credential?: string;
    hasStoredCredential?: boolean;
}

interface ImportResult {
    servers: ImportedServer[];
    metadata: {
        exportDate: string;
        aeroftpVersion: string;
        serverCount: number;
        hasCredentials: boolean;
    };
}

export const ExportImportDialog: React.FC<ExportImportDialogProps> = ({ servers, onImport, onClose }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'export' | 'import' | null>(null);
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [includeCredentials, setIncludeCredentials] = useState(true);
    const [showPassword, setShowPassword] = useState(false);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [selectedServerIds, setSelectedServerIds] = useState<Set<string>>(() => new Set(servers.map(s => s.id)));

    const allSelected = selectedServerIds.size === servers.length;
    const noneSelected = selectedServerIds.size === 0;

    const selectedServers = useMemo(
        () => servers.filter(s => selectedServerIds.has(s.id)),
        [servers, selectedServerIds]
    );

    const toggleServer = (id: string) => {
        setSelectedServerIds(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    const toggleAll = () => {
        if (allSelected) {
            setSelectedServerIds(new Set());
        } else {
            setSelectedServerIds(new Set(servers.map(s => s.id)));
        }
    };

    const handleExport = async () => {
        if (password !== confirmPassword) {
            setError(t('settings.passwordMismatch'));
            return;
        }
        if (password.length < 8) {
            setError(t('settings.passwordTooShort'));
            return;
        }
        if (noneSelected) return;

        // Open save dialog first
        const filePath = await save({
            title: t('settings.exportServers'),
            filters: [{ name: 'AeroFTP Profile', extensions: ['aeroftp'] }],
            defaultPath: `aeroftp_backup_${new Date().toISOString().slice(0, 10)}.aeroftp`,
        });
        if (!filePath) return;

        setLoading(true);
        setError(null);
        try {
            const serversJson = JSON.stringify(selectedServers);
            await invoke('export_server_profiles', {
                serversJson,
                password,
                includeCredentials,
                filePath,
            });
            setSuccess(t('settings.exportSuccess').replace('{count}', String(selectedServers.length)));
            setTimeout(() => onClose(), 2000);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const handleImport = async () => {
        if (password.length < 1) {
            setError(t('settings.passwordRequired'));
            return;
        }

        // Open file picker first
        const filePath = await open({
            title: t('settings.importServers'),
            filters: [{ name: 'AeroFTP Profile', extensions: ['aeroftp'] }],
            multiple: false,
        });
        if (!filePath) return;

        setLoading(true);
        setError(null);
        try {
            const result = await invoke<ImportResult>('import_server_profiles', {
                filePath,
                password,
            });

            const importedServers = result.servers;

            // Read current servers directly from localStorage (ground truth)
            // The `servers` prop may be stale or incomplete
            let currentServers: ServerProfile[] = [];
            try {
                const stored = localStorage.getItem('aeroftp-saved-servers');
                if (stored) currentServers = JSON.parse(stored);
            } catch { /* fallback to prop */ }
            if (currentServers.length === 0) currentServers = servers;

            // Merge: skip duplicates by host+port+username OR by ID
            const existingKeys = new Set(
                currentServers.map(s => `${s.host}:${s.port}:${s.username}`)
            );
            const existingIds = new Set(currentServers.map(s => s.id));

            const newServers: ServerProfile[] = importedServers
                .filter(s => !existingKeys.has(`${s.host}:${s.port}:${s.username}`) && !existingIds.has(s.id))
                .map(s => ({
                    id: s.id,
                    name: s.name,
                    host: s.host,
                    port: s.port,
                    username: s.username,
                    protocol: s.protocol as ServerProfile['protocol'],
                    initialPath: s.initialPath,
                    localInitialPath: s.localInitialPath,
                    color: s.color,
                    lastConnected: s.lastConnected,
                    options: s.options,
                    providerId: s.providerId,
                    hasStoredCredential: s.credential ? true : (s.hasStoredCredential || false),
                }));

            const skipped = importedServers.length - newServers.length;
            onImport(newServers);
            setSuccess(
                t('settings.importSuccess').replace('{count}', String(newServers.length)) +
                (skipped > 0 ? ` (${skipped} ${t('settings.duplicatesSkipped')})` : '')
            );
            setTimeout(() => onClose(), 2500);
        } catch (err) {
            const errStr = String(err);
            if (errStr.includes('Invalid password')) {
                setError(t('settings.invalidPassword'));
            } else {
                setError(errStr);
            }
        } finally {
            setLoading(false);
        }
    };

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={(e) => e.target === e.currentTarget && onClose()}>
            <div className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-[440px] overflow-hidden animate-scale-in">
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <h3 className="text-lg font-semibold flex items-center gap-2">
                        <Shield size={20} className="text-blue-500" />
                        {t('settings.exportImport')}
                    </h3>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">
                        <X size={18} />
                    </button>
                </div>

                <div className="p-5">
                    {/* Mode selection */}
                    {!mode ? (
                        <div className="space-y-3">
                            <button
                                onClick={() => setMode('export')}
                                disabled={servers.length === 0}
                                className="w-full p-4 border border-gray-200 dark:border-gray-600 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-700/50 flex items-center gap-3 transition-colors disabled:opacity-50"
                            >
                                <div className="w-10 h-10 rounded-lg bg-green-100 dark:bg-green-900/30 flex items-center justify-center">
                                    <Download size={20} className="text-green-600 dark:text-green-400" />
                                </div>
                                <div className="text-left">
                                    <div className="font-medium">{t('settings.exportServers')}</div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400">
                                        {t('settings.exportDescription').replace('{count}', String(servers.length))}
                                    </div>
                                </div>
                            </button>
                            <button
                                onClick={() => setMode('import')}
                                className="w-full p-4 border border-gray-200 dark:border-gray-600 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-700/50 flex items-center gap-3 transition-colors"
                            >
                                <div className="w-10 h-10 rounded-lg bg-blue-100 dark:bg-blue-900/30 flex items-center justify-center">
                                    <Upload size={20} className="text-blue-600 dark:text-blue-400" />
                                </div>
                                <div className="text-left">
                                    <div className="font-medium">{t('settings.importServers')}</div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400">
                                        {t('settings.importDescription')}
                                    </div>
                                </div>
                            </button>
                        </div>
                    ) : mode === 'export' ? (
                        <div className="space-y-4">
                            {/* Server selection list */}
                            <div>
                                <div className="flex items-center justify-between mb-2">
                                    <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                        {t('settings.selectServersToExport')}
                                    </span>
                                    <button
                                        onClick={toggleAll}
                                        className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                                    >
                                        {allSelected ? t('settings.deselectAll') : t('settings.selectAll')}
                                    </button>
                                </div>
                                <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                    {servers.map((server) => (
                                        <label
                                            key={server.id}
                                            className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0"
                                        >
                                            <input
                                                type="checkbox"
                                                checked={selectedServerIds.has(server.id)}
                                                onChange={() => toggleServer(server.id)}
                                                className="w-4 h-4 rounded text-blue-500"
                                            />
                                            <div
                                                className="w-2 h-2 rounded-full flex-shrink-0"
                                                style={{ backgroundColor: server.color || '#6B7280' }}
                                            />
                                            <div className="min-w-0 flex-1">
                                                <div className="text-sm font-medium truncate">{server.name}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                    {server.host}:{server.port} — {server.username}
                                                </div>
                                            </div>
                                            <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">
                                                {server.protocol || 'ftp'}
                                            </span>
                                        </label>
                                    ))}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                    {selectedServerIds.size} / {servers.length} {t('settings.selected')}
                                </div>
                            </div>

                            {/* Include credentials toggle */}
                            <label className="flex items-center gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={includeCredentials}
                                    onChange={(e) => setIncludeCredentials(e.target.checked)}
                                    className="w-4 h-4 rounded"
                                />
                                <div>
                                    <div className="text-sm font-medium flex items-center gap-1">
                                        <Lock size={14} />
                                        {t('settings.includeCredentials')}
                                    </div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400">
                                        {t('settings.includeCredentialsHint')}
                                    </div>
                                </div>
                            </label>

                            {/* Password fields */}
                            <div className="relative">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    placeholder={t('settings.encryptionPassword')}
                                    value={password}
                                    onChange={(e) => setPassword(e.target.value)}
                                    className="w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                                />
                                <button
                                    type="button"
                                    tabIndex={-1}
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600"
                                >
                                    {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                </button>
                            </div>
                            <input
                                type={showPassword ? 'text' : 'password'}
                                placeholder={t('settings.confirmPassword')}
                                value={confirmPassword}
                                onChange={(e) => setConfirmPassword(e.target.value)}
                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                            />

                            {/* Password strength indicator */}
                            {password.length > 0 && password.length < 8 && (
                                <div className="text-xs text-amber-600 dark:text-amber-400">
                                    {t('settings.passwordTooShort')}
                                </div>
                            )}

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={() => { setMode(null); setError(null); setPassword(''); setConfirmPassword(''); }}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                <button
                                    onClick={handleExport}
                                    disabled={loading || password.length < 8 || noneSelected}
                                    className="flex-1 px-4 py-2 text-sm bg-green-500 text-white rounded-lg hover:bg-green-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                >
                                    {loading ? (
                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                    ) : (
                                        <Download size={16} />
                                    )}
                                    {loading ? t('settings.exporting') : `${t('settings.exportServers')} (${selectedServerIds.size})`}
                                </button>
                            </div>
                        </div>
                    ) : (
                        <div className="space-y-4">
                            {/* Password field */}
                            <div className="relative">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    placeholder={t('settings.decryptionPassword')}
                                    value={password}
                                    onChange={(e) => setPassword(e.target.value)}
                                    className="w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                                />
                                <button
                                    type="button"
                                    tabIndex={-1}
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600"
                                >
                                    {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                </button>
                            </div>

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={() => { setMode(null); setError(null); setPassword(''); }}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                <button
                                    onClick={handleImport}
                                    disabled={loading || password.length < 1}
                                    className="flex-1 px-4 py-2 text-sm bg-blue-500 text-white rounded-lg hover:bg-blue-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                >
                                    {loading ? (
                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                    ) : (
                                        <Upload size={16} />
                                    )}
                                    {loading ? t('settings.importing') : t('settings.importServers')}
                                </button>
                            </div>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
};

export default ExportImportDialog;
