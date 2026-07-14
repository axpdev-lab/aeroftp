// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import { Eye, EyeOff, Loader2, ChevronDown, ChevronUp, SlidersHorizontal, FolderOpen, File as FileIcon, X, FolderPlus, FilePlus, Lock, Unlock, RotateCcw } from 'lucide-react';
import { TransferProgressBar } from '../TransferProgressBar';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, SecurityLevel, VaultV3CompressionProfile } from './useVaultState';
import { PasswordStrengthBar } from './PasswordStrengthBar';
import { InlinePasswordGenerator } from '../common/InlinePasswordGenerator';
import { PasswordMatchHint } from '../common/PasswordMatchHint';
import { CompressionEstimateBar } from '../common/CompressionEstimateBar';
import { formatSize } from '../../utils/formatters';
// Reuse the Compressor's scoped --compress-* theme so the AeroVault create
// surface reads as one Compressor-style flow (the owner's redesign, Phase 2).
import '../CompressDialog.css';

/** AeroVault compression profile -> zstd level used by the canary estimator. */
const PROFILE_ZSTD_LEVEL: Record<VaultV3CompressionProfile, number> = {
    fast: 3,
    balanced: 9,
    archive: 15,
};

/** Security levels offered on create (v1/Standard is open-only, dropped here). */
const CREATE_SECURITY_LEVELS: SecurityLevel[] = ['advanced', 'paranoid', 'experimental'];

interface VaultCreateProps {
    state: VaultState;
}

