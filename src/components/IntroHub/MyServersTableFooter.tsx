import { ChevronDown, ChevronUp } from 'lucide-react';
import { ServerProfile } from '../../types';
import { formatBytes } from '../../utils/formatters';
import { aggregateByDedupKey } from '../../utils/storageDedup';
import { useTranslation } from '../../i18n';

interface MyServersTableFooterProps {
    servers: ServerProfile[];
    colSpan: number;
    breakdownAvailable?: boolean;
    breakdownOpen?: boolean;
    onToggleBreakdown?: () => void;
}

export function MyServersTableFooter({
    servers,
    colSpan,
    breakdownAvailable = false,
    breakdownOpen = false,
    onToggleBreakdown,
}: MyServersTableFooterProps) {
    const t = useTranslation();
    const aggregate = aggregateByDedupKey(servers);
    const meanPct = aggregate.totalTotal > 0
        ? (aggregate.totalUsed / aggregate.totalTotal) * 100
        : null;
    const usageLabel = aggregate.dedupedQuotaCount > 0 && meanPct !== null
        ? `${formatBytes(aggregate.totalUsed)} / ${formatBytes(aggregate.totalTotal)} (${meanPct.toFixed(1)}%)`
        : null;
    const summary = usageLabel
        ? t('introHub.table.footerSummary', {
            count: servers.length,
            unique: aggregate.uniqueCount,
            usage: usageLabel,
        })
        : t('introHub.table.footerSummaryNoQuota', {
            count: servers.length,
            unique: aggregate.uniqueCount,
        });

    const interactive = breakdownAvailable && !!onToggleBreakdown;
    const cellInteractiveClasses = interactive
        ? 'cursor-pointer hover:bg-gray-50 dark:hover:bg-gray-800/60 transition-colors'
        : '';

    const handleKey = (e: React.KeyboardEvent<HTMLTableCellElement>) => {
        if (!interactive) return;
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onToggleBreakdown?.();
        }
    };

    return (
        <tfoot className="sticky bottom-0 z-10 bg-white dark:bg-gray-900 border-t border-gray-200 dark:border-gray-700 shadow-[0_-1px_3px_rgba(0,0,0,0.05)]">
            <tr>
                <td
                    colSpan={Math.max(1, colSpan)}
                    className={`px-3 py-2 text-xs text-gray-500 dark:text-gray-400 ${cellInteractiveClasses}`}
                    onClick={interactive ? onToggleBreakdown : undefined}
                    onKeyDown={interactive ? handleKey : undefined}
                    role={interactive ? 'button' : undefined}
                    tabIndex={interactive ? 0 : undefined}
                    aria-expanded={interactive ? breakdownOpen : undefined}
                    aria-label={interactive ? t('introHub.breakdown.title') : undefined}
                >
                    <span className="flex items-center gap-2">
                        <span title={t('introHub.table.footerDedupExplained')}>{summary}</span>
                        {interactive && (
                            <span className="ml-auto inline-flex items-center gap-1 text-gray-400 dark:text-gray-500">
                                <span className="hidden sm:inline">{t('introHub.breakdown.title')}</span>
                                {breakdownOpen
                                    ? <ChevronUp className="w-3.5 h-3.5" />
                                    : <ChevronDown className="w-3.5 h-3.5" />}
                            </span>
                        )}
                    </span>
                </td>
            </tr>
        </tfoot>
    );
}
