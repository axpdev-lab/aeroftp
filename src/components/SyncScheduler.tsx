import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { SyncSchedule, TimeWindow, Weekday } from '../types';
import { useTranslation } from '../i18n';
import { Clock, Play, Pause, CalendarDays } from 'lucide-react';
import { logger } from '../utils/logger';

interface SyncSchedulerProps {
    disabled?: boolean;
}

const ALL_DAYS: Weekday[] = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];

const INTERVAL_OPTIONS = [
    { value: 60, label: '1 min' },
    { value: 300, label: '5 min' },
    { value: 600, label: '10 min' },
    { value: 1800, label: '30 min' },
    { value: 3600, label: '1 hour' },
    { value: 7200, label: '2 hours' },
    { value: 14400, label: '4 hours' },
    { value: 86400, label: '24 hours' },
];

function formatCountdown(secs: number): string {
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return `${h}h ${m}m`;
}

export const SyncScheduler: React.FC<SyncSchedulerProps> = ({ disabled }) => {
    const t = useTranslation();
    const dayLabels: Record<Weekday, string> = {
        mon: t('syncPanel.dayMon'),
        tue: t('syncPanel.dayTue'),
        wed: t('syncPanel.dayWed'),
        thu: t('syncPanel.dayThu'),
        fri: t('syncPanel.dayFri'),
        sat: t('syncPanel.daySat'),
        sun: t('syncPanel.daySun'),
    };
    const [schedule, setSchedule] = useState<SyncSchedule>({
        enabled: false,
        interval_secs: 86400,
        time_window: null,
        paused: false,
        last_sync: null,
    });
    const [nextSyncSecs, setNextSyncSecs] = useState<number | null>(null);
    const [showTimeWindow, setShowTimeWindow] = useState(false);
    const unlistenRef = useRef<UnlistenFn | null>(null);

    // Load schedule on mount
    useEffect(() => {
        let mounted = true;

        invoke<SyncSchedule>('get_sync_schedule_cmd')
            .then(s => {
                if (!mounted) return;
                setSchedule(s);
                setShowTimeWindow(!!s.time_window);
            })
            .catch(() => {});

        // Listen for schedule events from backend
        listen<{ next_sync_in_secs?: number; enabled?: boolean; paused?: boolean }>(
            'cloud-sync-schedule',
            (event) => {
                if (event.payload.next_sync_in_secs != null) {
                    setNextSyncSecs(event.payload.next_sync_in_secs);
                }
            }
        ).then(fn => {
            if (mounted) {
                unlistenRef.current = fn;
            } else {
                fn(); // Already unmounted, clean up immediately
            }
        });

        return () => {
            mounted = false;
            unlistenRef.current?.();
        };
    }, []);

    const saveSchedule = async (updated: SyncSchedule) => {
        setSchedule(updated);
        try {
            await invoke('save_sync_schedule_cmd', { schedule: updated });
        } catch (e) { logger.error('[SyncScheduler] save failed:', e); }
    };

    const toggleEnabled = () => {
        saveSchedule({ ...schedule, enabled: !schedule.enabled });
    };

    const togglePaused = () => {
        saveSchedule({ ...schedule, paused: !schedule.paused });
    };

    const setInterval = (secs: number) => {
        saveSchedule({ ...schedule, interval_secs: secs });
    };

    const toggleTimeWindow = () => {
        if (showTimeWindow) {
            saveSchedule({ ...schedule, time_window: null });
            setShowTimeWindow(false);
        } else {
            const tw: TimeWindow = {
                start_hour: 0, start_minute: 0,
                end_hour: 23, end_minute: 59,
                days: [],
            };
            saveSchedule({ ...schedule, time_window: tw });
            setShowTimeWindow(true);
        }
    };

    const updateTimeWindow = (tw: TimeWindow) => {
        saveSchedule({ ...schedule, time_window: tw });
    };

    const toggleDay = (day: Weekday) => {
        const tw = schedule.time_window;
        if (!tw) return;
        const current = tw.days.length > 0 ? tw.days : [...ALL_DAYS];
        const next = current.includes(day)
            ? current.filter(d => d !== day)
            : [...current, day];
        updateTimeWindow({ ...tw, days: next });
    };

    return (
        <div className="space-y-2 p-2 rounded border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800/50">
            {/* Header */}
            <div className="flex items-center justify-between">
                <label className="flex items-center gap-1.5 text-xs font-medium">
                    <Clock size={14} className="text-blue-400" />
                    {t('syncPanel.schedulerTitle')}
                </label>
                <label className="flex items-center gap-1.5 cursor-pointer">
                    <input
                        type="checkbox"
                        checked={schedule.enabled}
                        onChange={toggleEnabled}
                        disabled={disabled}
                        className="w-3.5 h-3.5 accent-blue-500"
                    />
                    <span className="text-xs text-gray-500">{t('syncPanel.schedulerEnabled')}</span>
                </label>
            </div>

            {schedule.enabled && (
                <>
                    {/* Interval + Pause */}
                    <div className="flex items-center gap-2">
                        <select
                            className="text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1 py-0.5 flex-1"
                            value={schedule.interval_secs}
                            onChange={e => setInterval(Number(e.target.value))}
                            disabled={disabled}
                        >
                            {INTERVAL_OPTIONS.map(opt => (
                                <option key={opt.value} value={opt.value}>{opt.label}</option>
                            ))}
                        </select>
                        <button
                            className={`flex items-center gap-1 text-xs px-2 py-0.5 rounded transition-colors ${
                                schedule.paused
                                    ? 'bg-green-500/20 text-green-400 hover:bg-green-500/30'
                                    : 'bg-gray-500/20 text-gray-400 hover:bg-gray-500/30'
                            }`}
                            onClick={togglePaused}
                            disabled={disabled}
                        >
                            {schedule.paused ? <Play size={12} /> : <Pause size={12} />}
                            {schedule.paused ? t('syncPanel.schedulerResume') : t('syncPanel.schedulerPause')}
                        </button>
                    </div>

                    {/* Countdown */}
                    {nextSyncSecs != null && !schedule.paused && (
                        <div className="text-[10px] text-blue-400/80">
                            {t('syncPanel.schedulerNextSync', { time: formatCountdown(nextSyncSecs) })}
                        </div>
                    )}

                    {/* Time Window toggle */}
                    <label className="flex items-center gap-1.5 cursor-pointer">
                        <CalendarDays size={12} className="text-purple-400" />
                        <input
                            type="checkbox"
                            checked={showTimeWindow}
                            onChange={toggleTimeWindow}
                            disabled={disabled}
                            className="w-3 h-3 accent-purple-500"
                        />
                        <span className="text-xs text-gray-500">{t('syncPanel.schedulerTimeWindow')}</span>
                    </label>

                    {/* Time Window details */}
                    {showTimeWindow && schedule.time_window && (
                        <div className="space-y-1.5 pl-4 border-l-2 border-purple-500/30">
                            <div className="flex items-center gap-2 text-xs">
                                <span className="text-gray-500">{t('syncPanel.schedulerFrom')}:</span>
                                <input
                                    type="time"
                                    className="bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1 py-0.5 text-xs"
                                    value={`${String(schedule.time_window.start_hour).padStart(2, '0')}:${String(schedule.time_window.start_minute).padStart(2, '0')}`}
                                    onChange={e => {
                                        const [h, m] = e.target.value.split(':').map(Number);
                                        updateTimeWindow({ ...schedule.time_window!, start_hour: h, start_minute: m });
                                    }}
                                    disabled={disabled}
                                />
                                <span className="text-gray-500">{t('syncPanel.schedulerTo')}:</span>
                                <input
                                    type="time"
                                    className="bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1 py-0.5 text-xs"
                                    value={`${String(schedule.time_window.end_hour).padStart(2, '0')}:${String(schedule.time_window.end_minute).padStart(2, '0')}`}
                                    onChange={e => {
                                        const [h, m] = e.target.value.split(':').map(Number);
                                        updateTimeWindow({ ...schedule.time_window!, end_hour: h, end_minute: m });
                                    }}
                                    disabled={disabled}
                                />
                            </div>
                            <div className="flex items-center gap-1">
                                <span className="text-[10px] text-gray-500 mr-1">{t('syncPanel.schedulerDays')}:</span>
                                {ALL_DAYS.map(day => {
                                    const active = !schedule.time_window?.days?.length || schedule.time_window.days.includes(day);
                                    return (
                                        <button
                                            key={day}
                                            className={`text-[10px] px-1 py-0.5 rounded transition-colors ${
                                                active
                                                    ? 'bg-purple-500/30 text-purple-300'
                                                    : 'bg-gray-500/10 text-gray-500'
                                            }`}
                                            onClick={() => toggleDay(day)}
                                            disabled={disabled}
                                        >
                                            {dayLabels[day]}
                                        </button>
                                    );
                                })}
                            </div>
                        </div>
                    )}
                </>
            )}
        </div>
    );
};
