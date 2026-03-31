// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback, useRef } from 'react';
import { X, Tag, Check, Loader2, Plus } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';

interface ZohoLabel {
  id: string;
  attributes: {
    name?: string;
    color?: string;
    label_id?: string;
    index?: number;
  };
}

interface ZohoLabelsDialogProps {
  filePath: string;
  onClose: () => void;
  onRefresh?: () => void;
}

const PRESET_COLORS = [
  'F83A22', 'FF7537', 'FBE983', '7BD148',
  '14A765', '16A5A5', '4986E7', 'B99AFF',
];

export function ZohoLabelsDialog({ filePath, onClose, onRefresh }: ZohoLabelsDialogProps) {
  const t = useTranslation();
  const [teamLabels, setTeamLabels] = useState<ZohoLabel[]>([]);
  const [fileLabels, setFileLabels] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [toggling, setToggling] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [newName, setNewName] = useState('');
  const [newColor, setNewColor] = useState(PRESET_COLORS[4]);
  const [creating, setCreating] = useState(false);
  const nameInputRef = useRef<HTMLInputElement>(null);

  const loadLabels = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [team, file] = await Promise.all([
        invoke<ZohoLabel[]>('zoho_list_team_labels'),
        invoke<ZohoLabel[]>('zoho_get_file_labels', { path: filePath }),
      ]);
      setTeamLabels(team);
      // Match file labels by id or by attributes.label_id
      const fileLabelIds = new Set<string>();
      for (const fl of file) {
        fileLabelIds.add(fl.id);
        if (fl.attributes.label_id) fileLabelIds.add(fl.attributes.label_id);
      }
      setFileLabels(fileLabelIds);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [filePath]);

  useEffect(() => {
    loadLabels();
  }, [loadLabels]);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  useEffect(() => {
    if (showCreate) nameInputRef.current?.focus();
  }, [showCreate]);

  const toggleLabel = async (labelId: string) => {
    setToggling(labelId);
    try {
      if (fileLabels.has(labelId)) {
        await invoke('zoho_remove_file_label', { path: filePath, labelId });
        setFileLabels(prev => { const next = new Set(prev); next.delete(labelId); return next; });
      } else {
        await invoke('zoho_add_file_label', { path: filePath, labelId });
        setFileLabels(prev => new Set(prev).add(labelId));
      }
      onRefresh?.();
    } catch (err) {
      setError(String(err));
    } finally {
      setToggling(null);
    }
  };

  const handleCreateLabel = async () => {
    if (!newName.trim() || creating) return;
    setCreating(true);
    setError(null);
    try {
      const created = await invoke<ZohoLabel>('zoho_create_label', { name: newName.trim(), color: newColor });
      setTeamLabels(prev => [...prev, created]);
      setNewName('');
      setShowCreate(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setCreating(false);
    }
  };

  const fileName = filePath.split('/').pop() || filePath;

  const getLabelHexColor = (color?: string): string => {
    if (!color) return '#10b981';
    // Zoho returns 6-digit hex codes without #
    return color.startsWith('#') ? color : `#${color}`;
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />
      <div
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-full max-w-md overflow-hidden animate-scale-in"
        role="dialog"
        aria-modal="true"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Tag size={16} className="text-emerald-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('zohoworkdrive.manageLabels')}
            </h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">
            <X size={16} className="text-gray-500" />
          </button>
        </div>

        {/* File name */}
        <div className="px-5 py-2 border-b border-gray-200 dark:border-gray-700/50">
          <p className="text-xs text-gray-500 dark:text-gray-400 truncate" title={filePath}>
            {fileName}
          </p>
        </div>

        {/* Content */}
        <div className="px-5 py-3 max-h-[50vh] overflow-y-auto">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 size={20} className="animate-spin text-gray-400" />
            </div>
          ) : error ? (
            <div className="text-center py-4">
              <p className="text-xs text-red-500">{error}</p>
              <button onClick={loadLabels} className="mt-2 text-xs text-blue-500 hover:underline">
                {t('common.retry')}
              </button>
            </div>
          ) : teamLabels.length === 0 && !showCreate ? (
            <p className="text-xs text-gray-500 dark:text-gray-400 text-center py-4">
              {t('zohoworkdrive.noLabels')}
            </p>
          ) : (
            <div className="space-y-1">
              {teamLabels.map(label => {
                const isApplied = fileLabels.has(label.id) || (label.attributes.label_id ? fileLabels.has(label.attributes.label_id) : false);
                const isLoading = toggling === label.id;
                return (
                  <button
                    key={label.id}
                    onClick={() => toggleLabel(label.id)}
                    disabled={isLoading}
                    className={`w-full flex items-center gap-3 px-3 py-2 rounded-lg text-left transition-colors ${
                      isApplied
                        ? 'bg-emerald-50 dark:bg-emerald-900/20 border border-emerald-200 dark:border-emerald-800'
                        : 'hover:bg-gray-50 dark:hover:bg-gray-800 border border-transparent'
                    }`}
                  >
                    <div className="w-3 h-3 rounded-full flex-shrink-0" style={{ backgroundColor: getLabelHexColor(label.attributes.color) }} />
                    <span className="text-sm text-gray-900 dark:text-gray-100 flex-1 truncate">
                      {label.attributes.name || label.id}
                    </span>
                    {isLoading ? (
                      <Loader2 size={14} className="animate-spin text-gray-400" />
                    ) : isApplied ? (
                      <Check size={14} className="text-emerald-500" />
                    ) : null}
                  </button>
                );
              })}
            </div>
          )}

          {/* Create Label Form */}
          {showCreate && (
            <div className="mt-3 p-3 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800">
              <input
                ref={nameInputRef}
                type="text"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') handleCreateLabel(); }}
                placeholder={t('zohoworkdrive.labelName')}
                className="w-full px-2.5 py-1.5 text-sm bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg focus:outline-none focus:ring-2 focus:ring-emerald-500 text-gray-900 dark:text-gray-100 placeholder-gray-400"
              />
              <div className="flex items-center gap-1.5 mt-2">
                {PRESET_COLORS.map(c => (
                  <button
                    key={c}
                    onClick={() => setNewColor(c)}
                    className={`w-5 h-5 rounded-full transition-all ${newColor === c ? 'ring-2 ring-offset-1 ring-emerald-500 dark:ring-offset-gray-800' : 'hover:scale-110'}`}
                    style={{ backgroundColor: `#${c}` }}
                  />
                ))}
              </div>
              <div className="flex justify-end gap-2 mt-2.5">
                <button
                  onClick={() => { setShowCreate(false); setNewName(''); }}
                  className="px-2.5 py-1 text-xs rounded-lg text-gray-600 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700"
                >
                  {t('common.cancel')}
                </button>
                <button
                  onClick={handleCreateLabel}
                  disabled={!newName.trim() || creating}
                  className="flex items-center gap-1 px-2.5 py-1 text-xs rounded-lg bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {creating && <Loader2 size={10} className="animate-spin" />}
                  {t('common.create')}
                </button>
              </div>
            </div>
          )}
        </div>

        {/* Footer — Create Label button */}
        {!loading && !showCreate && (
          <div className="px-5 py-3 border-t border-gray-200 dark:border-gray-700">
            <button
              onClick={() => setShowCreate(true)}
              className="flex items-center gap-1.5 text-xs text-emerald-600 dark:text-emerald-400 hover:text-emerald-700 dark:hover:text-emerald-300"
            >
              <Plus size={14} />
              {t('zohoworkdrive.createLabel')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
