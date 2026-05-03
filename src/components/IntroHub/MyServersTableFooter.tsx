import { ServerProfile } from '../../types';
import { formatBytes } from '../../utils/formatters';
import { useTranslation } from '../../i18n';

interface MyServersTableFooterProps {
    servers: ServerProfile[];
    colSpan: number;
}

export function MyServersTableFooter({ servers, colSpan }: MyServersTableFooterProps) {
    const t = useTranslation();
    const totals = servers.reduce((acc, server) => {
        const quota = server.lastQuota;
        if (!quota || !quota.total || quota.total <= 0) return acc;
        return {
            used: acc.used + quota.used,
            total: acc.total + quota.total,
            quotaCount: acc.quotaCount + 1,
        };
    }, { used: 0, total: 0, quotaCount: 0 });
    const meanPct = totals.total > 0 ? (totals.used / totals.total) * 100 : null;
    const usageLabel = totals.quotaCount > 0
        ? `${formatBytes(totals.used)} / ${formatBytes(totals.total)} (${meanPct!.toFixed(1)}%)`
        : '—';

    return (
        <tfoot className="sticky bottom-0 z-10 bg-white dark:bg-gray-900 border-t border-gray-200 dark:border-gray-700 shadow-[0_-1px_3px_rgba(0,0,0,0.05)]">
            <tr>
                <td colSpan={Math.max(1, colSpan)} className="px-3 py-2 text-xs text-gray-500 dark:text-gray-400">
                    <span title={t('introHub.table.footerDedupNote')}>
                        {t('introHub.table.footerSummary', {
                            count: servers.length,
                            usage: usageLabel,
                            quotaCount: totals.quotaCount,
                        })}
                    </span>
                </td>
            </tr>
        </tfoot>
    );
}
