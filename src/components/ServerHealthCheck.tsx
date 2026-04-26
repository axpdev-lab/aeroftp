// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useCallback, useRef, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    Activity, X, RefreshCw, Wifi, WifiOff, AlertTriangle,
    Globe, Server, Lock, Zap, ChevronDown, ChevronRight, Copy, Check,
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { ServerProfile, isOAuthProvider, isFourSharedProvider } from '../types';

interface CheckDetail {
    name: string;
    status: string;
    latency_ms: number | null;
    details: string | null;
}

interface HealthCheckResult {
    server_id: string;
    host: string;
    status: string;
    score: number;
    checks: CheckDetail[];
    checked_at: string;
    error: string | null;
}

interface ServerHealthCheckProps {
    servers: ServerProfile[];
    onClose: () => void;
    singleServerId?: string;
}

const CHECK_ICONS: Record<string, React.ReactNode> = {
    dns_resolution: <Globe size={14} />,
    tcp_connect: <Server size={14} />,
    tls_handshake: <Lock size={14} />,
    http_response: <Zap size={14} />,
};

const CHECK_LABELS: Record<string, string> = {
    dns_resolution: 'DNS Resolution',
    tcp_connect: 'TCP Connect',
    tls_handshake: 'TLS Handshake',
    http_response: 'HTTP Response',
};

const STATUS_CONFIG = {
    healthy: { color: 'text-green-500', bg: 'bg-green-500', ring: 'ring-green-400/30', label: 'Healthy' },
    degraded: { color: 'text-yellow-500', bg: 'bg-yellow-500', ring: 'ring-yellow-400/30', label: 'Degraded' },
    unreachable: { color: 'text-red-500', bg: 'bg-red-500', ring: 'ring-red-400/30', label: 'Unreachable' },
    error: { color: 'text-gray-500', bg: 'bg-gray-500', ring: 'ring-gray-400/30', label: 'Error' },
};

/** Animated radial score gauge */
const ScoreGauge: React.FC<{ score: number; size?: number; legendTooltip?: string }> = ({ score, size = 48, legendTooltip }) => {
    const radius = (size - 6) / 2;
    const circumference = 2 * Math.PI * radius;
    const offset = circumference - (score / 100) * circumference;
    const color = score >= 80 ? '#22c55e' : score >= 50 ? '#eab308' : '#ef4444';

    return (
        <div className="relative" style={{ width: size, height: size }} title={legendTooltip}>
            <svg width={size} height={size} className="-rotate-90">
                <circle cx={size / 2} cy={size / 2} r={radius}
                    stroke="currentColor" className="text-gray-200 dark:text-gray-700"
                    strokeWidth={3} fill="none" />
                <circle cx={size / 2} cy={size / 2} r={radius}
                    stroke={color} strokeWidth={3} fill="none"
                    strokeDasharray={circumference} strokeDashoffset={offset}
                    strokeLinecap="round"
                    style={{ transition: 'stroke-dashoffset 0.8s ease-out' }} />
            </svg>
            <span className="absolute inset-0 flex items-center justify-center text-xs font-bold"
                style={{ color }}>
                {score}
            </span>
        </div>
    );
};

