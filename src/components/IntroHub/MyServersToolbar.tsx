import * as React from 'react';
import { LayoutGrid, List, Eye, EyeOff, Activity, ArrowRightLeft, Gauge, AtSign, Rows3, Rows2, HardDrive } from 'lucide-react';
import { ImportExportIcon } from '../icons/ImportExportIcon';
import { useTranslation } from '../../i18n';
import { MyServersViewMode } from '../../types/catalog';
import type { MyServersDensity } from '../../hooks/useMyServersDensity';

interface MyServersToolbarProps {
    viewMode: MyServersViewMode;
    onViewModeChange: (mode: MyServersViewMode) => void;
    credentialsMasked: boolean;
    onToggleMask: () => void;
    hideUsername: boolean;
    onToggleHideUsername: () => void;
    serverCount: number;
    filteredCount: number;
    onOpenExportImport?: () => void;
    onHealthCheck?: () => void;
    onSpeedTest?: () => void;
    /** Open Cross-Profile Transfer modal (always available: pre-selection optional). */
    onOpenCrossProfile?: () => void;
    /** Open Mount Manager modal. */
    onOpenMountManager?: () => void;
    /** 0/1/2: drives the 3 brightness states of the cross-profile button. */
    crossProfileSelectionCount?: number;
    /** Row density in list view ('compact' shrinks paddings + icon size). */
    listDensity?: MyServersDensity;
    /** Cycle the row density. Only rendered when in list view. */
    onToggleListDensity?: () => void;
}

