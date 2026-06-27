// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { Copy, Scissors, Loader2, Lock, AlertCircle, Users } from 'lucide-react';
import { ServerProfile } from '../types';
import { useTranslation } from '../i18n';
import {
    listUsers,
    relocateServerProfile,
    type UserMetadata,
} from '../utils/userPartitions';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { UserAvatar } from './UserAvatar';

// N4 (discussion #270): pick a destination account and copy/move a saved
// server profile into its vault partition. The source is always the active
// account, so this dialog only chooses the target. A passphrase is requested
// inline when the target account is protected; the backend refuses to write
// blind into a protected partition.

interface ProfileRelocateDialogProps {
    mode: 'copy' | 'move';
    profile: ServerProfile;
    onClose: () => void;
    // Fired after a successful relocation so the caller can refresh state
    // (a Move removes the source profile from the active list).
    onDone: (result: { moved: boolean; alreadyPresent: boolean }) => void;
}

// Matches SavedServers `generateId` so a relocated copy is independent and
// collision-free across machines.
const generateProfileId = () =>
    `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

const toastEvent = (
    type: 'success' | 'warning' | 'error',
    title: string,
    message: string,
) => {
    window.dispatchEvent(
        new CustomEvent('aeroftp-toast', {
            detail: { type, title, message, duration: 6000 },
        }),
    );
};

export const ProfileRelocateDialog: React.FC<ProfileRelocateDialogProps> = ({
    mode,
    profile,
    onClose,
    onDone,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [users, setUsers] = useState<UserMetadata[]>([]);
    const [loading, setLoading] = useState(true);
    const [selectedId, setSelectedId] = useState<number | null>(null);
    const [passphrase, setPassphrase] = useState('');
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const passphraseRef = useRef<HTMLInputElement>(null);

    const isMove = mode === 'move';

    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const list = await listUsers();
                if (cancelled) return;
                // Only OTHER accounts are valid targets.
                const others = list.filter(u => !u.isActive);
                setUsers(others);
                if (others.length === 1) setSelectedId(others[0].id);
            } catch (err) {
                if (!cancelled) setError(String(err));
            } finally {
                if (!cancelled) setLoading(false);
            }
        })();
        return () => {
            cancelled = true;
        };
    }, []);

    const target = useMemo(
        () => users.find(u => u.id === selectedId) ?? null,
        [users, selectedId],
    );

    useEffect(() => {
        // Focus the passphrase field when a protected target is chosen.
        if (target?.hasPassphrase) {
            passphraseRef.current?.focus();
        }
    }, [target?.id, target?.hasPassphrase]);

    useEffect(() => {
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape' && !busy) onClose();
        };
        window.addEventListener('keydown', onKey);
        return () => window.removeEventListener('keydown', onKey);
    }, [busy, onClose]);

    const canSubmit =
        !!target && !busy && (!target.hasPassphrase || passphrase.length > 0);

    const handleSubmit = async () => {
        if (!target || busy) return;
        setBusy(true);
        setError(null);
        try {
            const result = await relocateServerProfile(
                target.id,
                profile.id,
                generateProfileId(),
                isMove,
                target.hasPassphrase ? passphrase : null,
            );
            const name = result.profileName || profile.name;
            // A Copy into a target that already holds the drive inserts nothing
            // (dedup skip). A Move always materialises the profile (inserted),
            // so it reports success even when an equivalent already existed.
            if (result.alreadyPresent && !result.inserted) {
                toastEvent(
                    'warning',
                    t(isMove ? 'savedServers.moveToUser' : 'savedServers.copyToUser'),
                    t('savedServers.relocateAlreadyPresent', { name, user: target.name }),
                );
            } else {
                toastEvent(
                    'success',
                    t(isMove ? 'savedServers.moveToUser' : 'savedServers.copyToUser'),
                    t(
                        isMove
                            ? 'savedServers.relocateSuccessMove'
                            : 'savedServers.relocateSuccessCopy',
                        { name, user: target.name },
                    ),
                );
            }
            onDone({ moved: result.moved, alreadyPresent: result.alreadyPresent });
            onClose();
        } catch (err) {
            const raw = String(err);
            const friendly = raw.includes('WRONG_PASSPHRASE')
                ? t('savedServers.relocateWrongPassword', { user: target.name })
                : t('savedServers.relocateError', { error: raw });
            setError(friendly);
            setBusy(false);
        }
    };

    const title = t(
        isMove ? 'savedServers.relocateMoveTitle' : 'savedServers.relocateCopyTitle',
    );

    return (
        <div
            className="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-[60]"
            role="dialog"
            aria-modal="true"
            aria-label={title}
            onClick={busy ? undefined : onClose}
        >
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-900 rounded-lg shadow-2xl w-[420px] max-w-[92vw] border border-gray-200 dark:border-gray-700 overflow-hidden animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                <div {...modalDrag.dragHandleProps} className="flex items-center gap-2 px-5 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
                    {isMove ? (
                        <Scissors size={18} className="text-amber-500" />
                    ) : (
                        <Copy size={18} className="text-blue-500" />
                    )}
                    <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
                        {title}
                    </h3>
                </div>

                <div className="px-5 py-4 space-y-3">
                    <p className="text-xs text-gray-500 dark:text-gray-400 truncate">
                        <span className="font-medium text-gray-700 dark:text-gray-300">
                            {profile.name}
                        </span>
                    </p>

                    {loading ? (
                        <div className="flex items-center gap-2 text-sm text-gray-500 dark:text-gray-400 py-4 justify-center">
                            <Loader2 size={16} className="animate-spin" />
                            {t('common.loading')}
                        </div>
                    ) : users.length === 0 ? (
                        <div className="flex items-start gap-2 text-sm text-gray-600 dark:text-gray-300 bg-gray-50 dark:bg-gray-800 rounded-lg p-3">
                            <Users size={16} className="mt-0.5 flex-shrink-0 text-gray-400" />
                            <span>{t('savedServers.relocateNoOtherUsers')}</span>
                        </div>
                    ) : (
                        <>
                            <label className="block text-xs font-medium text-gray-600 dark:text-gray-400">
                                {t('savedServers.relocatePickUser')}
                            </label>
                            <div className="max-h-44 overflow-y-auto rounded-lg border border-gray-200 dark:border-gray-700 divide-y divide-gray-100 dark:divide-gray-800">
                                {users.map(u => {
                                    const active = u.id === selectedId;
                                    return (
                                        <button
                                            key={u.id}
                                            type="button"
                                            onClick={() => setSelectedId(u.id)}
                                            className={`w-full flex items-center gap-2 px-3 py-2 text-left text-sm transition-colors ${
                                                active
                                                    ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300'
                                                    : 'hover:bg-gray-50 dark:hover:bg-gray-800 text-gray-700 dark:text-gray-300'
                                            }`}
                                        >
                                            <UserAvatar
                                                name={u.name}
                                                avatarEmoji={u.avatarEmoji}
                                                avatarColor={u.avatarColor}
                                                size="sm"
                                            />
                                            <span className="flex-1 truncate">{u.name}</span>
                                            {u.hasPassphrase && (
                                                <Lock size={12} className="text-gray-400 flex-shrink-0" />
                                            )}
                                        </button>
                                    );
                                })}
                            </div>

                            {target?.hasPassphrase && (
                                <div className="space-y-1">
                                    <label className="block text-xs font-medium text-gray-600 dark:text-gray-400">
                                        {t('savedServers.relocateTargetPasswordLabel', {
                                            user: target.name,
                                        })}
                                    </label>
                                    <input
                                        ref={passphraseRef}
                                        type="password"
                                        value={passphrase}
                                        onChange={e => setPassphrase(e.target.value)}
                                        onKeyDown={e => {
                                            if (e.key === 'Enter' && canSubmit) handleSubmit();
                                        }}
                                        className="w-full px-3 py-2 text-sm border rounded-lg dark:bg-gray-800 dark:border-gray-600 text-gray-900 dark:text-gray-100"
                                        autoComplete="off"
                                    />
                                    <p className="text-[11px] text-gray-400">
                                        {t('savedServers.relocateTargetPasswordHint', {
                                            user: target.name,
                                        })}
                                    </p>
                                </div>
                            )}

                            {isMove && (
                                <p className="text-[11px] text-amber-600 dark:text-amber-400 flex items-start gap-1">
                                    <AlertCircle size={12} className="mt-0.5 flex-shrink-0" />
                                    {t('savedServers.relocateMoveWarning')}
                                </p>
                            )}
                        </>
                    )}

                    {error && (
                        <p className="text-xs text-red-500 flex items-start gap-1">
                            <AlertCircle size={12} className="mt-0.5 flex-shrink-0" />
                            {error}
                        </p>
                    )}
                </div>

                <div className="flex justify-end gap-2 px-5 py-3 bg-gray-50 dark:bg-gray-800/50 border-t border-gray-200 dark:border-gray-700">
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={busy}
                        className="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-50"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        type="button"
                        onClick={handleSubmit}
                        disabled={!canSubmit}
                        className={`px-4 py-2 text-sm text-white rounded-lg transition-colors flex items-center gap-1.5 disabled:opacity-50 disabled:cursor-not-allowed ${
                            isMove
                                ? 'bg-amber-500 hover:bg-amber-600'
                                : 'bg-blue-500 hover:bg-blue-600'
                        }`}
                    >
                        {busy && <Loader2 size={14} className="animate-spin" />}
                        {busy
                            ? t(isMove ? 'savedServers.relocateBusyMove' : 'savedServers.relocateBusyCopy')
                            : t(isMove ? 'savedServers.relocateSubmitMove' : 'savedServers.relocateSubmitCopy')}
                    </button>
                </div>
            </div>
        </div>
    );
};
