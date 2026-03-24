// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * GitHubActionsBrowser Component
 * Modal dialog for viewing GitHub Actions workflow runs with live status.
 */

import React, { useState, useEffect, useCallback, useRef } from 'react';
import {
  X, RefreshCw, Loader2, ExternalLink, CheckCircle, XCircle,
  Clock, AlertTriangle, Play, RotateCcw, StopCircle, GitBranch,
  Zap,
} from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { useTranslation } from '../i18n';

interface WorkflowRun {
  id: number;
  name: string;
  branch: string;
  sha: string;
  status: string;
  conclusion: string;
  event: string;
  run_number: number;
  display_title: string;
  created_at: string;
  updated_at: string;
  duration_seconds: number;
  html_url: string;
  actor_login: string;
  actor_avatar: string;
}

interface GitHubActionsBrowserProps {
  isOpen: boolean;
  onClose: () => void;
}

const STATUS_CONFIG: Record<string, { icon: React.ReactNode; color: string; label: string }> = {
  success: { icon: <CheckCircle size={14} />, color: 'text-green-400', label: 'Success' },
  failure: { icon: <XCircle size={14} />, color: 'text-red-400', label: 'Failed' },
  cancelled: { icon: <StopCircle size={14} />, color: 'text-gray-400', label: 'Cancelled' },
  skipped: { icon: <AlertTriangle size={14} />, color: 'text-yellow-500', label: 'Skipped' },
  timed_out: { icon: <Clock size={14} />, color: 'text-orange-400', label: 'Timed out' },
  in_progress: { icon: <Loader2 size={14} className="animate-spin" />, color: 'text-amber-400', label: 'Running' },
  queued: { icon: <Clock size={14} />, color: 'text-blue-400', label: 'Queued' },
  waiting: { icon: <Clock size={14} />, color: 'text-blue-300', label: 'Waiting' },
};

function getStatusConfig(run: WorkflowRun) {
  if (run.status === 'completed') {
    return STATUS_CONFIG[run.conclusion] || STATUS_CONFIG.success;
  }
  return STATUS_CONFIG[run.status] || { icon: <Clock size={14} />, color: 'text-gray-400', label: run.status };
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  if (mins < 60) return `${mins}m ${secs}s`;
  const hrs = Math.floor(mins / 60);
  return `${hrs}h ${mins % 60}m`;
}

function formatRelativeTime(dateStr: string): string {
  const now = Date.now();
  const date = new Date(dateStr).getTime();
  const diff = Math.floor((now - date) / 1000);
  if (diff < 60) return 'just now';
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return new Date(dateStr).toLocaleDateString();
}

