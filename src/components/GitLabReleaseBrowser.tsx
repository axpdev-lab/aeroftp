// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * GitLabReleaseBrowser Component
 * Modal dialog for browsing, creating, and managing GitLab releases and asset links.
 * Assets are uploaded via Generic Packages API and linked to releases.
 */

import React, { useState, useEffect, useCallback } from 'react';
import {
  Package, X, Plus, ArrowLeft, Trash2, ChevronDown, ChevronRight, Upload,
  FileDown, Download, Loader2, Tag, Calendar, FileBox, RefreshCw, FileText,
  ExternalLink,
} from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { save, open as openDialog } from '@tauri-apps/plugin-dialog';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { useTranslation } from '../i18n';
import { useHumanizedLog } from '../hooks/useHumanizedLog';

interface GitLabReleaseBrowserProps {
  isOpen: boolean;
  onClose: () => void;
  onError?: (title: string, message: string) => void;
}

interface Release {
  tag_name: string;
  name: string | null;
  description: string | null;
  created_at: string;
  released_at: string | null;
  author: string;
  assets_count: number;
  sources: Array<{ format: string; url: string }>;
}

interface AssetLink {
  id: number;
  name: string;
  url: string;
  direct_asset_url: string | null;
  link_type: string;
  external: boolean;
}

type View = 'list' | 'create';

const formatDate = (dateStr: string | null): string => {
  if (!dateStr) return '--';
  try {
    return new Intl.DateTimeFormat(undefined, {
      year: 'numeric', month: 'short', day: 'numeric',
    }).format(new Date(dateStr));
  } catch { return dateStr; }
};

const LINK_TYPE_COLORS: Record<string, string> = {
  package: 'text-blue-400 bg-blue-500/10',
  image: 'text-purple-400 bg-purple-500/10',
  runbook: 'text-teal-400 bg-teal-500/10',
  other: 'text-gray-400 bg-gray-500/10',
};