/** Animated latency bar */
const LatencyBar: React.FC<{ ms: number | null; maxMs?: number }> = ({ ms, maxMs = 500 }) => {
    if (ms === null) return <span className="text-gray-400 text-xs">—</span>;
    const pct = Math.min((ms / maxMs) * 100, 100);
    const color = ms < 100 ? 'bg-green-500' : ms < 300 ? 'bg-yellow-500' : 'bg-red-500';
    return (
        <div className="flex items-center gap-2 min-w-[120px]">
            <div className="flex-1 h-1.5 bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                <div className={`h-full rounded-full ${color}`}
                    style={{ width: `${pct}%`, transition: 'width 0.6s ease-out' }} />
            </div>
            <span className="text-xs font-mono tabular-nums w-[52px] text-right">
                {ms < 1 ? '<1ms' : ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${Math.round(ms)}ms`}
            </span>
        </div>
    );
};

/** Latency sparkline chart for all checks of a server */
const LatencyChart: React.FC<{ checks: CheckDetail[] }> = ({ checks }) => {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const validChecks = checks.filter(c => c.latency_ms !== null && c.status !== 'skip');

    useEffect(() => {
        const canvas = canvasRef.current;
        if (!canvas || validChecks.length === 0) return;
        const ctx = canvas.getContext('2d');
        if (!ctx) return;

        const dpr = window.devicePixelRatio || 1;
        const w = canvas.clientWidth;
        const h = canvas.clientHeight;
        canvas.width = w * dpr;
        canvas.height = h * dpr;
        ctx.scale(dpr, dpr);

        const values = validChecks.map(c => c.latency_ms!);
        const maxVal = Math.max(...values, 10);
        const padding = { top: 4, bottom: 14, left: 4, right: 4 };
        const chartW = w - padding.left - padding.right;
        const chartH = h - padding.top - padding.bottom;

        // Background
        ctx.clearRect(0, 0, w, h);

        // Area fill
        ctx.beginPath();
        values.forEach((val, i) => {
            const x = padding.left + (i / Math.max(values.length - 1, 1)) * chartW;
            const y = padding.top + chartH - (val / maxVal) * chartH;
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
        });
        // Close bottom
        ctx.lineTo(padding.left + chartW, padding.top + chartH);
        ctx.lineTo(padding.left, padding.top + chartH);
        ctx.closePath();

        const gradient = ctx.createLinearGradient(0, padding.top, 0, padding.top + chartH);
        gradient.addColorStop(0, 'rgba(59, 130, 246, 0.3)');
        gradient.addColorStop(1, 'rgba(59, 130, 246, 0.02)');
        ctx.fillStyle = gradient;
        ctx.fill();

        // Line
        ctx.beginPath();
        values.forEach((val, i) => {
            const x = padding.left + (i / Math.max(values.length - 1, 1)) * chartW;
            const y = padding.top + chartH - (val / maxVal) * chartH;
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
        });
        ctx.strokeStyle = '#3b82f6';
        ctx.lineWidth = 1.5;
        ctx.lineJoin = 'round';
        ctx.stroke();

        // Dots + labels
        values.forEach((val, i) => {
            const x = padding.left + (i / Math.max(values.length - 1, 1)) * chartW;
            const y = padding.top + chartH - (val / maxVal) * chartH;
            const color = val < 100 ? '#22c55e' : val < 300 ? '#eab308' : '#ef4444';

            ctx.beginPath();
            ctx.arc(x, y, 3, 0, Math.PI * 2);
            ctx.fillStyle = color;
            ctx.fill();
            ctx.strokeStyle = '#fff';
            ctx.lineWidth = 1;
            ctx.stroke();

            // Label below
            ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue('--color-text-secondary').trim() || '#9ca3af';
            ctx.font = '9px system-ui';
            ctx.textAlign = 'center';
            const label = validChecks[i].name.replace('_', '\n').split('_')[0].substring(0, 4).toUpperCase();
            ctx.fillText(label, x, h - 2);
        });
    }, [validChecks]);

    if (validChecks.length === 0) return null;

    return (
        <canvas ref={canvasRef} className="w-full h-[60px]" style={{ imageRendering: 'auto' }} />
    );
};

/** Status dot with pulse animation */
const StatusDot: React.FC<{ status: string; size?: number }> = ({ status, size = 8 }) => {
    const cfg = STATUS_CONFIG[status as keyof typeof STATUS_CONFIG] || STATUS_CONFIG.error;
    return (
        <span className="relative flex items-center justify-center" style={{ width: size * 2.5, height: size * 2.5 }}>
            {status === 'healthy' && (
                <span className={`absolute inset-0 rounded-full ${cfg.bg} opacity-20 animate-ping`} />
            )}
            <span className={`relative rounded-full ${cfg.bg}`} style={{ width: size, height: size }} />
        </span>
    );
};

export const ServerHealthCheck: React.FC<ServerHealthCheckProps> = ({ servers, onClose, singleServerId }) => {
    const t = useTranslation();
    const [results, setResults] = useState<Map<string, HealthCheckResult>>(new Map());
    const [checking, setChecking] = useState<Set<string>>(new Set());
    const [expanded, setExpanded] = useState<Set<string>>(new Set());
    const [allChecking, setAllChecking] = useState(false);
    const [copied, setCopied] = useState(false);
    const mountedRef = useRef(true);

    useEffect(() => {
        mountedRef.current = true;
        return () => { mountedRef.current = false; };
    }, []);

    // Auto-check single server on mount
    useEffect(() => {
        if (singleServerId) {
            const server = servers.find(s => s.id === singleServerId);
            if (server) {
                checkSingle(server);
                setExpanded(new Set([singleServerId]));
            }
        }
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [singleServerId]);

    const getHostAndPort = (server: ServerProfile) => {
        const protocol = server.protocol || 'ftp';
        const isCloud = isOAuthProvider(protocol) || isFourSharedProvider(protocol) ||
            ['mega', 'filen', 'internxt', 'azure', 'filelu', 'koofr', 'yandexdisk', 'jottacloud', 'kdrive', 'drime'].includes(protocol);
        const host = isCloud ? protocol : (server.host || 'localhost');
        const port = isCloud ? 443 : (server.port || 21);
        const endpoint = server.options?.endpoint || (protocol === 'webdav' || protocol === 's3' ? server.host : undefined);
        return { host, port, protocol, endpoint };
    };

    const checkSingle = useCallback(async (server: ServerProfile) => {
        const id = server.id;
        setChecking(prev => new Set(prev).add(id));

        try {
            const { host, port, protocol, endpoint } = getHostAndPort(server);
            const result = await invoke<HealthCheckResult>('server_health_check', {
                serverId: id, host, port, protocol, endpoint,
            });
            if (mountedRef.current) {
                setResults(prev => new Map(prev).set(id, result));
            }
        } catch (e) {
            if (mountedRef.current) {
                setResults(prev => new Map(prev).set(id, {
                    server_id: id, host: server.host || '', status: 'error', score: 0,
                    checks: [], checked_at: new Date().toISOString(),
                    error: String(e),
                }));
            }
        } finally {
            if (mountedRef.current) {
                setChecking(prev => { const s = new Set(prev); s.delete(id); return s; });
            }
        }
    }, []);

    const checkAll = useCallback(async () => {
        setAllChecking(true);
        const requests = servers.map(s => {
            const { host, port, protocol, endpoint } = getHostAndPort(s);
            return { server_id: s.id, host, port, protocol, endpoint };
        });

        try {
            const results = await invoke<HealthCheckResult[]>('server_health_check_batch', { servers: requests });
            if (mountedRef.current) {
                const map = new Map<string, HealthCheckResult>();
                results.forEach(r => map.set(r.server_id, r));
                setResults(map);
            }
        } catch (e) {
            console.error('Batch health check failed:', e);
        } finally {
            if (mountedRef.current) setAllChecking(false);
        }
    }, [servers]);

    const toggleExpanded = (id: string) => {
        setExpanded(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    const targetServers = singleServerId
        ? servers.filter(s => s.id === singleServerId)
        : servers;

    // Copy results to clipboard
    const copyResults = useCallback(() => {
        const lines: string[] = [];
        const date = new Date().toLocaleString();
        lines.push(`AeroFTP Server Health Check — ${date}`);
        lines.push('='.repeat(60));
        lines.push('');

        const sorted = targetServers
            .map(s => ({ server: s, result: results.get(s.id) }))
            .filter(x => x.result)
            .sort((a, b) => (a.result!.score) - (b.result!.score));

        for (const { server, result } of sorted) {
            if (!result) continue;
            const icon = result.status === 'healthy' ? '[OK]' : result.status === 'degraded' ? '[!!]' : '[XX]';
            lines.push(`${icon} ${server.name || server.host}  —  Score: ${result.score}/100  (${result.status.toUpperCase()})`);
            lines.push(`    Host: ${result.host}  |  Protocol: ${(server.protocol || 'ftp').toUpperCase()}`);
            for (const check of result.checks) {
                const st = check.status === 'pass' ? 'PASS' : check.status === 'fail' ? 'FAIL' : 'SKIP';
                const latency = check.latency_ms !== null ? `${Math.round(check.latency_ms)}ms` : '-';
                lines.push(`    ${st}  ${(CHECK_LABELS[check.name] || check.name).padEnd(16)} ${latency.padStart(8)}  ${check.details || ''}`);
            }
            if (result.error) lines.push(`    ERROR: ${result.error}`);
            lines.push('');
        }

        lines.push('-'.repeat(60));
        const h = Array.from(results.values()).filter(r => r.status === 'healthy').length;
        const d = Array.from(results.values()).filter(r => r.status === 'degraded').length;
        const u = Array.from(results.values()).filter(r => r.status === 'unreachable' || r.status === 'error').length;
        lines.push(`Summary: ${h} healthy, ${d} degraded, ${u} unreachable (${results.size} total)`);

        navigator.clipboard.writeText(lines.join('\n')).then(() => {
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
        });
    }, [targetServers, results]);

    // Summary stats
    const totalChecked = results.size;
    const healthy = Array.from(results.values()).filter(r => r.status === 'healthy').length;
    const degraded = Array.from(results.values()).filter(r => r.status === 'degraded').length;
    const unreachable = Array.from(results.values()).filter(r => r.status === 'unreachable' || r.status === 'error').length;

    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm animate-fade-in"
            onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
            <div className="w-full max-w-2xl bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 overflow-hidden animate-scale-in"
                onClick={e => e.stopPropagation()}>
                {/* Header */}
                <div className="px-5 py-4 border-b border-gray-200 dark:border-gray-700 flex items-center justify-between">
                    <div className="flex items-center gap-3">
                        <div className="p-2 bg-blue-100 dark:bg-blue-900/40 rounded-lg">
                            <Activity size={20} className="text-blue-600 dark:text-blue-400" />
                        </div>
                        <div>
                            <h2 className="text-base font-semibold">{t('healthCheck.title')}</h2>
                            <p className="text-xs text-gray-500">{t('healthCheck.subtitle')}</p>
                            <p className="text-[11px] text-gray-400 dark:text-gray-500 mt-0.5" title={t('healthCheck.scoreLegendDetail')}>
                                {t('healthCheck.scoreLegend')}
                            </p>
                        </div>
                    </div>
                    <div className="flex items-center gap-2">
                        {totalChecked > 0 && (
                            <button
                                onClick={copyResults}
                                className="px-3 py-1.5 text-xs font-medium bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors flex items-center gap-1.5"
                                title={t('healthCheck.copyResults')}
                            >
                                {copied ? <Check size={13} className="text-green-500" /> : <Copy size={13} />}
                                {copied ? t('healthCheck.copied') : t('healthCheck.copyResults')}
                            </button>
                        )}
                        {!singleServerId && (
                            <button
                                onClick={checkAll}
                                disabled={allChecking || servers.length === 0}
                                className="px-3 py-1.5 text-xs font-medium bg-blue-600 hover:bg-blue-700 disabled:bg-blue-400 text-white rounded-lg transition-colors flex items-center gap-1.5"
                            >
                                <RefreshCw size={13} className={allChecking ? 'animate-spin' : ''} />
                                {t('healthCheck.checkAll')}
                            </button>
                        )}
                        <button onClick={onClose}
                            className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-lg transition-colors">
                            <X size={18} />
                        </button>
                    </div>
                </div>

                {/* Summary bar */}
                {totalChecked > 0 && !singleServerId && (
                    <div className="px-5 py-2.5 bg-gray-50 dark:bg-gray-800/50 border-b border-gray-200 dark:border-gray-700 flex items-center gap-4 text-xs">
                        <span className="text-gray-500">{totalChecked} {t('healthCheck.checked')}</span>
                        {healthy > 0 && (
                            <span className="flex items-center gap-1 text-green-600">
                                <Wifi size={12} /> {healthy} {t('healthCheck.healthy')}
                            </span>
                        )}
                        {degraded > 0 && (
                            <span className="flex items-center gap-1 text-yellow-600">
                                <AlertTriangle size={12} /> {degraded} {t('healthCheck.degraded')}
                            </span>
                        )}
                        {unreachable > 0 && (
                            <span className="flex items-center gap-1 text-red-600">
                                <WifiOff size={12} /> {unreachable} {t('healthCheck.unreachable')}
                            </span>
                        )}
                    </div>
                )}

                {/* Batch checking overlay toast */}
                {allChecking && (
                    <div className="px-5 py-3 flex items-center justify-center gap-3 bg-blue-600/10 border-b border-blue-500/20">
                        <div className="w-5 h-5 border-2 border-blue-300 border-t-blue-600 rounded-full animate-spin" />
                        <span className="text-sm font-medium text-blue-400">
                            {t('healthCheck.batchChecking', { count: servers.length })}
                        </span>
                    </div>
                )}

                {/* Server list */}
                <div className="max-h-[60vh] overflow-y-auto">
                    {targetServers.length === 0 && (
                        <div className="py-12 text-center text-gray-400">
                            <Server size={32} className="mx-auto mb-2 opacity-40" />
                            <p className="text-sm">{t('healthCheck.noServers')}</p>
                        </div>
                    )}

                    {targetServers.map(server => {
                        const result = results.get(server.id);
                        const isChecking = checking.has(server.id);
                        const isExpanded = expanded.has(server.id);
                        const statusCfg = result
                            ? STATUS_CONFIG[result.status as keyof typeof STATUS_CONFIG] || STATUS_CONFIG.error
                            : null;

                        return (
                            <div key={server.id} className="border-b border-gray-100 dark:border-gray-800 last:border-b-0">
                                {/* Server row */}
                                <div className="px-5 py-3 flex items-center gap-3 hover:bg-gray-50 dark:hover:bg-gray-800/50 transition-colors cursor-pointer"
                                    onClick={() => {
                                        if (result) toggleExpanded(server.id);
                                        else checkSingle(server);
                                    }}>
                                    {/* Expand chevron */}
                                    <span className="w-4 flex-shrink-0 text-gray-400">
                                        {result && (
                                            isExpanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />
                                        )}
                                    </span>

                                    {/* Status indicator */}
                                    <div className="w-6 flex-shrink-0 flex justify-center">
                                        {isChecking ? (
                                            <RefreshCw size={14} className="animate-spin text-blue-500" />
                                        ) : result ? (
                                            <StatusDot status={result.status} />
                                        ) : (
                                            <span className="w-2 h-2 rounded-full bg-gray-300 dark:bg-gray-600" />
                                        )}
                                    </div>

                                    {/* Server name and info */}
                                    <div className="flex-1 min-w-0">
                                        <div className="text-sm font-medium truncate">
                                            {server.name || server.host}
                                        </div>
                                        <div className="text-xs text-gray-500 truncate">
                                            {result ? result.host : (server.host || server.protocol)}
                                            <span className="ml-2 px-1.5 py-0.5 bg-gray-100 dark:bg-gray-800 rounded text-[10px] uppercase">
                                                {server.protocol || 'ftp'}
                                            </span>
                                        </div>
                                    </div>

                                    {/* Score gauge */}
                                    <div className="flex-shrink-0">
                                        {result && !isChecking ? (
                                            <ScoreGauge
                                                score={result.score}
                                                legendTooltip={`${t('healthCheck.scoreLegend')} — ${t('healthCheck.scoreLegendDetail')}`}
                                            />
                                        ) : isChecking ? (
                                            <div className="w-12 h-12 flex items-center justify-center">
                                                <div className="w-8 h-8 border-2 border-blue-200 border-t-blue-500 rounded-full animate-spin" />
                                            </div>
                                        ) : null}
                                    </div>

                                    {/* Status label */}
                                    <div className="w-[80px] flex-shrink-0 text-right">
                                        {result && statusCfg && (
                                            <span className={`text-xs font-medium ${statusCfg.color}`}>
                                                {t(`healthCheck.${result.status}`)}
                                            </span>
                                        )}
                                    </div>

                                    {/* Individual check button */}
                                    <button
                                        onClick={(e) => { e.stopPropagation(); checkSingle(server); }}
                                        disabled={isChecking}
                                        className="p-1.5 hover:bg-gray-200 dark:hover:bg-gray-700 rounded-lg transition-colors flex-shrink-0"
                                        title={t('healthCheck.checkOne')}
                                    >
                                        <RefreshCw size={13} className={isChecking ? 'animate-spin text-blue-500' : 'text-gray-400'} />
                                    </button>
                                </div>

                                {/* Expanded details */}
                                {isExpanded && result && (
                                    <div className="px-5 pb-4 ml-10 mr-4 animate-fade-in">
                                        {/* Latency chart */}
                                        <div className="mb-3 bg-gray-50 dark:bg-gray-800/60 rounded-lg p-3">
                                            <LatencyChart checks={result.checks} />
                                        </div>

                                        {/* Check details */}
                                        <div className="space-y-1.5">
                                            {result.checks.map((check, i) => (
                                                <div key={i} className="flex items-center gap-3 py-1">
                                                    <span className={`w-5 flex-shrink-0 ${check.status === 'pass' ? 'text-green-500' : check.status === 'fail' ? 'text-red-500' : 'text-gray-400'}`}>
                                                        {CHECK_ICONS[check.name] || <Activity size={14} />}
                                                    </span>
                                                    <span className="text-xs font-medium w-[100px] flex-shrink-0">
                                                        {CHECK_LABELS[check.name] || check.name}
                                                    </span>
                                                    <div className="flex-1">
                                                        <LatencyBar ms={check.latency_ms} />
                                                    </div>
                                                    <span className="text-[10px] text-gray-500 max-w-[160px] truncate" title={check.details || ''}>
                                                        {check.details}
                                                    </span>
                                                </div>
                                            ))}
                                        </div>

                                        {/* Error message */}
                                        {result.error && (
                                            <div className="mt-2 text-xs text-red-500 flex items-center gap-1">
                                                <AlertTriangle size={12} />
                                                {result.error}
                                            </div>
                                        )}

                                        {/* Timestamp */}
                                        <div className="mt-2 text-[10px] text-gray-400 text-right">
                                            {new Date(result.checked_at).toLocaleTimeString()}
                                        </div>
                                    </div>
                                )}
                            </div>
                        );
                    })}
                </div>
            </div>
        </div>
    );
};

export default ServerHealthCheck;
