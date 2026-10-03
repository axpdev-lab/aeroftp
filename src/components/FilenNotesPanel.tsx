// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  X, Plus, Loader2, Star, Pin, Trash2, Archive, Lock, Save,
  RotateCcw, Clock, Tag, ChevronLeft, FileText, Code, CheckSquare,
  Hash, Check
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { SearchBox } from './SearchBox';
import { formatDate as formatDateFull } from '../utils';
import { useHumanizedLog } from '../hooks/useHumanizedLog';
import { changeNoteType, createTagOnNote, deleteTag, renameTag, tagNote, untagNote } from '../utils/filenNoteTags';

// ─── Types ───

interface FilenNote {
  uuid: string;
  title: string;
  preview: string;
  noteType: 'text' | 'md' | 'code' | 'rich' | 'checklist';
  favorite: boolean;
  pinned: boolean;
  trash: boolean;
  archive: boolean;
  createdTimestamp: number;
  editedTimestamp: number;
  tags: { uuid: string }[];
  participants: { userId: number; isOwner: boolean; email: string; permissionsWrite: boolean }[];
}

interface FilenNoteContent {
  content: string;
  preview: string;
  noteType: string;
  editedTimestamp: number;
  editorId: number;
}

interface FilenNoteHistoryEntry {
  id: number;
  content: string;
  preview: string;
  noteType: string;
  editedTimestamp: number;
  editorId: number;
}

interface FilenNoteTag {
  uuid: string;
  name: string;
  favorite: boolean;
  createdTimestamp: number;
  editedTimestamp: number;
}

type NoteFilter = 'all' | 'favorites' | 'pinned' | 'archived' | 'trash';
type NoteTypeOption = 'text' | 'md' | 'code' | 'rich' | 'checklist';

interface FilenNotesPanelProps {
  isOpen: boolean;
  onClose: () => void;
}

// ─── Constants ───

const NOTE_TYPE_ICONS: Record<NoteTypeOption, React.ReactNode> = {
  text: <FileText size={12} />,
  md: <Hash size={12} />,
  code: <Code size={12} />,
  rich: <FileText size={12} />,
  checklist: <CheckSquare size={12} />,
};

const NOTE_TYPE_LABELS: Record<NoteTypeOption, string> = {
  text: 'Text',
  md: 'Markdown',
  code: 'Code',
  rich: 'Rich Text',
  checklist: 'Checklist',
};

// ─── Component ───

