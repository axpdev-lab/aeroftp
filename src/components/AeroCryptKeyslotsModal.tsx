// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCrypt v4 keyslot manager (T6). Opened from the crypt badge context menu.
// Lists slots, migrates v3→v4, adds/rotates/removes factors. F6 honesty copy on
// remove is mandatory (verbatim from 08-v4-keyslot-spec §6).

import * as React from 'react';
import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { pickFile } from '../utils/pickPath';
import { KeyRound, Loader2, Plus, RefreshCw, Trash2, X } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';

interface SlotSummary {
    id: number;
    kind: string;
    saltLen: number;
}

interface SlotListResult {
    version: number;
    epoch: number;
    vaultIdShort: string;
    openedSlotId: number | null;
    slots: SlotSummary[];
}

interface SlotMutateResult extends SlotListResult {
    action: string;
    recoveryCode?: string | null;
    autoOfferedRecovery?: boolean;
}

interface Props {
    profileId?: string | null;
    profileName?: string;
    remoteScope?: string | null;
    onClose: () => void;
}

type PanelMode = 'list' | 'add' | 'rotate' | 'remove' | 'migrate';

export const AeroCryptKeyslotsModal: React.FC<Props> = ({
    profileId,
    profileName,
    remoteScope,
    onClose,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();

    const [password, setPassword] = useState('');
    const [keyfilePath, setKeyfilePath] = useState('');
    const [loading, setLoading] = useState(false);
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [info, setInfo] = useState<string | null>(null);
    const [list, setList] = useState<SlotListResult | null>(null);
    const [selectedId, setSelectedId] = useState<number | null>(null);
    const [mode, setMode] = useState<PanelMode>('list');

    // Add / rotate form
    const [slotType, setSlotType] = useState<'passphrase' | 'keyfile' | 'recovery'>('passphrase');
    const [newPassword, setNewPassword] = useState('');
    const [newPassword2, setNewPassword2] = useState('');
    const [newKeyfilePath, setNewKeyfilePath] = useState('');
    // One-time recovery code shown after add (must be saved by the user).
    const [shownRecoveryCode, setShownRecoveryCode] = useState<string | null>(null);
    // F6 gate: user must type YES to confirm remove
    const [removeConfirm, setRemoveConfirm] = useState('');

    const baseArgs = useCallback(() => {
        return {
            password: password || '',
            keyfilePath: keyfilePath.trim() || null,
            basePath: remoteScope || null,
            profileId: profileId || null,
        };
    }, [password, keyfilePath, remoteScope, profileId]);

    const pickKeyfile = async (setter: (p: string) => void) => {
        try {
            const picked = await pickFile({
                multiple: false,
                filters: [{ name: 'Keyfile', extensions: ['*'] }],
            });
            if (typeof picked === 'string' && picked) setter(picked);
        } catch (e) {
            setError(String(e));
        }
    };

    // Prefill unlock factors from the keystore when the profile is saved.
    useEffect(() => {
        if (!profileId) return;
        let cancelled = false;
        (async () => {
            try {
                const pw = await invoke<string>('get_credential', {
                    account: `aerocrypt_overlay_pw_${profileId}`,
                }).catch(() => '');
                const kf = await invoke<string>('get_credential', {
                    account: `aerocrypt_overlay_keyfile_path_${profileId}`,
                }).catch(() => '');
                if (cancelled) return;
                if (pw) setPassword(pw);
                if (kf) setKeyfilePath(kf);
            } catch {
                /* offline / no keystore: user types factors */
            }
        })();
        return () => {
            cancelled = true;
        };
    }, [profileId]);

    const refreshList = async () => {
        setError(null);
        setInfo(null);
        if (!password && !keyfilePath.trim()) {
            setError(t('aerocryptNative.keyslotsNeedFactors'));
            return;
        }
        setLoading(true);
        try {
            const result = await invoke<SlotListResult>('aerocrypt_list_slots', {
                password: password || '',
                keyfilePath: keyfilePath.trim() || null,
                basePath: remoteScope || null,
            });
            setList(result);
            if (result.slots.length > 0) {
                setSelectedId((prev) =>
                    prev != null && result.slots.some((s) => s.id === prev)
                        ? prev
                        : result.slots[0].id,
                );
            } else {
                setSelectedId(null);
            }
            setMode('list');
        } catch (e) {
            setList(null);
            setError(String(e).replace(/^Error:\s*/i, ''));
        } finally {
            setLoading(false);
        }
    };

    const runMigrate = async () => {
        setError(null);
        setInfo(null);
        setShownRecoveryCode(null);
        setBusy(true);
        try {
            const result = await invoke<SlotMutateResult>('aerocrypt_migrate_v4', {
                ...baseArgs(),
            });
            setList(result);
            setSelectedId(result.slots[0]?.id ?? null);
            setMode('list');
            if (result.recoveryCode) {
                setShownRecoveryCode(result.recoveryCode);
                setInfo(t('aerocryptNative.keyslotsRecoveryAutoOffered'));
            } else {
                setInfo(t('aerocryptNative.keyslotsMigrated'));
            }
        } catch (e) {
            setError(String(e).replace(/^Error:\s*/i, ''));
        } finally {
            setBusy(false);
        }
    };

    const runAdd = async () => {
        setError(null);
        setInfo(null);
        setShownRecoveryCode(null);
        if (slotType === 'passphrase') {
            if (!newPassword) {
                setError(t('aerocryptNative.keyslotsNewPasswordRequired'));
                return;
            }
            if (newPassword !== newPassword2) {
                setError(t('aerocryptNative.keyslotsPasswordMismatch'));
                return;
            }
        } else if (slotType === 'keyfile') {
            if (!newKeyfilePath.trim()) {
                setError(t('aerocryptNative.keyslotsNewKeyfileRequired'));
                return;
            }
        }
        // recovery: no form fields; backend generates a code
        setBusy(true);
        try {
            const result = await invoke<SlotMutateResult>('aerocrypt_add_slot', {
                ...baseArgs(),
                slotType,
                newPassword: slotType === 'passphrase' ? newPassword : newPassword || null,
                newKeyfilePath: slotType === 'keyfile' ? newKeyfilePath.trim() : null,
            });
            setList(result);
            setNewPassword('');
            setNewPassword2('');
            setNewKeyfilePath('');
            setMode('list');
            if (result.recoveryCode) {
                setShownRecoveryCode(result.recoveryCode);
                setInfo(
                    result.autoOfferedRecovery
                        ? t('aerocryptNative.keyslotsRecoveryAutoOffered')
                        : t('aerocryptNative.keyslotsRecoveryAdded'),
                );
            } else {
                setInfo(t('aerocryptNative.keyslotsAdded'));
            }
        } catch (e) {
            setError(String(e).replace(/^Error:\s*/i, ''));
        } finally {
            setBusy(false);
        }
    };

    const runRotate = async () => {
        setError(null);
        setInfo(null);
        if (selectedId == null) {
            setError(t('aerocryptNative.keyslotsSelectSlot'));
            return;
        }
        const selected = list?.slots.find((s) => s.id === selectedId);
        if (!selected) {
            setError(t('aerocryptNative.keyslotsSelectSlot'));
            return;
        }
        if (selected.kind === 'passphrase') {
            if (!newPassword) {
                setError(t('aerocryptNative.keyslotsNewPasswordRequired'));
                return;
            }
            if (newPassword !== newPassword2) {
                setError(t('aerocryptNative.keyslotsPasswordMismatch'));
                return;
            }
        } else if (selected.kind === 'keyfile') {
            if (!newKeyfilePath.trim()) {
                setError(t('aerocryptNative.keyslotsNewKeyfileRequired'));
                return;
            }
        } else {
            setError(t('aerocryptNative.keyslotsRotateUnsupported'));
            return;
        }
        setBusy(true);
        try {
            const result = await invoke<SlotMutateResult>('aerocrypt_rotate_slot', {
                ...baseArgs(),
                slotId: selectedId,
                newPassword: selected.kind === 'passphrase' ? newPassword : newPassword || null,
                newKeyfilePath: selected.kind === 'keyfile' ? newKeyfilePath.trim() : null,
            });
            // After rotate of the unlock factor, switch the modal's unlock password
            // so a subsequent refresh still works without retyping.
            if (selected.kind === 'passphrase' && list?.openedSlotId === selectedId) {
                setPassword(newPassword);
            }
            if (selected.kind === 'keyfile' && list?.openedSlotId === selectedId) {
                setKeyfilePath(newKeyfilePath.trim());
            }
            setList(result);
            setNewPassword('');
            setNewPassword2('');
            setNewKeyfilePath('');
            setMode('list');
            setInfo(t('aerocryptNative.keyslotsRotated'));
        } catch (e) {
            setError(String(e).replace(/^Error:\s*/i, ''));
        } finally {
            setBusy(false);
        }
    };

    const runRemove = async () => {
        setError(null);
        setInfo(null);
        if (selectedId == null) {
            setError(t('aerocryptNative.keyslotsSelectSlot'));
            return;
        }
        if (removeConfirm.trim().toUpperCase() !== 'YES') {
            setError(t('aerocryptNative.keyslotsRemoveTypeYes'));
            return;
        }
        setBusy(true);
        try {
            const result = await invoke<SlotMutateResult>('aerocrypt_remove_slot', {
                ...baseArgs(),
                slotId: selectedId,
            });
            setList(result);
            setRemoveConfirm('');
            setSelectedId(result.slots[0]?.id ?? null);
            setMode('list');
            setInfo(t('aerocryptNative.keyslotsRemoved'));
        } catch (e) {
            setError(String(e).replace(/^Error:\s*/i, ''));
        } finally {
            setBusy(false);
        }
    };

    const kindLabel = (kind: string) => {
        switch (kind) {
            case 'passphrase':
                return t('aerocryptNative.keyslotsKindPassphrase');
            case 'keyfile':
                return t('aerocryptNative.keyslotsKindKeyfile');
            case 'recovery':
                return t('aerocryptNative.keyslotsKindRecovery');
            case 'fido2-hmac':
                return t('aerocryptNative.keyslotsKindFido2');
            default:
                return kind;
        }
    };

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-full max-w-xl mx-4 max-h-[90vh] overflow-y-auto"
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 min-w-0">
                        <KeyRound className="w-5 h-5 text-emerald-600 dark:text-emerald-400 shrink-0" />
                        <span className="font-semibold text-gray-800 dark:text-gray-100 truncate">
                            {t('aerocryptNative.keyslotsTitle')}
                        </span>
                        {profileName && (
                            <span className="text-xs text-gray-400 dark:text-gray-500 truncate max-w-[12rem]">
                                {profileName}
                            </span>
                        )}
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500"
                        aria-label={t('common.close')}
                    >
                        <X className="w-4 h-4" />
                    </button>
                </div>

                <div className="p-4 space-y-3">
                    <p className="text-xs text-gray-600 dark:text-gray-300">
                        {t('aerocryptNative.keyslotsIntro')}
                    </p>

                    {/* Unlock factors */}
                    <div className="space-y-2 rounded border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900 p-3">
                        <label className="block text-xs font-medium text-gray-700 dark:text-gray-300">
                            {t('aerocryptNative.password')}
                            <input
                                type="password"
                                autoComplete="off"
                                value={password}
                                onChange={(e) => setPassword(e.target.value)}
                                className="mt-1 w-full px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100"
                                placeholder={t('aerocryptNative.passwordPlaceholder')}
                            />
                        </label>
                        <div className="flex items-end gap-2">
                            <label className="flex-1 block text-xs font-medium text-gray-700 dark:text-gray-300">
                                {t('aerocryptNative.keyfileLabel')}
                                <input
                                    type="text"
                                    readOnly
                                    value={keyfilePath}
                                    className="mt-1 w-full px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 truncate"
                                    placeholder={t('aerocryptNative.keyfilePlaceholder')}
                                />
                            </label>
                            <button
                                type="button"
                                onClick={() => void pickKeyfile(setKeyfilePath)}
                                className="px-2 py-1.5 text-xs rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                            >
                                {t('aerocryptNative.keyfileChoose')}
                            </button>
                            {keyfilePath && (
                                <button
                                    type="button"
                                    onClick={() => setKeyfilePath('')}
                                    className="px-2 py-1.5 text-xs rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                                >
                                    {t('aerocryptNative.keyfileClear')}
                                </button>
                            )}
                        </div>
                        <button
                            type="button"
                            disabled={loading || busy}
                            onClick={() => void refreshList()}
                            className="w-full flex items-center justify-center gap-1 px-3 py-1.5 text-sm rounded bg-emerald-600 text-white hover:bg-emerald-700 disabled:opacity-60"
                        >
                            {loading ? (
                                <Loader2 className="w-3.5 h-3.5 animate-spin" />
                            ) : (
                                <RefreshCw className="w-3.5 h-3.5" />
                            )}
                            {t('aerocryptNative.keyslotsLoad')}
                        </button>
                    </div>

                    {error && (
                        <div className="p-3 bg-red-50 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded text-sm text-red-800 dark:text-red-200">
                            {error}
                        </div>
                    )}
                    {info && (
                        <div className="p-3 bg-emerald-50 dark:bg-emerald-900/30 border border-emerald-300 dark:border-emerald-700 rounded text-sm text-emerald-800 dark:text-emerald-200">
                            {info}
                        </div>
                    )}

                    {list && (
                        <div className="space-y-2">
                            <div className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-gray-600 dark:text-gray-300">
                                <span>
                                    {t('aerocryptNative.versionLabel', {
                                        version: String(list.version),
                                    })}
                                </span>
                                {list.version >= 4 && (
                                    <span>
                                        {t('aerocryptNative.keyslotsEpoch', {
                                            epoch: String(list.epoch),
                                        })}
                                    </span>
                                )}
                                {list.vaultIdShort && (
                                    <span>
                                        {t('aerocryptNative.keyslotsVaultId', {
                                            id: list.vaultIdShort,
                                        })}
                                    </span>
                                )}
                                {list.openedSlotId != null && (
                                    <span>
                                        {t('aerocryptNative.keyslotsOpenedSlot', {
                                            id: String(list.openedSlotId),
                                        })}
                                    </span>
                                )}
                            </div>

                            {list.version === 3 && (
                                <div className="p-3 bg-amber-50 dark:bg-amber-900/30 border border-amber-300 dark:border-amber-700 rounded text-sm text-amber-800 dark:text-amber-200 space-y-2">
                                    <p>{t('aerocryptNative.keyslotsV3Notice')}</p>
                                    <button
                                        type="button"
                                        disabled={busy}
                                        onClick={() => void runMigrate()}
                                        className="px-3 py-1.5 text-sm rounded bg-amber-600 text-white hover:bg-amber-700 disabled:opacity-60"
                                    >
                                        {busy && mode === 'migrate' ? (
                                            <Loader2 className="w-3.5 h-3.5 animate-spin inline mr-1" />
                                        ) : null}
                                        {t('aerocryptNative.keyslotsConvertV4')}
                                    </button>
                                </div>
                            )}

                            {shownRecoveryCode && (
                                <div className="p-3 bg-amber-50 dark:bg-amber-900/40 border border-amber-300 dark:border-amber-700 rounded text-sm text-amber-900 dark:text-amber-100 space-y-2">
                                    <p className="font-medium">
                                        {t('aerocryptNative.keyslotsRecoverySaveOnce')}
                                    </p>
                                    <code className="block break-all text-xs font-mono bg-white/70 dark:bg-gray-900/70 px-2 py-1.5 rounded select-all">
                                        {shownRecoveryCode}
                                    </code>
                                    <button
                                        type="button"
                                        onClick={() => {
                                            void navigator.clipboard
                                                ?.writeText(shownRecoveryCode)
                                                .catch(() => undefined);
                                        }}
                                        className="px-2 py-1 text-xs rounded bg-amber-600 text-white hover:bg-amber-700"
                                    >
                                        {t('aerocryptNative.keyslotsRecoveryCopy')}
                                    </button>
                                </div>
                            )}

                            {list.version >= 4 && mode === 'list' && (
                                <>
                                    <ul className="divide-y divide-gray-200 dark:divide-gray-700 border border-gray-200 dark:border-gray-700 rounded overflow-hidden">
                                        {list.slots.map((s) => {
                                            const selected = selectedId === s.id;
                                            const opened = list.openedSlotId === s.id;
                                            return (
                                                <li key={s.id}>
                                                    <button
                                                        type="button"
                                                        onClick={() => setSelectedId(s.id)}
                                                        className={
                                                            'w-full text-left px-3 py-2 text-sm flex items-center justify-between gap-2 ' +
                                                            (selected
                                                                ? 'bg-emerald-50 dark:bg-emerald-900/30'
                                                                : 'bg-white dark:bg-gray-800 hover:bg-gray-50 dark:hover:bg-gray-700/60')
                                                        }
                                                    >
                                                        <span className="text-gray-900 dark:text-gray-100">
                                                            #{s.id} · {kindLabel(s.kind)}
                                                            {opened && (
                                                                <span className="ml-2 text-[11px] text-emerald-700 dark:text-emerald-300">
                                                                    {t(
                                                                        'aerocryptNative.keyslotsOpenedBadge',
                                                                    )}
                                                                </span>
                                                            )}
                                                        </span>
                                                        <span className="text-xs text-gray-500 dark:text-gray-400">
                                                            {t('aerocryptNative.keyslotsSaltLen', {
                                                                n: String(s.saltLen),
                                                            })}
                                                        </span>
                                                    </button>
                                                </li>
                                            );
                                        })}
                                    </ul>

                                    <div className="flex flex-wrap gap-2">
                                        <button
                                            type="button"
                                            disabled={busy}
                                            onClick={() => {
                                                setMode('add');
                                                setError(null);
                                                setInfo(null);
                                            }}
                                            className="flex items-center gap-1 px-3 py-1.5 text-sm rounded bg-emerald-600 text-white hover:bg-emerald-700 disabled:opacity-60"
                                        >
                                            <Plus className="w-3.5 h-3.5" />
                                            {t('aerocryptNative.keyslotsAdd')}
                                        </button>
                                        <button
                                            type="button"
                                            disabled={busy || selectedId == null}
                                            onClick={() => {
                                                setMode('rotate');
                                                setError(null);
                                                setInfo(null);
                                            }}
                                            className="flex items-center gap-1 px-3 py-1.5 text-sm rounded bg-sky-600 text-white hover:bg-sky-700 disabled:opacity-60"
                                        >
                                            <RefreshCw className="w-3.5 h-3.5" />
                                            {t('aerocryptNative.keyslotsRotate')}
                                        </button>
                                        <button
                                            type="button"
                                            disabled={
                                                busy ||
                                                selectedId == null ||
                                                list.slots.length < 2 ||
                                                selectedId === list.openedSlotId
                                            }
                                            onClick={() => {
                                                setMode('remove');
                                                setRemoveConfirm('');
                                                setError(null);
                                                setInfo(null);
                                            }}
                                            className="flex items-center gap-1 px-3 py-1.5 text-sm rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-60"
                                        >
                                            <Trash2 className="w-3.5 h-3.5" />
                                            {t('aerocryptNative.keyslotsRemove')}
                                        </button>
                                    </div>
                                    {list.slots.length >= 2 && selectedId === list.openedSlotId && (
                                        <p className="text-[11px] text-gray-500 dark:text-gray-400">
                                            {t('aerocryptNative.keyslotsRemoveUnlockHint')}
                                        </p>
                                    )}
                                </>
                            )}

                            {list.version >= 4 && mode === 'add' && (
                                <div className="space-y-2 rounded border border-gray-200 dark:border-gray-700 p-3 bg-gray-50 dark:bg-gray-900">
                                    <div className="font-medium text-sm text-gray-800 dark:text-gray-100">
                                        {t('aerocryptNative.keyslotsAdd')}
                                    </div>
                                    <div className="flex flex-wrap gap-3 text-sm">
                                        <label className="flex items-center gap-1 cursor-pointer text-gray-800 dark:text-gray-200">
                                            <input
                                                type="radio"
                                                name="slotType"
                                                checked={slotType === 'passphrase'}
                                                onChange={() => setSlotType('passphrase')}
                                            />
                                            {t('aerocryptNative.keyslotsKindPassphrase')}
                                        </label>
                                        <label className="flex items-center gap-1 cursor-pointer text-gray-800 dark:text-gray-200">
                                            <input
                                                type="radio"
                                                name="slotType"
                                                checked={slotType === 'keyfile'}
                                                onChange={() => setSlotType('keyfile')}
                                            />
                                            {t('aerocryptNative.keyslotsKindKeyfile')}
                                        </label>
                                        <label className="flex items-center gap-1 cursor-pointer text-gray-800 dark:text-gray-200">
                                            <input
                                                type="radio"
                                                name="slotType"
                                                checked={slotType === 'recovery'}
                                                onChange={() => setSlotType('recovery')}
                                            />
                                            {t('aerocryptNative.keyslotsKindRecovery')}
                                        </label>
                                    </div>
                                    {slotType === 'passphrase' ? (
                                        <>
                                            <input
                                                type="password"
                                                autoComplete="new-password"
                                                value={newPassword}
                                                onChange={(e) => setNewPassword(e.target.value)}
                                                placeholder={t(
                                                    'aerocryptNative.keyslotsNewPassword',
                                                )}
                                                className="w-full px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100"
                                            />
                                            <input
                                                type="password"
                                                autoComplete="new-password"
                                                value={newPassword2}
                                                onChange={(e) => setNewPassword2(e.target.value)}
                                                placeholder={t(
                                                    'aerocryptNative.keyslotsConfirmPassword',
                                                )}
                                                className="w-full px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100"
                                            />
                                        </>
                                    ) : slotType === 'keyfile' ? (
                                        <div className="flex gap-2">
                                            <input
                                                type="text"
                                                readOnly
                                                value={newKeyfilePath}
                                                placeholder={t(
                                                    'aerocryptNative.keyslotsNewKeyfile',
                                                )}
                                                className="flex-1 px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 truncate"
                                            />
                                            <button
                                                type="button"
                                                onClick={() => void pickKeyfile(setNewKeyfilePath)}
                                                className="px-2 py-1.5 text-xs rounded bg-gray-200 dark:bg-gray-700"
                                            >
                                                {t('aerocryptNative.keyfileChoose')}
                                            </button>
                                        </div>
                                    ) : (
                                        <p className="text-xs text-gray-600 dark:text-gray-300">
                                            {t('aerocryptNative.keyslotsRecoveryHint')}
                                        </p>
                                    )}
                                    <div className="flex gap-2">
                                        <button
                                            type="button"
                                            disabled={busy}
                                            onClick={() => void runAdd()}
                                            className="px-3 py-1.5 text-sm rounded bg-emerald-600 text-white hover:bg-emerald-700 disabled:opacity-60"
                                        >
                                            {busy && <Loader2 className="w-3.5 h-3.5 animate-spin inline mr-1" />}
                                            {t('aerocryptNative.keyslotsAddConfirm')}
                                        </button>
                                        <button
                                            type="button"
                                            disabled={busy}
                                            onClick={() => setMode('list')}
                                            className="px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700"
                                        >
                                            {t('common.cancel')}
                                        </button>
                                    </div>
                                </div>
                            )}

                            {list.version >= 4 && mode === 'rotate' && (
                                <div className="space-y-2 rounded border border-gray-200 dark:border-gray-700 p-3 bg-gray-50 dark:bg-gray-900">
                                    <div className="font-medium text-sm text-gray-800 dark:text-gray-100">
                                        {t('aerocryptNative.keyslotsRotate')} #{selectedId}
                                    </div>
                                    <p className="text-xs text-gray-600 dark:text-gray-300">
                                        {t('aerocryptNative.keyslotsRotateHint')}
                                    </p>
                                    {list.slots.find((s) => s.id === selectedId)?.kind ===
                                    'keyfile' ? (
                                        <div className="flex gap-2">
                                            <input
                                                type="text"
                                                readOnly
                                                value={newKeyfilePath}
                                                placeholder={t(
                                                    'aerocryptNative.keyslotsNewKeyfile',
                                                )}
                                                className="flex-1 px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 truncate"
                                            />
                                            <button
                                                type="button"
                                                onClick={() => void pickKeyfile(setNewKeyfilePath)}
                                                className="px-2 py-1.5 text-xs rounded bg-gray-200 dark:bg-gray-700"
                                            >
                                                {t('aerocryptNative.keyfileChoose')}
                                            </button>
                                        </div>
                                    ) : (
                                        <>
                                            <input
                                                type="password"
                                                autoComplete="new-password"
                                                value={newPassword}
                                                onChange={(e) => setNewPassword(e.target.value)}
                                                placeholder={t(
                                                    'aerocryptNative.keyslotsNewPassword',
                                                )}
                                                className="w-full px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100"
                                            />
                                            <input
                                                type="password"
                                                autoComplete="new-password"
                                                value={newPassword2}
                                                onChange={(e) => setNewPassword2(e.target.value)}
                                                placeholder={t(
                                                    'aerocryptNative.keyslotsConfirmPassword',
                                                )}
                                                className="w-full px-2 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100"
                                            />
                                        </>
                                    )}
                                    <div className="flex gap-2">
                                        <button
                                            type="button"
                                            disabled={busy}
                                            onClick={() => void runRotate()}
                                            className="px-3 py-1.5 text-sm rounded bg-sky-600 text-white hover:bg-sky-700 disabled:opacity-60"
                                        >
                                            {busy && <Loader2 className="w-3.5 h-3.5 animate-spin inline mr-1" />}
                                            {t('aerocryptNative.keyslotsRotateConfirm')}
                                        </button>
                                        <button
                                            type="button"
                                            disabled={busy}
                                            onClick={() => setMode('list')}
                                            className="px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700"
                                        >
                                            {t('common.cancel')}
                                        </button>
                                    </div>
                                </div>
                            )}

                            {list.version >= 4 && mode === 'remove' && (
                                <div className="space-y-2 rounded border border-red-300 dark:border-red-700 p-3 bg-red-50 dark:bg-red-900/20">
                                    <div className="font-medium text-sm text-red-900 dark:text-red-100">
                                        {t('aerocryptNative.keyslotsRemove')} #{selectedId}
                                    </div>
                                    {/* F6 honesty copy — verbatim from 08-v4-keyslot-spec §6 */}
                                    <p className="text-xs text-red-900 dark:text-red-100 whitespace-pre-wrap">
                                        {t('aerocryptNative.keyslotsF6Honesty')}
                                    </p>
                                    <label className="block text-xs font-medium text-red-900 dark:text-red-100">
                                        {t('aerocryptNative.keyslotsRemoveConfirmLabel')}
                                        <input
                                            type="text"
                                            value={removeConfirm}
                                            onChange={(e) => setRemoveConfirm(e.target.value)}
                                            autoComplete="off"
                                            className="mt-1 w-full px-2 py-1.5 text-sm rounded border border-red-300 dark:border-red-700 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100"
                                            placeholder="YES"
                                        />
                                    </label>
                                    <div className="flex gap-2">
                                        <button
                                            type="button"
                                            disabled={busy}
                                            onClick={() => void runRemove()}
                                            className="px-3 py-1.5 text-sm rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-60"
                                        >
                                            {busy && <Loader2 className="w-3.5 h-3.5 animate-spin inline mr-1" />}
                                            {t('aerocryptNative.keyslotsRemoveConfirm')}
                                        </button>
                                        <button
                                            type="button"
                                            disabled={busy}
                                            onClick={() => setMode('list')}
                                            className="px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700"
                                        >
                                            {t('common.cancel')}
                                        </button>
                                    </div>
                                </div>
                            )}
                        </div>
                    )}

                    <div className="flex justify-end pt-1">
                        <button
                            type="button"
                            onClick={onClose}
                            className="px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                        >
                            {t('common.close')}
                        </button>
                    </div>
                </div>
            </div>
        </div>
    );
};
