/**
 * GitHubReleaseBrowser Component
 * Modal dialog for browsing, creating, and managing GitHub releases and assets.
 * Supports release CRUD, asset inspection, and download links.
 */

import React, { useState, useEffect, useCallback } from 'react';
import {
  Package, X, Plus, ArrowLeft, Trash2, ChevronDown, ChevronRight,
  FileDown, Download, Loader2, Tag, Calendar, FileBox, RefreshCw, FileText,
} from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils/formatters';

interface GitHubReleaseBrowserProps {
  isOpen: boolean;
  onClose: () => void;
}

interface Release {
  tag: string;
  published_at: string | null;
  draft: boolean;
  prerelease: boolean;
  body: string;
  release_id: string;
}

interface Asset {
  name: string;
  size: number;
  download_count: number;
  content_type: string;
  browser_download_url: string;
  updated_at: string | null;
}

type View = 'list' | 'create';

const formatDate = (dateStr: string | null): string => {
  if (!dateStr) return '--';
  try {
    return new Intl.DateTimeFormat(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
    }).format(new Date(dateStr));
  } catch {
    return dateStr;
  }
};

export const GitHubReleaseBrowser: React.FC<GitHubReleaseBrowserProps> = ({
  isOpen,
  onClose,
}) => {
  const t = useTranslation();
  const [view, setView] = useState<View>('list');
  const [releases, setReleases] = useState<Release[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedTag, setExpandedTag] = useState<string | null>(null);
  const [assets, setAssets] = useState<Record<string, Asset[]>>({});
  const [assetsLoading, setAssetsLoading] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<{ type: 'release' | 'asset'; tag: string; assetName?: string } | null>(null);

  // Create form state
  const [formTag, setFormTag] = useState('');
  const [formName, setFormName] = useState('');
  const [formBody, setFormBody] = useState('');
  const [formDraft, setFormDraft] = useState(false);
  const [formPrerelease, setFormPrerelease] = useState(false);
  const [creating, setCreating] = useState(false);
  const [importingChangelog, setImportingChangelog] = useState(false);
  const [previewBody, setPreviewBody] = useState(false);

  /** Suggest next patch version from existing releases */
  const suggestedTag = React.useMemo(() => {
    if (releases.length === 0) return 'v1.0.0';
    // Find latest semver tag
    const sorted = [...releases]
      .map(r => r.tag.replace(/^v/, ''))
      .filter(t => /^\d+\.\d+\.\d+/.test(t))
      .sort((a, b) => {
        const [a1, a2, a3] = a.split('.').map(Number);
        const [b1, b2, b3] = b.split('.').map(Number);
        return b1 - a1 || b2 - a2 || b3 - a3;
      });
    if (sorted.length === 0) return 'v1.0.0';
    const [maj, min, patch] = sorted[0].split('.').map(Number);
    return `v${maj}.${min}.${patch + 1}`;
  }, [releases]);

  /** Extract a version section from CHANGELOG.md content */
  const extractChangelogSection = useCallback((content: string, tag: string): string | null => {
    // Normalize tag: "v3.0.5" -> "3.0.5"
    const version = tag.replace(/^v/, '');
    // Find "## [X.Y.Z]" or "## [vX.Y.Z]" header
    const pattern = new RegExp(`^## \\[v?${version.replace(/\./g, '\\.')}\\].*$`, 'm');
    const startMatch = content.match(pattern);
    if (!startMatch || startMatch.index === undefined) return null;

    // Find the next "## [" header after the match
    const afterStart = startMatch.index + startMatch[0].length;
    const nextSection = content.indexOf('\n## [', afterStart);
    const section = nextSection === -1
      ? content.slice(afterStart)
      : content.slice(afterStart, nextSection);

    return section.trim();
  }, []);

  /** Import body from CHANGELOG.md in the repository */
  const handleImportChangelog = useCallback(async () => {
    if (!formTag.trim()) return;
    setImportingChangelog(true);
    try {
      const content = await invoke<string>('github_read_file', { path: 'CHANGELOG.md' });
      const section = extractChangelogSection(content, formTag.trim());
      if (section) {
        setFormBody(section);
      } else {
        setFormBody(`_No section found for ${formTag.trim()} in CHANGELOG.md_`);
      }
    } catch (err) {
      console.error('[GitHubReleaseBrowser] changelog import failed:', err);
      setFormBody(`_Failed to import CHANGELOG: ${String(err)}_`);
    } finally {
      setImportingChangelog(false);
    }
  }, [formTag, extractChangelogSection]);

  const fetchReleases = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<{ releases: Release[]; count: number }>('github_list_releases');
      setReleases(result.releases);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  // Load releases when dialog opens
  useEffect(() => {
    if (isOpen) {
      setView('list');
      setExpandedTag(null);
      setAssets({});
      fetchReleases();
    }
  }, [isOpen, fetchReleases]);

  // WebKitGTK scrollbar fix
  useEffect(() => {
    if (isOpen) {
      document.documentElement.classList.add('modal-open');
      return () => {
        document.documentElement.classList.remove('modal-open');
      };
    }
  }, [isOpen]);

  // Escape key handler
  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (view === 'create') {
          setView('list');
        } else {
          onClose();
        }
      }
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [isOpen, view, onClose]);

  const handleToggleExpand = useCallback(async (tag: string) => {
    if (expandedTag === tag) {
      setExpandedTag(null);
      return;
    }
    setExpandedTag(tag);
    if (!assets[tag]) {
      setAssetsLoading(tag);
      try {
        const result = await invoke<{ assets: Asset[]; count: number; tag: string }>(
          'github_list_release_assets', { tag }
        );
        setAssets(prev => ({ ...prev, [tag]: result.assets }));
      } catch {
        setAssets(prev => ({ ...prev, [tag]: [] }));
      } finally {
        setAssetsLoading(null);
      }
    }
  }, [expandedTag, assets]);

  const handleDeleteRelease = useCallback((tag: string) => {
    setConfirmDelete({ type: 'release', tag });
  }, []);

  const handleDeleteAsset = useCallback((tag: string, assetName: string) => {
    setConfirmDelete({ type: 'asset', tag, assetName });
  }, []);

  const executeDelete = useCallback(async () => {
    if (!confirmDelete) return;
    const { type, tag, assetName } = confirmDelete;
    setConfirmDelete(null);
    try {
      if (type === 'release') {
        await invoke('github_delete_release', { tag });
        setReleases(prev => prev.filter(r => r.tag !== tag));
        if (expandedTag === tag) setExpandedTag(null);
      } else if (type === 'asset' && assetName) {
        await invoke('github_delete_release_asset', { tag, assetName });
        setAssets(prev => ({
          ...prev,
          [tag]: (prev[tag] || []).filter(a => a.name !== assetName),
        }));
      }
    } catch (err) {
      setError(String(err));
    }
  }, [confirmDelete, expandedTag]);

  const handleCreate = useCallback(async () => {
    if (!formTag.trim()) return;
    setCreating(true);
    setError(null);
    try {
      await invoke('github_create_release', {
        tag: formTag.trim(),
        name: formName.trim() || formTag.trim(),
        body: formBody.trim(),
        draft: formDraft,
        prerelease: formPrerelease,
      });
      setFormTag('');
      setFormName('');
      setFormBody('');
      setFormDraft(false);
      setFormPrerelease(false);
      setView('list');
      fetchReleases();
    } catch (err) {
      console.error('[GitHubReleaseBrowser] create failed:', err);
      setError(String(err));
    } finally {
      setCreating(false);
    }
  }, [formTag, formName, formBody, formDraft, formPrerelease, fetchReleases]);

  const openCreateView = useCallback(() => {
    setError(null);
    setFormTag('');
    setFormName('');
    setFormBody('');
    setFormDraft(false);
    setFormPrerelease(false);
    setView('create');
  }, []);

  if (!isOpen) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]"
      role="dialog"
      aria-modal="true"
      aria-label={t('github.releases') || 'GitHub Releases'}
    >
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/50 backdrop-blur-sm"
        onClick={view === 'create' ? () => setView('list') : onClose}
      />

      {/* Dialog */}
      <div
        className="relative w-full max-w-2xl overflow-hidden rounded-xl border border-gray-200 dark:border-gray-700 shadow-2xl animate-scale-in bg-white dark:bg-gray-800"
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div
          className="flex items-center justify-between px-5 py-3 border-b"
          style={{ borderColor: 'var(--color-border)' }}
        >
          <div className="flex items-center gap-2">
            {view === 'create' ? (
              <button
                onClick={() => setView('list')}
                className="p-1 rounded transition-colors hover:opacity-80"
                style={{ color: 'var(--color-text-secondary)' }}
              >
                <ArrowLeft size={16} />
              </button>
            ) : (
              <Package size={16} style={{ color: 'var(--color-accent)' }} />
            )}
            <h2
              className="text-sm font-semibold"
              style={{ color: 'var(--color-text-primary)' }}
            >
              {view === 'create'
                ? (t('github.createRelease') || 'Create Release')
                : (t('github.releases') || 'GitHub Releases')
              }
            </h2>
          </div>
          <div className="flex items-center gap-2">
            {view === 'list' && (
              <button
                onClick={fetchReleases}
                className="p-1.5 rounded-lg transition-colors text-gray-400 hover:text-gray-200 hover:bg-gray-700/50"
                title="Refresh"
              >
                <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
              </button>
            )}
            {view === 'list' && (
              <button
                onClick={openCreateView}
                className="flex items-center gap-1 px-2 py-1 text-xs rounded-lg transition-colors text-white"
                style={{ backgroundColor: 'var(--color-accent)' }}
                title={t('github.createRelease') || 'Create Release'}
              >
                <Plus size={12} />
                <span>{t('github.createRelease') || 'Create'}</span>
              </button>
            )}
            <button
              onClick={onClose}
              className="p-1 rounded transition-colors hover:opacity-80"
              style={{ color: 'var(--color-text-secondary)' }}
            >
              <X size={16} />
            </button>
          </div>
        </div>

        {/* Error banner */}
        {error && (
          <div
            className="px-5 py-2 text-xs border-b"
            style={{
              borderColor: 'var(--color-border)',
              backgroundColor: 'rgba(239, 68, 68, 0.1)',
              color: '#ef4444',
            }}
          >
            {error}
          </div>
        )}

        {/* Content */}
        {view === 'create' ? (
          <CreateReleaseForm
            tag={formTag}
            name={formName}
            body={formBody}
            draft={formDraft}
            prerelease={formPrerelease}
            creating={creating}
            onTagChange={setFormTag}
            onNameChange={setFormName}
            onBodyChange={setFormBody}
            onDraftChange={setFormDraft}
            onPrereleaseChange={setFormPrerelease}
            onCreate={handleCreate}
            onImportChangelog={handleImportChangelog}
            importingChangelog={importingChangelog}
            suggestedTag={suggestedTag}
            previewBody={previewBody}
            onTogglePreview={() => setPreviewBody(p => !p)}
          />
        ) : (
          <ReleaseList
            releases={releases}
            loading={loading}
            expandedTag={expandedTag}
            assets={assets}
            assetsLoading={assetsLoading}
            onToggleExpand={handleToggleExpand}
            onDeleteRelease={handleDeleteRelease}
            onDeleteAsset={handleDeleteAsset}
          />
        )}
      </div>

      {/* Confirmation dialog */}
      {confirmDelete && (
        <div className="absolute inset-0 z-10 flex items-center justify-center bg-black/40 rounded-xl">
          <div className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-xl p-5 mx-6 max-w-sm animate-scale-in">
            <p className="text-sm text-gray-700 dark:text-gray-200 mb-4">
              {confirmDelete.type === 'release'
                ? (t('github.confirmDeleteRelease') || `Delete release "${confirmDelete.tag}"? This cannot be undone.`)
                : (t('github.confirmDeleteAsset') || `Delete asset "${confirmDelete.assetName}"? This cannot be undone.`)
              }
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setConfirmDelete(null)}
                className="px-3 py-1.5 text-xs rounded-lg border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
              >
                {t('common.cancel') || 'Cancel'}
              </button>
              <button
                onClick={executeDelete}
                className="px-3 py-1.5 text-xs rounded-lg bg-red-600 hover:bg-red-700 text-white font-medium transition-colors"
              >
                {t('common.delete') || 'Delete'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

/* ------------------------------------------------------------------ */
/*  Release List                                                       */
/* ------------------------------------------------------------------ */

interface ReleaseListProps {
  releases: Release[];
  loading: boolean;
  expandedTag: string | null;
  assets: Record<string, Asset[]>;
  assetsLoading: string | null;
  onToggleExpand: (tag: string) => void;
  onDeleteRelease: (tag: string) => void;
  onDeleteAsset: (tag: string, assetName: string) => void;
}

const ReleaseList: React.FC<ReleaseListProps> = ({
  releases, loading, expandedTag, assets, assetsLoading,
  onToggleExpand, onDeleteRelease, onDeleteAsset,
}) => {
  const t = useTranslation();

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 size={20} className="animate-spin" style={{ color: 'var(--color-accent)' }} />
      </div>
    );
  }

  if (releases.length === 0) {
    return (
      <div
        className="flex flex-col items-center justify-center py-12 gap-2"
        style={{ color: 'var(--color-text-secondary)' }}
      >
        <Package size={24} className="opacity-40" />
        <span className="text-xs">{t('github.noReleases') || 'No releases found'}</span>
      </div>
    );
  }

  return (
    <div className="max-h-[60vh] overflow-y-auto">
      {releases.map(release => {
        const isExpanded = expandedTag === release.tag;
        const tagAssets = assets[release.tag];
        const isLoadingAssets = assetsLoading === release.tag;

        return (
          <div
            key={release.tag}
            className="border-b last:border-b-0"
            style={{ borderColor: 'var(--color-border)' }}
          >
            {/* Release row */}
            <div
              className="flex items-center gap-3 px-5 py-3 cursor-pointer transition-colors hover:opacity-90"
              style={{ backgroundColor: isExpanded ? 'var(--color-bg-primary)' : undefined }}
              onClick={() => onToggleExpand(release.tag)}
            >
              {/* Expand chevron */}
              <span className="flex-shrink-0" style={{ color: 'var(--color-text-secondary)' }}>
                {isExpanded
                  ? <ChevronDown size={14} />
                  : <ChevronRight size={14} />
                }
              </span>

              {/* Tag + name */}
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <Tag size={12} style={{ color: 'var(--color-accent)' }} className="flex-shrink-0" />
                  <span
                    className="text-sm font-semibold truncate"
                    style={{ color: 'var(--color-text-primary)' }}
                  >
                    {release.tag}
                  </span>
                  {release.draft && (
                    <span className="px-1.5 py-0.5 rounded text-[10px] font-medium text-yellow-500 bg-yellow-500/10">
                      Draft
                    </span>
                  )}
                  {release.prerelease && (
                    <span className="px-1.5 py-0.5 rounded text-[10px] font-medium text-orange-500 bg-orange-500/10">
                      Pre-release
                    </span>
                  )}
                </div>
                <div
                  className="flex items-center gap-3 mt-0.5 text-[11px]"
                  style={{ color: 'var(--color-text-secondary)' }}
                >
                  <span className="flex items-center gap-1">
                    <Calendar size={10} />
                    {formatDate(release.published_at)}
                  </span>
                  {tagAssets && (
                    <span className="flex items-center gap-1">
                      <FileBox size={10} />
                      {tagAssets.length} {tagAssets.length === 1 ? 'asset' : 'assets'}
                    </span>
                  )}
                </div>
              </div>

              {/* Delete button */}
              <button
                onClick={e => { e.stopPropagation(); onDeleteRelease(release.tag); }}
                className="p-1.5 rounded transition-colors hover:bg-red-500/10 flex-shrink-0"
                style={{ color: 'var(--color-text-secondary)' }}
                title={t('github.deleteRelease') || 'Delete release'}
              >
                <Trash2 size={14} />
              </button>
            </div>

            {/* Expanded assets */}
            {isExpanded && (
              <div
                className="px-5 pb-3"
                style={{ backgroundColor: 'var(--color-bg-primary)' }}
              >
                {isLoadingAssets ? (
                  <div className="flex items-center justify-center py-4">
                    <Loader2 size={14} className="animate-spin" style={{ color: 'var(--color-accent)' }} />
                  </div>
                ) : !tagAssets || tagAssets.length === 0 ? (
                  <div
                    className="text-xs text-center py-4"
                    style={{ color: 'var(--color-text-secondary)' }}
                  >
                    {t('github.noAssets') || 'No assets'}
                  </div>
                ) : (
                  <div className="rounded-lg border overflow-hidden" style={{ borderColor: 'var(--color-border)' }}>
                    <table className="w-full text-xs">
                      <thead>
                        <tr
                          className="border-b"
                          style={{
                            borderColor: 'var(--color-border)',
                            backgroundColor: 'var(--color-bg-secondary)',
                          }}
                        >
                          <th
                            className="text-left px-3 py-1.5 font-medium"
                            style={{ color: 'var(--color-text-secondary)' }}
                          >
                            {t('github.assetName') || 'Name'}
                          </th>
                          <th
                            className="text-right px-3 py-1.5 font-medium"
                            style={{ color: 'var(--color-text-secondary)' }}
                          >
                            {t('github.assetSize') || 'Size'}
                          </th>
                          <th
                            className="text-right px-3 py-1.5 font-medium"
                            style={{ color: 'var(--color-text-secondary)' }}
                          >
                            {t('github.assetDownloads') || 'Downloads'}
                          </th>
                          <th className="w-16" />
                        </tr>
                      </thead>
                      <tbody>
                        {tagAssets.map(asset => (
                          <tr
                            key={asset.name}
                            className="border-b last:border-b-0"
                            style={{ borderColor: 'var(--color-border)' }}
                          >
                            <td className="px-3 py-1.5">
                              <div
                                className="flex items-center gap-1.5 truncate max-w-[200px]"
                                style={{ color: 'var(--color-text-primary)' }}
                                title={asset.name}
                              >
                                <FileDown size={11} className="flex-shrink-0" style={{ color: 'var(--color-accent)' }} />
                                <span className="truncate">{asset.name}</span>
                              </div>
                              <div className="text-[10px] mt-0.5" style={{ color: 'var(--color-text-secondary)' }}>
                                {asset.content_type}
                              </div>
                            </td>
                            <td
                              className="px-3 py-1.5 text-right whitespace-nowrap"
                              style={{ color: 'var(--color-text-secondary)' }}
                            >
                              {asset.size > 0 ? formatBytes(asset.size) : '-'}
                            </td>
                            <td
                              className="px-3 py-1.5 text-right whitespace-nowrap"
                              style={{ color: 'var(--color-text-secondary)' }}
                            >
                              {asset.download_count.toLocaleString()}
                            </td>
                            <td className="px-2 py-1.5">
                              <div className="flex items-center justify-end gap-1">
                                <button
                                  onClick={async (e) => {
                                    e.stopPropagation();
                                    try {
                                      const defaultName = asset.name.replace(/[()]/g, '').replace(/\s+/g, '-');
                                      const savePath = await save({
                                        defaultPath: defaultName,
                                        title: `Download ${asset.name}`,
                                      });
                                      if (savePath) {
                                        await invoke('github_download_release_asset', {
                                          tag: release.tag,
                                          assetName: asset.name,
                                          localPath: savePath,
                                        });
                                      }
                                    } catch (err) {
                                      alert(`Download failed: ${err}`);
                                    }
                                  }}
                                  className="p-1 rounded transition-colors hover:opacity-80"
                                  style={{ color: 'var(--color-accent)' }}
                                  title={t('github.downloadAsset') || 'Download'}
                                >
                                  <Download size={12} />
                                </button>
                                {!asset.name.startsWith('Source code') && (
                                <button
                                  onClick={e => {
                                    e.stopPropagation();
                                    onDeleteAsset(release.tag, asset.name);
                                  }}
                                  className="p-1 rounded transition-colors hover:bg-red-500/10"
                                  style={{ color: 'var(--color-text-secondary)' }}
                                  title={t('github.deleteAsset') || 'Delete asset'}
                                >
                                  <Trash2 size={12} />
                                </button>
                                )}
                              </div>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
};

/* ------------------------------------------------------------------ */
/*  Create Release Form                                                */
/* ------------------------------------------------------------------ */

interface CreateReleaseFormProps {
  tag: string;
  name: string;
  body: string;
  draft: boolean;
  prerelease: boolean;
  creating: boolean;
  onTagChange: (v: string) => void;
  onNameChange: (v: string) => void;
  onBodyChange: (v: string) => void;
  onDraftChange: (v: boolean) => void;
  onPrereleaseChange: (v: boolean) => void;
  onCreate: () => void;
  onImportChangelog: () => void;
  importingChangelog: boolean;
  suggestedTag: string;
  previewBody: boolean;
  onTogglePreview: () => void;
}

const CreateReleaseForm: React.FC<CreateReleaseFormProps> = ({
  tag, name, body, draft, prerelease, creating,
  onTagChange, onNameChange, onBodyChange, onDraftChange, onPrereleaseChange, onCreate,
  onImportChangelog, importingChangelog, suggestedTag, previewBody, onTogglePreview,
}) => {
  const t = useTranslation();

  const inputStyle = {
    backgroundColor: 'var(--color-bg-primary)',
    borderColor: 'var(--color-border)',
    color: 'var(--color-text-primary)',
  };

  return (
    <div className="px-5 py-4 space-y-3">
      {/* Tag name */}
      <div>
        <label
          className="block text-xs font-medium mb-1"
          style={{ color: 'var(--color-text-secondary)' }}
        >
          {t('github.tagName') || 'Tag name'} *
        </label>
        <input
          type="text"
          value={tag}
          onChange={e => onTagChange(e.target.value)}
          placeholder={suggestedTag}
          className="w-full px-3 py-2 text-sm rounded-lg border focus:outline-none focus:ring-2"
          style={inputStyle}
        />
      </div>

      {/* Release name */}
      <div>
        <label
          className="block text-xs font-medium mb-1"
          style={{ color: 'var(--color-text-secondary)' }}
        >
          {t('github.releaseName') || 'Release name'}
        </label>
        <input
          type="text"
          value={name}
          onChange={e => onNameChange(e.target.value)}
          placeholder={t('github.releaseNamePlaceholder') || 'Release title'}
          className="w-full px-3 py-2 text-sm rounded-lg border focus:outline-none focus:ring-2"
          style={inputStyle}
        />
      </div>

      {/* Description */}
      <div>
        <div className="flex items-center justify-between mb-1">
          <label
            className="text-xs font-medium"
            style={{ color: 'var(--color-text-secondary)' }}
          >
            {t('github.releaseDescription') || 'Description'}
          </label>
          <button
            type="button"
            onClick={onImportChangelog}
            disabled={importingChangelog || !tag.trim()}
            className="flex items-center gap-1 text-xs px-2 py-0.5 rounded transition-colors hover:opacity-80 disabled:opacity-40 disabled:cursor-not-allowed"
            style={{ color: 'var(--color-accent)' }}
            title={t('github.importChangelog') || 'Import section from CHANGELOG.md'}
          >
            {importingChangelog ? <Loader2 size={10} className="animate-spin" /> : <FileText size={10} />}
            {t('github.importChangelog') || 'Import from CHANGELOG'}
          </button>
          {body.trim() && (
            <button
              type="button"
              onClick={onTogglePreview}
              className="flex items-center gap-1 text-xs px-2 py-0.5 rounded transition-colors hover:opacity-80"
              style={{ color: previewBody ? 'var(--color-accent)' : 'var(--color-text-secondary)' }}
            >
              {previewBody ? 'Edit' : 'Preview'}
            </button>
          )}
        </div>
        {previewBody ? (
          <div
            className="w-full px-3 py-2 text-sm rounded-lg border overflow-y-auto prose prose-sm dark:prose-invert max-w-none"
            style={{ ...inputStyle, minHeight: '12rem', maxHeight: '24rem' }}
            dangerouslySetInnerHTML={{ __html: body
              .replace(/#### /g, '<h4>')
              .replace(/### /g, '<h3>')
              .replace(/## /g, '<h2>')
              .replace(/# /g, '<h1>')
              .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
              .replace(/\n- /g, '\n<li>')
              .replace(/<li>/g, '</li><li>')
              .replace(/\n\n/g, '<br/><br/>')
              .replace(/\n/g, '<br/>')
            }}
          />
        ) : (
          <textarea
            value={body}
            onChange={e => onBodyChange(e.target.value)}
            rows={12}
            placeholder={t('github.releaseDescriptionPlaceholder') || 'Describe this release...'}
            className="w-full px-3 py-2 text-sm rounded-lg border focus:outline-none focus:ring-2 resize-y"
            style={{ ...inputStyle, minHeight: '12rem' }}
          />
        )}
      </div>

      {/* Toggles */}
      <div className="flex items-center gap-6">
        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={draft}
            onChange={e => onDraftChange(e.target.checked)}
            className="rounded"
          />
          <span className="text-xs" style={{ color: 'var(--color-text-primary)' }}>
            {t('github.draft') || 'Draft'}
          </span>
        </label>
        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={prerelease}
            onChange={e => onPrereleaseChange(e.target.checked)}
            className="rounded"
          />
          <span className="text-xs" style={{ color: 'var(--color-text-primary)' }}>
            {t('github.prerelease') || 'Pre-release'}
          </span>
        </label>
      </div>

      {/* Footer */}
      <div className="flex justify-end pt-2">
        <button
          onClick={onCreate}
          disabled={!tag.trim() || creating}
          className="flex items-center gap-1.5 px-4 py-1.5 text-xs rounded-lg text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          style={{ backgroundColor: 'var(--color-accent)' }}
        >
          {creating ? (
            <Loader2 size={12} className="animate-spin" />
          ) : (
            <Plus size={12} />
          )}
          {t('github.createRelease') || 'Create Release'}
        </button>
      </div>
    </div>
  );
};
