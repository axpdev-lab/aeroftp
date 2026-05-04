import { ChevronDown, ChevronUp } from 'lucide-react';
import { ServerProfile } from '../../types';
import { formatBytes } from '../../utils/formatters';
import { aggregateByDedupKey } from '../../utils/storageDedup';
import { useTranslation } from '../../i18n';

interface MyServersTableFooterProps {
    servers: ServerProfile[];
    breakdownAvailable?: boolean;
    breakdownOpen?: boolean;
    onToggleBreakdown?: () => void;
}

/**
 * Standalone footer bar — rendered outside the scrollable table container
 * (in MyServersPanel) so it stays pinned to the bottom of the card even when
 * the protocol-breakdown drawer is open above it. The HTML used to be a
 * `<tfoot>` inside the `<table>`, but `position: sticky` on `<tfoot>` does
 * not behave consistently across browsers when sibling content scrolls
 * around it — moving the bar outside the table fixes that and lets the
 * breakdown drawer expand over the scroll area without pushing the bar.
 */
export function MyServersTableFooter({
    servers,
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
    const interactiveClasses = interactive
        ? 'cursor-pointer hover:bg-gray-50 dark:hover:bg-gray-800/60'
        : '';

    const handleKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
        if (!interactive) return;
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onToggleBreakdown?.();
        }
    };

    return (
        <div
            className={`flex-none border-t border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-900 shadow-[0_-1px_3px_rgba(0,0,0,0.05)] transition-colors ${interactiveClasses}`}
            onClick={interactive ? onToggleBreakdown : undefined}
            onKeyDown={interactive ? handleKey : undefined}
            role={interactive ? 'button' : undefined}
            tabIndex={interactive ? 0 : undefined}
            aria-expanded={interactive ? breakdownOpen : undefined}
            aria-label={interactive ? t('introHub.breakdown.title') : undefined}
        >
            <div className="flex items-center gap-2 px-3 py-2 text-xs text-gray-500 dark:text-gray-400">
                <span title={t('introHub.table.footerDedupExplained')}>{summary}</span>
                {interactive && (
                    <span className="ml-auto inline-flex items-center gap-1 text-gray-400 dark:text-gray-500">
                        <span className="hidden sm:inline">{t('introHub.breakdown.title')}</span>
                        {breakdownOpen
                            ? <ChevronUp className="w-3.5 h-3.5" />
                            : <ChevronDown className="w-3.5 h-3.5" />}
                    </span>
                )}
            </div>
        </div>
    );
}
