// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * GitHubPagesBrowser Component
 * Modal dialog for viewing GitHub Pages site info and deployment history.
 */

import React, { useState, useEffect, useCallback } from 'react';
import {
  Globe, X, RefreshCw, Loader2, ExternalLink, CheckCircle,
  XCircle, Clock, AlertTriangle, Shield, GitBranch, FolderOpen,
} from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { useTranslation } from '../i18n';

interface PagesSite {
  url: string | null;
  status: string | null;
  cname: string | null;
  html_url: string | null;
  build_type: string | null;
  source: { branch: string; path: string } | null;
  https_enforced: boolean | null;
  public: boolean | null;
}

interface PagesBuild {
  status: string;
  error: { message: string | null } | null;
  pusher: { login: string | null; avatar_url: string | null } | null;
  commit: string | null;
  duration: number | null;
  created_at: string | null;
}

interface GitHubPagesBrowserProps {
  isOpen: boolean;
  onClose: () => void;
}

export const GitHubPagesBrowser: React.FC<GitHubPagesBrowserProps> = ({
  isOpen,
  onClose,
}) => {
  const t = useTranslation();
  const [site, setSite] = useState<PagesSite | null>(null);
  const [builds, setBuilds] = useState<PagesBuild[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rebuilding, setRebuilding] = useState(false);
  const [dnsHealth, setDnsHealth] = useState<{ healthy?: boolean; reason?: string } | null>(null);

  const fetchData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const siteData = await invoke<PagesSite | null>('github_get_pages');
      setSite(siteData);
      if (siteData) {
        const buildsData = await invoke<PagesBuild[]>('github_list_pages_builds');
        setBuilds(buildsData || []);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (isOpen) fetchData();
  }, [isOpen, fetchData]);

  const handleRebuild = useCallback(async () => {
    setRebuilding(true);
    try {
      await invoke('github_trigger_pages_build');
      setTimeout(fetchData, 2000);
    } catch (err) {
      setError(String(err));
    } finally {
      setRebuilding(false);
    }
  }, [fetchData]);

  const displayUrl = (site?.html_url || site?.url || '').replace(/^http:\/\//, 'https://');

  const handleOpenSite = useCallback(() => {
    if (displayUrl) shellOpen(displayUrl);
  }, [displayUrl]);

  if (!isOpen) return null;

  const normalizeStatus = (status: string | null): string => status || 'built';

  const StatusIcon: React.FC<{ status: string | null; size?: number }> = ({ status, size = 14 }) => {
    const s = normalizeStatus(status);
    if (s === 'built') return <CheckCircle size={size} className="text-green-500" />;
    if (s === 'building') return <Loader2 size={size} className="text-amber-500 animate-spin" />;
    if (s === 'errored') return <XCircle size={size} className="text-red-500" />;
    return <CheckCircle size={size} className="text-green-500" />;
  };

  const statusColor = (status: string | null) => {
    const s = normalizeStatus(status);
    if (s === 'built') return 'text-green-500';
    if (s === 'building') return 'text-amber-500';
    if (s === 'errored') return 'text-red-500';
    return 'text-green-500';
  };

  const formatDuration = (secs: number | null) => {
    if (!secs) return '—';
    return secs >= 60 ? `${Math.floor(secs / 60)}m ${secs % 60}s` : `${secs}s`;
  };

  const formatDate = (iso: string | null) => {
    if (!iso) return '—';
    const d = new Date(iso);
    const now = Date.now();
    const diff = now - d.getTime();
    if (diff < 60000) return 'just now';
    if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
    if (diff < 86400000) return `${Math.floor(diff / 3600000)}h ago`;
    return d.toLocaleDateString();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]" onClick={onClose}>
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" />
      <div
        className="relative w-full max-w-2xl overflow-hidden rounded-xl shadow-2xl animate-scale-in"
        style={{ backgroundColor: 'var(--color-bg-secondary)' }}
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2 font-medium text-sm text-gray-900 dark:text-gray-100">
            <Globe size={16} className="text-green-500" />
            GitHub Pages
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={fetchData}
              disabled={loading}
              className="p-1.5 rounded-lg transition-colors hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
              title={t('github.pagesRefresh') || 'Refresh'}
            >
              <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            </button>
            <button onClick={onClose} className="p-1.5 rounded-lg transition-colors hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400">
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Content */}
        <div className="px-5 py-4 space-y-4">
          {error && (
            <div className="flex items-center gap-2 text-sm text-red-400 px-3 py-2 rounded-lg bg-red-50 dark:bg-red-900/20">
              <AlertTriangle size={14} />
              {error}
            </div>
          )}

          {loading && !site ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 size={20} className="animate-spin text-blue-500" />
            </div>
          ) : !site ? (
            <div className="text-center py-8 space-y-3">
              <Globe size={32} className="mx-auto opacity-30" />
              <p className="text-sm text-gray-500 dark:text-gray-400">
                {t('github.pagesNotEnabled') || 'GitHub Pages is not enabled for this repository.'}
              </p>
              <p className="text-xs text-gray-400 dark:text-gray-500">
                {t('github.pagesEnableHint') || 'Enable it from repository Settings > Pages on GitHub.'}
              </p>
            </div>
          ) : (
            <>
              {/* Site Info Card */}
              <div className="rounded-lg border border-gray-200 dark:border-gray-700 p-4 space-y-3" style={{ backgroundColor: 'var(--color-bg-primary)' }}>
                {/* Status + Actions */}
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <StatusIcon status={site.status} />
                    <span className={`text-xs font-medium uppercase ${statusColor(site.status)}`}>
                      {normalizeStatus(site.status)}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    {site.build_type === 'legacy' && (
                      <button
                        onClick={handleRebuild}
                        disabled={rebuilding}
                        className="flex items-center gap-1 text-xs px-2.5 py-1 rounded-lg border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 transition-colors hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40"
                      >
                        {rebuilding ? <Loader2 size={10} className="animate-spin" /> : <RefreshCw size={10} />}
                        {t('github.pagesRebuild') || 'Rebuild'}
                      </button>
                    )}
                    <button
                      onClick={handleOpenSite}
                      className="flex items-center gap-1 text-xs px-2.5 py-1 rounded-lg transition-colors text-white bg-blue-500 hover:bg-blue-600"
                    >
                      <ExternalLink size={10} />
                      {t('github.pagesOpenSite') || 'Open Site'}
                    </button>
                  </div>
                </div>

                {/* URL */}
                {displayUrl && (
                  <p className="text-sm cursor-pointer hover:underline truncate text-blue-500 dark:text-blue-400" onClick={handleOpenSite}>
                    {displayUrl}
                  </p>
                )}

                {/* Info chips */}
                <div className="flex flex-wrap items-center gap-2 text-xs text-gray-500 dark:text-gray-400">
                  {site.source && (
                    <span className="flex items-center gap-1 px-2 py-0.5 rounded-full bg-gray-200 dark:bg-gray-700">
                      <GitBranch size={10} />
                      {site.source.branch}
                    </span>
                  )}
                  {site.source && (
                    <span className="flex items-center gap-1 px-2 py-0.5 rounded-full bg-gray-200 dark:bg-gray-700">
                      <FolderOpen size={10} />
                      {site.source.path || '/'}
                    </span>
                  )}
                  {site.build_type && (
                    <span className="px-2 py-0.5 rounded-full bg-gray-200 dark:bg-gray-700">
                      {site.build_type === 'workflow' ? (t('github.buildTypeActions') || 'GitHub Actions') : (t('github.buildTypeLegacy') || 'Legacy')}
                    </span>
                  )}
                  {site.https_enforced && (
                    <span className="flex items-center gap-1 px-2 py-0.5 rounded-full text-green-600 dark:text-green-400 bg-green-100 dark:bg-green-900/30">
                      <Shield size={10} />
                      HTTPS
                    </span>
                  )}
                  {site.cname && (
                    <span className="px-2 py-0.5 rounded-full bg-gray-200 dark:bg-gray-700">
                      {site.cname}
                    </span>
                  )}
                </div>

                {site.build_type === 'workflow' && (
                  <p className="text-xs text-gray-400 dark:text-gray-500">
                    {t('github.pagesWorkflowHint') || 'Deployments are managed by GitHub Actions. Push a commit to trigger a new build.'}
                  </p>
                )}
              </div>

              {/* DNS Health & Config — W3-02 */}
              {site.cname && (
                <div className="rounded-lg border p-3 space-y-2" style={{ borderColor: 'var(--color-border)', backgroundColor: 'var(--color-bg-primary)' }}>
                  <div className="flex items-center justify-between">
                    <h3 className="text-xs font-medium uppercase tracking-wide" style={{ color: 'var(--color-text-secondary)' }}>
                      {t('github.pagesDnsHealth') || 'DNS Health'}
                    </h3>
                    <button
                      onClick={async () => {
                        try {
                          const result = await invoke<{ healthy?: boolean; reason?: string }>('github_pages_health');
                          setDnsHealth(result);
                        } catch (err) {
                          setDnsHealth({ healthy: false, reason: String(err) });
                        }
                      }}
                      className="text-xs px-2 py-0.5 rounded transition-colors"
                      style={{ color: 'var(--color-accent)' }}
                    >
                      {t('common.check') || 'Check'}
                    </button>
                  </div>
                  {dnsHealth && (
                    <div className={`flex items-center gap-1.5 text-xs ${dnsHealth.healthy ? 'text-green-500' : 'text-amber-500'}`}>
                      {dnsHealth.healthy ? <CheckCircle size={12} /> : <AlertTriangle size={12} />}
                      <span>{dnsHealth.healthy ? (t('github.pagesDnsHealthy') || 'DNS configured correctly') : (dnsHealth.reason || 'DNS issue detected')}</span>
                    </div>
                  )}
                </div>
              )}

              {/* Build History */}
              <div>
                <h3 className="text-xs font-medium mb-2 uppercase tracking-wide text-gray-500 dark:text-gray-400">
                  {t('github.pagesBuilds') || 'Deployment History'}
                </h3>

                {loading ? (
                  <div className="flex items-center justify-center py-4 gap-2">
                    <Loader2 size={14} className="animate-spin text-gray-400" />
                    <span className="text-xs text-gray-400 dark:text-gray-500">{t('github.pagesLoadingDeployments') || 'Loading deployments...'}</span>
                  </div>
                ) : builds.length === 0 ? (
                  <p className="text-xs py-4 text-center text-gray-400 dark:text-gray-500">
                    {t('github.pagesNoBuilds') || 'No deployments found.'}
                  </p>
                ) : (
                  <div className="space-y-0.5 max-h-[35vh] overflow-y-auto pr-2">
                    {builds.map((build, i) => (
                      <div
                        key={i}
                        className={`flex items-center gap-3 px-3 py-2 rounded-lg text-xs ${i % 2 === 0 ? 'bg-gray-50 dark:bg-gray-900/50' : ''}`}
                      >
                        <div className="flex-shrink-0">
                          <StatusIcon status={build.status} size={12} />
                        </div>
                        <span className="font-mono flex-shrink-0 text-gray-500 dark:text-gray-400">
                          {build.commit ? build.commit.slice(0, 7) : '—'}
                        </span>
                        <span className="flex items-center gap-1 flex-shrink-0">
                          {build.pusher?.avatar_url && (
                            <img src={build.pusher.avatar_url} alt="" className="w-4 h-4 rounded-full" />
                          )}
                          <span className="text-gray-500 dark:text-gray-400">
                            {build.pusher?.login || '—'}
                          </span>
                        </span>
                        {build.error?.message ? (
                          <span className="text-red-400 truncate flex-1" title={build.error.message}>
                            {build.error.message}
                          </span>
                        ) : (
                          <span className="flex-1" />
                        )}
                        <span className="flex-shrink-0 text-gray-400 dark:text-gray-500">
                          {formatDuration(build.duration)}
                        </span>
                        <span className="flex-shrink-0 w-20 text-right text-gray-400 dark:text-gray-500">
                          {formatDate(build.created_at)}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
};

export default GitHubPagesBrowser;
