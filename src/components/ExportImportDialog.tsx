// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useMemo, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { pickFile, pickSave } from '../utils/pickPath';
import { Upload, Download, Shield, AlertCircle, CheckCircle2, X, Eye, EyeOff, Lock, FolderInput } from 'lucide-react';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { ServerProfile } from '../types';
import { useTranslation } from '../i18n';
import { SearchBox } from './SearchBox';
import { Checkbox } from './ui/Checkbox';
import { BridgeSourcePanel } from './BridgeSourcePanel';
import { bridgeProfileKey } from './bridge/bridgeImportCommit';
import { BridgeSourceDescriptor, GENERIC_BRIDGE_SOURCES } from './bridge/bridgeSources';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useDetectedBridgeConfigs, shortenConfigPath, orderBridgeSourcesByDetection } from '../hooks/useDetectedBridgeConfigs';
import { ServerChecklistNoMatch, ServerChecklistSearch, ServerChecklistSummary, SelectShownButton, useServerChecklistFilter } from './ServerChecklistFilter';

interface ExportImportDialogProps {
    servers: ServerProfile[];
    /**
     * Store the imported profiles; those in `replaceIds` replace the stored
     * profile of the same id, and the write is refused unless the active
     * account is still `expectedUserId` (the one the secrets were restored into).
     */
    onImport: (servers: ServerProfile[], replaceIds?: ReadonlySet<string>, expectedUserId?: number | null) => void | Promise<void>;
    onClose: () => void;
    // Optional starting mode so callers (e.g. the Settings Backup table) can
    // open the dialog straight into a specific action. Undefined preserves the
    // existing landing-page behavior (My Servers export/import flow).
    initialMode?: 'export' | 'import' | 'bridge-import' | 'bridge-export';
    // Optional bridge-config file to import on open (AeroFile "Import to
    // AeroFTP"). When set, the dialog runs the same identify+route flow as a
    // drag-and-drop so it lands straight on that source's preview.
    initialBridgeFilePath?: string;
    // Profiles to select when the export step opens (My Servers context
    // menu "Export..."). Undefined selects every profile, as before.
    initialSelectedServerIds?: string[];
}

// The selection the export step starts from: the profiles a caller handed
// over, or every profile.
const defaultExportSelection = (servers: ServerProfile[], preselected?: string[]): Set<string> =>
    new Set(preselected ?? servers.map(s => s.id));

// A profile the backend stores, redacted (secret-presence flags, never a
// secret), with what the import did with it (`profile_import_plan`). An
// overwritten one is laid over the stored profile of its id when written
// (`appendImportedProfiles`).
type ImportedServer = ServerProfile & { importOutcome: 'added' | 'overwritten' | 'duplicated' };

/** What a `.aeroftp` import does with a profile whose id already exists. */
type ExistingProfileMode = 'skip' | 'overwrite' | 'duplicate';
// Each mode's label and the line that says what it does, as i18n keys.
const EXISTING_PROFILE_MODES: { mode: ExistingProfileMode; label: string; hint: string }[] = [
    { mode: 'skip', label: 'settings.importExistingSkip', hint: 'settings.importExistingSkipHint' },
    { mode: 'overwrite', label: 'settings.importExistingOverwrite', hint: 'settings.importExistingOverwriteHint' },
    { mode: 'duplicate', label: 'settings.importExistingDuplicate', hint: 'settings.importExistingDuplicateHint' },
];

interface ImportResult {
    // The profiles to store; the ones the import skipped are only named.
    servers: ImportedServer[];
    skipped: string[];
    similar: number;
    // Stored profiles whose secrets the file carried and the backend restored.
    secretsStored: number;
    // The account the file was compared with and the secrets restored into.
    userId: number | null;
    // Stored Jottacloud profiles that brought their rotating refresh chain,
    // named as in the file.
    jottaChains: { id: string; name: string }[];
    // Stored profiles whose custom OAuth app differs from the one kept here.
    oauthAppsKept: string[];
    // Profiles an Overwrite refused: the file points them at another server.
    targetChanged: string[];
    metadata: {
        exportDate: string;
        aeroftpVersion: string;
        serverCount: number;
        hasCredentials: boolean;
    };
}

/**
 * My Servers export and import: the encrypted `.aeroftp` backup (export with
 * or without credentials; import with Skip, Overwrite or Duplicate for a
 * profile that already exists) and the bridges to other clients' configs.
 */
