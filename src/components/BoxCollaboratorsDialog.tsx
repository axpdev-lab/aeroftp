// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef, useCallback } from 'react';
import { X, Users, UserPlus, Loader2, Trash2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { useModalFocusTrap } from '../hooks/useModalFocusTrap';
import { useDraggableModal } from '../hooks/useDraggableModal';
import {
  addBoxCollaborator,
  BOX_ROLES,
  listBoxCollaborators,
  removeBoxCollaborator,
  type BoxCollaborator,
  type BoxRole,
} from '../utils/boxDriveSocial';

interface BoxCollaboratorsDialogProps {
  path: string;
  name: string;
  onClose: () => void;
}

const ROLE_KEY: Record<BoxRole, string> = {
  editor: 'box.roleEditor',
  viewer: 'box.roleViewer',
  previewer: 'box.rolePreviewer',
  uploader: 'box.roleUploader',
  'previewer uploader': 'box.rolePreviewerUploader',
  'viewer uploader': 'box.roleViewerUploader',
  'co-owner': 'box.roleCoOwner',
};


/** Who a Box file or folder is shared with: list, invite by email with a role, remove. */
export function BoxCollaboratorsDialog({ path, name, onClose }: BoxCollaboratorsDialogProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const [collaborators, setCollaborators] = useState<BoxCollaborator[] | null>(null);
  const [email, setEmail] = useState('');
  const [role, setRole] = useState<BoxRole>('viewer');
  const [adding, setAdding] = useState(false);
  const [removingId, setRemovingId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const emailRef = useRef<HTMLInputElement>(null);

  const roleLabel = (r: string) => (r in ROLE_KEY ? t(ROLE_KEY[r as BoxRole]) : r);

  const reload = useCallback(async () => {
    try {
      setCollaborators(await listBoxCollaborators(invoke, path));
      setLoadError(null);
    } catch (err) {
      // A failed load is not an empty share list: keep what is on screen and say why.
      setLoadError(String(err));
    }
  }, [path]);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  useModalFocusTrap(panelRef, emailRef);

  const handleAdd = async () => {
    if (!email.trim() || adding) return;
    setAdding(true);
    setError(null);
    setNotice(null);
    try {
      await addBoxCollaborator(invoke, path, email, role);
      setEmail('');
      setNotice(t('box.collaboratorAdded'));
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setAdding(false);
    }
  };

  const handleRemove = async (id: string) => {
    setRemovingId(id);
    setError(null);
    setNotice(null);
    try {
      await removeBoxCollaborator(invoke, id);
      setCollaborators((prev) => (prev ?? []).filter((c) => c.id !== id));
      setNotice(t('box.collaboratorRemoved'));
    } catch (err) {
      setError(String(err));
    } finally {
      setRemovingId(null);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />
      <div
        {...modalDrag.panelProps}
        ref={panelRef}
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-full max-w-md overflow-hidden animate-scale-in"
        role="dialog"
        aria-modal="true"
        aria-label={t('box.collaborators')}
      >
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Users size={16} className="text-blue-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('box.collaborators')}</h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700" aria-label={t('common.close')}>
            <X size={16} className="text-gray-500" />
          </button>
        </div>

        <div className="px-5 py-2 border-b border-gray-200 dark:border-gray-700/50">
          <p className="text-xs text-gray-500 dark:text-gray-400 truncate" title={path}>{name}</p>
        </div>

        <div className="px-5 py-3 max-h-64 overflow-y-auto space-y-1.5">
          {loadError && <p className="text-xs text-red-500 py-2">{loadError}</p>}
          {collaborators === null ? (
            !loadError && <div className="flex justify-center py-4"><Loader2 size={16} className="animate-spin text-gray-400" /></div>
          ) : collaborators.length === 0 ? (
            <p className="text-xs text-gray-500 dark:text-gray-400 py-2">{t('box.noCollaborators')}</p>
          ) : (
            collaborators.map((c) => (
              <div key={c.id} className="flex items-center justify-between gap-2 rounded-lg border border-gray-200 dark:border-gray-700 px-2 py-1.5">
                <div className="min-w-0">
                  <div className="text-xs text-gray-900 dark:text-gray-100 truncate">{c.who}</div>
                  <div className="text-[11px] text-gray-500 dark:text-gray-400">{roleLabel(c.role)}</div>
                </div>
                <button
                  onClick={() => void handleRemove(c.id)}
                  disabled={removingId === c.id}
                  className="p-1 rounded text-red-500 hover:bg-red-500/10 disabled:opacity-50"
                  title={t('common.remove')}
                  aria-label={t('common.remove')}
                >
                  {removingId === c.id ? <Loader2 size={12} className="animate-spin" /> : <Trash2 size={12} />}
                </button>
              </div>
            ))
          )}
        </div>

        <div className="px-5 py-3 border-t border-gray-200 dark:border-gray-700/50 space-y-2">
          <div className="text-[11px] uppercase tracking-wide text-gray-500">{t('box.addCollaborator')}</div>
          <input
            ref={emailRef}
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void handleAdd(); } }}
            placeholder={t('box.collaboratorEmail')}
            className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg focus:outline-none focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100 placeholder-gray-400"
          />
          <label className="flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
            {t('box.collaboratorRole')}
            <select
              value={role}
              onChange={(e) => setRole(e.target.value as BoxRole)}
              className="flex-1 text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-2 py-1 dark:bg-gray-800"
            >
              {BOX_ROLES.map((r) => <option key={r} value={r}>{roleLabel(r)}</option>)}
            </select>
          </label>
          {error && <p className="text-xs text-red-500">{error}</p>}
          {notice && !error && <p className="text-xs text-green-500">{notice}</p>}
        </div>

        <div className="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-xs rounded-lg border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800"
          >
            {t('common.close')}
          </button>
          <button
            onClick={() => void handleAdd()}
            disabled={!email.trim() || adding}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {adding ? <Loader2 size={12} className="animate-spin" /> : <UserPlus size={12} />}
            {t('box.addCollaborator')}
          </button>
        </div>
      </div>
    </div>
  );
}
