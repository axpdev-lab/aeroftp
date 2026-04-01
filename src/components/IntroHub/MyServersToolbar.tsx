import * as React from 'react';
import { Search, X, LayoutGrid, List, Eye, EyeOff, Activity, Star } from 'lucide-react';
import { ImportExportIcon } from '../icons/ImportExportIcon';
import { useTranslation } from '../../i18n';
import { MyServersViewMode, MyServersFilterBy, FILTER_CHIPS } from '../../types/catalog';

interface MyServersToolbarProps {
    searchQuery: string;
    onSearchChange: (query: string) => void;
    activeFilter: MyServersFilterBy;
    onFilterChange: (filter: MyServersFilterBy) => void;
    viewMode: MyServersViewMode;
    onViewModeChange: (mode: MyServersViewMode) => void;
    credentialsMasked: boolean;
    onToggleMask: () => void;
    serverCount: number;
    filteredCount: number;
    chipCounts: Record<MyServersFilterBy, number>;
    onOpenExportImport?: () => void;
    onHealthCheck?: () => void;
}

export function MyServersToolbar({
    searchQuery,
    onSearchChange,
    activeFilter,
    onFilterChange,
    viewMode,
    onViewModeChange,
    credentialsMasked,
    onToggleMask,
    serverCount,
    filteredCount,
    chipCounts,
    onOpenExportImport,
    onHealthCheck,
}: MyServersToolbarProps) {
    const t = useTranslation();

    return (
        <div className="flex items-center gap-2 mb-4 flex-wrap">
            {/* Search bar */}
            <div className="relative flex-1 min-w-[200px]">
                <Search size={15} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
                <input
                    type="text"
                    value={searchQuery}
                    onChange={(e) => onSearchChange(e.target.value)}
                    placeholder={t('introHub.searchServers')}
                    className="w-full h-9 pl-9 pr-8 text-sm rounded-lg bg-gray-100 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:border-blue-400 dark:focus:border-blue-500 focus:ring-1 focus:ring-blue-400/25 transition-colors"
                />
                {searchQuery && (
                    <button
                        onClick={() => onSearchChange('')}
                        className="absolute right-2.5 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                    >
                        <X size={14} />
                    </button>
                )}
            </div>

            {/* Filter chips with counts */}
            {FILTER_CHIPS.map((chip) => {
                const count = chipCounts[chip.id] ?? 0;
                return (
                    <button
                        key={chip.id}
                        onClick={() => onFilterChange(chip.id)}
                        className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs font-medium whitespace-nowrap transition-colors ${
                            activeFilter === chip.id
                                ? 'bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300 border border-blue-200 dark:border-blue-700'
                                : 'bg-gray-100 dark:bg-gray-700/50 text-gray-500 dark:text-gray-400 border border-transparent hover:bg-gray-200 dark:hover:bg-gray-700'
                        }`}
                    >
                        {chip.id === 'favorites' && <Star size={10} />}
                        <span>{t(chip.labelKey)}</span>
                        <span className={`text-[10px] tabular-nums ${
                            activeFilter === chip.id ? 'text-blue-500 dark:text-blue-300' : 'text-gray-400 dark:text-gray-500'
                        }`}>{count}</span>
                    </button>
                );
            })}

            {/* View mode toggle */}
            <div className="flex items-center border border-gray-200 dark:border-gray-600 rounded-lg overflow-hidden">
                <button
                    onClick={() => onViewModeChange('grid')}
                    className={`p-2 transition-colors ${
                        viewMode === 'grid'
                            ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400'
                            : 'text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700'
                    }`}
                    title={t('introHub.viewGrid')}
                >
                    <LayoutGrid size={15} />
                </button>
                <button
                    onClick={() => onViewModeChange('list')}
                    className={`p-2 transition-colors ${
                        viewMode === 'list'
                            ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400'
                            : 'text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700'
                    }`}
                    title={t('introHub.viewList')}
                >
                    <List size={15} />
                </button>
            </div>

            {/* Mask toggle */}
            <button
                onClick={onToggleMask}
                className="p-2 rounded-lg text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
                title={credentialsMasked ? t('savedServers.showCredentials') : t('savedServers.hideCredentials')}
            >
                {credentialsMasked ? <EyeOff size={15} /> : <Eye size={15} />}
            </button>

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