export const ExportImportDialog: React.FC<ExportImportDialogProps> = ({ servers, onImport, onClose, initialMode, initialBridgeFilePath, initialSelectedServerIds }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [mode, setMode] = useState<'export' | 'import' | 'bridge-import' | 'bridge-export' | 'bridge-src' | null>(initialMode ?? null);
    // Generic bridge source (the 12 expansion sources routed through BridgeSourcePanel)
    const [bridgeSrc, setBridgeSrc] = useState<BridgeSourceDescriptor | null>(null);
    const [bridgeSrcDir, setBridgeSrcDir] = useState<'import' | 'export'>('import');
    // Filter for the bridge app picker (the list is long, so show ~5 + scroll)
    const [bridgeFilter, setBridgeFilter] = useState('');
    // A profile file dropped onto the dialog and identified by
    // bridge_identify; handed to BridgeSourcePanel to skip browse.
    const [bridgePresetPath, setBridgePresetPath] = useState<string | null>(null);
    // Which bridge tools are actually installed on this machine. Probed once,
    // and only while the import picker is on screen: the export picker offers
    // every tool regardless, since exporting does not need a config to exist.
    const { detected: detectedBridgeConfigs } = useDetectedBridgeConfigs(mode === 'bridge-import');
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [includeCredentials, setIncludeCredentials] = useState(true);
    const [showPassword, setShowPassword] = useState(false);
    const [existingMode, setExistingMode] = useState<ExistingProfileMode>('skip');
    // Shown after an import that copied a Jottacloud refresh chain, kept the
    // stored OAuth app over the file's, or refused an overwrite pointing a
    // profile at another server; it keeps the dialog open to be read.
    const [importWarning, setImportWarning] = useState<string | null>(null);
    // KeePassXC-style import flow: the .aeroftp file is chosen first and its
    // path is held here, then the decryption password is requested. Null
    // until a file has been selected.
    const [importFilePath, setImportFilePath] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    // The close scheduled after a successful import; a new import or leaving
    // the dialog cancels it, so it never hides a later attempt's result.
    const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    /** Cancel the close scheduled after a successful import, if any. */
    const cancelScheduledClose = () => {
        if (closeTimerRef.current !== null) {
            clearTimeout(closeTimerRef.current);
            closeTimerRef.current = null;
        }
    };
    useEffect(() => cancelScheduledClose, []);
    const [selectedServerIds, setSelectedServerIds] = useState<Set<string>>(
        () => defaultExportSelection(servers, initialSelectedServerIds),
    );
    // Callers load `servers` from the vault after mounting the dialog, so the
    // first render can see an empty or stale list. Until the user touches the
    // selection, follow the list as it arrives instead of keeping a
    // selection computed from nothing.
    const selectionTouchedRef = useRef(false);
    useEffect(() => {
        if (!selectionTouchedRef.current) {
            setSelectedServerIds(defaultExportSelection(servers, initialSelectedServerIds));
        }
    }, [servers, initialSelectedServerIds]);
    const updateSelection = (next: Set<string>) => {
        selectionTouchedRef.current = true;
        setSelectedServerIds(next);
    };

    const exportFilter = useServerChecklistFilter(servers);
    const allServerIds = useMemo(() => servers.map(s => s.id), [servers]);
    const shownServerIds = useMemo(() => exportFilter.visible.map(s => s.id), [exportFilter.visible]);

    // Selected profiles, filter or not: a row the filter hides stays in the
    // export (the summary under the list counts it).
    const selectedServers = useMemo(
        () => servers.filter(s => selectedServerIds.has(s.id)),
        [servers, selectedServerIds]
    );
    const noneSelected = selectedServers.length === 0;

    // Opened on a preselected profile: bring its row into view, since in a
    // long list it can sit far below the fold.
    const exportListRef = useRef<HTMLDivElement>(null);
    const preselectScrolledRef = useRef(false);
    useEffect(() => {
        if (mode !== 'export' || preselectScrolledRef.current || !initialSelectedServerIds?.length) return;
        const first = initialSelectedServerIds.find(id => servers.some(s => s.id === id));
        if (!first) return;
        preselectScrolledRef.current = true;
        const row = exportListRef.current?.querySelector<HTMLElement>(`[data-server-id="${CSS.escape(first)}"]`);
        row?.scrollIntoView({ block: 'nearest' });
    }, [mode, servers, initialSelectedServerIds]);

    // Pre-compute existing server keys for duplicate detection in import previews.
    // The parent (App / SettingsPanel / IntroHub) feeds `servers`
    // from the active user's partition via loadSavedServerProfiles, so a
    // separate localStorage read is no longer needed.
    const existingServerKeys = useMemo(
        () => new Set(servers.map(bridgeProfileKey)),
        [servers],
    );

    const toggleServer = (id: string) => {
        selectionTouchedRef.current = true;
        setSelectedServerIds(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    /**
     * Validate the password and the profile selection, choose where to save,
     * write the encrypted backup (with or without credentials), and close the
     * dialog shortly after success.
     */
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
        const filePath = await pickSave({
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
            cancelScheduledClose();
            closeTimerRef.current = setTimeout(() => onClose(), 2000);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    /**
     * Step 1 of the import: choose the .aeroftp file. The decryption password
     * is only requested once a file is loaded (KeePassXC pattern), so the user
     * sees what they are unlocking before typing a secret. Choosing another
     * file clears the last result, so Import is available for it.
     */
    const handleChooseImportFile = async () => {
        // Before the picker opens: the user may stay in it longer than the
        // close scheduled by a previous success.
        cancelScheduledClose();
        const filePath = await pickFile({
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
        setImportWarning(null);
        setPassword('');
        setImportFilePath(filePath);
    };

    /**
     * Step 2 of the import: decrypt the already-chosen file with the password,
     * let the backend decide each profile with `existingMode`, store the
     * result, and report it. The dialog then closes itself, or stays open on a
     * warning, or on an error that says what a failed write left.
     */
    const handleImport = async () => {
        const filePath = importFilePath;
        // Done once it succeeded (Enter in the password field as well as the
        // button): importing the same file again needs choosing it again.
        if (!filePath || loading || success !== null || importWarning !== null) {
            return;
        }
        if (password.length < 1) {
            setError(t('settings.passwordRequired'));
            return;
        }

        cancelScheduledClose();
        setLoading(true);
        setError(null);
        setImportWarning(null);
        try {
            const result = await invoke<ImportResult>('import_server_profiles', {
                filePath,
                password,
                existing: existingMode,
            });

            // The backend compared the file with the active user's profiles and
            // decided each one with `existingMode` (the CLI's profile-import
            // plans the same way), restoring secrets only for the profiles it
            // stores. A profile that only shares host:port:username with an
            // existing one is added and counted as similar.
            // Every field the backend returns is kept: share-link base, icons,
            // the Filen API key flag and the rest ride along with the profile.
            const imported: ServerProfile[] = result.servers.map(({ importOutcome: _outcome, ...profile }) => profile);
            const count = (outcome: ImportedServer['importOutcome']) =>
                result.servers.filter(s => s.importOutcome === outcome).length;
            const overwritten = count('overwritten');
            const duplicated = count('duplicated');
            try {
                await onImport(imported, new Set(
                    result.servers.filter(s => s.importOutcome === 'overwritten').map(s => s.id),
                ), result.userId);
            } catch (err) {
                // The backend restored the secrets the file carries before this
                // write: say for how many profiles, and that the same import
                // with the same option finishes it.
                setError(String(err) + (result.secretsStored > 0
                    ? ` ${t('settings.importNotSavedSecretsKept', { count: result.secretsStored })}`
                    : ''));
                return;
            }
            setSuccess(
                // "new servers": the overwritten ones replaced servers already there.
                t('settings.importSuccess').replace('{count}', String(imported.length - overwritten)) +
                (overwritten > 0 ? ` (${t('settings.overwrittenCount', { count: overwritten })})` : '') +
                (duplicated > 0 ? ` (${t('settings.duplicatedCount', { count: duplicated })})` : '') +
                (result.skipped.length > 0 ? ` (${t('settings.duplicatesSkippedCount', { count: result.skipped.length })})` : '') +
                (result.similar > 0 ? ` (${t('settings.similarToExistingCount', { count: result.similar })})` : '')
            );
            const warnings = [
                ...(result.jottaChains.length > 0
                    ? [t('settings.importJottaChainWarning', { names: result.jottaChains.map(c => c.name).join(', ') })]
                    : []),
                ...(result.oauthAppsKept.length > 0
                    ? [t('settings.importOauthAppKept', { names: result.oauthAppsKept.join(', ') })]
                    : []),
                ...(result.targetChanged.length > 0
                    ? [t('settings.importTargetChanged', { names: result.targetChanged.join(', ') })]
                    : []),
            ];
            if (warnings.length > 0) {
                setImportWarning(warnings.join(' '));
            } else {
                closeTimerRef.current = setTimeout(() => onClose(), 2500);
            }
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

    /** Back to the start screen: clear every field and result, cancel a pending close. */
    const resetMode = () => {
        cancelScheduledClose();
        setMode(null);
        setError(null);
        setSuccess(null);
        setPassword('');
        setConfirmPassword('');
        setImportFilePath(null);
        setExistingMode('skip');
        setImportWarning(null);
        setBridgeSrc(null);
        setBridgePresetPath(null);
        exportFilter.setQuery('');
    };

    // Route to a bridge source's import/export surface. Shared by the
    // source list click and by drag-and-drop. All 15 sources go through
    // BridgeSourcePanel and can receive the dropped file directly.
    const routeBridgeSource = useCallback(
        (s: BridgeSourceDescriptor, asImport: boolean, presetPath?: string) => {
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
                const matched = res.candidates
                    .map(id => GENERIC_BRIDGE_SOURCES.find(s => s.id === id))
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

    // Opened from a recognized bridge-config file (AeroFile "Import to
    // AeroFTP"): run the identify+route flow once, exactly like a drop.
    const initialBridgeHandledRef = useRef<string | null>(null);
    useEffect(() => {
        if (initialBridgeFilePath && initialBridgeHandledRef.current !== initialBridgeFilePath) {
            initialBridgeHandledRef.current = initialBridgeFilePath;
            void handleBridgeDrop(initialBridgeFilePath);
        }
    }, [initialBridgeFilePath, handleBridgeDrop]);

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={(e) => e.target === e.currentTarget && onClose()}>
            <div {...modalDrag.panelProps} className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[480px] max-h-[85vh] overflow-hidden animate-scale-in flex flex-col">
                {/* Header */}
                <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700 flex-shrink-0 cursor-grab active:cursor-grabbing">
                    <h3 className="text-lg font-semibold flex items-center gap-2">
                        <Shield size={20} className="text-blue-500" />
                        {mode === 'bridge-import' ? t('settings.bridgeImport')
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
                            const q = bridgeFilter.trim().toLowerCase();
                            const filtered = q
                                ? GENERIC_BRIDGE_SOURCES.filter(s => s.label.toLowerCase().includes(q))
                                : GENERIC_BRIDGE_SOURCES;
                            // Tools whose config was found on this machine float to the top,
                            // so the one you are migrating from is the first thing you see.
                            // Array.sort is stable, so the curated order survives inside
                            // each group. Import only: an export target does not need a
                            // config file to already exist.
                            const ordered = isImport
                                ? orderBridgeSourcesByDetection(filtered, detectedBridgeConfigs)
                                : filtered;
                            return (
                                <div className="space-y-3">
                                    <div className="text-sm text-gray-600 dark:text-gray-300">
                                        {isImport ? t('settings.bridgeSelectSource') : t('settings.bridgeSelectTarget')}
                                    </div>
                                    <SearchBox
                                        autoFocus
                                        value={bridgeFilter}
                                        onChange={setBridgeFilter}
                                        placeholder={t('settings.bridgeSearchApps')}
                                        iconSize={16}
                                        className="w-full pl-3 pr-3 py-2 text-sm rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 focus:outline-none focus:border-blue-400 dark:focus:border-blue-500"
                                    />
                                    <div className="space-y-2 max-h-[336px] overflow-y-auto pr-1">
                                        {ordered.length === 0 ? (
                                            <div className="py-6 text-center text-sm text-gray-400 dark:text-gray-500">{t('settings.bridgeNoApps')}</div>
                                        ) : ordered.map(s => {
                                            const foundPath = isImport ? detectedBridgeConfigs[s.id] : undefined;
                                            return (
                                            <button
                                                key={s.id}
                                                onClick={() => pick(s)}
                                                disabled={!isImport && servers.length === 0}
                                                title={foundPath}
                                                className={`w-full p-3 border rounded-lg hover:border-blue-400 dark:hover:border-blue-500 flex items-center gap-3 transition-colors disabled:opacity-50 ${foundPath ? 'border-green-300 dark:border-green-800' : 'border-gray-200 dark:border-gray-700'}`}
                                            >
                                                <div className={`w-9 h-9 rounded-lg ${s.accentBg} flex items-center justify-center flex-shrink-0`}>
                                                    <Icon size={18} className={s.accent} />
                                                </div>
                                                <div className="text-left min-w-0">
                                                    <div className="font-medium flex items-center gap-1.5">
                                                        {s.label}
                                                        {foundPath && <CheckCircle2 size={13} className="text-green-600 dark:text-green-400 flex-shrink-0" />}
                                                    </div>
                                                    {/* The path IS the message: it says the config exists and
                                                        where it is, in every language, with no string to
                                                        translate. Falls back to the generic blurb otherwise. */}
                                                    <div className={`text-xs truncate ${foundPath ? 'text-green-700 dark:text-green-400' : 'text-gray-500 dark:text-gray-400'}`}>
                                                        {foundPath
                                                            ? shortenConfigPath(foundPath)
                                                            : (isImport ? t('settings.bridgeGenericImportDesc') : t('settings.bridgeGenericExportDesc')).replace('{app}', s.label)}
                                                    </div>
                                                </div>
                                            </button>
                                            );
                                        })}
                                    </div>
                                    <div className="flex gap-2">
                                        <button onClick={() => { setBridgeFilter(''); resetMode(); }} className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700">{t('common.back')}</button>
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
                                    <SelectShownButton
                                        selected={selectedServerIds}
                                        shownIds={shownServerIds}
                                        filtering={exportFilter.filtering}
                                        onChange={updateSelection}
                                    />
                                </div>
                                <ServerChecklistSearch
                                    value={exportFilter.query}
                                    onChange={exportFilter.setQuery}
                                    autoFocus={!initialSelectedServerIds}
                                />
                                <div ref={exportListRef} className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                    {exportFilter.visible.length === 0 && exportFilter.filtering && (
                                        <ServerChecklistNoMatch query={exportFilter.query} />
                                    )}
                                    {exportFilter.visible.map((server) => (
                                        <div
                                            key={server.id}
                                            data-server-id={server.id}
                                            onClick={() => toggleServer(server.id)}
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
                                <ServerChecklistSummary
                                    selected={selectedServerIds}
                                    allIds={allServerIds}
                                    shownIds={shownServerIds}
                                    filtering={exportFilter.filtering}
                                />
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
                                    // Opened from a profile's "Export...": the selection
                                    // is already made, the password is the next step.
                                    autoFocus={!!initialSelectedServerIds}
                                    className="w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                                />
                                <button data-agent="deny"
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
                            <PasswordMatchHint password={password} confirm={confirmPassword} />

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
                                    {loading ? t('settings.exporting') : `${t('settings.exportServers')} (${selectedServers.length})`}
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
                                            disabled={loading}
                                            className="text-xs text-blue-500 hover:text-blue-600 disabled:opacity-50 font-medium flex-shrink-0"
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
                                        <button data-agent="deny"
                                            type="button"
                                            tabIndex={-1}
                                            onClick={() => setShowPassword(!showPassword)}
                                            className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600"
                                        >
                                            {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                        </button>
                                    </div>

                                    {/* What happens to a profile that already exists */}
                                    <div role="radiogroup" aria-label={t('settings.importExistingLabel')} className="space-y-1.5">
                                        <div className="text-xs font-medium text-gray-600 dark:text-gray-400">{t('settings.importExistingLabel')}</div>
                                        <div className="flex gap-1 p-1 rounded-lg bg-gray-100 dark:bg-gray-900/40">
                                            {EXISTING_PROFILE_MODES.map(({ mode: m, label }) => (
                                                <button
                                                    key={m}
                                                    type="button"
                                                    role="radio"
                                                    aria-checked={existingMode === m}
                                                    data-testid={`import-existing-${m}`}
                                                    onClick={() => setExistingMode(m)}
                                                    className={`flex-1 px-2 py-1 text-xs rounded-md transition-colors ${existingMode === m
                                                        ? 'bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 shadow-sm font-medium'
                                                        : 'text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200'}`}
                                                >
                                                    {t(label)}
                                                </button>
                                            ))}
                                        </div>
                                        <p className="text-xs text-gray-500 dark:text-gray-400">{t(EXISTING_PROFILE_MODES.find(o => o.mode === existingMode)!.hint)}</p>
                                    </div>
                                </>
                            )}

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}
                            {importWarning && <div data-testid="import-warning" className="text-amber-600 dark:text-amber-400 text-sm flex items-start gap-2"><AlertCircle size={14} className="mt-0.5 flex-shrink-0" />{importWarning}</div>}

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
                                        // Done once it succeeded: a second click would import the
                                        // file again (in Duplicate mode, another copy and
                                        // another holder of a Jottacloud chain).
                                        disabled={loading || password.length < 1 || success !== null || importWarning !== null}
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
                    ) : null}
                </div>
            </div>
        </div>
    );
};

export default ExportImportDialog;
