// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import { Eye, EyeOff, Loader2, ChevronDown, FolderOpen, File as FileIcon, X, FolderPlus, FilePlus, Lock, Unlock, RotateCcw } from 'lucide-react';
import { TransferProgressBar } from '../TransferProgressBar';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, SecurityLevel, VaultV3CompressionProfile } from './useVaultState';
import { PasswordStrengthBar } from './PasswordStrengthBar';
import { PasswordMatchHint } from '../common/PasswordMatchHint';
import { CompressionEstimateBar } from '../common/CompressionEstimateBar';
import { formatSize } from '../../utils/formatters';

/** AeroVault compression profile -> zstd level used by the canary estimator. */
const PROFILE_ZSTD_LEVEL: Record<VaultV3CompressionProfile, number> = {
    fast: 3,
    balanced: 9,
    archive: 15,
};

interface VaultCreateProps {
    state: VaultState;
}

export const VaultCreate: React.FC<VaultCreateProps> = ({ state }) => {
    const t = useTranslation();
    const isZip = state.isPlaintextZip;
    const firstFieldRef = useRef<HTMLInputElement>(null);

    // Focus the first text field when the create form opens, so the user can type
    // straight away instead of clicking it first.
    useEffect(() => {
        const id = window.setTimeout(() => firstFieldRef.current?.focus(), 50);
        return () => window.clearTimeout(id);
    }, []);
    const availableSecurityLevels = Object.keys(securityLevels) as SecurityLevel[];
    const compressionProfiles: { id: VaultV3CompressionProfile; label: string; detail: string }[] = [
        { id: 'fast', label: 'Fast', detail: 'zstd -3' },
        { id: 'balanced', label: 'Balanced', detail: 'zstd -9' },
        { id: 'archive', label: 'Archive', detail: 'zstd -15' },
    ];

    // Real (canary) size estimate for the .aerozip output, recomputed (debounced)
    // whenever the input or the compression profile changes. The backend samples
    // and compresses with the real zstd level, then extrapolates.
    const inputPaths = state.initialFolderPath
        ? [state.initialFolderPath]
        : [...state.stagedDirs, ...state.stagedFiles];
    const inputKey = JSON.stringify(inputPaths);
    const [zipEstimate, setZipEstimate] = useState<{ original: number; estimated: number; exact: boolean } | null>(null);
    const [zipEstimateLoading, setZipEstimateLoading] = useState(false);
    useEffect(() => {
        if (!isZip || inputPaths.length === 0) { setZipEstimate(null); return; }
        let cancelled = false;
        setZipEstimateLoading(true);
        const handle = setTimeout(async () => {
            try {
                const r = await invoke<{ input_bytes: number; estimated_bytes: number; exact: boolean }>(
                    'estimate_compressed_size',
                    { paths: inputPaths, codec: 'zstd', level: PROFILE_ZSTD_LEVEL[state.compressionProfile] },
                );
                if (!cancelled) setZipEstimate({ original: r.input_bytes, estimated: r.estimated_bytes, exact: r.exact });
            } catch {
                if (!cancelled) setZipEstimate(null);
            } finally {
                if (!cancelled) setZipEstimateLoading(false);
            }
        }, 250);
        return () => { cancelled = true; clearTimeout(handle); };
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [isZip, inputKey, state.compressionProfile]);

    // .aerozip ships with recovery parity ON by default (behaviour unchanged); the
    // user can opt out via the checkbox to maximise compression ratio. The encrypted
    // dialog keeps its own default (off), so only flip it on when entering zip mode.
    useEffect(() => {
        if (isZip) state.setErrorCorrectionEnabled(true);
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [isZip]);

    // Ehud #8: mixed file/folder staging via two pickers feeding one list (Tauri's
    // dialog cannot pick files AND folders in a single dialog). Combined with the
    // create-screen drag&drop (Ehud #2) this gives a regular zipping experience.
    const stageFiles = async () => {
        const sel = await open({ multiple: true });
        if (!sel) return;
        await state.handleStageDrop(Array.isArray(sel) ? (sel as string[]) : [sel as string]);
    };
    const stageFolders = async () => {
        const sel = await open({ directory: true, multiple: true });
        if (!sel) return;
        await state.handleStageDrop(Array.isArray(sel) ? (sel as string[]) : [sel as string]);
    };

    const stagedCount = state.stagedFiles.length + state.stagedDirs.length;

    // Restore every create-form control to its initial default.
    const resetToDefaults = () => {
        state.setSecurityLevel('advanced');
        state.setCompressionProfile(isZip ? 'archive' : 'balanced');
        state.setErrorCorrectionEnabled(isZip);
        state.setRecoveryPlacement('embedded');
        state.setErrorCorrectionPct(20);
        state.setDescription('');
    };

    return (
        <div className="p-4 flex flex-col gap-3">
            {/* Staged contents: files + folders to include in the new vault.
                Seeded from the opening selection, grown by drag&drop (Ehud #2)
                and the Add files / Add folders pickers (Ehud #8). */}
            {!state.initialFolderPath && (
                <div className={`px-3 py-2 rounded text-xs border ${state.dragOver
                    ? 'bg-emerald-100 dark:bg-emerald-800/40 border-emerald-400 dark:border-emerald-600'
                    : 'bg-emerald-50 dark:bg-emerald-900/20 border-emerald-200 dark:border-emerald-800'} text-emerald-700 dark:text-emerald-300 transition-colors`}>
                    {stagedCount > 0 ? (
                        <>
                            <div className="font-medium mb-1">
                                {stagedCount} {stagedCount === 1 ? t('vault.itemSingular') : t('vault.itemPlural')}
                            </div>
                            <div className="flex flex-col gap-0.5 max-h-28 overflow-y-auto pr-1">
                                {state.stagedDirs.map(d => (
                                    <div key={d} className="flex items-center gap-1.5 group">
                                        <FolderOpen size={11} className="shrink-0" />
                                        <span className="truncate flex-1">{d.replace(/[\\/]+$/, '').split(/[\\/]/).pop()}</span>
                                        <button onClick={() => state.removeStagedDir(d)} aria-label={t('vault.remove')}
                                            className="opacity-0 group-hover:opacity-100 hover:text-red-500 transition-opacity">
                                            <X size={11} />
                                        </button>
                                    </div>
                                ))}
                                {state.stagedFiles.map(f => (
                                    <div key={f} className="flex items-center gap-1.5 group">
                                        <FileIcon size={11} className="shrink-0" />
                                        <span className="truncate flex-1">{f.split(/[\\/]/).pop()}</span>
                                        <button onClick={() => state.removeStagedFile(f)} aria-label={t('vault.remove')}
                                            className="opacity-0 group-hover:opacity-100 hover:text-red-500 transition-opacity">
                                            <X size={11} />
                                        </button>
                                    </div>
                                ))}
                            </div>
                        </>
                    ) : (
                        <div className="text-[11px] text-emerald-600/80 dark:text-emerald-400/80">{t('vault.dropFilesHint')}</div>
                    )}
                    <div className="flex gap-2 mt-2">
                        <button onClick={stageFiles}
                            className="flex items-center gap-1 px-2 py-1 rounded border border-emerald-300 dark:border-emerald-700 hover:bg-emerald-100 dark:hover:bg-emerald-800/40">
                            <FilePlus size={12} /> {t('vault.addFiles')}
                        </button>
                        <button onClick={stageFolders}
                            className="flex items-center gap-1 px-2 py-1 rounded border border-emerald-300 dark:border-emerald-700 hover:bg-emerald-100 dark:hover:bg-emerald-800/40">
                            <FolderPlus size={12} /> {t('vault.addFolder')}
                        </button>
                    </div>
                </div>
            )}

            {/* Folder mode banner */}
            {state.initialFolderPath && (
                <div className="px-3 py-2 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded text-xs text-amber-700 dark:text-amber-300">
                    <div className="flex items-center gap-2">
                        <FolderOpen size={14} className="shrink-0" />
                        <span className="font-medium truncate">{state.initialFolderPath.split('/').pop()}</span>
                    </div>
                    {state.folderScanResult ? (
                        <p className="mt-1 text-[11px]">
                            {state.folderScanResult.file_count} files in {state.folderScanResult.dir_count} directories ({formatSize(state.folderScanResult.total_size)})
                        </p>
                    ) : (
                        <div className="mt-1 flex items-center gap-1.5 text-[11px]">
                            <Loader2 size={10} className="animate-spin" />
                            Scanning folder...
                        </div>
                    )}
                </div>
            )}

            {/* Vault / Archive name (Ehud #322 follow-up D): a required name that
                drives the saved filename, shown for every security level AND the
                .aerozip archive. Mirrors the Compressor's "Archive Name" field
                (label + extension suffix). Autofocused on open; Create stays
                disabled until it is non-empty. The value is also stored as the
                v1/v2 `description` metadata. */}
            <label className="text-sm text-gray-500 dark:text-gray-400">
                {isZip ? t('compress.archiveName') : t('vault.vaultName')}
                <span className="text-red-400 ml-0.5">*</span>
            </label>
            <div className="flex gap-2 items-center">
                <input ref={firstFieldRef} value={state.description} onChange={e => state.setDescription(e.target.value)}
                    className="flex-1 bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm"
                    placeholder={isZip ? 'My archive' : 'My secure vault'} />
                <span className="text-xs font-mono text-gray-500 dark:text-gray-400 whitespace-nowrap">{isZip ? '.aerozip' : '.aerovault'}</span>
            </div>

            {isZip && (
                <>
                    <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.compressionProfile')}</label>
                    <div className="grid grid-cols-3 gap-2">
                        {compressionProfiles.map((profile) => {
                            const selected = state.compressionProfile === profile.id;
                            return (
                                <button
                                    key={profile.id}
                                    onClick={() => state.setCompressionProfile(profile.id)}
                                    className={`rounded border px-3 py-2 text-left ${selected
                                        ? 'border-amber-500 bg-amber-500/10 text-amber-700 dark:text-amber-300'
                                        : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800 hover:border-amber-500 dark:hover:border-amber-500 hover:bg-amber-500/20 dark:hover:bg-amber-500/20 hover:text-amber-700 dark:hover:text-amber-300'} transition-colors cursor-pointer`}
                                >
                                    <div className="text-sm font-medium">{profile.label}</div>
                                    <div className="text-[11px] text-gray-500 dark:text-gray-400">{profile.detail}</div>
                                </button>
                            );
                        })}
                    </div>

                    {/* Recovery (Reed-Solomon parity) is opt-out for .aerozip: ON by
                        default so existing behaviour is unchanged, but disablable so the
                        archive can match canonical compression ratios. When unchecked we
                        send pct=0 and the backend creates a plain archive with no parity. */}
                    <div className="mt-2 flex items-center gap-2">
                        <input
                            type="checkbox"
                            id="zip-ecc-enabled"
                            checked={state.errorCorrectionEnabled}
                            onChange={e => state.setErrorCorrectionEnabled(e.target.checked)}
                            className="accent-amber-600"
                        />
                        <label htmlFor="zip-ecc-enabled" className="text-sm text-gray-500 dark:text-gray-400 cursor-pointer">
                            {t('vault.enableErrorCorrection')}
                        </label>
                    </div>
                    {state.errorCorrectionEnabled && (
                        <>
                            <label className="text-[11px] text-gray-500 dark:text-gray-400 mt-1">
                                {t('vault.recoveryLevel')}
                            </label>
                            <div className="grid grid-cols-4 gap-1.5">
                                {([
                                    { id: 7, label: t('vault.recoveryLevelLow') },
                                    { id: 15, label: t('vault.recoveryLevelMedium') },
                                    { id: 25, label: t('vault.recoveryLevelQuartile') },
                                    { id: 30, label: t('vault.recoveryLevelHigh') },
                                ] as const).map(lvl => {
                                    const selected = state.errorCorrectionPct === lvl.id;
                                    return (
                                        <button
                                            key={lvl.id}
                                            onClick={() => state.setErrorCorrectionPct(lvl.id)}
                                            className={`rounded border px-1.5 py-1 text-center ${selected
                                                ? 'border-amber-500 bg-amber-500/10 text-amber-700 dark:text-amber-300'
                                                : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800 hover:border-amber-500 dark:hover:border-amber-500 hover:bg-amber-500/20 dark:hover:bg-amber-500/20 hover:text-amber-700 dark:hover:text-amber-300'} transition-colors cursor-pointer`}
                                        >
                                            <div className="text-[11px] font-medium">{lvl.label}</div>
                                            <div className="text-[10px] text-gray-500 dark:text-gray-400">~{lvl.id}%</div>
                                        </button>
                                    );
                                })}
                            </div>
                            <div className="flex items-center gap-2">
                                <input
                                    type="range"
                                    min={5}
                                    max={50}
                                    step={1}
                                    value={state.errorCorrectionPct}
                                    onChange={e => state.setErrorCorrectionPct(Number(e.target.value))}
                                    className="flex-1 accent-amber-600"
                                    aria-label={t('vault.recoveryLevel')}
                                />
                                <div className="flex items-center gap-1">
                                    <input
                                        type="number"
                                        min={5}
                                        max={50}
                                        value={state.errorCorrectionPct}
                                        onChange={e => state.setErrorCorrectionPct(Math.min(50, Math.max(5, Math.round(Number(e.target.value) || 5))))}
                                        className="w-14 bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-1.5 py-0.5 text-[12px] text-right"
                                    />
                                    <span className="text-[11px] text-gray-500 dark:text-gray-400">%</span>
                                </div>
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('vault.zipRecoveryHint')}
                            </div>
                        </>
                    )}
                    {inputPaths.length > 0 && (zipEstimate || zipEstimateLoading) && (
                        <div className="mt-1">
                            <CompressionEstimateBar
                                originalBytes={zipEstimate?.original ?? 0}
                                estimatedBytes={zipEstimate?.estimated ?? 0}
                                parityBytes={state.errorCorrectionEnabled && zipEstimate ? Math.round(zipEstimate.estimated * state.errorCorrectionPct / 100) : 0}
                                exact={zipEstimate?.exact ?? false}
                                loading={zipEstimateLoading && !zipEstimate}
                            />
                        </div>
                    )}
                </>
            )}

            {!isZip && (
                <>
            {/* Security Level Selector */}
            <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.securityLevel')}</label>
            <div className="relative">
                <button
                    onClick={() => state.setShowLevelDropdown(!state.showLevelDropdown)}
                    className={`w-full flex items-center justify-between px-3 py-2.5 rounded border ${securityLevels[state.securityLevel].borderColor} bg-gray-50 dark:bg-gray-800 text-left hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors`}
                >
                    <div className="flex items-center gap-2">
                        {React.createElement(securityLevels[state.securityLevel].icon, {
                            size: 16,
                            className: securityLevels[state.securityLevel].color
                        })}
                        <div>
                            <div className={`text-sm font-medium ${securityLevels[state.securityLevel].color}`}>
                                {securityLevels[state.securityLevel].label}
                                {state.securityLevel === 'advanced' && <span className="ml-2 text-xs text-emerald-300">({t('vault.securityRecommended')})</span>}
                            </div>
                            <div className="text-xs text-gray-500">{securityLevels[state.securityLevel].description}</div>
                        </div>
                    </div>
                    <ChevronDown size={16} className="text-gray-500 dark:text-gray-400" />
                </button>

                {/* Dropdown */}
                {state.showLevelDropdown && (
                    <div className="absolute z-10 mt-1 w-full bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-600 rounded-lg shadow-xl overflow-hidden">
                        {availableSecurityLevels.map((level) => {
                            const config = securityLevels[level];
                            const Icon = config.icon;
                            const isSelected = level === state.securityLevel;
                            return (
                                <button
                                    key={level}
                                    onClick={() => { state.setSecurityLevel(level); state.setShowLevelDropdown(false); }}
                                    className={`w-full flex items-start gap-3 px-3 py-3 text-left transition-colors hover:bg-gray-100 dark:hover:bg-gray-700 ${isSelected ? 'bg-gray-100 dark:bg-gray-700/60' : ''}`}
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

            {state.securityLevel === 'experimental' && (
                <>
                    <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.compressionProfile')}</label>
                    <div className="grid grid-cols-3 gap-2">
                        {compressionProfiles.map((profile) => {
                            const selected = state.compressionProfile === profile.id;
                            return (
                                <button
                                    key={profile.id}
                                    onClick={() => state.setCompressionProfile(profile.id)}
                                    className={`rounded border px-3 py-2 text-left ${selected
                                        ? 'border-amber-500 bg-amber-500/10 text-amber-300'
                                        : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800 hover:border-amber-500 dark:hover:border-amber-500 hover:bg-amber-500/20 dark:hover:bg-amber-500/20 hover:text-amber-700 dark:hover:text-amber-300'} transition-colors cursor-pointer`}
                                >
                                    <div className="text-sm font-medium">{profile.label}</div>
                                    <div className="text-[11px] text-gray-500 dark:text-gray-400">{profile.detail}</div>
                                </button>
                            );
                        })}
                    </div>

                    {/* Error Correction (Reed-Solomon) toggle for v3 vaults.
                        Uses dedicated backend create_with_error_correction (non-critical extension).
                        Enables scrub/repair actions and badge in the vault UI. */}
                    <div className="mt-2 flex items-center gap-2">
                        <input
                            type="checkbox"
                            id="ecc-enabled"
                            checked={state.errorCorrectionEnabled}
                            onChange={e => state.setErrorCorrectionEnabled(e.target.checked)}
                            className="accent-amber-600"
                        />
                        <label htmlFor="ecc-enabled" className="text-sm text-gray-500 dark:text-gray-400 cursor-pointer">
                            {t('vault.enableErrorCorrection')}
                        </label>
                    </div>
                    {state.errorCorrectionEnabled && (
                        <div className="pl-6 flex flex-col gap-2">
                            <div className="text-[11px] text-amber-600 dark:text-amber-400">
                                {t('vault.errorCorrectionDesc')}
                            </div>
                            <label className="text-[11px] text-gray-500 dark:text-gray-400">{t('vault.recoveryPlacement')}</label>
                            <div className="grid grid-cols-3 gap-2">
                                {([
                                    { id: 'embedded', label: t('vault.placementEmbedded'), detail: t('vault.placementEmbeddedDesc') },
                                    { id: 'detached', label: t('vault.placementDetached'), detail: t('vault.placementDetachedDesc') },
                                    { id: 'both', label: t('vault.placementBoth'), detail: t('vault.placementBothDesc') },
                                ] as const).map(p => {
                                    const selected = state.recoveryPlacement === p.id;
                                    return (
                                        <button
                                            key={p.id}
                                            onClick={() => state.setRecoveryPlacement(p.id)}
                                            className={`rounded border px-2 py-1.5 text-left ${selected
                                                ? 'border-amber-500 bg-amber-500/10 text-amber-300'
                                                : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800 hover:border-amber-500 dark:hover:border-amber-500 hover:bg-amber-500/20 dark:hover:bg-amber-500/20 hover:text-amber-700 dark:hover:text-amber-300'} transition-colors cursor-pointer`}
                                        >
                                            <div className="text-[12px] font-medium">{p.label}</div>
                                            <div className="text-[10px] text-gray-500 dark:text-gray-400">{p.detail}</div>
                                        </button>
                                    );
                                })}
                            </div>
                            {state.recoveryPlacement !== 'embedded' && (
                                <div className="text-[11px] text-emerald-600 dark:text-emerald-400">
                                    {t('vault.detachedStableStorageNote')}
                                </div>
                            )}
                            {/* QR-style overhead level (#276): named presets + slider + numeric input. */}
                            <label className="text-[11px] text-gray-500 dark:text-gray-400 mt-1">
                                {t('vault.recoveryLevel')}
                            </label>
                            <div className="grid grid-cols-4 gap-1.5">
                                {([
                                    { id: 7, label: t('vault.recoveryLevelLow') },
                                    { id: 15, label: t('vault.recoveryLevelMedium') },
                                    { id: 25, label: t('vault.recoveryLevelQuartile') },
                                    { id: 30, label: t('vault.recoveryLevelHigh') },
                                ] as const).map(lvl => {
                                    const selected = state.errorCorrectionPct === lvl.id;
                                    return (
                                        <button
                                            key={lvl.id}
                                            onClick={() => state.setErrorCorrectionPct(lvl.id)}
                                            className={`rounded border px-1.5 py-1 text-center ${selected
                                                ? 'border-amber-500 bg-amber-500/10 text-amber-300'
                                                : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800 hover:border-amber-500 dark:hover:border-amber-500 hover:bg-amber-500/20 dark:hover:bg-amber-500/20 hover:text-amber-700 dark:hover:text-amber-300'} transition-colors cursor-pointer`}
                                        >
                                            <div className="text-[11px] font-medium">{lvl.label}</div>
                                            <div className="text-[10px] text-gray-500 dark:text-gray-400">~{lvl.id}%</div>
                                        </button>
                                    );
                                })}
                            </div>
                            <div className="flex items-center gap-2">
                                <input
                                    type="range"
                                    min={5}
                                    max={50}
                                    step={1}
                                    value={state.errorCorrectionPct}
                                    onChange={e => state.setErrorCorrectionPct(Number(e.target.value))}
                                    className="flex-1 accent-amber-600"
                                    aria-label={t('vault.recoveryLevel')}
                                />
                                <div className="flex items-center gap-1">
                                    <input
                                        type="number"
                                        min={5}
                                        max={50}
                                        value={state.errorCorrectionPct}
                                        onChange={e => state.setErrorCorrectionPct(Math.min(50, Math.max(5, Math.round(Number(e.target.value) || 5))))}
                                        className="w-14 bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-1.5 py-0.5 text-[12px] text-right"
                                    />
                                    <span className="text-[11px] text-gray-500 dark:text-gray-400">%</span>
                                </div>
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('vault.recoveryLevelHint')}
                            </div>
                        </div>
                    )}
                </>
            )}

            <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.password')}</label>
            <div className="relative">
                <input type={state.showPassword ? 'text' : 'password'} value={state.password} onChange={e => state.setPassword(e.target.value)}
                    className="w-full bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm pr-8" />
                <button tabIndex={-1} onClick={() => state.setShowPassword(!state.showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400">
                    {state.showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
            </div>

            <PasswordStrengthBar password={state.password} />

            <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.confirmPassword')}</label>
            <input type={state.showPassword ? 'text' : 'password'} value={state.confirmPassword} onChange={e => state.setConfirmPassword(e.target.value)}
                className="bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm" />
            <PasswordMatchHint password={state.password} confirm={state.confirmPassword} />
                </>
            )}

            {/* Folder progress */}
            {state.folderProgress && (
                <div className="px-3 py-2 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded text-xs">
                    <div className="flex items-center justify-between mb-1">
                        <span className="text-blue-700 dark:text-blue-300">
                            {state.folderProgress.current} / {state.folderProgress.total}
                        </span>
                        <span className="text-blue-500 dark:text-blue-400 truncate ml-2 max-w-[200px]">
                            {state.folderProgress.current_file}
                        </span>
                    </div>
                    <div className="w-full bg-blue-200 dark:bg-blue-800 rounded-full h-1.5">
                        <div
                            className="bg-blue-500 h-1.5 rounded-full transition-all"
                            style={{ width: `${state.folderProgress.total > 0 ? (state.folderProgress.current / state.folderProgress.total) * 100 : 0}%` }}
                        />
                    </div>
                </div>
            )}

            {state.loading && state.vaultProgress && (
                <div className="mt-3 space-y-1">
                    <TransferProgressBar percentage={state.vaultProgress.percentage} size="lg" />
                    <div className="flex justify-between text-[11px] text-gray-500 dark:text-gray-400 tabular-nums">
                        <span>{formatSize(state.vaultProgress.transferred)} / {formatSize(state.vaultProgress.total)}</span>
                        <span>{state.vaultProgress.percentage}%</span>
                    </div>
                    {/* Inverse drain bar: starts full and empties in step with the
                        progress above (filled = input still to read), the byte
                        figure shrinking together with the bar. */}
                    {state.vaultProgress.total > 0 && (() => {
                        const remaining = Math.max(0, state.vaultProgress.total - state.vaultProgress.transferred);
                        const filled = Math.max(0, Math.min(100, (remaining / state.vaultProgress.total) * 100));
                        return (
                            <>
                                <div className="w-full h-2.5 rounded-full overflow-hidden bg-gray-200 dark:bg-gray-700">
                                    <div className="h-full rounded-full bg-amber-500 transition-all duration-300" style={{ width: `${filled}%` }} />
                                </div>
                                <div className="flex justify-end text-[11px] text-gray-500 dark:text-gray-400 tabular-nums">
                                    {formatSize(remaining)}
                                </div>
                            </>
                        );
                    })()}
                </div>
            )}

            <div className="flex gap-2 items-center mt-2">
                <button onClick={resetToDefaults} disabled={state.loading}
                    className="flex items-center gap-1 px-2 py-1.5 text-xs text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 rounded transition-colors disabled:opacity-50">
                    <RotateCcw size={13} /> {t('vault.resetDefaults')}
                </button>
                <div className="flex gap-2 ml-auto">
                    <button onClick={() => state.setMode('home')} className="px-3 py-1.5 text-sm hover:bg-gray-100 dark:hover:bg-gray-700 rounded transition-colors">
                        {t('vault.cancel')}
                    </button>
                    <button onClick={state.handleCreate} disabled={state.loading || !state.description.trim()} className={`flex items-center gap-2 px-4 py-1.5 ${isZip ? 'bg-amber-600' : securityLevels[state.securityLevel].bgColor} hover:opacity-90 rounded text-sm disabled:opacity-50 transition-opacity`}>
                        {state.loading ? <Loader2 size={14} className="animate-spin" /> : (isZip ? <Unlock size={14} /> : <Lock size={14} />)}
                        {t('vault.create')} {isZip ? '.aerozip' : '.aerovault'}
                    </button>
                </div>
            </div>
        </div>
    );
};
