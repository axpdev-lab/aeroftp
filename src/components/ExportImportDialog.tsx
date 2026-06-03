// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useMemo, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Upload, Download, Shield, AlertCircle, CheckCircle2, X, Eye, EyeOff, Lock, Server, RefreshCw, FolderInput, AlertTriangle } from 'lucide-react';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';
import { ServerProfile } from '../types';
import { loadSavedServerProfiles, storeSavedServerProfiles } from '../utils/serverProfileStore';
import { useTranslation } from '../i18n';
import { Checkbox } from './ui/Checkbox';
import { BridgeSourcePanel } from './BridgeSourcePanel';
import { BridgeSourceDescriptor, LEGACY_BRIDGE_SOURCES, GENERIC_BRIDGE_SOURCES } from './bridge/bridgeSources';

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

interface RcloneImportResult {
    servers: ImportedServer[];
    skipped: Array<{ name: string; rcloneType: string; reason: string }>;
    sourcePath: string;
    totalRemotes: number;
}

export const ExportImportDialog: React.FC<ExportImportDialogProps> = ({ servers, onImport, onClose }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'export' | 'import' | 'rclone' | 'rclone-export' | 'winscp' | 'winscp-export' | 'filezilla' | 'filezilla-export' | 'bridge-import' | 'bridge-export' | 'bridge-src' | null>(null);
    // Generic bridge source (the 12 expansion sources routed through BridgeSourcePanel)
    const [bridgeSrc, setBridgeSrc] = useState<BridgeSourceDescriptor | null>(null);
    const [bridgeSrcDir, setBridgeSrcDir] = useState<'import' | 'export'>('import');
    // A profile file dropped onto the dialog and identified by
    // bridge_identify; handed to BridgeSourcePanel to skip browse.
    const [bridgePresetPath, setBridgePresetPath] = useState<string | null>(null);
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [includeCredentials, setIncludeCredentials] = useState(true);
    const [showPassword, setShowPassword] = useState(false);
    // KeePassXC-style import flow: the .aeroftp file is chosen first and its
    // path is held here, then the decryption password is requested. Null
    // until a file has been selected.
    const [importFilePath, setImportFilePath] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [selectedServerIds, setSelectedServerIds] = useState<Set<string>>(() => new Set(servers.map(s => s.id)));

    // Rclone-specific state
    const [rcloneDetectedPath, setRcloneDetectedPath] = useState<string | null>(null);
    const [rcloneResult, setRcloneResult] = useState<RcloneImportResult | null>(null);
    const [rcloneSelectedIds, setRcloneSelectedIds] = useState<Set<string>>(new Set());

    // WinSCP-specific state (reuses RcloneImportResult shape: same servers/skipped structure)
    const [winscpDetectedPath, setWinscpDetectedPath] = useState<string | null>(null);
    const [winscpResult, setWinscpResult] = useState<RcloneImportResult | null>(null);
    const [winscpSelectedIds, setWinscpSelectedIds] = useState<Set<string>>(new Set());

    // FileZilla-specific state
    const [filezillaDetectedPath, setFilezillaDetectedPath] = useState<string | null>(null);
    const [filezillaResult, setFilezillaResult] = useState<RcloneImportResult | null>(null);
    const [filezillaSelectedIds, setFilezillaSelectedIds] = useState<Set<string>>(new Set());

    const allSelected = selectedServerIds.size === servers.length;
    const noneSelected = selectedServerIds.size === 0;

    const selectedServers = useMemo(
        () => servers.filter(s => selectedServerIds.has(s.id)),
        [servers, selectedServerIds]
    );

    // Pre-compute existing server keys for duplicate detection in import previews.
    // The parent (ConnectionScreen / SettingsPanel / IntroHub) feeds `servers`
    // from the active user's partition via loadSavedServerProfiles, so a
    // separate localStorage read is no longer needed.
    const existingServerKeys = useMemo(
        () => new Set(servers.map(s => `${s.host}:${s.port}:${s.username}`)),
        [servers],
    );

    // Auto-detect rclone config when entering rclone mode
    useEffect(() => {
        if (mode === 'rclone' && rcloneDetectedPath === null) {
            invoke<string | null>('detect_rclone_config').then(path => {
                setRcloneDetectedPath(path || '');
            }).catch(() => setRcloneDetectedPath(''));
        }
    }, [mode, rcloneDetectedPath]);

    // Auto-detect WinSCP config when entering winscp mode
    useEffect(() => {
        if (mode === 'winscp' && winscpDetectedPath === null) {
            invoke<string | null>('detect_winscp_config').then(path => {
                setWinscpDetectedPath(path || '');
            }).catch(() => setWinscpDetectedPath(''));
        }
    }, [mode, winscpDetectedPath]);

    // Auto-detect FileZilla config when entering filezilla mode
    useEffect(() => {
        if (mode === 'filezilla' && filezillaDetectedPath === null) {
            invoke<string | null>('detect_filezilla_config').then(path => {
                setFilezillaDetectedPath(path || '');
            }).catch(() => setFilezillaDetectedPath(''));
        }
    }, [mode, filezillaDetectedPath]);

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

    // Step 1 of the import: choose the .aeroftp file. The decryption
    // password is only requested once a file is loaded (KeePassXC pattern),
    // so the user sees what they are unlocking before typing a secret.
    const handleChooseImportFile = async () => {
        const filePath = await open({
            title: t('settings.importServers'),
            filters: [
                { name: 'AeroFTP Profile', extensions: ['aeroftp'] },
                { name: 'All Files', extensions: ['*'] },
            ],
            multiple: false,
        });
        if (!filePath || typeof filePath !== 'string') return;
        setError(null);
        setSuccess(null);
        setPassword('');
        setImportFilePath(filePath);
    };

    // Step 2 of the import: decrypt the already-chosen file with the password.
    const handleImport = async () => {
        const filePath = importFilePath;
        if (!filePath) {
            return;
        }
        if (password.length < 1) {
            setError(t('settings.passwordRequired'));
            return;
        }

        setLoading(true);
        setError(null);
        try {
            const result = await invoke<ImportResult>('import_server_profiles', {
                filePath,
                password,
            });

            const importedServers = result.servers;

            // Read current servers directly from the active user's vault
            // partition (ground truth). The `servers` prop may be stale or
            // incomplete during an in-flight import.
            let currentServers = await loadSavedServerProfiles();
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

    // ---- Rclone import handlers ----

    const handleRcloneScan = async (customPath?: string) => {
        const filePath = customPath || rcloneDetectedPath;
        if (!filePath) return;

        setLoading(true);
        setError(null);
        setRcloneResult(null);
        try {
            const result = await invoke<RcloneImportResult>('import_rclone_config', { filePath });
            setRcloneResult(result);
            // Pre-select all importable servers
            setRcloneSelectedIds(new Set(result.servers.map(s => s.id)));
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const handleRcloneBrowse = async () => {
        const filePath = await open({
            title: t('settings.rcloneSelectConfig'),
            filters: [
                { name: 'rclone config', extensions: ['conf'] },
                { name: 'All Files', extensions: ['*'] },
            ],
            multiple: false,
        });
        if (!filePath) return;
        setRcloneDetectedPath(filePath);
        await handleRcloneScan(filePath);
    };

    const handleRcloneConfirm = async () => {
        if (!rcloneResult) return;

        const selected: ServerProfile[] = rcloneResult.servers
            .filter(s => rcloneSelectedIds.has(s.id))
            .map(s => ({
                id: s.id,
                name: s.name,
                host: s.host,
                port: s.port,
                username: s.username,
                protocol: s.protocol as ServerProfile['protocol'],
                initialPath: s.initialPath,
                options: s.options as ServerProfile['options'],
                providerId: s.providerId,
                hasStoredCredential: s.hasStoredCredential || false,
            }));

        // Separate new vs. existing (by host:port:username)
        const added = selected.filter(s => !existingServerKeys.has(`${s.host}:${s.port}:${s.username}`));
        const updated = selected.filter(s => existingServerKeys.has(`${s.host}:${s.port}:${s.username}`));

        // For updated servers: replace the entry in the active user's vault
        // partition so credentials and options are refreshed. Capture a
        // snapshot so the partition can be rolled back if the caller's
        // onImport throws.
        const backup = await loadSavedServerProfiles().catch(() => null);
        if (updated.length > 0 && backup && backup.length > 0) {
            try {
                const updatedKeys = new Set(updated.map(s => `${s.host}:${s.port}:${s.username}`));
                const filtered = backup.filter(s => !updatedKeys.has(`${s.host}:${s.port}:${s.username}`));
                await storeSavedServerProfiles(filtered).catch(() => {});
            } catch { /* best-effort: caller's onImport still receives the union */ }
        }

        try {
            onImport([...updated, ...added]);
        } catch {
            // Rollback the partition snapshot on failure.
            if (backup !== null) await storeSavedServerProfiles(backup).catch(() => {});
            setError('Import failed. No changes were made.');
            return;
        }

        const parts: string[] = [];
        if (added.length > 0) parts.push(t('settings.importSuccess').replace('{count}', String(added.length)));
        if (updated.length > 0) parts.push(t('settings.serversUpdated').replace('{count}', String(updated.length)));
        setSuccess(parts.join(', '));
        setTimeout(() => onClose(), 2500);
    };

    const toggleRcloneServer = (id: string) => {
        setRcloneSelectedIds(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    const toggleAllRclone = () => {
        if (!rcloneResult) return;
        if (rcloneSelectedIds.size === rcloneResult.servers.length) {
            setRcloneSelectedIds(new Set());
        } else {
            setRcloneSelectedIds(new Set(rcloneResult.servers.map(s => s.id)));
        }
    };

    // ---- WinSCP import handlers ----

    const handleWinscpScan = async (customPath?: string) => {
        const filePath = customPath || winscpDetectedPath;
        if (!filePath) return;

        setLoading(true);
        setError(null);
        setWinscpResult(null);
        try {
            const result = await invoke<RcloneImportResult>('import_winscp_config', { filePath });
            setWinscpResult(result);
            setWinscpSelectedIds(new Set(result.servers.map(s => s.id)));
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const handleWinscpBrowse = async () => {
        const filePath = await open({
            title: t('settings.winscpSelectConfig'),
            filters: [
                { name: 'WinSCP config', extensions: ['ini'] },
                { name: 'All Files', extensions: ['*'] },
            ],
            multiple: false,
        });
        if (!filePath) return;
        setWinscpDetectedPath(filePath);
        await handleWinscpScan(filePath);
    };

    const handleWinscpConfirm = async () => {
        if (!winscpResult) return;

        const selected: ServerProfile[] = winscpResult.servers
            .filter(s => winscpSelectedIds.has(s.id))
            .map(s => ({
                id: s.id,
                name: s.name,
                host: s.host,
                port: s.port,
                username: s.username,
                protocol: s.protocol as ServerProfile['protocol'],
                initialPath: s.initialPath,
                options: s.options as ServerProfile['options'],
                providerId: s.providerId,
                hasStoredCredential: s.hasStoredCredential || false,
            }));

        const added = selected.filter(s => !existingServerKeys.has(`${s.host}:${s.port}:${s.username}`));
        const updated = selected.filter(s => existingServerKeys.has(`${s.host}:${s.port}:${s.username}`));

        const backup = await loadSavedServerProfiles().catch(() => null);
        if (updated.length > 0 && backup && backup.length > 0) {
            try {
                const updatedKeys = new Set(updated.map(s => `${s.host}:${s.port}:${s.username}`));
                const filtered = backup.filter(s => !updatedKeys.has(`${s.host}:${s.port}:${s.username}`));
                await storeSavedServerProfiles(filtered).catch(() => {});
            } catch { /* best-effort */ }
        }

        try {
            onImport([...updated, ...added]);
        } catch {
            if (backup !== null) await storeSavedServerProfiles(backup).catch(() => {});
            setError('Import failed. No changes were made.');
            return;
        }

        const parts: string[] = [];
        if (added.length > 0) parts.push(t('settings.importSuccess').replace('{count}', String(added.length)));
        if (updated.length > 0) parts.push(t('settings.serversUpdated').replace('{count}', String(updated.length)));
        setSuccess(parts.join(', '));
        setTimeout(() => onClose(), 2500);
    };

    const toggleWinscpServer = (id: string) => {
        setWinscpSelectedIds(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    const toggleAllWinscp = () => {
        if (!winscpResult) return;
        if (winscpSelectedIds.size === winscpResult.servers.length) {
            setWinscpSelectedIds(new Set());
        } else {
            setWinscpSelectedIds(new Set(winscpResult.servers.map(s => s.id)));
        }
    };

    // ---- FileZilla import handlers ----

    const handleFilezillaScan = async (customPath?: string) => {
        const filePath = customPath || filezillaDetectedPath;
        if (!filePath) return;
        setLoading(true);
        setError(null);
        setFilezillaResult(null);
        try {
            const result = await invoke<RcloneImportResult>('import_filezilla_config', { filePath });
            setFilezillaResult(result);
            setFilezillaSelectedIds(new Set(result.servers.map(s => s.id)));
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const handleFilezillaBrowse = async () => {
        const filePath = await open({
            title: t('settings.filezillaSelectConfig'),
            filters: [
                { name: 'FileZilla config', extensions: ['xml'] },
                { name: 'All Files', extensions: ['*'] },
            ],
            multiple: false,
        });
        if (!filePath) return;
        setFilezillaDetectedPath(filePath);
        await handleFilezillaScan(filePath);
    };

    const handleFilezillaConfirm = async () => {
        if (!filezillaResult) return;
        const selected: ServerProfile[] = filezillaResult.servers
            .filter(s => filezillaSelectedIds.has(s.id))
            .map(s => ({
                id: s.id, name: s.name, host: s.host, port: s.port, username: s.username,
                protocol: s.protocol as ServerProfile['protocol'],
                initialPath: s.initialPath,
                options: s.options as ServerProfile['options'],
                providerId: s.providerId,
                hasStoredCredential: s.hasStoredCredential || false,
            }));
        const added = selected.filter(s => !existingServerKeys.has(`${s.host}:${s.port}:${s.username}`));
        const updated = selected.filter(s => existingServerKeys.has(`${s.host}:${s.port}:${s.username}`));
        const backup = await loadSavedServerProfiles().catch(() => null);
        if (updated.length > 0 && backup && backup.length > 0) {
            try {
                const updatedKeys = new Set(updated.map(s => `${s.host}:${s.port}:${s.username}`));
                const filtered = backup.filter(s => !updatedKeys.has(`${s.host}:${s.port}:${s.username}`));
                await storeSavedServerProfiles(filtered).catch(() => {});
            } catch { /* best-effort */ }
        }
        try {
            onImport([...updated, ...added]);
        } catch {
            if (backup !== null) await storeSavedServerProfiles(backup).catch(() => {});
            setError('Import failed. No changes were made.');
            return;
        }
        const parts: string[] = [];
        if (added.length > 0) parts.push(t('settings.importSuccess').replace('{count}', String(added.length)));
        if (updated.length > 0) parts.push(t('settings.serversUpdated').replace('{count}', String(updated.length)));
        setSuccess(parts.join(', '));
        setTimeout(() => onClose(), 2500);
    };

    const toggleFilezillaServer = (id: string) => {
        setFilezillaSelectedIds(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    const toggleAllFilezilla = () => {
        if (!filezillaResult) return;
        if (filezillaSelectedIds.size === filezillaResult.servers.length) {
            setFilezillaSelectedIds(new Set());
        } else {
            setFilezillaSelectedIds(new Set(filezillaResult.servers.map(s => s.id)));
        }
    };

    const handleFilezillaExport = async () => {
        if (noneSelected) return;
        const filePath = await save({
            title: t('settings.filezillaExportTitle'),
            filters: [{ name: 'FileZilla config', extensions: ['xml'] }],
            defaultPath: 'sitemanager.xml',
        });
        if (!filePath) return;
        setLoading(true);
        setError(null);
        try {
            const serversJson = JSON.stringify(selectedServers);
            const result = await invoke<{ exported: number }>('export_filezilla_config', {
                serversJson, includeCredentials, filePath,
            });
            setSuccess(t('settings.filezillaExportSuccess').replace('{count}', String(result.exported)));
            setTimeout(() => onClose(), 2000);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    // ---- WinSCP export handler ----

    const handleWinscpExport = async () => {
        if (noneSelected) return;

        const filePath = await save({
            title: t('settings.winscpExportTitle'),
            filters: [{ name: 'WinSCP config', extensions: ['ini'] }],
            defaultPath: 'WinSCP.ini',
        });
        if (!filePath) return;

        setLoading(true);
        setError(null);
        try {
            const serversJson = JSON.stringify(selectedServers);
            const result = await invoke<{ exported: number }>('export_winscp_config', {
                serversJson,
                includeCredentials,
                filePath,
            });
            setSuccess(t('settings.winscpExportSuccess').replace('{count}', String(result.exported)));
            setTimeout(() => onClose(), 2000);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const handleRcloneExport = async () => {
        if (noneSelected) return;

        const filePath = await save({
            title: t('settings.rcloneExportTitle'),
            filters: [{ name: 'rclone config', extensions: ['conf'] }],
            defaultPath: 'rclone.conf',
        });
        if (!filePath) return;

        setLoading(true);
        setError(null);
        try {
            const serversJson = JSON.stringify(selectedServers);
            const result = await invoke<{ exported: number }>('export_rclone_config', {
                serversJson,
                includeCredentials,
                filePath,
            });
            setSuccess(t('settings.rcloneExportSuccess').replace('{count}', String(result.exported)));
            setTimeout(() => onClose(), 2000);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const resetMode = () => {
        setMode(null);
        setError(null);
        setSuccess(null);
        setPassword('');
        setConfirmPassword('');
        setImportFilePath(null);
        setRcloneResult(null);
        setRcloneSelectedIds(new Set());
        setWinscpResult(null);
        setWinscpSelectedIds(new Set());
        setFilezillaResult(null);
        setFilezillaSelectedIds(new Set());
        setBridgeSrc(null);
        setBridgePresetPath(null);
    };

    // Route to a bridge source's import surface. Shared by the source
    // list click and by drag-and-drop. Legacy sources keep their own
    // dedicated mode (no preset path: those panels manage their own
    // file selection); generic sources go through BridgeSourcePanel and
    // can receive the dropped file directly.
    const routeBridgeSource = useCallback(
        (s: BridgeSourceDescriptor, asImport: boolean, presetPath?: string) => {
            if (asImport && s.legacyImportMode) {
                setBridgePresetPath(null);
                setMode(s.legacyImportMode);
                return;
            }
            if (!asImport && s.legacyExportMode) {
                setBridgePresetPath(null);
                setMode(s.legacyExportMode);
                return;
            }
            setBridgePresetPath(presetPath ?? null);
            setBridgeSrc(s);
            setBridgeSrcDir(asImport ? 'import' : 'export');
            setMode('bridge-src');
        },
        [],
    );

    // Drag-and-drop a client profile file anywhere on the open dialog:
    // identify the format and jump straight to that source's import
    // form with the file preloaded. One match routes directly; zero or
    // several matches open the source list (with the file kept ready)
    // so the user picks. Never auto-imports: the preview + confirm step
    // in BridgeSourcePanel still applies.
    const handleBridgeDrop = useCallback(
        async (filePath: string) => {
            try {
                const res = await invoke<{ fileName: string; candidates: string[] }>(
                    'bridge_identify',
                    { filePath },
                );
                const all = [...LEGACY_BRIDGE_SOURCES, ...GENERIC_BRIDGE_SOURCES];
                const matched = res.candidates
                    .map(id => all.find(s => s.id === id))
                    .filter((s): s is BridgeSourceDescriptor => !!s);
                if (matched.length === 1) {
                    setError(null);
                    routeBridgeSource(matched[0], true, filePath);
                } else {
                    // 0 or >1: let the user choose from the list, keeping
                    // the dropped file ready so the pick still uses it.
                    setBridgePresetPath(filePath);
                    setBridgeSrc(null);
                    setBridgeSrcDir('import');
                    setMode('bridge-import');
                }
            } catch (e) {
                setError(String(e));
            }
        },
        [routeBridgeSource],
    );

    // OS file drag-and-drop is delivered through the Tauri webview API
    // (HTML drop does not expose absolute paths in the sandbox). The
    // listener lives for the dialog's lifetime (it is only mounted while
    // the modal is open).
    useEffect(() => {
        let unlisten: (() => void) | undefined;
        let cancelled = false;
        (async () => {
            const webview = getCurrentWebview();
            const un = await webview.onDragDropEvent((event) => {
                if (event.payload.type === 'drop' && event.payload.paths.length > 0) {
                    void handleBridgeDrop(event.payload.paths[0]);
                }
            });
            if (cancelled) un();
            else unlisten = un;
        })();
        return () => {
            cancelled = true;
            if (unlisten) unlisten();
        };
    }, [handleBridgeDrop]);

    // Protocol display helper
    const protocolLabel = (proto?: string) => (proto || 'ftp').toUpperCase();

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={(e) => e.target === e.currentTarget && onClose()}>
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[480px] max-h-[85vh] overflow-hidden animate-scale-in flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700 flex-shrink-0">
                    <h3 className="text-lg font-semibold flex items-center gap-2">
                        <Shield size={20} className="text-blue-500" />
                        {mode === 'rclone' ? t('settings.rcloneImport')
                            : mode === 'winscp' ? t('settings.winscpImport')
                            : mode === 'filezilla' ? t('settings.filezillaImport')
                            : mode === 'rclone-export' ? t('settings.rcloneExport')
                            : mode === 'winscp-export' ? t('settings.winscpExport')
                            : mode === 'filezilla-export' ? t('settings.filezillaExport')
                            : mode === 'bridge-import' ? t('settings.bridgeImport')
                            : mode === 'bridge-export' ? t('settings.bridgeExport')
                            : mode === 'bridge-src' && bridgeSrc ? `${bridgeSrc.label} ${bridgeSrcDir === 'import' ? t('settings.bridgeImport') : t('settings.bridgeExport')}`
                            : t('settings.exportImport')}
                    </h3>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">
                        <X size={18} />
                    </button>
                </div>

                <div className="p-5 overflow-y-auto">
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
                            {/* Bridge section - unified import/export for third-party clients */}
                            <div className="pt-2 border-t border-gray-100 dark:border-gray-700 space-y-3">
                                <div className="text-[10px] uppercase tracking-wider text-gray-400 dark:text-gray-500 font-medium">{t('settings.bridgeTitle')}</div>
                                <button
                                    onClick={() => setMode('bridge-import')}
                                    className="w-full p-4 border border-cyan-200 dark:border-cyan-800/50 rounded-lg hover:bg-cyan-50 dark:hover:bg-cyan-900/20 flex items-center gap-3 transition-colors"
                                >
                                    <div className="w-10 h-10 rounded-lg bg-cyan-100 dark:bg-cyan-900/30 flex items-center justify-center">
                                        <FolderInput size={20} className="text-cyan-600 dark:text-cyan-400" />
                                    </div>
                                    <div className="text-left">
                                        <div className="font-medium">{t('settings.bridgeImport')}</div>
                                        <div className="text-xs text-gray-500 dark:text-gray-400">
                                            {t('settings.bridgeImportDesc')}
                                        </div>
                                    </div>
                                </button>
                                <button
                                    onClick={() => setMode('bridge-export')}
                                    disabled={servers.length === 0}
                                    className="w-full p-4 border border-cyan-200 dark:border-cyan-800/50 rounded-lg hover:bg-cyan-50 dark:hover:bg-cyan-900/20 flex items-center gap-3 transition-colors disabled:opacity-50"
                                >
                                    <div className="w-10 h-10 rounded-lg bg-cyan-100 dark:bg-cyan-900/30 flex items-center justify-center">
                                        <Download size={20} className="text-cyan-600 dark:text-cyan-400" />
                                    </div>
                                    <div className="text-left">
                                        <div className="font-medium">{t('settings.bridgeExport')}</div>
                                        <div className="text-xs text-gray-500 dark:text-gray-400">
                                            {t('settings.bridgeExportDesc')}
                                        </div>
                                    </div>
                                </button>
                            </div>
                        </div>
                    ) : (mode === 'bridge-import' || mode === 'bridge-export') ? (
                        /* ---- Bridge: select source/target app (all 15) ---- */
                        (() => {
                            const isImport = mode === 'bridge-import';
                            const Icon = isImport ? FolderInput : Download;
                            const pick = (s: BridgeSourceDescriptor) =>
                                routeBridgeSource(s, isImport, bridgePresetPath ?? undefined);
                            return (
                                <div className="space-y-3">
                                    <div className="text-sm text-gray-600 dark:text-gray-300">
                                        {isImport ? t('settings.bridgeSelectSource') : t('settings.bridgeSelectTarget')}
                                    </div>
                                    {[...LEGACY_BRIDGE_SOURCES, ...GENERIC_BRIDGE_SOURCES].map(s => (
                                        <button
                                            key={s.id}
                                            onClick={() => pick(s)}
                                            disabled={!isImport && servers.length === 0}
                                            className="w-full p-3 border border-gray-200 dark:border-gray-700 rounded-lg hover:border-blue-400 dark:hover:border-blue-500 flex items-center gap-3 transition-colors disabled:opacity-50"
                                        >
                                            <div className={`w-9 h-9 rounded-lg ${s.accentBg} flex items-center justify-center flex-shrink-0`}>
                                                <Icon size={18} className={s.accent} />
                                            </div>
                                            <div className="text-left min-w-0">
                                                <div className="font-medium">{s.label}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                    {(isImport ? t('settings.bridgeGenericImportDesc') : t('settings.bridgeGenericExportDesc')).replace('{app}', s.label)}
                                                </div>
                                            </div>
                                        </button>
                                    ))}
                                    <div className="flex gap-2">
                                        <button onClick={resetMode} className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700">{t('common.back')}</button>
                                    </div>
                                </div>
                            );
                        })()
                    ) : mode === 'bridge-src' && bridgeSrc ? (
                        <BridgeSourcePanel
                            source={bridgeSrc}
                            direction={bridgeSrcDir}
                            servers={servers}
                            existingServerKeys={existingServerKeys}
                            onImport={onImport}
                            onClose={onClose}
                            presetFilePath={bridgePresetPath ?? undefined}
                            onBack={() => { setBridgeSrc(null); setBridgePresetPath(null); setError(null); setSuccess(null); setMode(bridgeSrcDir === 'import' ? 'bridge-import' : 'bridge-export'); }}
                        />
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
                                        <div
                                            key={server.id}
                                            className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0"
                                        >
                                            <Checkbox
                                                checked={selectedServerIds.has(server.id)}
                                                onChange={() => toggleServer(server.id)}
                                            />
                                            <div
                                                className="w-2 h-2 rounded-full flex-shrink-0"
                                                style={{ backgroundColor: server.color || '#6B7280' }}
                                            />
                                            <div className="min-w-0 flex-1">
                                                <div className="text-sm font-medium truncate">{server.name}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                    {server.host}:{server.port}: {server.username}
                                                </div>
                                            </div>
                                            <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">
                                                {server.protocol || 'ftp'}
                                            </span>
                                        </div>
                                    ))}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                    {selectedServerIds.size} / {servers.length} {t('settings.selected')}
                                </div>
                            </div>

                            {/* Include credentials toggle */}
                            <div className="flex flex-col gap-2 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg">
                                <Checkbox
                                    checked={includeCredentials}
                                    onChange={setIncludeCredentials}
                                    label={
                                        <div>
                                            <div className="text-sm font-medium flex items-center gap-1">
                                                <Lock size={14} />
                                                {t('settings.includeCredentials')}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400">
                                                {t('settings.includeCredentialsHint')}
                                            </div>
                                        </div>
                                    }
                                />
                                <div className="text-[11px] text-gray-500 dark:text-gray-400 leading-snug pl-7">
                                    {t('settings.exportFormatComparisonHint')}
                                </div>
                            </div>

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

                            {/* Password strength indicator (0-100 score, parity with AeroVault) */}
                            {password.length > 0 && (
                                <PasswordStrengthBar password={password} />
                            )}
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
                                    onClick={resetMode}
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
                    ) : mode === 'import' ? (
                        <div className="space-y-4">
                            {!importFilePath ? (
                                /* Step 1: choose the file before asking for a password */
                                <>
                                    <p className="text-sm text-gray-600 dark:text-gray-400">
                                        {t('settings.importChooseFileHint') || 'Select an encrypted .aeroftp file to import. You will be asked for its password next.'}
                                    </p>
                                    <button
                                        onClick={handleChooseImportFile}
                                        className="w-full px-4 py-3 text-sm border border-dashed border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 flex items-center justify-center gap-2"
                                    >
                                        <FolderInput size={16} />
                                        {t('settings.importChooseFile') || 'Choose .aeroftp file...'}
                                    </button>
                                </>
                            ) : (
                                /* Step 2: file is loaded, now request the password */
                                <>
                                    <div className="flex items-center justify-between gap-2 p-2.5 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900/40">
                                        <div className="flex items-center gap-2 min-w-0">
                                            <Lock size={16} className="text-blue-500 flex-shrink-0" />
                                            <span className="text-sm text-gray-800 dark:text-gray-100 truncate" title={importFilePath}>
                                                {importFilePath.split(/[\\/]/).pop()}
                                            </span>
                                        </div>
                                        <button
                                            type="button"
                                            onClick={handleChooseImportFile}
                                            className="text-xs text-blue-500 hover:text-blue-600 font-medium flex-shrink-0"
                                        >
                                            {t('settings.importChangeFile') || 'Change'}
                                        </button>
                                    </div>

                                    {/* Password field */}
                                    <div className="relative">
                                        <input
                                            type={showPassword ? 'text' : 'password'}
                                            placeholder={t('settings.decryptionPassword')}
                                            value={password}
                                            onChange={(e) => setPassword(e.target.value)}
                                            onKeyDown={(e) => {
                                                if (e.key === 'Enter' && password.length >= 1 && !loading) handleImport();
                                            }}
                                            autoFocus
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
                                </>
                            )}

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                {importFilePath && (
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
                                )}
                            </div>
                        </div>
                    ) : mode === 'rclone' ? (
                        /* ---- Rclone Import Mode ---- */
                        <div className="space-y-4">
                            {/* Security upgrade notice */}
                            <div className="flex items-start gap-2 p-3 bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 rounded-lg">
                                <Shield size={16} className="text-green-600 dark:text-green-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-green-700 dark:text-green-300">
                                    {t('settings.rcloneSecurityUpgrade')}
                                </div>
                            </div>

                            {!rcloneResult ? (
                                /* Step 1: Detect/select config file */
                                <>
                                    {rcloneDetectedPath === null ? (
                                        <div className="flex items-center justify-center py-6">
                                            <RefreshCw size={20} className="animate-spin text-gray-400" />
                                            <span className="ml-2 text-sm text-gray-500">{t('settings.rcloneDetecting')}</span>
                                        </div>
                                    ) : rcloneDetectedPath ? (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 rounded-lg">
                                                <div className="text-xs text-gray-500 dark:text-gray-400 mb-1">{t('settings.rcloneConfigFound')}</div>
                                                <div className="text-sm font-mono truncate" title={rcloneDetectedPath}>
                                                    {rcloneDetectedPath}
                                                </div>
                                            </div>
                                            <div className="flex gap-2">
                                                <button
                                                    onClick={() => handleRcloneScan()}
                                                    disabled={loading}
                                                    className="flex-1 px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                                >
                                                    {loading ? (
                                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                                    ) : (
                                                        <FolderInput size={16} />
                                                    )}
                                                    {loading ? t('settings.rcloneScanning') : t('settings.rcloneScanConfig')}
                                                </button>
                                                <button
                                                    onClick={handleRcloneBrowse}
                                                    disabled={loading}
                                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                                >
                                                    {t('settings.rcloneBrowse')}
                                                </button>
                                            </div>
                                        </div>
                                    ) : (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                                <div className="text-sm text-blue-700 dark:text-blue-300 flex items-center gap-2">
                                                    <AlertCircle size={14} />
                                                    {t('settings.rcloneNotFound')}
                                                </div>
                                                <div className="text-xs text-blue-600 dark:text-blue-400 mt-1">
                                                    {t('settings.rcloneNotFoundHint')}
                                                </div>
                                            </div>
                                            <button
                                                onClick={handleRcloneBrowse}
                                                disabled={loading}
                                                className="w-full px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                            >
                                                <FolderInput size={16} />
                                                {t('settings.rcloneBrowse')}
                                            </button>
                                        </div>
                                    )}
                                </>
                            ) : (
                                /* Step 2: Preview and select remotes */
                                <>
                                    {/* Summary */}
                                    <div className="text-sm text-gray-600 dark:text-gray-300">
                                        {t('settings.rcloneFound')
                                            .replace('{total}', String(rcloneResult.totalRemotes))
                                            .replace('{supported}', String(rcloneResult.servers.length))}
                                    </div>

                                    {/* Importable servers */}
                                    {rcloneResult.servers.length > 0 && (
                                        <div>
                                            <div className="flex items-center justify-between mb-2">
                                                <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                                    {t('settings.rcloneSelectRemotes')}
                                                </span>
                                                <button
                                                    onClick={toggleAllRclone}
                                                    className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                                                >
                                                    {rcloneSelectedIds.size === rcloneResult.servers.length
                                                        ? t('settings.deselectAll')
                                                        : t('settings.selectAll')}
                                                </button>
                                            </div>
                                            <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                                {rcloneResult.servers.map((server) => (
                                                    <div
                                                        key={server.id}
                                                        className={`flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0 ${existingServerKeys.has(`${server.host}:${server.port}:${server.username}`) ? 'opacity-50' : ''}`}
                                                        onClick={() => toggleRcloneServer(server.id)}
                                                    >
                                                        <Checkbox
                                                            checked={rcloneSelectedIds.has(server.id)}
                                                            onChange={() => toggleRcloneServer(server.id)}
                                                        />
                                                        <div className="min-w-0 flex-1">
                                                            <div className="text-sm font-medium truncate flex items-center gap-1.5">
                                                                {server.name}
                                                                {existingServerKeys.has(`${server.host}:${server.port}:${server.username}`) && (
                                                                    <span className="text-[9px] px-1.5 py-0.5 rounded bg-amber-100 dark:bg-amber-900/30 text-amber-700 dark:text-amber-400 font-medium whitespace-nowrap">{t('settings.alreadyExists')}</span>
                                                                )}
                                                            </div>
                                                            <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                                {server.host}{server.port !== 443 ? `:${server.port}` : ''}{server.username ? ` - ${server.username}` : ''}
                                                            </div>
                                                        </div>
                                                        <div className="flex items-center gap-1.5 flex-shrink-0">
                                                            {server.hasStoredCredential && (
                                                                <Lock size={12} className="text-green-500" />
                                                            )}
                                                            <span className="text-[10px] text-gray-400 uppercase">
                                                                {protocolLabel(server.protocol)}
                                                            </span>
                                                        </div>
                                                    </div>
                                                ))}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                                {rcloneSelectedIds.size} / {rcloneResult.servers.length} {t('settings.selected')}
                                            </div>
                                        </div>
                                    )}

                                    {/* OAuth re-auth notice */}
                                    {rcloneResult.servers.some(s =>
                                        ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'yandexdisk', 'jottacloud'].includes(s.protocol || '')
                                    ) && (
                                        <div className="flex items-start gap-2 p-2.5 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                            <AlertCircle size={14} className="text-blue-500 mt-0.5 flex-shrink-0" />
                                            <div className="text-xs text-blue-700 dark:text-blue-300">
                                                {t('settings.rcloneOauthNotice')}
                                            </div>
                                        </div>
                                    )}

                                    {/* Skipped remotes */}
                                    {rcloneResult.skipped.length > 0 && (
                                        <div>
                                            <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                                                {t('settings.rcloneSkipped')} ({rcloneResult.skipped.length})
                                            </div>
                                            <div className="text-xs text-gray-400 dark:text-gray-500 space-y-0.5">
                                                {rcloneResult.skipped.map((s, i) => (
                                                    <div key={i} className="truncate">
                                                        <span className="font-medium">{s.name}</span>
                                                        <span className="mx-1">-</span>
                                                        <span>{s.rcloneType}</span>
                                                    </div>
                                                ))}
                                            </div>
                                            <div className="text-xs text-gray-400 dark:text-gray-500 mt-1.5 italic">
                                                {t('settings.rcloneMoreComingSoon')}
                                            </div>
                                        </div>
                                    )}
                                </>
                            )}

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                {rcloneResult && rcloneResult.servers.length > 0 && (
                                    <button
                                        onClick={handleRcloneConfirm}
                                        disabled={rcloneSelectedIds.size === 0}
                                        className="flex-1 px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                    >
                                        <Upload size={16} />
                                        {t('settings.rcloneImportSelected').replace('{count}', String(rcloneSelectedIds.size))}
                                    </button>
                                )}
                            </div>
                        </div>
                    ) : mode === 'winscp' ? (
                        /* ---- WinSCP Import Mode ---- */
                        <div className="space-y-4">
                            {/* Security upgrade notice */}
                            <div className="flex items-start gap-2 p-3 bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 rounded-lg">
                                <Shield size={16} className="text-green-600 dark:text-green-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-green-700 dark:text-green-300">
                                    {t('settings.winscpSecurityUpgrade')}
                                </div>
                            </div>

                            {!winscpResult ? (
                                /* Step 1: Detect/select config file */
                                <>
                                    {winscpDetectedPath === null ? (
                                        <div className="flex items-center justify-center py-6">
                                            <RefreshCw size={20} className="animate-spin text-gray-400" />
                                            <span className="ml-2 text-sm text-gray-500">{t('settings.winscpDetecting')}</span>
                                        </div>
                                    ) : winscpDetectedPath ? (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 rounded-lg">
                                                <div className="text-xs text-gray-500 dark:text-gray-400 mb-1">{t('settings.winscpConfigFound')}</div>
                                                <div className="text-sm font-mono truncate" title={winscpDetectedPath}>
                                                    {winscpDetectedPath}
                                                </div>
                                            </div>
                                            <div className="flex gap-2">
                                                <button
                                                    onClick={() => handleWinscpScan()}
                                                    disabled={loading}
                                                    className="flex-1 px-4 py-2 text-sm bg-purple-500 text-white rounded-lg hover:bg-purple-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                                >
                                                    {loading ? (
                                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                                    ) : (
                                                        <FolderInput size={16} />
                                                    )}
                                                    {loading ? t('settings.winscpScanning') : t('settings.winscpScanConfig')}
                                                </button>
                                                <button
                                                    onClick={handleWinscpBrowse}
                                                    disabled={loading}
                                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                                >
                                                    {t('settings.winscpBrowse')}
                                                </button>
                                            </div>
                                        </div>
                                    ) : (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                                <div className="text-sm text-blue-700 dark:text-blue-300 flex items-center gap-2">
                                                    <AlertCircle size={14} />
                                                    {t('settings.winscpNotFound')}
                                                </div>
                                                <div className="text-xs text-blue-600 dark:text-blue-400 mt-1">
                                                    {t('settings.winscpNotFoundHint')}
                                                </div>
                                            </div>
                                            <button
                                                onClick={handleWinscpBrowse}
                                                disabled={loading}
                                                className="w-full px-4 py-2 text-sm bg-purple-500 text-white rounded-lg hover:bg-purple-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                            >
                                                <FolderInput size={16} />
                                                {t('settings.winscpBrowse')}
                                            </button>
                                        </div>
                                    )}
                                </>
                            ) : (
                                /* Step 2: Preview and select sessions */
                                <>
                                    {/* Summary */}
                                    <div className="text-sm text-gray-600 dark:text-gray-300">
                                        {t('settings.winscpFound')
                                            .replace('{total}', String(winscpResult.totalRemotes))
                                            .replace('{supported}', String(winscpResult.servers.length))}
                                    </div>

                                    {/* Importable servers */}
                                    {winscpResult.servers.length > 0 && (
                                        <div>
                                            <div className="flex items-center justify-between mb-2">
                                                <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                                    {t('settings.winscpSelectSessions')}
                                                </span>
                                                <button
                                                    onClick={toggleAllWinscp}
                                                    className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                                                >
                                                    {winscpSelectedIds.size === winscpResult.servers.length
                                                        ? t('settings.deselectAll')
                                                        : t('settings.selectAll')}
                                                </button>
                                            </div>
                                            <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                                {winscpResult.servers.map((server) => (
                                                    <div
                                                        key={server.id}
                                                        className={`flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0 ${existingServerKeys.has(`${server.host}:${server.port}:${server.username}`) ? 'opacity-50' : ''}`}
                                                        onClick={() => toggleWinscpServer(server.id)}
                                                    >
                                                        <Checkbox
                                                            checked={winscpSelectedIds.has(server.id)}
                                                            onChange={() => toggleWinscpServer(server.id)}
                                                        />
                                                        <div className="min-w-0 flex-1">
                                                            <div className="text-sm font-medium truncate flex items-center gap-1.5">
                                                                {server.name}
                                                                {existingServerKeys.has(`${server.host}:${server.port}:${server.username}`) && (
                                                                    <span className="text-[9px] px-1.5 py-0.5 rounded bg-amber-100 dark:bg-amber-900/30 text-amber-700 dark:text-amber-400 font-medium whitespace-nowrap">{t('settings.alreadyExists')}</span>
                                                                )}
                                                            </div>
                                                            <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                                {server.host}{server.port !== 443 ? `:${server.port}` : ''}{server.username ? ` - ${server.username}` : ''}
                                                            </div>
                                                        </div>
                                                        <div className="flex items-center gap-1.5 flex-shrink-0">
                                                            {server.hasStoredCredential && (
                                                                <Lock size={12} className="text-green-500" />
                                                            )}
                                                            <span className="text-[10px] text-gray-400 uppercase">
                                                                {protocolLabel(server.protocol)}
                                                            </span>
                                                        </div>
                                                    </div>
                                                ))}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                                {winscpSelectedIds.size} / {winscpResult.servers.length} {t('settings.selected')}
                                            </div>
                                        </div>
                                    )}

                                    {/* SCP notice */}
                                    {winscpResult.servers.some(s => s.protocol === 'sftp') && (
                                        <div className="flex items-start gap-2 p-2.5 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                            <AlertCircle size={14} className="text-blue-500 mt-0.5 flex-shrink-0" />
                                            <div className="text-xs text-blue-700 dark:text-blue-300">
                                                {t('settings.winscpScpNotice')}
                                            </div>
                                        </div>
                                    )}

                                    {/* Skipped sessions */}
                                    {winscpResult.skipped.length > 0 && (
                                        <div>
                                            <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                                                {t('settings.winscpSkipped')} ({winscpResult.skipped.length})
                                            </div>
                                            <div className="text-xs text-gray-400 dark:text-gray-500 space-y-0.5">
                                                {winscpResult.skipped.map((s, i) => (
                                                    <div key={i} className="truncate">
                                                        <span className="font-medium">{s.name}</span>
                                                        <span className="mx-1">-</span>
                                                        <span>FSProtocol {s.rcloneType}</span>
                                                    </div>
                                                ))}
                                            </div>
                                        </div>
                                    )}
                                </>
                            )}

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                {winscpResult && winscpResult.servers.length > 0 && (
                                    <button
                                        onClick={handleWinscpConfirm}
                                        disabled={winscpSelectedIds.size === 0}
                                        className="flex-1 px-4 py-2 text-sm bg-purple-500 text-white rounded-lg hover:bg-purple-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                    >
                                        <Upload size={16} />
                                        {t('settings.winscpImportSelected').replace('{count}', String(winscpSelectedIds.size))}
                                    </button>
                                )}
                            </div>
                        </div>
                    ) : mode === 'winscp-export' ? (
                        /* ---- WinSCP Export Mode ---- */
                        <div className="space-y-4">
                            <div className="flex items-start gap-2 p-3 bg-purple-50 dark:bg-purple-900/20 border border-purple-200 dark:border-purple-800 rounded-lg">
                                <FolderInput size={16} className="text-purple-600 dark:text-purple-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-purple-700 dark:text-purple-300">
                                    {t('settings.winscpExportNotice')}
                                </div>
                            </div>

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
                                        <div
                                            key={server.id}
                                            className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0"
                                            onClick={() => toggleServer(server.id)}
                                        >
                                            <Checkbox
                                                checked={selectedServerIds.has(server.id)}
                                                onChange={() => toggleServer(server.id)}
                                            />
                                            <div
                                                className="w-2 h-2 rounded-full flex-shrink-0"
                                                style={{ backgroundColor: server.color || '#6B7280' }}
                                            />
                                            <div className="min-w-0 flex-1">
                                                <div className="text-sm font-medium truncate">{server.name}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                    {server.host}:{server.port} - {server.username}
                                                </div>
                                            </div>
                                            <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">
                                                {server.protocol || 'ftp'}
                                            </span>
                                        </div>
                                    ))}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                    {selectedServerIds.size} / {servers.length} {t('settings.selected')}
                                </div>
                            </div>

                            <div className="flex items-center gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg">
                                <Checkbox
                                    checked={includeCredentials}
                                    onChange={setIncludeCredentials}
                                    label={
                                        <div>
                                            <div className="text-sm font-medium flex items-center gap-1">
                                                <Lock size={14} />
                                                {t('settings.includeCredentials')}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400">
                                                {t('settings.winscpExportCredHint')}
                                            </div>
                                        </div>
                                    }
                                />
                            </div>

                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                <button
                                    onClick={handleWinscpExport}
                                    disabled={loading || noneSelected}
                                    className="flex-1 px-4 py-2 text-sm bg-purple-500 text-white rounded-lg hover:bg-purple-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                >
                                    {loading ? (
                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                    ) : (
                                        <Download size={16} />
                                    )}
                                    {loading ? t('settings.exporting') : t('settings.winscpExportButton').replace('{count}', String(selectedServerIds.size))}
                                </button>
                            </div>
                        </div>
                    ) : mode === 'filezilla' ? (
                        /* ---- FileZilla Import Mode ---- */
                        <div className="space-y-4">
                            <div className="flex items-start gap-2 p-3 bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 rounded-lg">
                                <Shield size={16} className="text-green-600 dark:text-green-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-green-700 dark:text-green-300">
                                    {t('settings.filezillaSecurityUpgrade')}
                                </div>
                            </div>

                            {!filezillaResult ? (
                                <>
                                    {filezillaDetectedPath === null ? (
                                        <div className="flex items-center justify-center py-6">
                                            <RefreshCw size={20} className="animate-spin text-gray-400" />
                                            <span className="ml-2 text-sm text-gray-500">{t('settings.filezillaDetecting')}</span>
                                        </div>
                                    ) : filezillaDetectedPath ? (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 rounded-lg">
                                                <div className="text-xs text-gray-500 dark:text-gray-400 mb-1">{t('settings.filezillaConfigFound')}</div>
                                                <div className="text-sm font-mono truncate" title={filezillaDetectedPath}>{filezillaDetectedPath}</div>
                                            </div>
                                            <div className="flex gap-2">
                                                <button onClick={() => handleFilezillaScan()} disabled={loading} className="flex-1 px-4 py-2 text-sm bg-emerald-500 text-white rounded-lg hover:bg-emerald-600 disabled:opacity-50 flex items-center justify-center gap-2">
                                                    {loading ? <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" /> : <FolderInput size={16} />}
                                                    {loading ? t('settings.filezillaScanning') : t('settings.filezillaScanConfig')}
                                                </button>
                                                <button onClick={handleFilezillaBrowse} disabled={loading} className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700">{t('settings.filezillaBrowse')}</button>
                                            </div>
                                        </div>
                                    ) : (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                                <div className="text-sm text-blue-700 dark:text-blue-300 flex items-center gap-2"><AlertCircle size={14} />{t('settings.filezillaNotFound')}</div>
                                                <div className="text-xs text-blue-600 dark:text-blue-400 mt-1">{t('settings.filezillaNotFoundHint')}</div>
                                            </div>
                                            <button onClick={handleFilezillaBrowse} disabled={loading} className="w-full px-4 py-2 text-sm bg-emerald-500 text-white rounded-lg hover:bg-emerald-600 disabled:opacity-50 flex items-center justify-center gap-2">
                                                <FolderInput size={16} />{t('settings.filezillaBrowse')}
                                            </button>
                                        </div>
                                    )}
                                </>
                            ) : (
                                <>
                                    <div className="text-sm text-gray-600 dark:text-gray-300">
                                        {t('settings.filezillaFound').replace('{total}', String(filezillaResult.totalRemotes)).replace('{supported}', String(filezillaResult.servers.length))}
                                    </div>
                                    {filezillaResult.servers.length > 0 && (
                                        <div>
                                            <div className="flex items-center justify-between mb-2">
                                                <span className="text-sm font-medium text-gray-700 dark:text-gray-300">{t('settings.filezillaSelectSites')}</span>
                                                <button onClick={toggleAllFilezilla} className="text-xs text-blue-500 hover:text-blue-600 font-medium">
                                                    {filezillaSelectedIds.size === filezillaResult.servers.length ? t('settings.deselectAll') : t('settings.selectAll')}
                                                </button>
                                            </div>
                                            <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                                {filezillaResult.servers.map((server) => (
                                                    <div key={server.id} className={`flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0 ${existingServerKeys.has(`${server.host}:${server.port}:${server.username}`) ? 'opacity-50' : ''}`} onClick={() => toggleFilezillaServer(server.id)}>
                                                        <Checkbox checked={filezillaSelectedIds.has(server.id)} onChange={() => toggleFilezillaServer(server.id)} />
                                                        <div className="min-w-0 flex-1">
                                                            <div className="text-sm font-medium truncate flex items-center gap-1.5">
                                                                {server.name}
                                                                {existingServerKeys.has(`${server.host}:${server.port}:${server.username}`) && (
                                                                    <span className="text-[9px] px-1.5 py-0.5 rounded bg-amber-100 dark:bg-amber-900/30 text-amber-700 dark:text-amber-400 font-medium whitespace-nowrap">{t('settings.alreadyExists')}</span>
                                                                )}
                                                            </div>
                                                            <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                                {server.host}{server.port !== 443 ? `:${server.port}` : ''}{server.username ? ` - ${server.username}` : ''}
                                                            </div>
                                                        </div>
                                                        <div className="flex items-center gap-1.5 flex-shrink-0">
                                                            {server.hasStoredCredential && <Lock size={12} className="text-green-500" />}
                                                            <span className="text-[10px] text-gray-400 uppercase">{protocolLabel(server.protocol)}</span>
                                                        </div>
                                                    </div>
                                                ))}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">{filezillaSelectedIds.size} / {filezillaResult.servers.length} {t('settings.selected')}</div>
                                        </div>
                                    )}
                                    {filezillaResult.skipped.length > 0 && (
                                        <div>
                                            <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('settings.filezillaSkipped')} ({filezillaResult.skipped.length})</div>
                                            <div className="text-xs text-gray-400 dark:text-gray-500 space-y-0.5">
                                                {filezillaResult.skipped.map((s, i) => (
                                                    <div key={i} className="truncate"><span className="font-medium">{s.name}</span><span className="mx-1">-</span><span>Protocol {s.rcloneType}</span></div>
                                                ))}
                                            </div>
                                        </div>
                                    )}
                                </>
                            )}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}
                            <div className="flex gap-2">
                                <button onClick={resetMode} className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700">{t('common.back')}</button>
                                {filezillaResult && filezillaResult.servers.length > 0 && (
                                    <button onClick={handleFilezillaConfirm} disabled={filezillaSelectedIds.size === 0} className="flex-1 px-4 py-2 text-sm bg-emerald-500 text-white rounded-lg hover:bg-emerald-600 disabled:opacity-50 flex items-center justify-center gap-2">
                                        <Upload size={16} />{t('settings.filezillaImportSelected').replace('{count}', String(filezillaSelectedIds.size))}
                                    </button>
                                )}
                            </div>
                        </div>
                    ) : mode === 'filezilla-export' ? (
                        /* ---- FileZilla Export Mode ---- */
                        <div className="space-y-4">
                            <div className="flex items-start gap-2 p-3 bg-emerald-50 dark:bg-emerald-900/20 border border-emerald-200 dark:border-emerald-800 rounded-lg">
                                <FolderInput size={16} className="text-emerald-600 dark:text-emerald-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-emerald-700 dark:text-emerald-300">{t('settings.filezillaExportNotice')}</div>
                            </div>
                            <div>
                                <div className="flex items-center justify-between mb-2">
                                    <span className="text-sm font-medium text-gray-700 dark:text-gray-300">{t('settings.selectServersToExport')}</span>
                                    <button onClick={toggleAll} className="text-xs text-blue-500 hover:text-blue-600 font-medium">{allSelected ? t('settings.deselectAll') : t('settings.selectAll')}</button>
                                </div>
                                <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                    {servers.map((server) => (
                                        <div key={server.id} className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0" onClick={() => toggleServer(server.id)}>
                                            <Checkbox checked={selectedServerIds.has(server.id)} onChange={() => toggleServer(server.id)} />
                                            <div className="w-2 h-2 rounded-full flex-shrink-0" style={{ backgroundColor: server.color || '#6B7280' }} />
                                            <div className="min-w-0 flex-1">
                                                <div className="text-sm font-medium truncate">{server.name}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">{server.host}:{server.port} - {server.username}</div>
                                            </div>
                                            <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">{server.protocol || 'ftp'}</span>
                                        </div>
                                    ))}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">{selectedServerIds.size} / {servers.length} {t('settings.selected')}</div>
                            </div>
                            <div className="flex items-center gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg">
                                <Checkbox checked={includeCredentials} onChange={setIncludeCredentials} label={<div><div className="text-sm font-medium flex items-center gap-1"><Lock size={14} />{t('settings.includeCredentials')}</div><div className="text-xs text-gray-500 dark:text-gray-400">{t('settings.filezillaExportCredHint')}</div></div>} />
                            </div>
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}
                            <div className="flex gap-2">
                                <button onClick={resetMode} className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700">{t('common.back')}</button>
                                <button onClick={handleFilezillaExport} disabled={loading || noneSelected} className="flex-1 px-4 py-2 text-sm bg-emerald-500 text-white rounded-lg hover:bg-emerald-600 disabled:opacity-50 flex items-center justify-center gap-2">
                                    {loading ? <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" /> : <Download size={16} />}
                                    {loading ? t('settings.exporting') : t('settings.filezillaExportButton').replace('{count}', String(selectedServerIds.size))}
                                </button>
                            </div>
                        </div>
                    ) : (mode === 'rclone-export') ? (
                        /* ---- Rclone Export Mode ---- */
                        <div className="space-y-4">
                            {/* Info notice */}
                            <div className="flex items-start gap-2 p-3 bg-orange-50 dark:bg-orange-900/20 border border-orange-200 dark:border-orange-800 rounded-lg">
                                <FolderInput size={16} className="text-orange-600 dark:text-orange-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-orange-700 dark:text-orange-300">
                                    {t('settings.rcloneExportNotice')}
                                </div>
                            </div>

                            {/* Server selection (reuse selectedServerIds) */}
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
                                        <div
                                            key={server.id}
                                            className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0"
                                            onClick={() => toggleServer(server.id)}
                                        >
                                            <Checkbox
                                                checked={selectedServerIds.has(server.id)}
                                                onChange={() => toggleServer(server.id)}
                                            />
                                            <div
                                                className="w-2 h-2 rounded-full flex-shrink-0"
                                                style={{ backgroundColor: server.color || '#6B7280' }}
                                            />
                                            <div className="min-w-0 flex-1">
                                                <div className="text-sm font-medium truncate">{server.name}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                    {server.host}:{server.port}: {server.username}
                                                </div>
                                            </div>
                                            <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">
                                                {server.protocol || 'ftp'}
                                            </span>
                                        </div>
                                    ))}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                    {selectedServerIds.size} / {servers.length} {t('settings.selected')}
                                </div>
                            </div>

                            {/* Include credentials toggle */}
                            <div className="flex items-center gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg">
                                <Checkbox
                                    checked={includeCredentials}
                                    onChange={setIncludeCredentials}
                                    label={
                                        <div>
                                            <div className="text-sm font-medium flex items-center gap-1">
                                                <Lock size={14} />
                                                {t('settings.includeCredentials')}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400">
                                                {t('settings.rcloneExportCredHint')}
                                            </div>
                                        </div>
                                    }
                                />
                            </div>

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                <button
                                    onClick={handleRcloneExport}
                                    disabled={loading || noneSelected}
                                    className="flex-1 px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                >
                                    {loading ? (
                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                    ) : (
                                        <Download size={16} />
                                    )}
                                    {loading ? t('settings.exporting') : t('settings.rcloneExportButton').replace('{count}', String(selectedServerIds.size))}
                                </button>
                            </div>
                        </div>
                    ) : null}
                </div>
            </div>
        </div>
    );
};

export default ExportImportDialog;