export function MyServersToolbar({
    viewMode,
    onViewModeChange,
    credentialsMasked,
    onToggleMask,
    hideUsername,
    onToggleHideUsername,
    serverCount,
    filteredCount,
    onOpenExportImport,
    onHealthCheck,
    onSpeedTest,
    onOpenCrossProfile,
    onOpenMountManager,
    crossProfileSelectionCount = 0,
    listDensity = 'compact',
    onToggleListDensity,
}: MyServersToolbarProps) {
    const t = useTranslation();
    // Cross-Profile button visual states:
    //  - 0 selected: same indigo background as the other toolbar action buttons
    //                (Health Check, Speed Test) so it sits on the same baseline
    //  - 1 selected: brighter indigo accent: selection in progress
    //  - 2 selected: saturated + ring: strongest signal; the pair is ready to transfer
    const cpButtonClass = crossProfileSelectionCount >= 2
        ? 'bg-indigo-500 text-white ring-2 ring-indigo-400/60 shadow-md hover:bg-indigo-600'
        : crossProfileSelectionCount === 1
            ? 'bg-indigo-100 dark:bg-indigo-900/40 text-indigo-600 dark:text-indigo-300 hover:bg-indigo-200 dark:hover:bg-indigo-800/50'
            : 'bg-indigo-50 dark:bg-indigo-900/30 text-indigo-600 dark:text-indigo-400 hover:bg-indigo-100 dark:hover:bg-indigo-800/40';

    return (
        <div className="flex items-center gap-2 mb-3">
            {/* Filtered count label (search + filters now live in the sidebar). */}
            <span className="text-xs text-gray-400 dark:text-gray-500 tabular-nums">
                {filteredCount === serverCount ? serverCount : `${filteredCount} / ${serverCount}`}
            </span>

            <div className="flex-1" />

            {/* View mode toggle: show only the inactive view as a single
                "switch to" button. Two buttons (active highlighted + inactive)
                doubled the toolbar real-estate without adding signal: the user
                already knows which view they're in by looking at the cards. */}
            <div className="flex items-center border border-gray-200 dark:border-gray-600 rounded-lg overflow-hidden">
                {viewMode === 'grid' ? (
                    <button
                        onClick={() => onViewModeChange('list')}
                        className="p-2 transition-colors text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300"
                        title={t('introHub.viewList')}
                    >
                        <List size={15} />
                    </button>
                ) : (
                    <button
                        onClick={() => onViewModeChange('grid')}
                        className="p-2 transition-colors text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300"
                        title={t('introHub.viewGrid')}
                    >
                        <LayoutGrid size={15} />
                    </button>
                )}
                {viewMode === 'list' && onToggleListDensity && (
                    <button
                        onClick={onToggleListDensity}
                        className="p-2 transition-colors border-l border-gray-200 dark:border-gray-600 text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700"
                        title={listDensity === 'compact' ? t('introHub.densityComfortable') : t('introHub.densityCompact')}
                        aria-pressed={listDensity === 'compact'}
                    >
                        {listDensity === 'compact' ? <Rows3 size={15} /> : <Rows2 size={15} />}
                    </button>
                )}
            </div>

            {/* Mask toggle */}
            <button
                onClick={onToggleMask}
                className="p-2 rounded-lg text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
                title={credentialsMasked ? t('savedServers.showCredentials') : t('savedServers.hideCredentials')}
            >
                {credentialsMasked ? <EyeOff size={15} /> : <Eye size={15} />}
            </button>

            {/* Hide username toggle */}
            <button
                onClick={onToggleHideUsername}
                className={`p-2 rounded-lg transition-colors ${
                    hideUsername
                        ? 'text-gray-300 dark:text-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700'
                        : 'text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300'
                }`}
                title={hideUsername ? t('savedServers.showUsername') : t('savedServers.hideUsername')}
            >
                <AtSign size={15} />
            </button>

            {/* Cross-Profile Transfer - always visible, brightness scales with selection */}
            {onOpenCrossProfile && (
                <button
                    onClick={onOpenCrossProfile}
                    className={`relative p-2 rounded-lg transition-colors ${cpButtonClass}`}
                    title={
                        crossProfileSelectionCount === 0 ? t('transfer.crossProfile.title') :
                        crossProfileSelectionCount === 1 ? t('introHub.crossProfileOpenWithSource') :
                        t('introHub.crossProfileOpenWithPair')
                    }
                >
                    <ArrowRightLeft size={15} />
                    {crossProfileSelectionCount > 0 && (
                        <span className={`absolute -top-1 -right-1 min-w-[16px] h-4 px-1 rounded-full text-[9px] font-bold leading-4 text-center tabular-nums ${
                            crossProfileSelectionCount >= 2
                                ? 'bg-white text-indigo-600 ring-1 ring-indigo-300'
                                : 'bg-indigo-500 text-white'
                        }`}>
                            {crossProfileSelectionCount}
                        </span>
                    )}
                </button>
            )}

            {onOpenMountManager && (
                <button
                    onClick={onOpenMountManager}
                    className="p-2 rounded-lg bg-sky-50 dark:bg-sky-900/30 hover:bg-sky-100 dark:hover:bg-sky-800/40 text-sky-600 dark:text-sky-400 transition-colors"
                    title={t('mountManager.title')}
                >
                    <HardDrive size={15} />
                </button>
            )}

            {/* Health Check - emerald like original */}
            {onHealthCheck && serverCount > 0 && (
                <button
                    onClick={onHealthCheck}
                    className="p-2 rounded-lg bg-emerald-50 dark:bg-emerald-900/30 hover:bg-emerald-100 dark:hover:bg-emerald-800/40 text-emerald-600 dark:text-emerald-400 transition-colors"
                    title={t('healthCheck.title')}
                >
                    <Activity size={15} />
                </button>
            )}

            {/* Speed Test - indigo, paired with Health Check */}
            {onSpeedTest && serverCount > 0 && (
                <button
                    onClick={onSpeedTest}
                    className="p-2 rounded-lg bg-indigo-50 dark:bg-indigo-900/30 hover:bg-indigo-100 dark:hover:bg-indigo-800/40 text-indigo-600 dark:text-indigo-400 transition-colors"
                    title={t('speedTest.title')}
                >
                    <Gauge size={15} />
                </button>
            )}

            {/* Export/Import - amber like original */}
            {onOpenExportImport && (
                <button
                    onClick={onOpenExportImport}
                    className="p-2 rounded-lg bg-amber-50 dark:bg-amber-900/30 hover:bg-amber-100 dark:hover:bg-amber-800/40 text-amber-600 dark:text-amber-400 transition-colors"
                    title={t('settings.exportImport')}
                >
                    <ImportExportIcon size={15} />
                </button>
            )}

        </div>
    );
}