export const GitHubActionsBrowser: React.FC<GitHubActionsBrowserProps> = ({
  isOpen,
  onClose,
}) => {
  const t = useTranslation();
  const [runs, setRuns] = useState<WorkflowRun[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionInProgress, setActionInProgress] = useState<number | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchRuns = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<WorkflowRun[]>('github_list_actions_runs', { perPage: 20 });
      setRuns(result || []);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const runsRef = useRef(runs);
  runsRef.current = runs;

  useEffect(() => {
    if (isOpen) {
      fetchRuns();
      pollRef.current = setInterval(() => {
        const hasActive = runsRef.current.some(r =>
          r.status === 'in_progress' || r.status === 'queued' || r.status === 'waiting'
        );
        if (hasActive) fetchRuns();
      }, 15000);
    }
    return () => {
      if (pollRef.current) {
        clearInterval(pollRef.current);
        pollRef.current = null;
      }
    };
  }, [isOpen, fetchRuns]);

  const runAction = async (command: string, runId: number) => {
    setActionInProgress(runId);
    try {
      await invoke(command, { runId });
      setTimeout(fetchRuns, 2000);
    } catch (err) {
      setError(String(err));
    } finally {
      setActionInProgress(null);
    }
  };

  if (!isOpen) return null;

  const hasActiveRuns = runs.some(r => r.status === 'in_progress' || r.status === 'queued');

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]" onClick={onClose}>
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" />
      <div
        className="relative w-full max-w-2xl max-h-[75vh] overflow-hidden rounded-xl shadow-2xl flex flex-col animate-scale-in"
        style={{ backgroundColor: 'var(--color-bg-secondary)' }}
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Zap size={18} className="text-amber-400" />
            <span className="font-semibold text-gray-900 dark:text-gray-100">GitHub Actions</span>
            {hasActiveRuns && (
              <span className="flex items-center gap-1 text-xs text-amber-400">
                <Loader2 size={12} className="animate-spin" />
                Live
              </span>
            )}
          </div>
          <div className="flex items-center gap-1">
            <button
              onClick={fetchRuns}
              disabled={loading}
              className="p-1.5 rounded-lg transition-colors hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
              title={t('github.actionsRefresh') || 'Refresh'}
            >
              <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 rounded-lg transition-colors hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
            >
              <X size={16} />
            </button>
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4">
          {error && (
            <div className="flex items-center gap-2 text-sm text-red-400 px-3 py-2 rounded-lg bg-red-50 dark:bg-red-900/20">
              <AlertTriangle size={14} />
              {error}
            </div>
          )}

          {loading && runs.length === 0 ? (
            <div className="flex items-center justify-center py-12 text-gray-500 dark:text-gray-400">
              <Loader2 size={20} className="animate-spin mr-2" />
              {t('github.actionsLoadingRuns') || 'Loading workflow runs...'}
            </div>
          ) : runs.length === 0 ? (
            <div className="text-center py-12 text-gray-500 dark:text-gray-400">
              {t('github.actionsNoRuns') || 'No workflow runs found.'}
            </div>
          ) : (
            <div className="space-y-0.5">
              {runs.map((run, index) => {
                const cfg = getStatusConfig(run);
                const isActive = run.status === 'in_progress' || run.status === 'queued';
                const isFailed = run.conclusion === 'failure';
                const isActioning = actionInProgress === run.id;
                const isEven = index % 2 === 0;

                return (
                  <div
                    key={run.id}
                    className={`flex items-center gap-3 px-3 py-2 rounded-lg text-xs transition-colors hover:bg-gray-100 dark:hover:bg-gray-700/50 ${
                      isActive ? 'bg-amber-50 dark:bg-amber-500/10 border border-amber-200 dark:border-amber-500/20' : isEven ? 'bg-gray-50 dark:bg-gray-900/50' : ''
                    }`}
                  >
                    {/* Status icon */}
                    <span className={cfg.color} title={cfg.label}>
                      {cfg.icon}
                    </span>

                    {/* Main info */}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-medium text-gray-900 dark:text-gray-100 truncate">
                          {run.display_title || run.name}
                        </span>
                        <span className="text-[10px] text-gray-400 dark:text-gray-500">
                          #{run.run_number}
                        </span>
                      </div>
                      <div className="flex items-center gap-2 text-[11px] text-gray-500 dark:text-gray-400">
                        <span className="flex items-center gap-1">
                          <GitBranch size={10} />
                          {run.branch}
                        </span>
                        <span>·</span>
                        <span className="font-mono">{run.sha.substring(0, 7)}</span>
                        <span>·</span>
                        <span>{run.actor_login}</span>
                      </div>
                    </div>

                    {/* Duration & time */}
                    <div className="flex flex-col items-end text-[11px] text-gray-500 dark:text-gray-400 shrink-0">
                      <span>{formatRelativeTime(run.created_at)}</span>
                      {run.duration_seconds > 0 && (
                        <span className="text-gray-400 dark:text-gray-500">
                          {formatDuration(run.duration_seconds)}
                        </span>
                      )}
                    </div>

                    {/* Actions */}
                    <div className="flex items-center gap-1 shrink-0">
                      {isActive && (
                        <button
                          onClick={() => runAction('github_cancel_workflow', run.id)}
                          disabled={isActioning}
                          className="p-1 rounded hover:bg-red-500/20 text-red-400"
                          title={t('github.cancelRun') || 'Cancel'}
                        >
                          {isActioning ? <Loader2 size={14} className="animate-spin" /> : <StopCircle size={14} />}
                        </button>
                      )}
                      {!isActive && isFailed && (
                        <button
                          onClick={() => runAction('github_rerun_failed_jobs', run.id)}
                          disabled={isActioning}
                          className="p-1 rounded hover:bg-amber-500/20 text-amber-400"
                          title={t('github.rerunFailedJobs') || 'Re-run failed jobs'}
                        >
                          {isActioning ? <Loader2 size={14} className="animate-spin" /> : <RotateCcw size={14} />}
                        </button>
                      )}
                      {!isActive && (
                        <button
                          onClick={() => runAction('github_rerun_workflow', run.id)}
                          disabled={isActioning}
                          className="p-1 rounded hover:bg-green-500/20 text-green-400"
                          title={t('github.rerunAllJobs') || 'Re-run all jobs'}
                        >
                          {isActioning ? <Loader2 size={14} className="animate-spin" /> : <Play size={14} />}
                        </button>
                      )}
                      <button
                        onClick={() => run.html_url && shellOpen(run.html_url)}
                        className="p-1 rounded hover:bg-[var(--color-bg-tertiary)] text-gray-500 dark:text-gray-400"
                        title={t('github.viewOnGithub') || 'View on GitHub'}
                      >
                        <ExternalLink size={14} />
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
