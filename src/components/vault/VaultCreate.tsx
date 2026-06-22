// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { Eye, EyeOff, Loader2, ChevronDown, FolderOpen, File as FileIcon, X, FolderPlus, FilePlus } from 'lucide-react';
import { TransferProgressBar } from '../TransferProgressBar';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, SecurityLevel, VaultV3CompressionProfile } from './useVaultState';
import { PasswordStrengthBar } from './PasswordStrengthBar';
import { formatSize } from '../../utils/formatters';

interface VaultCreateProps {
    state: VaultState;
}

export const VaultCreate: React.FC<VaultCreateProps> = ({ state }) => {
    const t = useTranslation();
    const isZip = state.isPlaintextZip;
    const availableSecurityLevels = Object.keys(securityLevels) as SecurityLevel[];
    const compressionProfiles: { id: VaultV3CompressionProfile; label: string; detail: string }[] = [
        { id: 'fast', label: 'Fast', detail: 'zstd -3' },
        { id: 'balanced', label: 'Balanced', detail: 'zstd -9' },
        { id: 'archive', label: 'Archive', detail: 'zstd -15' },
    ];

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

            {isZip && (
                <>
                    <div className="px-3 py-2 rounded border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 text-xs text-amber-700 dark:text-amber-300">
                        <div className="font-medium">{t('vault.zipPlaintextTitle')}</div>
                        <div className="mt-0.5">{t('vault.zipPlaintextDesc')}</div>
                    </div>

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
                                        : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800'}`}
                                >
                                    <div className="text-sm font-medium">{profile.label}</div>
                                    <div className="text-[11px] text-gray-500 dark:text-gray-400">{profile.detail}</div>
                                </button>
                            );
                        })}
                    </div>

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
                                        : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800'}`}
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

            {!isZip && (
                <>
            {/* Security Level Selector */}
            <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.securityLevel')}</label>
            <div className="relative">
                <button
                    onClick={() => state.setShowLevelDropdown(!state.showLevelDropdown)}
                    className={`w-full flex items-center justify-between px-3 py-2.5 rounded border ${securityLevels[state.securityLevel].borderColor} bg-gray-50 dark:bg-gray-800 text-left`}
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
                                        : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800'}`}
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
                                                : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800'}`}
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
                                                : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800'}`}
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

            {state.securityLevel !== 'experimental' && (
                <>
                    <label className="text-sm text-gray-500 dark:text-gray-400 mt-2">{t('vault.description_label')}</label>
                    <input value={state.description} onChange={e => state.setDescription(e.target.value)}
                        className="bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm" placeholder="My secure vault" />
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
                </div>
            )}

            <div className="flex gap-2 justify-end mt-2">
                <button onClick={() => state.setMode('home')} className="px-3 py-1.5 text-sm hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                    {t('vault.cancel')}
                </button>
                <button onClick={state.handleCreate} disabled={state.loading} className={`flex items-center gap-2 px-4 py-1.5 ${isZip ? 'bg-amber-600' : securityLevels[state.securityLevel].bgColor} hover:opacity-90 rounded text-sm disabled:opacity-50`}>
                    {state.loading && <Loader2 size={14} className="animate-spin" />}
                    {isZip ? t('vault.createZip') : t('vault.create')}
                </button>
            </div>
        </div>
    );
};
