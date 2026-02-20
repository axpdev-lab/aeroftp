/**
 * SyncAdvancedConfig — Advanced tab with all granular sync controls
 * Direction, compare options, verify/retry, bandwidth, parallel, compression, delta
 * Grouped into visual sections for professional appearance
 */

import React, { useState } from 'react';
import {
    ArrowDown, ArrowUp, ArrowLeftRight,
    ShieldCheck, RotateCcw, Gauge, Zap, Shrink, HardDrive,
    ArrowDownToLine, ArrowUpFromLine,
    FolderTree, FileDown, Undo2, Trash2, GitCompare, Settings2, Activity,
    ChevronDown, FlaskConical
} from 'lucide-react';
import {
    CompareOptions, RetryPolicy, VerifyPolicy, CompressionMode,
    SyncDirection, ProviderType, TransferOptimizationHints, isFtpProtocol
} from '../../types';
import { useTranslation } from '../../i18n';
import { BANDWIDTH_OPTIONS } from './syncConstants';
import { SyncOptimizationBadges } from './SyncOptimizationBadges';
import { SyncScheduler } from '../SyncScheduler';
import { WatcherStatus } from '../WatcherStatus';

interface SyncAdvancedConfigProps {
    options: CompareOptions;
    onOptionsChange: (o: CompareOptions) => void;
    verifyPolicy: VerifyPolicy;
    onVerifyPolicyChange: (v: VerifyPolicy) => void;
    retryPolicy: RetryPolicy;
    onRetryPolicyChange: (r: RetryPolicy) => void;
    downloadLimit: number;
    uploadLimit: number;
    onSpeedLimitChange: (dl: number, ul: number) => void;
    parallelStreams: number;
    onParallelStreamsChange: (n: number) => void;
    compressionMode: CompressionMode;
    onCompressionModeChange: (m: CompressionMode) => void;
    deltaSyncEnabled: boolean;
    onDeltaSyncEnabledChange: (e: boolean) => void;
    protocol?: ProviderType;
    hints: TransferOptimizationHints | null;
    disabled: boolean;
    localPath?: string;
    onOpenMultiPath: () => void;
    onOpenTemplate: () => void;
    onOpenRollback: () => void;
    onClearHistory: () => void;
    // Canary mode
    canaryMode: boolean;
    onCanaryModeChange: (enabled: boolean) => void;
    canaryPercent: number;
    onCanaryPercentChange: (pct: number) => void;
    canarySelection: string;
    onCanarySelectionChange: (sel: string) => void;
}

const getDirectionDescription = (direction: SyncDirection, t: (key: string) => string): { Icon: typeof ArrowDownToLine; text: string } => {
    switch (direction) {
        case 'remote_to_local':
            return { Icon: ArrowDownToLine, text: t('syncPanel.descRemoteToLocal') };
        case 'local_to_remote':
            return { Icon: ArrowUpFromLine, text: t('syncPanel.descLocalToRemote') };
        case 'bidirectional':
            return { Icon: ArrowLeftRight, text: t('syncPanel.descBidirectional') };
    }
};

/** Section card wrapper — supports accordion (collapsible) mode */
const Section: React.FC<{
    icon: React.ReactNode;
    title: string;
    children: React.ReactNode;
    className?: string;
    collapsible?: boolean;
    open?: boolean;
    onToggle?: () => void;
}> = ({ icon, title, children, className = '', collapsible, open, onToggle }) => (
    <div className={`sync-adv-section ${collapsible && !open ? 'sync-adv-collapsed' : ''} ${className}`}>
        <div
            className={`sync-adv-section-header ${collapsible ? 'sync-adv-clickable' : ''}`}
            onClick={collapsible ? onToggle : undefined}
        >
            {icon}
            <span>{title}</span>
            {collapsible && (
                <ChevronDown size={14} className={`sync-adv-chevron ${open ? 'sync-adv-chevron-open' : ''}`} />
            )}
        </div>
        <div className={`sync-adv-section-body ${collapsible ? (open ? 'sync-adv-body-open' : 'sync-adv-body-closed') : ''}`}>
            {children}
        </div>
    </div>
);

