// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroShareContacts - the "Rubrica" (address book) for AeroShare friends.
 *
 * A single, reusable contacts manager (list / add / edit-alias / delete) backed
 * by the ONE authoritative contact store (`peer_contacts`, via the peerContact*
 * wrappers - the same source the Send dialog lists and the receive side resolves
 * for auto-accept + the per-sender inbox folder). Mounted in TWO places without
 * duplicating code: the third tab of the AeroShare dialog and the AeroShare
 * panel in Settings. Self-contained (its own state + inline error/confirm), so
 * it needs no surrounding context and renders the same in both hosts.
 */

import { useCallback, useEffect, useState } from 'react';
import { UserPlus, Pencil, Trash2, Check, X, Loader2, Users } from 'lucide-react';
import { useTranslation } from '../../i18n';
import {
  peerFriendsList,
  peerContactAdd,
  peerContactRemove,
  shortAfid,
  type PeerFriend,
} from '../../utils/aeroShare';

interface AeroShareContactsProps {
  /** Denser layout when embedded in the Settings page (vs the dialog). */
  compact?: boolean;
}

export function AeroShareContacts({ compact = false }: AeroShareContactsProps) {
  const t = useTranslation();

  const [contacts, setContacts] = useState<PeerFriend[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Add form.
  const [newAfid, setNewAfid] = useState('');
  const [newAlias, setNewAlias] = useState('');

  // Inline edit (alias) + delete-confirm, keyed by AFID.
  const [editAfid, setEditAfid] = useState<string | null>(null);
  const [editAlias, setEditAlias] = useState('');
  const [confirmAfid, setConfirmAfid] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await peerFriendsList();
      setContacts(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const addContact = useCallback(async () => {
    const afid = newAfid.trim();
    if (!afid || busy) return;
    setBusy(true);
    try {
      await peerContactAdd(afid, newAlias.trim());
      setNewAfid('');
      setNewAlias('');
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [newAfid, newAlias, busy, refresh]);

  const saveAlias = useCallback(
    async (afid: string) => {
      if (busy) return;
      setBusy(true);
      try {
        await peerContactAdd(afid, editAlias.trim());
        setEditAfid(null);
        await refresh();
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [editAlias, busy, refresh],
  );

  const removeContact = useCallback(
    async (afid: string) => {
      if (busy) return;
      setBusy(true);
      try {
        await peerContactRemove(afid);
        setConfirmAfid(null);
        await refresh();
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [busy, refresh],
  );

  const inputCls =
    'w-full px-2.5 py-1.5 text-xs rounded-md border border-gray-300 dark:border-gray-600 ' +
    'bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 focus:outline-none ' +
    'focus:ring-1 focus:ring-violet-500 focus:border-violet-500';
  const iconBtn =
    'p-1.5 rounded-md text-gray-500 dark:text-gray-400 hover:bg-gray-100 ' +
    'dark:hover:bg-gray-700 transition-colors disabled:opacity-50';

  return (
    <div className={compact ? 'space-y-3' : 'space-y-4'}>
      <p className="text-xs text-gray-500 dark:text-gray-400">{t('aeroShare.contacts.intro')}</p>

      {/* Add form */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <input
          className={inputCls}
          placeholder={t('aeroShare.contacts.afidPlaceholder')}
          value={newAfid}
          onChange={(e) => setNewAfid(e.target.value)}
          spellCheck={false}
        />
        <input
          className={`${inputCls} sm:max-w-[40%]`}
          placeholder={t('aeroShare.contacts.namePlaceholder')}
          value={newAlias}
          onChange={(e) => setNewAlias(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && addContact()}
        />
        <button
          onClick={addContact}
          disabled={!newAfid.trim() || busy}
          className="flex items-center justify-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md bg-violet-600 text-white hover:bg-violet-700 transition-colors disabled:opacity-50 whitespace-nowrap"
        >
          <UserPlus size={13} />
          {t('aeroShare.contacts.add')}
        </button>
      </div>

      {error && (
        <div className="px-3 py-2 text-xs rounded-md bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400 border border-red-200 dark:border-red-800">
          {error}
        </div>
      )}

      {/* List */}
      {loading ? (
        <div className="flex items-center gap-2 py-6 justify-center text-gray-400">
          <Loader2 size={16} className="animate-spin" />
        </div>
      ) : contacts.length === 0 ? (
        <div className="flex flex-col items-center gap-2 py-8 text-gray-400 dark:text-gray-500">
          <Users size={28} className="opacity-50" />
          <span className="text-xs">{t('aeroShare.contacts.empty')}</span>
        </div>
      ) : (
        <ul className="divide-y divide-gray-100 dark:divide-gray-700/60">
          {contacts.map((c) => {
            const editing = editAfid === c.afid;
            const confirming = confirmAfid === c.afid;
            return (
              <li key={c.afid} className="flex items-center gap-2 py-2">
                <div className="min-w-0 flex-1">
                  {editing ? (
                    <input
                      autoFocus
                      className={inputCls}
                      value={editAlias}
                      onChange={(e) => setEditAlias(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') saveAlias(c.afid);
                        if (e.key === 'Escape') setEditAfid(null);
                      }}
                    />
                  ) : (
                    <>
                      <div className="text-sm font-medium text-gray-800 dark:text-gray-100 truncate">
                        {c.alias || shortAfid(c.afid)}
                      </div>
                      <div className="text-[11px] font-mono text-gray-400 dark:text-gray-500 truncate">
                        {shortAfid(c.afid)}
                      </div>
                    </>
                  )}
                </div>

                {editing ? (
                  <>
                    <button
                      className={iconBtn}
                      title={t('aeroShare.contacts.save')}
                      disabled={busy}
                      onClick={() => saveAlias(c.afid)}
                    >
                      <Check size={15} className="text-green-600 dark:text-green-400" />
                    </button>
                    <button
                      className={iconBtn}
                      title={t('aeroShare.contacts.cancel')}
                      onClick={() => setEditAfid(null)}
                    >
                      <X size={15} />
                    </button>
                  </>
                ) : confirming ? (
                  <>
                    <span className="text-[11px] text-gray-500 dark:text-gray-400">
                      {t('aeroShare.contacts.removeConfirm')}
                    </span>
                    <button
                      className={iconBtn}
                      title={t('aeroShare.contacts.remove')}
                      disabled={busy}
                      onClick={() => removeContact(c.afid)}
                    >
                      <Check size={15} className="text-red-600 dark:text-red-400" />
                    </button>
                    <button
                      className={iconBtn}
                      title={t('aeroShare.contacts.cancel')}
                      onClick={() => setConfirmAfid(null)}
                    >
                      <X size={15} />
                    </button>
                  </>
                ) : (
                  <>
                    <button
                      className={iconBtn}
                      title={t('aeroShare.contacts.edit')}
                      onClick={() => {
                        setEditAfid(c.afid);
                        setEditAlias(c.alias || '');
                        setConfirmAfid(null);
                      }}
                    >
                      <Pencil size={14} />
                    </button>
                    <button
                      className={iconBtn}
                      title={t('aeroShare.contacts.remove')}
                      onClick={() => {
                        setConfirmAfid(c.afid);
                        setEditAfid(null);
                      }}
                    >
                      <Trash2 size={14} />
                    </button>
                  </>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