export const VaultCreate: React.FC<VaultCreateProps> = ({ state }) => {
    const t = useTranslation();
    const isZip = state.isPlaintextZip;
    const firstFieldRef = useRef<HTMLInputElement>(null);

    // Advanced disclosure: the security level (and its Error Correction options)
    // are folded away by default so the create surface fronts the 7z-like choice
    // (encrypted vs plaintext); the sane default (Advanced) needs no interaction.
    const [showSecurity, setShowSecurity] = useState(false);

    // Focus the name field when the create form opens, so the user can type
    // straight away instead of clicking it first.
    useEffect(() => {
        const id = window.setTimeout(() => firstFieldRef.current?.focus(), 50);
        return () => window.clearTimeout(id);
    }, []);

    // Labels are i18n (never hardcoded). The third profile is the MAXIMUM
    // compression level (zstd -15); it is NOT called "Archive" so it does not
    // collide with the encrypted vault's "Archive" mode (the v3 dedup archive).
    // The `id` stays `archive` because that is the backend profile token; only
    // the user-facing label changed. The detail (`zstd -N`) is a literal codec
    // flag, not translatable prose.
    const compressionProfiles: { id: VaultV3CompressionProfile; label: string; detail: string }[] = [
        { id: 'fast', label: t('compress.fast'), detail: 'zstd -3' },
        { id: 'balanced', label: t('compress.balanced'), detail: 'zstd -9' },
        { id: 'archive', label: t('compress.maximum'), detail: 'zstd -15' },
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

    // ── Compressor-themed sub-renderers (shared between encrypted/plaintext) ──

    const renderCompressionProfiles = () => (
        <div className="grid grid-cols-3 gap-2">
            {compressionProfiles.map((profile) => (
                <button
                    key={profile.id}
                    onClick={() => state.setCompressionProfile(profile.id)}
                    className={`compress-format-card ${state.compressionProfile === profile.id ? 'active' : ''} rounded-lg px-3 py-2 text-left transition-all`}
                >
                    <div className="text-sm font-medium">{profile.label}</div>
                    <div className="text-[11px]" style={{ color: 'var(--compress-text-muted)' }}>{profile.detail}</div>
                </button>
            ))}
        </div>
    );

    // Reed-Solomon overhead selector (named presets + slider), shared by the
    // plaintext-zip and encrypted-Archive Error Correction blocks.
    const renderRecoveryLevel = () => (
        <>
            <label className="text-[11px] mt-1 block" style={{ color: 'var(--compress-text-secondary)' }}>
                {t('vault.recoveryLevel')}
            </label>
            <div className="grid grid-cols-4 gap-1.5">
                {([
                    { id: 7, label: t('vault.recoveryLevelLow') },
                    { id: 15, label: t('vault.recoveryLevelMedium') },
                    { id: 25, label: t('vault.recoveryLevelQuartile') },
                    { id: 30, label: t('vault.recoveryLevelHigh') },
                ] as const).map(lvl => (
                    <button
                        key={lvl.id}
                        onClick={() => state.setErrorCorrectionPct(lvl.id)}
                        className={`compress-format-card ${state.errorCorrectionPct === lvl.id ? 'active' : ''} rounded px-1.5 py-1 text-center transition-all`}
                    >
                        <div className="text-[11px] font-medium">{lvl.label}</div>
                        <div className="text-[10px]" style={{ color: 'var(--compress-text-muted)' }}>~{lvl.id}%</div>
                    </button>
                ))}
            </div>
            <div className="flex items-center gap-2">
                <input
                    type="range" min={5} max={50} step={1}
                    value={state.errorCorrectionPct}
                    onChange={e => state.setErrorCorrectionPct(Number(e.target.value))}
                    className="flex-1 accent-amber-600"
                    aria-label={t('vault.recoveryLevel')}
                />
                <div className="flex items-center gap-1">
                    <input
                        type="number" min={5} max={50}
                        value={state.errorCorrectionPct}
                        onChange={e => state.setErrorCorrectionPct(Math.min(50, Math.max(5, Math.round(Number(e.target.value) || 5))))}
                        className="w-14 rounded px-1.5 py-0.5 text-[12px] text-right outline-none"
                        style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                    />
                    <span className="text-[11px]" style={{ color: 'var(--compress-text-muted)' }}>%</span>
                </div>
            </div>
        </>
    );

    return (
        <div className="compress-dialog flex flex-col min-h-0 flex-1" style={{ color: 'var(--compress-text)' }}>
            {/* Scrollable body: with the Archive (v3) Error Correction options
                expanded the form can exceed a short window, so the body scrolls
                while the footer stays pinned (mirrors the Compressor). */}
            <div className="p-4 flex flex-col gap-3 overflow-y-auto">
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
                drives the saved filename, shown for every mode. Mirrors the
                Compressor's "Archive Name" field (label + extension suffix).
                Autofocused on open; Create stays disabled until it is non-empty.
                The value is also stored as the v1/v2 `description` metadata. */}
            <div>
                <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                    {isZip ? t('compress.archiveName') : t('vault.vaultName')}
                    <span className="text-red-400 ml-0.5">*</span>
                </label>
                <div className="flex gap-2 items-center">
                    <input
                        ref={firstFieldRef}
                        value={state.description}
                        onChange={e => state.setDescription(e.target.value)}
                        className="flex-1 rounded-lg px-3 py-2 text-sm outline-none transition-colors"
                        style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                        onFocus={e => (e.currentTarget.style.borderColor = 'var(--compress-accent)')}
                        onBlur={e => (e.currentTarget.style.borderColor = 'var(--compress-input-border)')}
                        placeholder={isZip ? 'My archive' : 'My secure vault'}
                    />
                    <span className="text-xs font-mono whitespace-nowrap" style={{ color: 'var(--compress-text-muted)' }}>{isZip ? '.aerozip' : '.aerovault'}</span>
                </div>
                {/* Output path preview (Compressor-style, Phase 3): the new vault is
                    written straight into the source folder, no save dialog. The
                    optional "Change" button picks a different destination folder.
                    Hidden when no folder context is known (create from Home), where
                    handleCreate falls back to a native save dialog. */}
                {state.fullOutputPath && (
                    <div className="flex items-center gap-2 mt-1.5">
                        <span className="text-xs font-mono truncate flex-1" title={state.fullOutputPath} style={{ color: 'var(--compress-text-muted)' }}>
                            {state.fullOutputPath}
                        </span>
                        <button
                            type="button"
                            tabIndex={-1}
                            onClick={state.handleChangeOutputDir}
                            disabled={state.loading}
                            className="flex items-center gap-1 px-2 py-1 text-[11px] rounded shrink-0 transition-colors disabled:opacity-50"
                            style={{ background: 'var(--compress-bg-deep)', border: '1px solid var(--compress-border)', color: 'var(--compress-text-secondary)' }}
                        >
                            <FolderOpen size={12} /> {t('connection.change')}
                        </button>
                    </div>
                )}
            </div>

            {/* Mode cards (7z-like format grid): encrypted (.aerovault, password
                protected) vs plaintext (.aerozip, no password). Selecting a card
                only flips `containerKind`; handleCreate routes to the existing
                per-format command (Option C, no backend change). */}
            <div>
                <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                    {t('compress.format')}
                </label>
                <div className="grid grid-cols-2 gap-2">
                    <button
                        onClick={() => state.setContainerKind('vault')}
                        className={`compress-format-card ${!isZip ? 'active' : ''} rounded-lg px-3 py-2.5 text-left transition-all`}
                    >
                        <div className="flex items-center gap-1.5">
                            <Lock size={13} style={{ color: 'var(--compress-accent)' }} />
                            <span className="text-sm font-semibold">{t('vault.encryptedBadge')}</span>
                        </div>
                        <div className="text-[10px] mt-0.5" style={{ color: 'var(--compress-text-muted)' }}>.aerovault · AES-256</div>
                    </button>
                    <button
                        onClick={() => state.setContainerKind('zip')}
                        className={`compress-format-card ${isZip ? 'active' : ''} rounded-lg px-3 py-2.5 text-left transition-all`}
                    >
                        <div className="flex items-center gap-1.5">
                            <Unlock size={13} className="text-amber-500" />
                            <span className="text-sm font-semibold">{t('vault.notEncryptedBadge')}</span>
                        </div>
                        <div className="text-[10px] mt-0.5" style={{ color: 'var(--compress-text-muted)' }}>.aerozip · {t('vault.zipBadge')}</div>
                    </button>
                </div>
            </div>

            {/* ── Encrypted mode: password (always) + Security Level (Advanced) ── */}
            {!isZip && (
                <>
                    <div>
                        <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                            {t('vault.password')}
                        </label>
                        <div className="relative">
                            <input
                                type={state.showPassword ? 'text' : 'password'}
                                value={state.password}
                                onChange={e => state.setPassword(e.target.value)}
                                className="w-full rounded-lg px-3 py-2 text-sm pr-16 outline-none transition-colors"
                                style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                onFocus={e => (e.currentTarget.style.borderColor = 'var(--compress-accent)')}
                                onBlur={e => (e.currentTarget.style.borderColor = 'var(--compress-input-border)')}
                            />
                            <InlinePasswordGenerator
                                onGenerated={password => { state.setPassword(password); state.setConfirmPassword(password); }}
                                className="absolute right-8 top-1/2 -translate-y-1/2"
                            />
                            <button tabIndex={-1} type="button" onClick={() => state.setShowPassword(!state.showPassword)}
                                className="absolute right-2.5 top-1/2 -translate-y-1/2" style={{ color: 'var(--compress-text-muted)' }}>
                                {state.showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                            </button>
                        </div>
                        <div className="mt-1.5"><PasswordStrengthBar password={state.password} /></div>
                        <div className="relative mt-2">
                            <input
                                type={state.showPassword ? 'text' : 'password'}
                                value={state.confirmPassword}
                                onChange={e => state.setConfirmPassword(e.target.value)}
                                placeholder={t('vault.confirmPassword')}
                                aria-label={t('vault.confirmPassword')}
                                className="w-full rounded-lg px-3 py-2 text-sm outline-none transition-colors"
                                style={{ background: 'var(--compress-input-bg)', border: '1px solid var(--compress-input-border)', color: 'var(--compress-text)' }}
                                onFocus={e => (e.currentTarget.style.borderColor = 'var(--compress-accent)')}
                                onBlur={e => (e.currentTarget.style.borderColor = 'var(--compress-input-border)')}
                            />
                            <PasswordMatchHint password={state.password} confirm={state.confirmPassword} />
                        </div>
                    </div>

                    {/* Advanced disclosure: the security level + its Error Correction
                        options. Collapsed by default (the Advanced default is sane);
                        the toggle shows the current level so it is never hidden. */}
                    <div>
                        <button
                            onClick={() => setShowSecurity(!showSecurity)}
                            className="w-full flex items-center justify-between rounded-lg px-3 py-2 text-left transition-colors"
                            style={{ background: 'var(--compress-bg-deep)', border: '1px solid var(--compress-border)' }}
                        >
                            <span className="flex items-center gap-2 text-xs font-medium" style={{ color: 'var(--compress-text-secondary)' }}>
                                <SlidersHorizontal size={13} style={{ color: 'var(--compress-accent)' }} />
                                {t('vault.securityLevel')}
                                <span style={{ color: 'var(--compress-text-muted)' }}>· {securityLevels[state.securityLevel].label}</span>
                            </span>
                            {showSecurity
                                ? <ChevronUp size={14} style={{ color: 'var(--compress-text-muted)' }} />
                                : <ChevronDown size={14} style={{ color: 'var(--compress-text-muted)' }} />}
                        </button>

                        {showSecurity && (
                            <div className="mt-2 flex flex-col gap-2">
                                <div className="grid grid-cols-3 gap-2">
                                    {CREATE_SECURITY_LEVELS.map((level) => {
                                        const config = securityLevels[level];
                                        const Icon = config.icon;
                                        const selected = level === state.securityLevel;
                                        return (
                                            <button
                                                key={level}
                                                onClick={() => state.setSecurityLevel(level)}
                                                className={`compress-format-card ${selected ? 'active' : ''} rounded-lg px-3 py-2.5 text-left transition-all`}
                                            >
                                                <div className="flex items-center gap-1.5">
                                                    <Icon size={14} className={config.color} />
                                                    <span className="text-sm font-semibold">{config.label}</span>
                                                </div>
                                                <div className="text-[10px] mt-0.5" style={{ color: 'var(--compress-text-muted)' }}>{config.description}</div>
                                            </button>
                                        );
                                    })}
                                </div>
                                {state.securityLevel === 'advanced' && (
                                    <div className="text-[11px]" style={{ color: 'var(--compress-text-muted)' }}>
                                        {securityLevels.advanced.label} · {t('vault.securityRecommended')}
                                    </div>
                                )}

                                {/* Archive (v3): compression profile + opt-in Error Correction. */}
                                {state.securityLevel === 'experimental' && (
                                    <>
                                        <label className="text-xs font-medium block" style={{ color: 'var(--compress-text-secondary)' }}>
                                            {t('vault.compressionProfile')}
                                        </label>
                                        {renderCompressionProfiles()}

                                        <div className="mt-1 flex items-center gap-2">
                                            <input
                                                type="checkbox"
                                                id="ecc-enabled"
                                                checked={state.errorCorrectionEnabled}
                                                onChange={e => state.setErrorCorrectionEnabled(e.target.checked)}
                                                className="accent-amber-600"
                                            />
                                            <label htmlFor="ecc-enabled" className="text-sm cursor-pointer" style={{ color: 'var(--compress-text-secondary)' }}>
                                                {t('vault.enableErrorCorrection')}
                                            </label>
                                        </div>
                                        {state.errorCorrectionEnabled && (
                                            <div className="pl-6 flex flex-col gap-2">
                                                <div className="text-[11px] text-amber-600 dark:text-amber-400">
                                                    {t('vault.errorCorrectionDesc')}
                                                </div>
                                                <label className="text-[11px]" style={{ color: 'var(--compress-text-secondary)' }}>{t('vault.recoveryPlacement')}</label>
                                                <div className="grid grid-cols-3 gap-2">
                                                    {([
                                                        { id: 'embedded', label: t('vault.placementEmbedded'), detail: t('vault.placementEmbeddedDesc') },
                                                        { id: 'detached', label: t('vault.placementDetached'), detail: t('vault.placementDetachedDesc') },
                                                        { id: 'both', label: t('vault.placementBoth'), detail: t('vault.placementBothDesc') },
                                                    ] as const).map(p => (
                                                        <button
                                                            key={p.id}
                                                            onClick={() => state.setRecoveryPlacement(p.id)}
                                                            className={`compress-format-card ${state.recoveryPlacement === p.id ? 'active' : ''} rounded-lg px-2 py-1.5 text-left transition-all`}
                                                        >
                                                            <div className="text-[12px] font-medium">{p.label}</div>
                                                            <div className="text-[10px]" style={{ color: 'var(--compress-text-muted)' }}>{p.detail}</div>
                                                        </button>
                                                    ))}
                                                </div>
                                                {state.recoveryPlacement !== 'embedded' && (
                                                    <div className="text-[11px] text-emerald-600 dark:text-emerald-400">
                                                        {t('vault.detachedStableStorageNote')}
                                                    </div>
                                                )}
                                                {renderRecoveryLevel()}
                                                <div className="text-[10px]" style={{ color: 'var(--compress-text-muted)' }}>
                                                    {t('vault.recoveryLevelHint')}
                                                </div>
                                            </div>
                                        )}
                                    </>
                                )}
                            </div>
                        )}
                    </div>
                </>
            )}

            {/* ── Plaintext mode: compression profile + opt-out Error Correction ── */}
            {isZip && (
                <>
                    <div>
                        <label className="text-xs font-medium block mb-1.5" style={{ color: 'var(--compress-text-secondary)' }}>
                            {t('vault.compressionProfile')}
                        </label>
                        {renderCompressionProfiles()}
                    </div>

                    {/* Recovery (Reed-Solomon parity) is opt-out for .aerozip: ON by
                        default so existing behaviour is unchanged, but disablable so the
                        archive can match canonical compression ratios. When unchecked we
                        send pct=0 and the backend creates a plain archive with no parity. */}
                    <div className="flex items-center gap-2">
                        <input
                            type="checkbox"
                            id="zip-ecc-enabled"
                            checked={state.errorCorrectionEnabled}
                            onChange={e => state.setErrorCorrectionEnabled(e.target.checked)}
                            className="accent-amber-600"
                        />
                        <label htmlFor="zip-ecc-enabled" className="text-sm cursor-pointer" style={{ color: 'var(--compress-text-secondary)' }}>
                            {t('vault.enableErrorCorrection')}
                        </label>
                    </div>
                    {state.errorCorrectionEnabled && (
                        <>
                            {renderRecoveryLevel()}
                            <div className="text-[10px]" style={{ color: 'var(--compress-text-muted)' }}>
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
                <div className="mt-1 space-y-1">
                    <TransferProgressBar percentage={state.vaultProgress.percentage} size="lg" />
                    <div className="flex justify-between text-[11px] tabular-nums" style={{ color: 'var(--compress-text-muted)' }}>
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
                                <div className="w-full h-2.5 rounded-full overflow-hidden" style={{ background: 'var(--compress-bg-deep)' }}>
                                    <div className="h-full rounded-full bg-amber-500 transition-all duration-300" style={{ width: `${filled}%` }} />
                                </div>
                                <div className="flex justify-end text-[11px] tabular-nums" style={{ color: 'var(--compress-text-muted)' }}>
                                    {formatSize(remaining)}
                                </div>
                            </>
                        );
                    })()}
                </div>
            )}

            </div>

            <div className="flex gap-2 items-center px-4 py-3 border-t" style={{ borderColor: 'var(--compress-border)' }}>
                <button onClick={resetToDefaults} disabled={state.loading}
                    className="flex items-center gap-1 px-2 py-1.5 text-xs rounded transition-colors disabled:opacity-50"
                    style={{ color: 'var(--compress-text-secondary)' }}
                    onMouseEnter={e => (e.currentTarget.style.background = 'var(--compress-bg-hover)')}
                    onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}>
                    <RotateCcw size={13} /> {t('vault.resetDefaults')}
                </button>
                <div className="flex gap-2 ml-auto">
                    <button onClick={() => state.setMode('home')}
                        className="px-3 py-1.5 text-sm rounded transition-colors"
                        style={{ color: 'var(--compress-text-secondary)' }}
                        onMouseEnter={e => (e.currentTarget.style.background = 'var(--compress-bg-hover)')}
                        onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}>
                        {t('vault.cancel')}
                    </button>
                    <button onClick={state.handleCreate} disabled={state.loading || !state.description.trim()}
                        className="flex items-center gap-2 px-4 py-1.5 rounded text-sm text-white transition-opacity hover:opacity-90 disabled:opacity-50"
                        style={{ background: isZip ? '#d97706' : 'var(--compress-accent)' }}>
                        {state.loading ? <Loader2 size={14} className="animate-spin" /> : (isZip ? <Unlock size={14} /> : <Lock size={14} />)}
                        {t('vault.create')} {isZip ? '.aerozip' : '.aerovault'}
                    </button>
                </div>
            </div>
        </div>
    );
};