export const GitLabReleaseBrowser: React.FC<GitLabReleaseBrowserProps> = ({
  isOpen, onClose, onError,
}) => {
  const t = useTranslation();
  const humanLog = useHumanizedLog();
  const [view, setView] = useState<View>('list');
  const [releases, setReleases] = useState<Release[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedTag, setExpandedTag] = useState<string | null>(null);
  const [assetLinks, setAssetLinks] = useState<Record<string, AssetLink[]>>({});
  const [assetsLoading, setAssetsLoading] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<{ type: 'release' | 'asset'; tag: string; linkId?: number; linkName?: string } | null>(null);

  // Create form
  const [formTag, setFormTag] = useState('');
  const [formName, setFormName] = useState('');
  const [formBody, setFormBody] = useState('');
  const [creating, setCreating] = useState(false);
  const [importingChangelog, setImportingChangelog] = useState(false);

  const suggestedTag = React.useMemo(() => {
    if (releases.length === 0) return 'v1.0.0';
    const sorted = [...releases]
      .map(r => r.tag_name.replace(/^v/, ''))
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

  const extractChangelogSection = useCallback((content: string, tag: string): string | null => {
    const version = tag.replace(/^v/, '');
    const pattern = new RegExp(`^## \\[v?${version.replace(/\./g, '\\.')}\\].*$`, 'm');
    const startMatch = content.match(pattern);
    if (!startMatch || startMatch.index === undefined) return null;
    const afterStart = startMatch.index + startMatch[0].length;
    const nextSection = content.indexOf('\n## [', afterStart);
    return (nextSection === -1 ? content.slice(afterStart) : content.slice(afterStart, nextSection)).trim();
  }, []);

  const handleImportChangelog = useCallback(async () => {
    if (!formTag.trim()) return;
    setImportingChangelog(true);
    try {
      const content = await invoke<string>('gitlab_read_file', { path: 'CHANGELOG.md' });
      const section = extractChangelogSection(content, formTag.trim());
      setFormBody(section || `_No section found for ${formTag.trim()} in CHANGELOG.md_`);
    } catch (err) {
      setFormBody(`_Failed to import CHANGELOG: ${String(err)}_`);
    } finally {
      setImportingChangelog(false);
    }
  }, [formTag, extractChangelogSection]);

  const fetchReleases = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<Release[]>('gitlab_list_releases');
      setReleases(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (isOpen) {
      setView('list');
      setExpandedTag(null);
      setAssetLinks({});
      fetchReleases();
    }
  }, [isOpen, fetchReleases]);

  useEffect(() => {
    if (isOpen) {
      document.documentElement.classList.add('modal-open');
      return () => { document.documentElement.classList.remove('modal-open'); };
    }
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (view === 'create') setView('list'); else onClose();
      }
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [isOpen, view, onClose]);

  const handleToggleExpand = useCallback(async (tag: string) => {
    if (expandedTag === tag) { setExpandedTag(null); return; }
    setExpandedTag(tag);
    if (!assetLinks[tag]) {
      setAssetsLoading(tag);
      try {
        const result = await invoke<AssetLink[]>('gitlab_list_release_assets', { tag });
        setAssetLinks(prev => ({ ...prev, [tag]: result }));
      } catch {
        setAssetLinks(prev => ({ ...prev, [tag]: [] }));
      } finally {
        setAssetsLoading(null);
      }
    }
  }, [expandedTag, assetLinks]);

  const handleUploadAsset = useCallback(async (tag: string) => {
    try {
      const selected = await openDialog({ multiple: false, title: t('gitlab.uploadAsset') || 'Upload Asset' });
      if (!selected) return;
      const filePath = typeof selected === 'string' ? selected : (selected as { path: string }).path;
      const fileName = filePath.split(/[/\\]/).pop() || filePath;
      const logId = humanLog.logRaw('activity.release_upload_start', 'UPLOAD', { provider: 'GitLab', filename: fileName }, 'running');
      try {
        await invoke('gitlab_upload_release_asset', { tag, localPath: filePath, assetName: fileName });
        humanLog.updateEntry(logId, { status: 'success', message: t('activity.release_upload_success', { provider: 'GitLab', filename: fileName }) });
      } catch (err) {
        humanLog.updateEntry(logId, { status: 'error', message: t('activity.release_upload_error', { provider: 'GitLab', filename: fileName }) });
        throw err;
      }
      const result = await invoke<AssetLink[]>('gitlab_list_release_assets', { tag });
      setAssetLinks(prev => ({ ...prev, [tag]: result }));
    } catch (err) {
      if (onError) onError('Upload Asset', String(err));
    }
  }, [onError, t, humanLog]);

  const executeDelete = useCallback(async () => {
    if (!confirmDelete) return;
    const { type, tag, linkId, linkName } = confirmDelete;
    setConfirmDelete(null);
    const isRelease = type === 'release';
    const logId = humanLog.logRaw(
      isRelease ? 'activity.release_delete_start' : 'activity.release_asset_delete_start',
      'DELETE',
      { provider: 'GitLab', tag, filename: linkName || String(linkId || '') },
      'running',
    );
    try {
      if (isRelease) {
        await invoke('gitlab_delete_release', { tag });
        setReleases(prev => prev.filter(r => r.tag_name !== tag));
        if (expandedTag === tag) setExpandedTag(null);
        humanLog.updateEntry(logId, { status: 'success', message: t('activity.release_delete_success', { provider: 'GitLab', tag }) });
      } else if (type === 'asset' && linkId !== undefined) {
        await invoke('gitlab_delete_release_asset', { tag, linkId });
        setAssetLinks(prev => ({
          ...prev,
          [tag]: (prev[tag] || []).filter(a => a.id !== linkId),
        }));
        humanLog.updateEntry(logId, { status: 'success', message: t('activity.release_asset_delete_success', { provider: 'GitLab', filename: linkName || String(linkId) }) });
      }
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: t('activity.release_delete_error', { provider: 'GitLab', error: String(err) }) });
      setError(String(err));
    }
  }, [confirmDelete, expandedTag, humanLog]);

  const handleCreate = useCallback(async () => {
    if (!formTag.trim()) return;
    setCreating(true);
    setError(null);
    const tagValue = formTag.trim();
    const logId = humanLog.logRaw('activity.release_create_start', 'UPDATE', { provider: 'GitLab', tag: tagValue }, 'running');
    try {
      await invoke('gitlab_create_release', {
        tag: tagValue,
        name: formName.trim() || tagValue,
        description: formBody.trim(),
      });
      humanLog.updateEntry(logId, { status: 'success', message: t('activity.release_create_success', { provider: 'GitLab', tag: tagValue }) });
      setFormTag(''); setFormName(''); setFormBody('');
      setView('list');
      fetchReleases();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: t('activity.release_create_error', { provider: 'GitLab', tag: tagValue }) });
      setError(String(err));
    } finally {
      setCreating(false);
    }
  }, [formTag, formName, formBody, fetchReleases, humanLog]);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]" role="dialog" aria-modal="true" aria-label={t('gitlab.releases') || 'GitLab Releases'}>
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={view === 'create' ? () => setView('list') : onClose} />

      <div className="relative w-full max-w-2xl overflow-hidden rounded-lg border border-gray-200 dark:border-gray-700 shadow-2xl animate-scale-in" style={{ backgroundColor: 'var(--color-bg-secondary)' }} onClick={e => e.stopPropagation()}>
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-3 border-b" style={{ borderColor: 'var(--color-border)' }}>
          <div className="flex items-center gap-2">
            {view === 'create' ? (
              <button onClick={() => setView('list')} className="p-1 rounded transition-colors hover:opacity-80" style={{ color: 'var(--color-text-secondary)' }}><ArrowLeft size={16} /></button>
            ) : (
              <Tag size={16} className="text-orange-400" />
            )}
            <h2 className="text-sm font-semibold" style={{ color: 'var(--color-text-primary)' }}>
              {view === 'create' ? (t('gitlab.createRelease') || 'Create Release') : (t('gitlab.releases') || 'GitLab Releases')}
            </h2>
          </div>
          <div className="flex items-center gap-2">
            {view === 'list' && (
              <button onClick={fetchReleases} className="p-1.5 rounded-lg transition-colors text-gray-400 hover:text-gray-200 hover:bg-gray-700/50" title="Refresh">
                <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
              </button>
            )}
            {view === 'list' && (
              <button onClick={() => { setError(null); setFormTag(''); setFormName(''); setFormBody(''); setView('create'); }} className="flex items-center gap-1 px-2 py-1 text-xs rounded-lg transition-colors text-white bg-orange-600 hover:bg-orange-700" title={t('gitlab.createRelease') || 'Create Release'}>
                <Plus size={12} /><span>{t('gitlab.createRelease') || 'Create'}</span>
              </button>
            )}
            <button onClick={onClose} className="p-1 rounded transition-colors hover:opacity-80" style={{ color: 'var(--color-text-secondary)' }}><X size={16} /></button>
          </div>
        </div>

        {error && (
          <div className="px-5 py-2 text-xs border-b" style={{ borderColor: 'var(--color-border)', backgroundColor: 'rgba(239, 68, 68, 0.1)', color: '#ef4444' }}>{error}</div>
        )}

        {/* Content */}
        {view === 'create' ? (
          <div className="px-5 py-4 space-y-3">
            <div>
              <label className="block text-xs font-medium mb-1" style={{ color: 'var(--color-text-secondary)' }}>{t('gitlab.tagName') || 'Tag name'} *</label>
              <input type="text" value={formTag} onChange={e => setFormTag(e.target.value)} placeholder={suggestedTag} className="w-full px-3 py-2 text-sm rounded-lg border focus:outline-none focus:ring-2 focus:ring-orange-500" style={{ backgroundColor: 'var(--color-bg-primary)', borderColor: 'var(--color-border)', color: 'var(--color-text-primary)' }} />
            </div>
            <div>
              <label className="block text-xs font-medium mb-1" style={{ color: 'var(--color-text-secondary)' }}>{t('gitlab.releaseName') || 'Release name'}</label>
              <input type="text" value={formName} onChange={e => setFormName(e.target.value)} placeholder={t('gitlab.releaseNamePlaceholder') || 'Release title'} className="w-full px-3 py-2 text-sm rounded-lg border focus:outline-none focus:ring-2 focus:ring-orange-500" style={{ backgroundColor: 'var(--color-bg-primary)', borderColor: 'var(--color-border)', color: 'var(--color-text-primary)' }} />
            </div>
            <div>
              <div className="flex items-center justify-between mb-1">
                <label className="text-xs font-medium" style={{ color: 'var(--color-text-secondary)' }}>{t('gitlab.releaseDescription') || 'Description'}</label>
                <button type="button" onClick={handleImportChangelog} disabled={importingChangelog || !formTag.trim()} className="flex items-center gap-1 text-xs px-2 py-0.5 rounded transition-colors hover:opacity-80 disabled:opacity-40 disabled:cursor-not-allowed text-orange-400" title={t('gitlab.importChangelogTooltip') || 'Import section from CHANGELOG.md'}>
                  {importingChangelog ? <Loader2 size={10} className="animate-spin" /> : <FileText size={10} />}
                  {t('gitlab.importChangelog') || 'Import from CHANGELOG'}
                </button>
              </div>
              <textarea value={formBody} onChange={e => setFormBody(e.target.value)} rows={10} placeholder={t('gitlab.releaseDescriptionPlaceholder') || 'Describe this release...'} className="w-full px-3 py-2 text-sm rounded-lg border focus:outline-none focus:ring-2 focus:ring-orange-500 resize-y" style={{ backgroundColor: 'var(--color-bg-primary)', borderColor: 'var(--color-border)', color: 'var(--color-text-primary)', minHeight: '10rem' }} />
            </div>
            <div className="flex justify-end pt-2">
              <button onClick={handleCreate} disabled={!formTag.trim() || creating} className="flex items-center gap-1.5 px-4 py-1.5 text-xs rounded-lg text-white bg-orange-600 hover:bg-orange-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed">
                {creating ? <Loader2 size={12} className="animate-spin" /> : <Plus size={12} />}
                {t('gitlab.createRelease') || 'Create Release'}
              </button>
            </div>
          </div>
        ) : loading ? (
          <div className="flex items-center justify-center py-12"><Loader2 size={20} className="animate-spin text-orange-400" /></div>
        ) : releases.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-12 gap-2" style={{ color: 'var(--color-text-secondary)' }}>
            <Package size={24} className="opacity-40" />
            <span className="text-xs">{t('gitlab.noReleases') || 'No releases found'}</span>
          </div>
        ) : (
          <div className="max-h-[60vh] overflow-y-auto">
            {releases.map(release => {
              const isExpanded = expandedTag === release.tag_name;
              const tagLinks = assetLinks[release.tag_name];
              const isLoadingLinks = assetsLoading === release.tag_name;

              return (
                <div key={release.tag_name} className="border-b last:border-b-0" style={{ borderColor: 'var(--color-border)' }}>
                  <div className="flex items-center gap-3 px-5 py-3 cursor-pointer transition-colors hover:opacity-90" style={{ backgroundColor: isExpanded ? 'var(--color-bg-primary)' : undefined }} onClick={() => handleToggleExpand(release.tag_name)}>
                    <span className="flex-shrink-0" style={{ color: 'var(--color-text-secondary)' }}>
                      {isExpanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                    </span>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <Tag size={12} className="text-orange-400 flex-shrink-0" />
                        <span className="text-sm font-semibold truncate" style={{ color: 'var(--color-text-primary)' }}>{release.tag_name}</span>
                        {release.name && release.name !== release.tag_name && (
                          <span className="text-xs truncate" style={{ color: 'var(--color-text-secondary)' }}>{release.name}</span>
                        )}
                      </div>
                      <div className="flex items-center gap-3 mt-0.5 text-[11px]" style={{ color: 'var(--color-text-secondary)' }}>
                        <span className="flex items-center gap-1"><Calendar size={10} />{formatDate(release.released_at || release.created_at)}</span>
                        <span>{release.author}</span>
                        {release.assets_count > 0 && (
                          <span className="flex items-center gap-1"><FileBox size={10} />{release.assets_count}</span>
                        )}
                      </div>
                    </div>
                    <button onClick={e => { e.stopPropagation(); setConfirmDelete({ type: 'release', tag: release.tag_name }); }} className="p-1.5 rounded transition-colors hover:bg-red-500/10 flex-shrink-0" style={{ color: 'var(--color-text-secondary)' }} title={t('gitlab.deleteRelease') || 'Delete release'}>
                      <Trash2 size={14} />
                    </button>
                  </div>

                  {isExpanded && (
                    <div className="px-5 pb-3" style={{ backgroundColor: 'var(--color-bg-primary)' }}>
                      {/* Source archives */}
                      {release.sources.length > 0 && (
                        <div className="mb-2">
                          <div className="text-[10px] font-medium uppercase tracking-wider mb-1" style={{ color: 'var(--color-text-secondary)' }}>Source code</div>
                          <div className="flex gap-2">
                            {release.sources.map(src => (
                              <button key={src.format} onClick={() => shellOpen(src.url)} className="flex items-center gap-1 px-2 py-1 text-xs rounded border transition-colors hover:opacity-80" style={{ borderColor: 'var(--color-border)', color: 'var(--color-text-secondary)' }}>
                                <Download size={10} /> {src.format}
                              </button>
                            ))}
                          </div>
                        </div>
                      )}

                      {/* Asset links */}
                      {isLoadingLinks ? (
                        <div className="flex items-center justify-center py-4"><Loader2 size={14} className="animate-spin text-orange-400" /></div>
                      ) : !tagLinks || tagLinks.length === 0 ? (
                        <div className="text-xs text-center py-3" style={{ color: 'var(--color-text-secondary)' }}>{t('gitlab.noAssets') || 'No assets'}</div>
                      ) : (
                        <div className="rounded-lg border overflow-hidden" style={{ borderColor: 'var(--color-border)' }}>
                          <table className="w-full text-xs">
                            <thead>
                              <tr className="border-b" style={{ borderColor: 'var(--color-border)', backgroundColor: 'var(--color-bg-secondary)' }}>
                                <th className="text-left px-3 py-1.5 font-medium" style={{ color: 'var(--color-text-secondary)' }}>{t('gitlab.assetName') || 'Name'}</th>
                                <th className="text-center px-3 py-1.5 font-medium" style={{ color: 'var(--color-text-secondary)' }}>{t('gitlab.linkType') || 'Type'}</th>
                                <th className="w-16" />
                              </tr>
                            </thead>
                            <tbody>
                              {tagLinks.map(link => (
                                <tr key={link.id} className="border-b last:border-b-0" style={{ borderColor: 'var(--color-border)' }}>
                                  <td className="px-3 py-1.5">
                                    <div className="flex items-center gap-1.5 truncate max-w-[280px]" style={{ color: 'var(--color-text-primary)' }} title={link.name}>
                                      <FileDown size={11} className="flex-shrink-0 text-orange-400" />
                                      <span className="truncate">{link.name}</span>
                                      {link.external && <ExternalLink size={9} className="flex-shrink-0 text-gray-500" />}
                                    </div>
                                  </td>
                                  <td className="px-3 py-1.5 text-center">
                                    <span className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${LINK_TYPE_COLORS[link.link_type] || LINK_TYPE_COLORS.other}`}>
                                      {link.link_type}
                                    </span>
                                  </td>
                                  <td className="px-2 py-1.5">
                                    <div className="flex items-center justify-end gap-1">
                                      <button onClick={async () => {
                                        try {
                                          const savePath = await save({ defaultPath: link.name, title: `Download ${link.name}` });
                                          if (savePath) {
                                            const logId = humanLog.logRaw('activity.release_asset_download_start', 'DOWNLOAD', { provider: 'GitLab', filename: link.name }, 'running');
                                            try {
                                              await invoke('gitlab_download_release_asset', { url: link.direct_asset_url || link.url, localPath: savePath });
                                              humanLog.updateEntry(logId, { status: 'success', message: t('activity.release_asset_download_success', { provider: 'GitLab', filename: link.name }) });
                                            } catch (dlErr) {
                                              humanLog.updateEntry(logId, { status: 'error', message: t('activity.release_asset_download_error', { provider: 'GitLab', filename: link.name }) });
                                              throw dlErr;
                                            }
                                          }
                                        } catch (err) { if (onError) onError('Download', String(err)); }
                                      }} className="p-1 rounded transition-colors hover:opacity-80 text-orange-400" title={t('gitlab.downloadAsset') || 'Download'}>
                                        <Download size={12} />
                                      </button>
                                      <button onClick={e => { e.stopPropagation(); setConfirmDelete({ type: 'asset', tag: release.tag_name, linkId: link.id, linkName: link.name }); }} className="p-1 rounded transition-colors hover:bg-red-500/10" style={{ color: 'var(--color-text-secondary)' }} title={t('gitlab.deleteAsset') || 'Delete asset'}>
                                        <Trash2 size={12} />
                                      </button>
                                    </div>
                                  </td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </div>
                      )}
                      <button onClick={e => { e.stopPropagation(); handleUploadAsset(release.tag_name); }} className="mt-2 flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg border transition-colors hover:opacity-80 text-orange-400" style={{ borderColor: 'var(--color-border)', backgroundColor: 'var(--color-bg-primary)' }}>
                        <Upload size={12} /> {t('gitlab.uploadAsset') || 'Upload Asset'}
                      </button>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Confirmation dialog */}
      {confirmDelete && (
        <div className="absolute inset-0 z-10 flex items-center justify-center bg-black/40 rounded-lg">
          <div className="border border-gray-200 dark:border-gray-700 rounded-lg shadow-xl p-5 mx-6 max-w-sm animate-scale-in" style={{ backgroundColor: 'var(--color-bg-secondary)' }}>
            <p className="text-sm text-gray-700 dark:text-gray-200 mb-4">
              {confirmDelete.type === 'release'
                ? (t('gitlab.confirmDeleteRelease') || `Delete release "${confirmDelete.tag}"? The git tag will be preserved.`)
                : (t('gitlab.confirmDeleteAsset') || `Remove asset "${confirmDelete.linkName}"?`)
              }
            </p>
            <div className="flex justify-end gap-2">
              <button onClick={() => setConfirmDelete(null)} className="px-3 py-1.5 text-xs rounded-lg border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors">{t('common.cancel') || 'Cancel'}</button>
              <button onClick={executeDelete} className="px-3 py-1.5 text-xs rounded-lg bg-red-600 hover:bg-red-700 text-white font-medium transition-colors">{t('common.delete') || 'Delete'}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};