export const SyncAdvancedConfig: React.FC<SyncAdvancedConfigProps> = React.memo(({
    options,
    onOptionsChange,
    verifyPolicy,
    onVerifyPolicyChange,
    retryPolicy,
    onRetryPolicyChange,
    downloadLimit,
    uploadLimit,
    onSpeedLimitChange,
    parallelStreams,
    onParallelStreamsChange,
    compressionMode,
    onCompressionModeChange,
    deltaSyncEnabled,
    onDeltaSyncEnabledChange,
    protocol,
    hints,
    disabled,
    localPath,
    onOpenMultiPath,
    onOpenTemplate,
    onOpenRollback,
    onClearHistory,
    canaryMode,
    onCanaryModeChange,
    canaryPercent,
    onCanaryPercentChange,
    canarySelection,
    onCanarySelectionChange,
}) => {
    const t = useTranslation();
    const isProvider = !!protocol && !isFtpProtocol(protocol);
    const isSftp = protocol === 'sftp';
    const dirDesc = getDirectionDescription(options.direction, t);
    const [openSection, setOpenSection] = useState<'direction' | 'compare' | 'transfer' | 'automation'>('direction');

    const handleDirectionChange = (direction: SyncDirection) => {
        onOptionsChange({ ...options, direction });
    };

    return (
        <div className="sync-advanced-config">

            {/* ── Section 1: Direction (accordion) ── */}
            <Section
                icon={<ArrowLeftRight size={14} className="text-blue-400" />}
                title={t('syncPanel.direction')}
                collapsible
                open={openSection === 'direction'}
                onToggle={() => setOpenSection('direction')}
            >
                <div className="direction-buttons">
                    <button
                        className={options.direction === 'remote_to_local' ? 'active' : ''}
                        onClick={() => handleDirectionChange('remote_to_local')}
                        disabled={disabled}
                    >
                        <ArrowDown size={14} className="inline mr-1" /> {t('syncPanel.dirRemoteToLocal')}
                    </button>
                    <button
                        className={options.direction === 'local_to_remote' ? 'active' : ''}
                        onClick={() => handleDirectionChange('local_to_remote')}
                        disabled={disabled}
                    >
                        <ArrowUp size={14} className="inline mr-1" /> {t('syncPanel.dirLocalToRemote')}
                    </button>
                    <button
                        className={options.direction === 'bidirectional' ? 'active' : ''}
                        onClick={() => handleDirectionChange('bidirectional')}
                        disabled={disabled}
                    >
                        <ArrowLeftRight size={14} className="inline mr-1" /> {t('syncPanel.dirBoth')}
                    </button>
                </div>

                <div className="sync-action-description mt-2.5">
                    <dirDesc.Icon size={16} className="inline mr-1.5 flex-shrink-0" />
                    {dirDesc.text}
                </div>
            </Section>

            {/* ── Section 2: Compare & Verify (accordion) ── */}
            <Section
                icon={<GitCompare size={14} className="text-cyan-400" />}
                title={t('syncPanel.compareOptions')}
                collapsible
                open={openSection === 'compare'}
                onToggle={() => setOpenSection('compare')}
            >
                <div className="sync-compare-options">
                    <label>
                        <input
                            type="checkbox"
                            checked={options.compare_timestamp}
                            onChange={e => onOptionsChange({ ...options, compare_timestamp: e.target.checked })}
                            disabled={disabled}
                        />
                        {t('syncPanel.timestamp')}
                    </label>
                    <label>
                        <input
                            type="checkbox"
                            checked={options.compare_size}
                            onChange={e => onOptionsChange({ ...options, compare_size: e.target.checked })}
                            disabled={disabled}
                        />
                        {t('syncPanel.size')}
                    </label>
                    <label>
                        <input
                            type="checkbox"
                            checked={options.compare_checksum}
                            onChange={e => onOptionsChange({ ...options, compare_checksum: e.target.checked })}
                            disabled={disabled}
                        />
                        {t('syncPanel.checksum')}
                    </label>
                </div>

                <div className="sync-adv-divider" />

                <div className="sync-compare-options">
                    <label className="flex items-center gap-1">
                        <ShieldCheck size={12} className="text-cyan-500" />
                        <select
                            className="sync-adv-select"
                            value={verifyPolicy}
                            onChange={e => onVerifyPolicyChange(e.target.value as VerifyPolicy)}
                            disabled={disabled}
                        >
                            <option value="none">{t('syncPanel.verifyNone')}</option>
                            <option value="size_only">{t('syncPanel.verifySize')}</option>
                            <option value="size_and_mtime">{t('syncPanel.verifySizeMtime')}</option>
                            <option value="full">{t('syncPanel.verifyFull')}</option>
                        </select>
                    </label>
                    <label className="flex items-center gap-1">
                        <RotateCcw size={12} className="text-amber-500" />
                        <select
                            className="sync-adv-select"
                            value={retryPolicy.max_retries}
                            onChange={e => onRetryPolicyChange({ ...retryPolicy, max_retries: Number(e.target.value) })}
                            disabled={disabled}
                        >
                            <option value="1">{t('syncPanel.retries', { count: '1' })}</option>
                            <option value="3">{t('syncPanel.retries', { count: '3' })}</option>
                            <option value="5">{t('syncPanel.retries', { count: '5' })}</option>
                            <option value="10">{t('syncPanel.retries', { count: '10' })}</option>
                        </select>
                    </label>
                </div>
            </Section>

            {/* ── Section 3: Transfer Control (accordion) ── */}
            <Section
                icon={<Settings2 size={14} className="text-purple-400" />}
                title={t('syncPanel.transferControl')}
                collapsible
                open={openSection === 'transfer'}
                onToggle={() => setOpenSection('transfer')}
            >
                {/* Bandwidth */}
                <div className="sync-adv-row">
                    <label className="flex items-center gap-1">
                        <Gauge size={12} className="text-purple-400" />
                        <span className="text-xs text-gray-400">{t('syncPanel.bandwidthDownload')}:</span>
                        <select
                            className="sync-adv-select"
                            value={downloadLimit}
                            onChange={e => onSpeedLimitChange(Number(e.target.value), uploadLimit)}
                            disabled={disabled}
                        >
                            {BANDWIDTH_OPTIONS.map(opt => (
                                <option key={opt.value} value={opt.value}>
                                    {opt.value === 0 ? t(opt.label) : opt.label}
                                </option>
                            ))}
                        </select>
                    </label>
                    <label className="flex items-center gap-1">
                        <Gauge size={12} className="text-purple-400" />
                        <span className="text-xs text-gray-400">{t('syncPanel.bandwidthUpload')}:</span>
                        <select
                            className="sync-adv-select"
                            value={uploadLimit}
                            onChange={e => onSpeedLimitChange(downloadLimit, Number(e.target.value))}
                            disabled={disabled}
                        >
                            {BANDWIDTH_OPTIONS.map(opt => (
                                <option key={opt.value} value={opt.value}>
                                    {opt.value === 0 ? t(opt.label) : opt.label}
                                </option>
                            ))}
                        </select>
                    </label>
                </div>

                <div className="sync-adv-divider" />

                {/* Parallel Streams */}
                <div className="sync-adv-row">
                    <label className="flex items-center gap-1">
                        <Zap size={12} className="text-yellow-400" />
                        <span className="text-xs text-gray-400">{t('syncPanel.parallelStreams')}:</span>
                        <select
                            className="sync-adv-select"
                            value={parallelStreams}
                            onChange={e => onParallelStreamsChange(Number(e.target.value))}
                            disabled={disabled}
                        >
                            <option value="1">{t('syncPanel.parallelSequential')}</option>
                            <option value="2">2 {t('syncPanel.parallelStreamLabel')}</option>
                            <option value="3">3 {t('syncPanel.parallelStreamLabel')}</option>
                            <option value="4">4 {t('syncPanel.parallelStreamLabel')}</option>
                            <option value="6">6 {t('syncPanel.parallelStreamLabel')}</option>
                            <option value="8">8 {t('syncPanel.parallelStreamLabel')}</option>
                        </select>
                    </label>
                    {parallelStreams > 1 && (
                        <span className="text-[10px] text-yellow-400/70 flex items-center gap-0.5">
                            <Zap size={10} /> {t('syncPanel.parallelTurboLabel')}
                        </span>
                    )}
                </div>

                {/* Compression & Delta Sync (SFTP only) */}
                {(isSftp || isProvider) && (
                    <>
                        <div className="sync-adv-divider" />
                        <div className="sync-adv-row">
                            {isSftp && (
                                <label className="flex items-center gap-1">
                                    <Shrink size={12} className="text-teal-400" />
                                    <span className="text-xs text-gray-400">{t('syncPanel.compression')}:</span>
                                    <select
                                        className="sync-adv-select"
                                        value={compressionMode}
                                        onChange={e => onCompressionModeChange(e.target.value as CompressionMode)}
                                        disabled={disabled}
                                    >
                                        <option value="off">{t('syncPanel.compressionOff')}</option>
                                        <option value="auto">{t('syncPanel.compressionAuto')}</option>
                                        <option value="on">{t('syncPanel.compressionOn')}</option>
                                    </select>
                                </label>
                            )}
                            {isSftp && (
                                <label className="flex items-center gap-1.5 text-xs">
                                    <input
                                        type="checkbox"
                                        checked={deltaSyncEnabled}
                                        onChange={e => onDeltaSyncEnabledChange(e.target.checked)}
                                        disabled={disabled}
                                    />
                                    <HardDrive size={12} className="text-indigo-400" />
                                    {t('syncPanel.deltaSync')}
                                </label>
                            )}
                        </div>
                    </>
                )}

                {/* Provider Optimization Badges */}
                <SyncOptimizationBadges hints={hints} compact />
            </Section>

            {/* ── Section 4: Tools ── */}
            <div className="sync-adv-tools">
                <button
                    className="sync-adv-tool-btn sync-adv-tool-blue"
                    onClick={onOpenMultiPath}
                    disabled={disabled}
                >
                    <FolderTree size={13} /> {t('syncPanel.multiPath')}
                </button>
                <button
                    className="sync-adv-tool-btn sync-adv-tool-purple"
                    onClick={onOpenTemplate}
                    disabled={disabled}
                >
                    <FileDown size={13} /> {t('syncPanel.templates')}
                </button>
                <button
                    className="sync-adv-tool-btn sync-adv-tool-amber"
                    onClick={onOpenRollback}
                    disabled={disabled}
                >
                    <Undo2 size={13} /> {t('syncPanel.rollback')}
                </button>
                <button
                    className="sync-adv-tool-btn sync-adv-tool-red ml-auto"
                    onClick={onClearHistory}
                    disabled={disabled}
                    title={t('syncPanel.clearHistory')}
                >
                    <Trash2 size={13} /> {t('syncPanel.clearHistory')}
                </button>
            </div>

            {/* ── Section 5: Automation (accordion) ── */}
            <Section
                icon={<Activity size={14} className="text-emerald-400" />}
                title={t('syncPanel.automation')}
                collapsible
                open={openSection === 'automation'}
                onToggle={() => setOpenSection('automation')}
            >
                <SyncScheduler disabled={disabled} />
                <WatcherStatus watchPath={localPath} />

                <div className="sync-adv-divider" />

                {/* Canary Mode */}
                <div className="space-y-2">
                    <label className="flex items-center gap-2 text-xs cursor-pointer">
                        <input
                            type="checkbox"
                            checked={canaryMode}
                            onChange={e => onCanaryModeChange(e.target.checked)}
                            disabled={disabled}
                        />
                        <FlaskConical size={12} className="text-amber-500" />
                        <span className="text-gray-700 dark:text-gray-300 font-medium">
                            {t('syncPanel.canaryMode') || 'Canary Mode'}
                        </span>
                    </label>

                    {canaryMode && (
                        <div className="pl-5 space-y-2">
                            {/* Percent slider */}
                            <div className="flex items-center gap-2">
                                <span className="text-[10px] text-gray-500 dark:text-gray-400 w-16">
                                    {t('syncPanel.canarySample') || 'Sample'}:
                                </span>
                                <input
                                    type="range"
                                    min={5}
                                    max={50}
                                    step={5}
                                    value={canaryPercent}
                                    onChange={e => onCanaryPercentChange(Number(e.target.value))}
                                    disabled={disabled}
                                    className="flex-1 h-1 accent-amber-500"
                                />
                                <span className="text-[10px] text-amber-400 font-mono w-8 text-right">
                                    {canaryPercent}%
                                </span>
                            </div>

                            {/* Selection strategy */}
                            <div className="flex items-center gap-2">
                                <span className="text-[10px] text-gray-500 dark:text-gray-400 w-16">
                                    {t('syncPanel.canaryStrategy') || 'Strategy'}:
                                </span>
                                <select
                                    className="sync-adv-select text-[10px] flex-1"
                                    value={canarySelection}
                                    onChange={e => onCanarySelectionChange(e.target.value)}
                                    disabled={disabled}
                                >
                                    <option value="random">{t('syncPanel.canaryRandom') || 'Random'}</option>
                                    <option value="newest">{t('syncPanel.canaryNewest') || 'Newest'}</option>
                                    <option value="largest">{t('syncPanel.canaryLargest') || 'Largest'}</option>
                                </select>
                            </div>

                            <p className="text-[10px] text-gray-400 dark:text-gray-500 leading-relaxed">
                                {t('syncPanel.canaryDesc') || 'Run a trial sync on a subset of files before committing to the full operation.'}
                            </p>
                        </div>
                    )}
                </div>
            </Section>
        </div>
    );
});

SyncAdvancedConfig.displayName = 'SyncAdvancedConfig';