export function FilenNotesPanel({ isOpen, onClose }: FilenNotesPanelProps) {
  const t = useTranslation();
  const humanLog = useHumanizedLog();

  // State: list view
  const [notes, setNotes] = useState<FilenNote[]>([]);
  const [tags, setTags] = useState<FilenNoteTag[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<NoteFilter>('all');
  const [searchQuery, setSearchQuery] = useState('');

  // State: editor view
  const [selectedNote, setSelectedNote] = useState<FilenNote | null>(null);
  const [noteContent, setNoteContent] = useState('');
  const [noteTitle, setNoteTitle] = useState('');
  const [noteType, setNoteType] = useState<NoteTypeOption>('text');
  const [loadingContent, setLoadingContent] = useState(false);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);

  // State: history view
  const [history, setHistory] = useState<FilenNoteHistoryEntry[] | null>(null);
  const [loadingHistory, setLoadingHistory] = useState(false);

  // State: create note
  const [creating, setCreating] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newType, setNewType] = useState<NoteTypeOption>('text');
  const [showCreateForm, setShowCreateForm] = useState(false);

  // State: tags
  const [newTagName, setNewTagName] = useState<string | null>(null);
  const [showTagManager, setShowTagManager] = useState(false);
  const [renamingTag, setRenamingTag] = useState<{ uuid: string; name: string } | null>(null);
  const [pendingDeleteTag, setPendingDeleteTag] = useState<FilenNoteTag | null>(null);

  // Refs
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const saveTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The open note, so a late request result is applied only to the note it was made on
  const selectedUuidRef = useRef<string | null>(null);
  const creatingTagRef = useRef(false);
  // A second type change while one is pending could leave the server and the
  // editor on different types when the first one fails: the selector waits.
  const [typeChangePending, setTypeChangePending] = useState(false);

  useEffect(() => {
    selectedUuidRef.current = selectedNote?.uuid ?? null;
  }, [selectedNote]);

  // ── Data loading ──

  const loadNotes = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [noteList, tagList] = await Promise.all([
        invoke<FilenNote[]>('filen_notes_list'),
        invoke<FilenNoteTag[]>('filen_notes_tags_list'),
      ]);
      setNotes(noteList);
      setTags(tagList);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (isOpen) {
      loadNotes();
    }
    return () => {
      if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
    };
  }, [isOpen, loadNotes]);

  // ── Note actions ──

  const openNote = useCallback(async (note: FilenNote) => {
    setSelectedNote(note);
    setNoteTitle(note.title);
    setNoteType(note.noteType);
    setLoadingContent(true);
    setDirty(false);
    setHistory(null);
    try {
      const content = await invoke<FilenNoteContent>('filen_notes_get_content', { uuid: note.uuid });
      setNoteContent(content.content);
      setNoteType(content.noteType as NoteTypeOption);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoadingContent(false);
    }
  }, []);

  const handleBackToList = useCallback(async () => {
    // Cancel pending auto-save to prevent stale overwrites
    if (saveTimeoutRef.current) {
      clearTimeout(saveTimeoutRef.current);
      saveTimeoutRef.current = null;
    }
    if (dirty && selectedNote) {
      setSaving(true);
      try {
        await invoke('filen_notes_edit_content', {
          uuid: selectedNote.uuid,
          content: noteContent,
          noteType: noteType,
        });
        if (noteTitle !== selectedNote.title) {
          await invoke('filen_notes_edit_title', {
            uuid: selectedNote.uuid,
            title: noteTitle,
          });
        }
      } catch (err) {
        setError(String(err));
      } finally {
        setSaving(false);
      }
    }
    setSelectedNote(null);
    setNoteContent('');
    setDirty(false);
    loadNotes();
  }, [dirty, selectedNote, noteContent, noteTitle, noteType, loadNotes]);

  // ── Keyboard ──

  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (history) {
          setHistory(null);
        } else if (selectedNote) {
          handleBackToList();
        } else {
          onClose();
        }
      }
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [isOpen, selectedNote, history, onClose, handleBackToList]);

  const handleContentChange = useCallback((value: string) => {
    setNoteContent(value);
    setDirty(true);

    // Auto-save after 2s of inactivity
    if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
    saveTimeoutRef.current = setTimeout(async () => {
      if (!selectedNote) return;
      setSaving(true);
      try {
        await invoke('filen_notes_edit_content', {
          uuid: selectedNote.uuid,
          content: value,
          noteType: noteType,
        });
        setDirty(false);
      } catch (err) {
        setError(String(err));
      } finally {
        setSaving(false);
      }
    }, 2000);
  }, [selectedNote, noteType]);

  const handleTitleChange = useCallback((value: string) => {
    setNoteTitle(value);
    setDirty(true);
  }, []);

  const saveNow = useCallback(async () => {
    if (!selectedNote || saving) return;
    // Cancel pending auto-save
    if (saveTimeoutRef.current) {
      clearTimeout(saveTimeoutRef.current);
      saveTimeoutRef.current = null;
    }
    setSaving(true);
    const logId = humanLog.logRaw('activity.provider_operation', 'INFO', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_edit_content', {
        uuid: selectedNote.uuid,
        content: noteContent,
        noteType: noteType,
      });
      if (noteTitle !== selectedNote.title) {
        await invoke('filen_notes_edit_title', {
          uuid: selectedNote.uuid,
          title: noteTitle,
        });
      }
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] Saved note "${noteTitle}"` });
      setDirty(false);
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to save note' });
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }, [selectedNote, saving, noteContent, noteTitle, noteType, humanLog]);

  const createNote = useCallback(async () => {
    if (!newTitle.trim()) return;
    setCreating(true);
    const logId = humanLog.logRaw('activity.provider_operation', 'INFO', { provider: 'Filen' }, 'running');
    try {
      const uuid = await invoke<string>('filen_notes_create', {
        title: newTitle.trim(),
        content: '',
        noteType: newType,
      });
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] Created note "${newTitle.trim()}"` });
      const createdTitle = newTitle.trim();
      const createdType = newType;
      setShowCreateForm(false);
      setNewTitle('');
      // Reload and auto-open the created note from fresh data
      const freshNotes = await invoke<FilenNote[]>('filen_notes_list');
      setNotes(freshNotes);
      const created = freshNotes.find(n => n.uuid === uuid);
      if (created) {
        openNote(created);
      } else {
        // Fallback: open editor directly with known data
        setSelectedNote({
          uuid, title: createdTitle, preview: '', noteType: createdType,
          favorite: false, pinned: false, trash: false, archive: false,
          createdTimestamp: Math.floor(Date.now() / 1000),
          editedTimestamp: Math.floor(Date.now() / 1000),
          tags: [], participants: [],
        });
        setNoteTitle(createdTitle);
        setNoteType(createdType);
        setNoteContent('');
        setDirty(false);
      }
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to create note' });
      setError(String(err));
    } finally {
      setCreating(false);
    }
  }, [newTitle, newType, openNote, humanLog]);

  const toggleFavorite = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    const action = note.favorite ? 'Unfavorited' : 'Favorited';
    const logId = humanLog.logRaw('activity.provider_operation', 'INFO', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_toggle_favorite', { uuid: note.uuid, favorite: !note.favorite });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, favorite: !n.favorite } : n));
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] ${action} note "${note.title}"` });
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to toggle favorite' });
      setError(String(err));
    }
  }, [humanLog]);

  const togglePinned = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    const action = note.pinned ? 'Unpinned' : 'Pinned';
    const logId = humanLog.logRaw('activity.provider_operation', 'INFO', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_toggle_pinned', { uuid: note.uuid, pinned: !note.pinned });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, pinned: !n.pinned } : n));
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] ${action} note "${note.title}"` });
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to toggle pinned' });
      setError(String(err));
    }
  }, [humanLog]);

  const trashNote = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    const logId = humanLog.logRaw('activity.provider_operation', 'INFO', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_trash', { uuid: note.uuid });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, trash: true } : n));
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] Trashed note "${note.title}"` });
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to trash note' });
      setError(String(err));
    }
  }, [humanLog]);

  const restoreNote = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    const logId = humanLog.logRaw('activity.provider_operation', 'RESTORE', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_restore', { uuid: note.uuid });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, trash: false, archive: false } : n));
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] Restored note "${note.title}"` });
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to restore note' });
      setError(String(err));
    }
  }, [humanLog]);

  const deleteNotePermanently = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    const logId = humanLog.logRaw('activity.provider_operation', 'DELETE', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_delete', { uuid: note.uuid });
      setNotes(prev => prev.filter(n => n.uuid !== note.uuid));
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] Permanently deleted note "${note.title}"` });
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to permanently delete note' });
      setError(String(err));
    }
  }, [humanLog]);

  const archiveNote = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    const logId = humanLog.logRaw('activity.provider_operation', 'INFO', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_archive', { uuid: note.uuid });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, archive: true } : n));
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] Archived note "${note.title}"` });
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to archive note' });
      setError(String(err));
    }
  }, [humanLog]);

  // ── History ──

  const loadHistory = useCallback(async () => {
    if (!selectedNote) return;
    setLoadingHistory(true);
    try {
      const entries = await invoke<FilenNoteHistoryEntry[]>('filen_notes_history', { uuid: selectedNote.uuid });
      setHistory(entries);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoadingHistory(false);
    }
  }, [selectedNote]);

  const restoreVersion = useCallback(async (historyId: number) => {
    if (!selectedNote) return;
    const logId = humanLog.logRaw('activity.provider_operation', 'RESTORE', { provider: 'Filen' }, 'running');
    try {
      await invoke('filen_notes_history_restore', { uuid: selectedNote.uuid, historyId });
      humanLog.updateEntry(logId, { status: 'success', message: `[Filen] Restored note version` });
      setHistory(null);
      openNote(selectedNote);
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: '[Filen] Failed to restore note version' });
      setError(String(err));
    }
  }, [selectedNote, openNote, humanLog]);

  // ── Filtering ──

  const filteredNotes = React.useMemo(() => {
    let result = notes;

    switch (filter) {
      case 'favorites':
        result = result.filter(n => n.favorite && !n.trash && !n.archive);
        break;
      case 'pinned':
        result = result.filter(n => n.pinned && !n.trash && !n.archive);
        break;
      case 'archived':
        result = result.filter(n => n.archive && !n.trash);
        break;
      case 'trash':
        result = result.filter(n => n.trash);
        break;
      default:
        result = result.filter(n => !n.trash && !n.archive);
    }

    if (searchQuery.trim()) {
      const q = searchQuery.toLowerCase();
      result = result.filter(
        n => n.title.toLowerCase().includes(q) || n.preview.toLowerCase().includes(q)
      );
    }

    // Pinned first, then by edited timestamp
    return result.sort((a, b) => {
      if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
      return b.editedTimestamp - a.editedTimestamp;
    });
  }, [notes, filter, searchQuery]);

  // ── Note type and tags ──

  // The type goes through Filen's own change endpoint: sent only with the next
  // content save, a type change without a text edit was lost on reload.
  const handleTypeChange = useCallback(async (next: NoteTypeOption) => {
    if (!selectedNote) return;
    const previous = noteType;
    // A pending auto-save carries the old type and, landing after the change,
    // would put it back: cancel it and save the edit ahead of the change.
    if (saveTimeoutRef.current) {
      clearTimeout(saveTimeoutRef.current);
      saveTimeoutRef.current = null;
    }
    setNoteType(next);
    setTypeChangePending(true);
    try {
      if (dirty) {
        await invoke('filen_notes_edit_content', { uuid: selectedNote.uuid, content: noteContent, noteType: previous });
      }
      await changeNoteType(invoke, selectedNote.uuid, next);
      setNotes(prev => prev.map(n => (n.uuid === selectedNote.uuid ? { ...n, noteType: next } : n)));
      setSelectedNote(prev => (prev && prev.uuid === selectedNote.uuid ? { ...prev, noteType: next } : prev));
    } catch (err) {
      if (selectedUuidRef.current === selectedNote.uuid) setNoteType(previous);
      setError(String(err));
    } finally {
      setTypeChangePending(false);
    }
  }, [selectedNote, noteType, dirty, noteContent]);

  const setNoteTags = useCallback((noteUuid: string, update: (tags: { uuid: string }[]) => { uuid: string }[]) => {
    setNotes(prev => prev.map(n => (n.uuid === noteUuid ? { ...n, tags: update(n.tags) } : n)));
    setSelectedNote(prev => (prev && prev.uuid === noteUuid ? { ...prev, tags: update(prev.tags) } : prev));
  }, []);

  const reloadTags = useCallback(async () => {
    setTags(await invoke<FilenNoteTag[]>('filen_notes_tags_list'));
  }, []);

  const handleAddTag = useCallback(async (tagUuid: string) => {
    if (!selectedNote) return;
    try {
      await tagNote(invoke, selectedNote.uuid, tagUuid);
      setNoteTags(selectedNote.uuid, current => [...current, { uuid: tagUuid }]);
    } catch (err) {
      setError(String(err));
    }
  }, [selectedNote, setNoteTags]);

  const handleRemoveTag = useCallback(async (tagUuid: string) => {
    if (!selectedNote) return;
    try {
      await untagNote(invoke, selectedNote.uuid, tagUuid);
      setNoteTags(selectedNote.uuid, current => current.filter(tg => tg.uuid !== tagUuid));
    } catch (err) {
      setError(String(err));
    }
  }, [selectedNote, setNoteTags]);

  const handleCreateTag = useCallback(async () => {
    // Enter pressed again while the request runs would create the tag twice
    if (!selectedNote || !newTagName?.trim() || creatingTagRef.current) return;
    creatingTagRef.current = true;
    try {
      const tagUuid = await createTagOnNote(invoke, selectedNote.uuid, newTagName);
      setNoteTags(selectedNote.uuid, current => [...current, { uuid: tagUuid }]);
      setNewTagName(null);
      await reloadTags();
    } catch (err) {
      setError(String(err));
    } finally {
      creatingTagRef.current = false;
    }
  }, [selectedNote, newTagName, setNoteTags, reloadTags]);

  const handleRenameTag = useCallback(async () => {
    if (!renamingTag?.name.trim()) return;
    try {
      await renameTag(invoke, renamingTag.uuid, renamingTag.name);
      setRenamingTag(null);
      await reloadTags();
    } catch (err) {
      setError(String(err));
    }
  }, [renamingTag, reloadTags]);

  const handleDeleteTag = useCallback(async (tag: FilenNoteTag) => {
    setPendingDeleteTag(null);
    try {
      await deleteTag(invoke, tag.uuid);
      setNotes(prev => prev.map(n => ({ ...n, tags: n.tags.filter(tg => tg.uuid !== tag.uuid) })));
      setSelectedNote(prev => (prev ? { ...prev, tags: prev.tags.filter(tg => tg.uuid !== tag.uuid) } : prev));
      await reloadTags();
    } catch (err) {
      setError(String(err));
    }
  }, [reloadTags]);

  // ── Tag resolution ──

  const getTagName = useCallback((tagUuid: string) => {
    return tags.find(t => t.uuid === tagUuid)?.name || tagUuid.slice(0, 8);
  }, [tags]);

  const formatDate = useCallback((ts: number) => {
    if (!ts) return '';
    return formatDateFull(new Date(ts * 1000));
  }, []);

  if (!isOpen) return null;

  // ─── Render: History view ───
  const renderHistory = () => (
    <div className="flex flex-col h-full">
      <div className="flex items-center gap-2 px-4 py-3 border-b border-gray-200 dark:border-gray-700">
        <button onClick={() => setHistory(null)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">
          <ChevronLeft size={16} />
        </button>
        <Clock size={14} className="text-blue-400" />
        <span className="text-sm font-medium text-gray-900 dark:text-gray-100">
          {t('filenNotes.history')}
        </span>
      </div>
      <div className="flex-1 overflow-y-auto">
        {loadingHistory ? (
          <div className="flex items-center justify-center py-12">
            <Loader2 size={20} className="animate-spin text-gray-400" />
          </div>
        ) : history && history.length === 0 ? (
          <p className="text-center text-gray-500 text-sm py-12">{t('filenNotes.noHistory')}</p>
        ) : (
          history?.map(entry => (
            <div key={entry.id} className="px-4 py-3 border-b border-gray-100 dark:border-gray-700/50 hover:bg-gray-50 dark:hover:bg-gray-700/30">
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-500">{formatDate(entry.editedTimestamp)}</span>
                <button
                  onClick={() => restoreVersion(entry.id)}
                  className="text-xs text-blue-500 hover:text-blue-400 flex items-center gap-1"
                >
                  <RotateCcw size={11} />
                  {t('filenNotes.restore')}
                </button>
              </div>
              <p className="text-sm text-gray-700 dark:text-gray-300 mt-1 line-clamp-2">{entry.preview || entry.content.slice(0, 100)}</p>
            </div>
          ))
        )}
      </div>
    </div>
  );

  // ─── Render: Editor view ───
  const renderEditor = () => (
    <div className="flex flex-col h-full">
      {/* Editor header */}
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-gray-200 dark:border-gray-700">
        <button onClick={handleBackToList} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500">
          <ChevronLeft size={16} />
        </button>
        <input
          type="text"
          value={noteTitle}
          onChange={e => handleTitleChange(e.target.value)}
          className="flex-1 bg-transparent text-sm font-medium text-gray-900 dark:text-gray-100 outline-none placeholder-gray-400"
          placeholder={t('filenNotes.untitled')}
        />
        <div className="flex items-center gap-1.5">
          {saving ? (
            <span className="flex items-center gap-1 text-[10px] text-blue-400">
              <Loader2 size={11} className="animate-spin" />
              {t('filenNotes.saving')}
            </span>
          ) : dirty ? (
            <span className="flex items-center gap-1 text-[10px] text-amber-400">
              <span className="w-1.5 h-1.5 rounded-full bg-amber-400" />
              {t('filenNotes.unsaved')}
            </span>
          ) : selectedNote && noteContent ? (
            <span className="flex items-center gap-1 text-[10px] text-green-400">
              <Check size={10} />
              {t('filenNotes.saved')}
            </span>
          ) : null}
          <button
            onClick={saveNow}
            disabled={!dirty || saving}
            className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 disabled:opacity-30 disabled:cursor-default"
            title={t('filenNotes.saveNow')}
          >
            <Save size={13} />
          </button>
          <select
            value={noteType}
            onChange={e => void handleTypeChange(e.target.value as NoteTypeOption)}
            disabled={typeChangePending}
            aria-label={t('filenNotes.noteType')}
            className="text-xs bg-gray-100 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded px-1.5 py-0.5 text-gray-700 dark:text-gray-300"
          >
            {Object.entries(NOTE_TYPE_LABELS).map(([k, label]) => (
              <option key={k} value={k}>{label}</option>
            ))}
          </select>
          <button
            onClick={loadHistory}
            disabled={loadingHistory}
            className="p-1.5 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500"
            title={t('filenNotes.history')}
          >
            <Clock size={13} />
          </button>
        </div>
      </div>

      {/* Editor body */}
      {loadingContent ? (
        <div className="flex-1 flex items-center justify-center">
          <Loader2 size={20} className="animate-spin text-gray-400" />
        </div>
      ) : (
        <textarea
          ref={editorRef}
          value={noteContent}
          onChange={e => handleContentChange(e.target.value)}
          className="flex-1 w-full resize-none bg-transparent text-sm text-gray-800 dark:text-gray-200 px-4 py-3 outline-none font-mono leading-relaxed placeholder-gray-400"
          placeholder={t('filenNotes.startTyping')}
          spellCheck={false}
        />
      )}

      {/* Editor footer: tags (add, remove, create; rename and delete in the manager) */}
      {selectedNote && (
        <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-700 space-y-1.5">
          <div className="flex flex-wrap items-center gap-1">
            <Tag size={11} className="text-gray-400" />
            {selectedNote.tags.map(tag => (
              <span key={tag.uuid} className="inline-flex items-center gap-0.5 text-xs bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 px-1.5 py-0.5 rounded">
                {getTagName(tag.uuid)}
                <button
                  onClick={() => void handleRemoveTag(tag.uuid)}
                  className="hover:text-red-500"
                  title={t('filenNotes.removeTag')}
                  aria-label={t('filenNotes.removeTag')}
                >
                  <X size={10} />
                </button>
              </span>
            ))}
            {newTagName === null ? (
              <select
                value=""
                onChange={e => {
                  if (e.target.value === '__new__') setNewTagName('');
                  else if (e.target.value) void handleAddTag(e.target.value);
                }}
                aria-label={t('filenNotes.addTag')}
                className="text-xs bg-transparent border border-dashed border-gray-300 dark:border-gray-600 rounded px-1 py-0.5 text-gray-500"
              >
                <option value="">{t('filenNotes.addTag')}</option>
                {tags.filter(tg => !selectedNote.tags.some(st => st.uuid === tg.uuid)).map(tg => (
                  <option key={tg.uuid} value={tg.uuid}>{tg.name}</option>
                ))}
                <option value="__new__">{t('filenNotes.newTag')}</option>
              </select>
            ) : (
              <input
                autoFocus
                value={newTagName}
                onChange={e => setNewTagName(e.target.value)}
                onKeyDown={e => {
                  if (e.key === 'Enter' && !e.nativeEvent.isComposing) { e.preventDefault(); void handleCreateTag(); }
                  if (e.key === 'Escape') { e.stopPropagation(); setNewTagName(null); }
                }}
                onBlur={() => { if (!newTagName.trim()) setNewTagName(null); }}
                placeholder={t('filenNotes.tagName')}
                aria-label={t('filenNotes.tagName')}
                className="text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1.5 py-0.5 w-28"
              />
            )}
            {tags.length > 0 && (
              <button
                onClick={() => setShowTagManager(v => !v)}
                className="ml-auto text-[11px] text-gray-500 hover:underline"
              >
                {t('filenNotes.manageTags')}
              </button>
            )}
          </div>
          {showTagManager && (
            <div className="space-y-1">
              {tags.map(tg => (
                <div key={tg.uuid} className="flex items-center gap-1.5 text-xs">
                  {renamingTag?.uuid === tg.uuid ? (
                    <input
                      autoFocus
                      value={renamingTag.name}
                      onChange={e => setRenamingTag({ uuid: tg.uuid, name: e.target.value })}
                      onKeyDown={e => {
                        if (e.key === 'Enter' && !e.nativeEvent.isComposing) { e.preventDefault(); void handleRenameTag(); }
                        if (e.key === 'Escape') { e.stopPropagation(); setRenamingTag(null); }
                      }}
                      aria-label={t('filenNotes.tagName')}
                      className="flex-1 bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1.5 py-0.5"
                    />
                  ) : (
                    <span className="flex-1 truncate text-gray-700 dark:text-gray-300">{tg.name}</span>
                  )}
                  {pendingDeleteTag?.uuid === tg.uuid ? (
                    <>
                      <span className="text-red-500">{t('filenNotes.deleteTagConfirm', { name: tg.name })}</span>
                      <button onClick={() => void handleDeleteTag(tg)} className="text-red-500 hover:underline">{t('common.delete')}</button>
                      <button onClick={() => setPendingDeleteTag(null)} className="text-gray-500 hover:underline">{t('common.cancel')}</button>
                    </>
                  ) : (
                    <>
                      <button onClick={() => setRenamingTag({ uuid: tg.uuid, name: tg.name })} className="text-gray-500 hover:underline">{t('common.rename')}</button>
                      <button onClick={() => setPendingDeleteTag(tg)} className="text-red-500 hover:underline">{t('common.delete')}</button>
                    </>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );

  // ─── Render: List view ───
  const renderList = () => (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-gray-200 dark:border-gray-700">
        <SearchBox
          value={searchQuery}
          onChange={setSearchQuery}
          placeholder={t('filenNotes.searchPlaceholder')}
          iconSize={12}
          containerClassName="flex-1"
          className="w-full pl-2.5 pr-2 py-1.5 text-xs bg-gray-100 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded text-gray-800 dark:text-gray-200 outline-none placeholder-gray-400 focus:border-blue-400"
        />
        <button
          onClick={() => { setShowCreateForm(true); setNewTitle(''); }}
          className="flex-shrink-0 flex items-center gap-1 px-2 py-1.5 rounded bg-blue-500 hover:bg-blue-600 text-white transition-colors"
          title={t('filenNotes.createNote')}
        >
          <Plus size={14} />
          <span className="text-[9px] font-bold opacity-70">BETA</span>
        </button>
      </div>

      {/* Filter tabs */}
      <div className="flex items-center gap-0.5 px-4 py-1.5 border-b border-gray-200 dark:border-gray-700 overflow-x-auto">
        {(['all', 'favorites', 'pinned', 'archived', 'trash'] as NoteFilter[]).map(f => (
          <button
            key={f}
            onClick={() => setFilter(f)}
            className={`px-2.5 py-1 text-xs rounded whitespace-nowrap transition-colors ${
              filter === f
                ? 'bg-blue-500/10 text-blue-500 font-medium'
                : 'text-gray-500 hover:text-gray-700 dark:hover:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700'
            }`}
          >
            {t(`filenNotes.filter.${f}`)}
          </button>
        ))}
      </div>

      {/* Create form */}
      {showCreateForm && (
        <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-700 bg-blue-50 dark:bg-blue-900/10">
          <div className="flex items-center gap-2">
            <input
              type="text"
              value={newTitle}
              onChange={e => setNewTitle(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter') createNote(); if (e.key === 'Escape') setShowCreateForm(false); }}
              className="flex-1 text-sm bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-2.5 py-1.5 text-gray-800 dark:text-gray-200 outline-none focus:border-blue-400"
              placeholder={t('filenNotes.newNotePlaceholder')}
              autoFocus
            />
            <select
              value={newType}
              onChange={e => setNewType(e.target.value as NoteTypeOption)}
              className="text-xs bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-1.5 py-1.5 text-gray-700 dark:text-gray-300"
            >
              {Object.entries(NOTE_TYPE_LABELS).map(([k, label]) => (
                <option key={k} value={k}>{label}</option>
              ))}
            </select>
            <button
              onClick={createNote}
              disabled={creating || !newTitle.trim()}
              className="px-3 py-1.5 text-xs bg-blue-500 hover:bg-blue-600 disabled:opacity-50 text-white rounded transition-colors"
            >
              {creating ? <Loader2 size={12} className="animate-spin" /> : t('filenNotes.create')}
            </button>
            <button
              onClick={() => setShowCreateForm(false)}
              className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-500"
            >
              <X size={14} />
            </button>
          </div>
        </div>
      )}

      {/* Notes list */}
      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Loader2 size={20} className="animate-spin text-gray-400" />
          </div>
        ) : error ? (
          <div className="px-4 py-8 text-center">
            <p className="text-sm text-red-500">{error}</p>
            <button onClick={loadNotes} className="mt-2 text-xs text-blue-500 hover:underline">{t('common.retry')}</button>
          </div>
        ) : filteredNotes.length === 0 ? (
          <p className="text-center text-gray-500 text-sm py-12">{t('filenNotes.empty')}</p>
        ) : (
          filteredNotes.map(note => (
            <div
              key={note.uuid}
              onClick={() => openNote(note)}
              className="group px-4 py-3 border-b border-gray-100 dark:border-gray-700/50 hover:bg-gray-50 dark:hover:bg-gray-700/30 cursor-pointer transition-colors"
            >
              <div className="flex items-start justify-between gap-2">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-1.5">
                    {note.pinned && <Pin size={10} className="text-blue-400 flex-shrink-0" />}
                    <span className={`text-sm font-medium truncate ${note.trash ? 'text-gray-400 line-through' : 'text-gray-900 dark:text-gray-100'}`}>
                      {note.title || t('filenNotes.untitled')}
                    </span>
                    <span className="flex-shrink-0">{NOTE_TYPE_ICONS[note.noteType]}</span>
                  </div>
                  {note.preview && (
                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5 line-clamp-1">{note.preview}</p>
                  )}
                  <div className="flex items-center gap-2 mt-1">
                    <span className="text-[10px] text-gray-400">{formatDate(note.editedTimestamp)}</span>
                    {note.tags.length > 0 && (
                      <div className="flex items-center gap-0.5">
                        <Tag size={8} className="text-gray-400" />
                        <span className="text-[10px] text-gray-400">{note.tags.length}</span>
                      </div>
                    )}
                    {note.participants.length > 1 && (
                      <span className="text-[10px] text-gray-400">{note.participants.length} {t('filenNotes.participants')}</span>
                    )}
                  </div>
                </div>
                <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
                  {note.trash ? (
                    <>
                      <button onClick={e => restoreNote(note, e)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-green-500" title={t('filenNotes.restore')}>
                        <RotateCcw size={12} />
                      </button>
                      <button onClick={e => deleteNotePermanently(note, e)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-red-500" title={t('filenNotes.deletePermanently')}>
                        <Trash2 size={12} />
                      </button>
                    </>
                  ) : (
                    <>
                      <button onClick={e => toggleFavorite(note, e)} className={`p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 ${note.favorite ? 'text-yellow-500' : 'text-gray-400'}`} title={t('filenNotes.favorite')}>
                        <Star size={12} fill={note.favorite ? 'currentColor' : 'none'} />
                      </button>
                      <button onClick={e => togglePinned(note, e)} className={`p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 ${note.pinned ? 'text-blue-500' : 'text-gray-400'}`} title={t('filenNotes.pin')}>
                        <Pin size={12} />
                      </button>
                      <button onClick={e => archiveNote(note, e)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400" title={t('filenNotes.archive')}>
                        <Archive size={12} />
                      </button>
                      <button onClick={e => trashNote(note, e)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-red-500" title={t('filenNotes.trash')}>
                        <Trash2 size={12} />
                      </button>
                    </>
                  )}
                </div>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );

  return (
    <div className="fixed inset-0 z-[9999] flex items-start justify-center pt-[5vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />
      <div
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[600px] h-[70vh] flex flex-col animate-scale-in overflow-hidden"
        role="dialog"
        aria-modal="true"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <FileText size={16} className="text-emerald-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('filenNotes.title')}
            </h2>
            <span className="text-xs text-gray-400">
              ({filteredNotes.length})
            </span>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500">
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-hidden">
          {history ? renderHistory() : selectedNote ? renderEditor() : renderList()}
        </div>

        {/* Encryption badge */}
        <div className="px-4 py-1.5 border-t border-gray-200 dark:border-gray-700 bg-emerald-50 dark:bg-emerald-900/10">
          <p className="text-[10px] text-emerald-700 dark:text-emerald-300 text-center flex items-center justify-center gap-1">
            <Lock size={9} />
            {t('filenNotes.encryptionBadge')}
          </p>
        </div>
      </div>
    </div>
  );
}

