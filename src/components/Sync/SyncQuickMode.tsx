/**
 * SyncQuickMode — Quick Sync tab with preset cards and speed mode pills
 * "Made for freedom, made for all" — accessible to beginners and power users
 */

import React from 'react';
import {
    ArrowDown, ArrowUp, ArrowLeftRight, Zap, Skull
} from 'lucide-react';
import { SyncProfile } from '../../types';
import { useTranslation } from '../../i18n';
import { SpeedMode, SPEED_PRESETS, SPEED_MODE_KEYS } from './syncConstants';

interface SyncQuickModeProps {
    profiles: SyncProfile[];
    activeProfileId: string;
    onSelectProfile: (id: string) => void;
    speedMode: SpeedMode;
    onSpeedModeChange: (mode: SpeedMode) => void;
    showManiac: boolean;
    maniacConfirmed: boolean;
    onManiacConfirm: () => void;
    disabled: boolean;
}

// Preset card configuration
const PROFILE_ICONS: Record<string, typeof ArrowDown> = {
    mirror: ArrowDown,
    two_way: ArrowLeftRight,
    backup: ArrowUp,
};

const PROFILE_COLORS: Record<string, string> = {
    mirror: '#3b82f6',
    two_way: '#10b981',
    backup: '#f59e0b',
};

export const SyncQuickMode: React.FC<SyncQuickModeProps> = React.memo(({
    profiles,
    activeProfileId,
    onSelectProfile,
    speedMode,
    onSpeedModeChange,
    showManiac,
    maniacConfirmed,
    onManiacConfirm,
    disabled,
}) => {
    const t = useTranslation();

    // Only show built-in profiles as preset cards
    const builtinProfiles = profiles.filter(p => p.builtin);

    const handleSpeedChange = (mode: SpeedMode) => {
        if (disabled) return;
        onSpeedModeChange(mode);
    };

    const speedModes: SpeedMode[] = ['normal', 'fast', 'turbo', 'extreme'];
    if (showManiac) speedModes.push('maniac');

    return (
        <div className="sync-quick-mode">
            {/* Sync Mode Preset Cards */}
            <div className="mb-3">
                <label className="text-xs font-medium text-gray-400 mb-1.5 block">
                    {t('syncPanel.direction')}
                </label>
                <div className="sync-preset-grid">
                    {builtinProfiles.map(profile => {
                        const Icon = PROFILE_ICONS[profile.id] || ArrowLeftRight;
                        const color = PROFILE_COLORS[profile.id] || '#3b82f6';
                        const isActive = activeProfileId === profile.id;
                        const label = profile.id === 'mirror' ? t('syncPanel.profileMirror')
                            : profile.id === 'two_way' ? t('syncPanel.profileTwoWay')
                            : profile.id === 'backup' ? t('syncPanel.profileBackup')
                            : profile.name;

                        return (
                            <button
                                key={profile.id}
                                className={`sync-preset-card ${isActive ? 'selected' : ''}`}
                                onClick={() => !disabled && onSelectProfile(profile.id)}
                                disabled={disabled}
                                style={isActive ? { borderColor: color, background: `${color}15` } : undefined}
                            >
                                <Icon size={20} style={{ color }} className="mb-1" />
                                <div className="text-xs font-semibold">{label}</div>
                                <div className="text-[10px] text-gray-500 mt-0.5">
                                    {profile.id === 'mirror' ? t('syncPanel.profileMirrorDesc')
                                        : profile.id === 'two_way' ? t('syncPanel.profileTwoWayDesc')
                                        : profile.id === 'backup' ? t('syncPanel.profileBackupDesc')
                                        : ''}
                                </div>
                            </button>
                        );
                    })}
                </div>
            </div>

            {/* Speed Mode Pills */}
            <div className="mb-2">
                <label className="text-xs font-medium text-gray-400 mb-1.5 block">
                    <Zap size={12} className="inline mr-1" />
                    {t('syncPanel.speedMode')}
                </label>
                <div className="sync-speed-pills">
                    {speedModes.map(mode => {
                        const isActive = speedMode === mode;
                        const preset = SPEED_PRESETS[mode];
                        return (
                            <button
                                key={mode}
                                className={`sync-speed-pill ${mode} ${isActive ? 'active' : ''}`}
                                onClick={() => handleSpeedChange(mode)}
                                disabled={disabled}
                                title={t('syncPanel.speedStreams', { count: preset.parallelStreams })}
                            >
                                {mode === 'maniac' && <Skull size={12} className="mr-1" />}
                                {t(SPEED_MODE_KEYS[mode])}
                            </button>
                        );
                    })}
                </div>
            </div>

            {/* Maniac Warning Card */}
            {speedMode === 'maniac' && !maniacConfirmed && (
                <div className="sync-maniac-warning">
                    <div className="flex items-center gap-2 mb-2">
                        <Skull size={16} className="text-red-500" />
                        <span className="font-bold text-sm">{t('syncPanel.maniacWarningTitle')}</span>
                    </div>
                    <p className="text-xs mb-3 leading-relaxed opacity-80">
                        {t('syncPanel.maniacWarningBody')}
                    </p>
                    <button
                        className="text-xs px-4 py-1.5 rounded-lg bg-red-500/20 text-red-400 border border-red-500/40 hover:bg-red-500/30 transition-colors font-medium"
                        onClick={onManiacConfirm}
                    >
                        {t('syncPanel.maniacConfirm')}
                    </button>
                </div>
            )}

            {/* Maniac Active Indicator */}
            {speedMode === 'maniac' && maniacConfirmed && (
                <div className="sync-maniac-warning" style={{ borderColor: '#10b981', background: 'rgba(16,185,129,0.08)' }}>
                    <div className="flex items-center gap-2">
                        <Skull size={14} className="text-green-500" />
                        <span className="text-xs font-medium text-green-500">
                            {t('syncPanel.maniacWarningTitle')} — {t('syncPanel.maniacActive')}
                        </span>
                    </div>
                </div>
            )}
        </div>
    );
});

SyncQuickMode.displayName = 'SyncQuickMode';
