import * as React from 'react';
import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  X, Plus, Loader2, Star, Pin, Trash2, Archive, Lock, Save,
  RotateCcw, Search, Clock, Tag, ChevronLeft, FileText, Code, CheckSquare,
  Hash, Check
} from 'lucide-react';
import { useTranslation } from '../i18n';

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

  // Refs
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const saveTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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
      setDirty(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }, [selectedNote, saving, noteContent, noteTitle, noteType]);

  const createNote = useCallback(async () => {
    if (!newTitle.trim()) return;
    setCreating(true);
    try {
      const uuid = await invoke<string>('filen_notes_create', {
        title: newTitle.trim(),
        content: '',
        noteType: newType,
      });
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
      setError(String(err));
    } finally {
      setCreating(false);
    }
  }, [newTitle, newType, openNote]);

  const toggleFavorite = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke('filen_notes_toggle_favorite', { uuid: note.uuid, favorite: !note.favorite });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, favorite: !n.favorite } : n));
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const togglePinned = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke('filen_notes_toggle_pinned', { uuid: note.uuid, pinned: !note.pinned });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, pinned: !n.pinned } : n));
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const trashNote = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke('filen_notes_trash', { uuid: note.uuid });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, trash: true } : n));
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const restoreNote = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke('filen_notes_restore', { uuid: note.uuid });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, trash: false, archive: false } : n));
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const deleteNotePermanently = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke('filen_notes_delete', { uuid: note.uuid });
      setNotes(prev => prev.filter(n => n.uuid !== note.uuid));
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const archiveNote = useCallback(async (note: FilenNote, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke('filen_notes_archive', { uuid: note.uuid });
      setNotes(prev => prev.map(n => n.uuid === note.uuid ? { ...n, archive: true } : n));
    } catch (err) {
      setError(String(err));
    }
  }, []);

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
    try {
      await invoke('filen_notes_history_restore', { uuid: selectedNote.uuid, historyId });
      setHistory(null);
      openNote(selectedNote);
    } catch (err) {
      setError(String(err));
    }
  }, [selectedNote, openNote]);

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

  // ── Tag resolution ──

  const getTagName = useCallback((tagUuid: string) => {
    return tags.find(t => t.uuid === tagUuid)?.name || tagUuid.slice(0, 8);
  }, [tags]);

  const formatDate = useCallback((ts: number) => {
    if (!ts) return '';
    return new Date(ts * 1000).toLocaleDateString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
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
            onChange={e => setNoteType(e.target.value as NoteTypeOption)}
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

      {/* Editor footer: tags */}
      {selectedNote && selectedNote.tags.length > 0 && (
        <div className="flex items-center gap-1 px-4 py-2 border-t border-gray-200 dark:border-gray-700">
          <Tag size={11} className="text-gray-400" />
          {selectedNote.tags.map(tag => (
            <span key={tag.uuid} className="text-xs bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 px-1.5 py-0.5 rounded">
              {getTagName(tag.uuid)}
            </span>
          ))}
        </div>
      )}
    </div>
  );

  // ─── Render: List view ───
  const renderList = () => (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-gray-200 dark:border-gray-700">
        <div className="flex-1 relative">
          <Search size={12} className="absolute left-2 top-1/2 -translate-y-1/2 text-gray-400" />
          <input
            type="text"
            value={searchQuery}
            onChange={e => setSearchQuery(e.target.value)}
            className="w-full pl-7 pr-2 py-1.5 text-xs bg-gray-100 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded text-gray-800 dark:text-gray-200 outline-none placeholder-gray-400 focus:border-blue-400"
            placeholder={t('filenNotes.searchPlaceholder')}
          />
        </div>
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
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-2xl w-[600px] h-[70vh] flex flex-col animate-scale-in overflow-hidden"
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

