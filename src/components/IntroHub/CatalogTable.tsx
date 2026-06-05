// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Company-centric Add Service list view (issue #224). Renders the
 * `PROVIDER_CATALOG` single source of truth as a sortable, searchable
 * table with hideable columns and a dynamic footer total. A row click
 * opens Quick Connect on the company's default connection method; a
 * badge click opens it pre-selected on that exact protocol.
 */

import * as React from 'react';
import { useMemo, useState, useRef, useCallback } from 'react';
import { Search, X, Columns3, ChevronUp, ChevronDown, ChevronsUpDown, Globe } from 'lucide-react';
import { ProviderType } from '../../types';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { CountryFlag } from '../CountryFlag';
import { useTranslation } from '../../i18n';
import { useTableColumns, type TableColumnDef } from '../../hooks/useTableColumns';
import { TableColumnsManager } from '../ui/TableColumnsManager';
import type { HealthStatus } from '../../hooks/useProviderHealth';
import {
    CatalogCompany,
    CatalogProtocolRef,
    totalFreeStorageGb,
    freeProtocols,
    paidProtocols,
    companyRegions,
} from '../providerCatalog';

type CatalogColId = 'company' | 'region' | 'freeGb' | 'free' | 'paid' | 'health';

const CATALOG_COLUMNS: TableColumnDef<CatalogColId>[] = [
    { id: 'company', labelKey: 'introHub.list.company', sortable: true, defaultVisible: true, defaultWidth: 200, minWidth: 140, pinnedStart: true, defaultAlign: 'left' },
    { id: 'region', labelKey: 'introHub.list.region', sortable: true, defaultVisible: true, defaultWidth: 80, minWidth: 56, defaultAlign: 'center' },
    { id: 'freeGb', labelKey: 'introHub.list.freeStorage', sortable: true, defaultVisible: true, defaultWidth: 110, minWidth: 80, defaultAlign: 'right' },
    { id: 'free', labelKey: 'introHub.list.freeProtocols', sortable: false, defaultVisible: true, defaultWidth: 200, minWidth: 120, defaultAlign: 'left' },
    { id: 'paid', labelKey: 'introHub.list.paidProtocols', sortable: false, defaultVisible: true, defaultWidth: 160, minWidth: 100, defaultAlign: 'left' },
    { id: 'health', labelKey: 'introHub.list.health', sortable: false, defaultVisible: true, defaultWidth: 70, minWidth: 48, pinnedEnd: true, defaultAlign: 'center' },
];

const SORTABLE: CatalogColId[] = ['company', 'region', 'freeGb'];

/** Tokens a company is searchable by (name, regions, note, protocol labels). */
function searchText(c: CatalogCompany): string {
    return [c.company, c.countryCode, c.freeNote, ...companyRegions(c), ...c.protocols.map(p => p.label)]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
}

function HealthDot({ status, enabled }: { status: HealthStatus; enabled: boolean }) {
    const t = useTranslation();
    // Feature off: show a dimmed grey dot rather than a stale green one.
    if (!enabled) {
        return <span className="inline-block w-2.5 h-2.5 rounded-full bg-gray-300 dark:bg-gray-600 opacity-40" />;
    }
    if (status === 'unknown') {
        return <span className="inline-block w-2.5 h-2.5 rounded-full bg-gray-300 dark:bg-gray-600" />;
    }
    const cls = status === 'up' ? 'bg-green-500'
        : status === 'slow' ? 'bg-amber-500'
        : status === 'down' ? 'bg-red-500'
        : 'bg-gray-400 animate-pulse';
    return (
        <span
            className={`inline-block w-2.5 h-2.5 rounded-full ${cls}`}
            title={t(`introHub.health.${status}`)}
            aria-label={t(`introHub.health.${status}`)}
        />
    );
}

function ProtocolBadge({ p, paid, onClick }: { p: CatalogProtocolRef; paid: boolean; onClick: () => void }) {
    return (
        <button
            type="button"
            onClick={(e) => { e.stopPropagation(); onClick(); }}
            title={p.note}
            className={`text-[10px] px-1.5 py-0.5 rounded font-medium transition-colors ${
                paid
                    ? 'bg-amber-100 text-amber-700 hover:bg-amber-200 dark:bg-amber-900/40 dark:text-amber-300 dark:hover:bg-amber-900/60'
                    : 'bg-blue-100 text-blue-700 hover:bg-blue-200 dark:bg-blue-900/40 dark:text-blue-300 dark:hover:bg-blue-900/60'
            }`}
        >
            {p.label}
        </button>
    );
}

interface CatalogTableProps {
    companies: CatalogCompany[];
    onSelectProvider: (protocol: ProviderType, providerId?: string) => void;
    getHealth: (logoId: string) => HealthStatus;
    /** When false the health feature is off: dots render dimmed grey. */
    healthEnabled: boolean;
}

export function CatalogTable({ companies, onSelectProvider, getHealth, healthEnabled }: CatalogTableProps) {
    const t = useTranslation();
    const [query, setQuery] = useState('');
    const [showColumns, setShowColumns] = useState(false);
    const columnsBtnRef = useRef<HTMLDivElement>(null);

    const cols = useTableColumns<CatalogColId>({
        columns: CATALOG_COLUMNS,
        storageKey: 'discover_catalog_table',
        sortableColIds: SORTABLE,
    });
    const { config, orderedVisibleColumns, orderedAllColumns, setSort, setVisible, setOrder, setAlign, resolveAlign, reset } = cols;
    // Default sort: most free storage first, until the user picks a column.
    const effectiveSort = config.sort ?? { colId: 'freeGb' as CatalogColId, dir: 'desc' as const };

    const filtered = useMemo(() => {
        const q = query.trim().toLowerCase();
        const rows = q ? companies.filter(c => searchText(c).includes(q)) : companies.slice();
        const dir = effectiveSort.dir === 'asc' ? 1 : -1;
        rows.sort((a, b) => {
            switch (effectiveSort.colId) {
                case 'company':
                    return dir * a.company.localeCompare(b.company);
                case 'region':
                    return dir * (a.countryCode || 'zz').localeCompare(b.countryCode || 'zz');
                case 'freeGb':
                default: {
                    const av = a.freeStorageGb ?? -1;
                    const bv = b.freeStorageGb ?? -1;
                    if (av !== bv) return dir * (av - bv);
                    return a.company.localeCompare(b.company);
                }
            }
        });
        return rows;
    }, [companies, query, effectiveSort.colId, effectiveSort.dir]);

    const totalGb = useMemo(() => totalFreeStorageGb(filtered), [filtered]);

    const toggleSort = useCallback((colId: CatalogColId) => {
        if (!SORTABLE.includes(colId)) return;
        if (effectiveSort.colId === colId) {
            setSort({ colId, dir: effectiveSort.dir === 'asc' ? 'desc' : 'asc' });
        } else {
            setSort({ colId, dir: colId === 'freeGb' ? 'desc' : 'asc' });
        }
    }, [effectiveSort, setSort]);

    // Click-outside for the columns popover.
    React.useEffect(() => {
        if (!showColumns) return;
        const onDown = (e: MouseEvent) => {
            if (columnsBtnRef.current && !columnsBtnRef.current.contains(e.target as Node)) {
                setShowColumns(false);
            }
        };
        document.addEventListener('mousedown', onDown);
        return () => document.removeEventListener('mousedown', onDown);
    }, [showColumns]);

    const alignClass = (id: CatalogColId): string => {
        const a = resolveAlign(id);
        return a === 'right' ? 'text-right' : a === 'center' ? 'text-center' : 'text-left';
    };

    const renderHeaderCell = (col: TableColumnDef<CatalogColId>) => {
        const sortable = SORTABLE.includes(col.id);
        const active = effectiveSort.colId === col.id;
        return (
            <th
                key={col.id}
                className={`py-2 px-3 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap ${alignClass(col.id)} ${sortable ? 'cursor-pointer select-none hover:text-gray-700 dark:hover:text-gray-200' : ''}`}
                onClick={sortable ? () => toggleSort(col.id) : undefined}
                style={{ width: col.defaultWidth }}
            >
                <span className="inline-flex items-center gap-1">
                    {t(col.labelKey)}
                    {sortable && (active
                        ? (effectiveSort.dir === 'asc' ? <ChevronUp size={12} /> : <ChevronDown size={12} />)
                        : <ChevronsUpDown size={12} className="opacity-40" />)}
                </span>
            </th>
        );
    };

    const renderBodyCell = (col: TableColumnDef<CatalogColId>, c: CatalogCompany) => {
        const Logo = PROVIDER_LOGOS[c.logoId];
        switch (col.id) {
            case 'company':
                return (
                    <td key={col.id} className={`py-1.5 px-3 ${alignClass(col.id)}`}>
                        <div className="flex items-center gap-2 min-w-0">
                            <div className="w-5 h-5 shrink-0 flex items-center justify-center">
                                {Logo ? <Logo size={16} /> : <div className="w-4 h-4 rounded bg-gray-400" />}
                            </div>
                            <span className="font-medium text-gray-900 dark:text-gray-100 truncate">{c.company}</span>
                        </div>
                    </td>
                );
            case 'region': {
                const regions = companyRegions(c);
                const MAX_FLAGS = 3;
                const shown = regions.slice(0, MAX_FLAGS);
                const extra = regions.length - shown.length;
                return (
                    <td key={col.id} className={`py-1.5 px-3 ${alignClass(col.id)}`}>
                        <div className="flex items-center justify-center gap-0.5" title={regions.join(', ')}>
                            {regions.length === 0 ? (
                                <span className="text-gray-300 dark:text-gray-600">-</span>
                            ) : (
                                <>
                                    {shown.map((code, i) => code === 'global'
                                        ? <Globe key={`${code}-${i}`} size={13} className="text-gray-400 dark:text-gray-500" />
                                        : <CountryFlag key={`${code}-${i}`} code={code} title={code} className="w-4 h-3 rounded-[1px] shadow-sm" />)}
                                    {extra > 0 && (
                                        <span className="text-[9px] text-gray-400 dark:text-gray-500 tabular-nums">+{extra}</span>
                                    )}
                                </>
                            )}
                        </div>
                    </td>
                );
            }
            case 'freeGb':
                return (
                    <td key={col.id} className={`py-1.5 px-3 tabular-nums ${alignClass(col.id)}`}>
                        {c.freeStorageGb != null
                            ? <span className="text-gray-900 dark:text-gray-100">{c.freeStorageGb} GB</span>
                            : <span className="text-[10px] text-gray-400 dark:text-gray-500" title={c.freeNote}>{c.freeNote || '-'}</span>}
                    </td>
                );
            case 'free': {
                const free = freeProtocols(c);
                return (
                    <td key={col.id} className={`py-1.5 px-3 ${alignClass(col.id)}`}>
                        <div className="flex flex-wrap gap-1">
                            {free.length === 0
                                ? <span className="text-gray-300 dark:text-gray-600">-</span>
                                : free.map((p, i) => (
                                    <ProtocolBadge key={`${p.label}-${i}`} p={p} paid={false}
                                        onClick={() => onSelectProvider(p.protocol, p.providerId)} />
                                ))}
                        </div>
                    </td>
                );
            }
            case 'paid': {
                const paid = paidProtocols(c);
                return (
                    <td key={col.id} className={`py-1.5 px-3 ${alignClass(col.id)}`}>
                        <div className="flex flex-wrap gap-1">
                            {paid.length === 0
                                ? <span className="text-gray-300 dark:text-gray-600">-</span>
                                : paid.map((p, i) => (
                                    <ProtocolBadge key={`${p.label}-${i}`} p={p} paid
                                        onClick={() => onSelectProvider(p.protocol, p.providerId)} />
                                ))}
                        </div>
                    </td>
                );
            }
            case 'health':
                return (
                    <td key={col.id} className={`py-1.5 px-3 ${alignClass(col.id)}`}>
                        <div className="flex items-center justify-center">
                            <HealthDot status={c.healthCheckUrl ? getHealth(c.logoId) : 'unknown'} enabled={healthEnabled} />
                        </div>
                    </td>
                );
            default:
                return <td key={col.id} />;
        }
    };

    return (
        <div className="flex flex-col h-full min-h-0">
            {/* Toolbar: search + column manager */}
            <div className="flex items-center gap-2 mb-3">
                <div className="relative flex-1 max-w-md">
                    <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
                    <input
                        type="text"
                        value={query}
                        onChange={(e) => setQuery(e.target.value)}
                        placeholder={t('introHub.list.searchPlaceholder')}
                        className="w-full pl-9 pr-8 py-1.5 bg-gray-50 dark:bg-gray-700/60 border border-gray-200 dark:border-gray-600 rounded-lg text-xs focus:outline-none focus:ring-2 focus:ring-blue-500/40"
                    />
                    {query && (
                        <button
                            onClick={() => setQuery('')}
                            className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                            aria-label={t('common.close')}
                        >
                            <X size={13} />
                        </button>
                    )}
                </div>
                <div className="relative" ref={columnsBtnRef}>
                    <button
                        onClick={() => setShowColumns(v => !v)}
                        className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg text-xs text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700/50 border border-gray-200 dark:border-gray-600"
                        title={t('table.manageColumns')}
                    >
                        <Columns3 size={14} />
                        <span className="hidden sm:inline">{t('introHub.list.columns')}</span>
                    </button>
                    {showColumns && (
                        <TableColumnsManager<CatalogColId>
                            columns={orderedAllColumns}
                            visibility={config.visibility}
                            orderedAllColumns={orderedAllColumns}
                            onSetVisible={setVisible}
                            onSetOrder={setOrder}
                            onReset={reset}
                            onClose={() => setShowColumns(false)}
                            resolveAlign={resolveAlign}
                            onSetAlign={setAlign}
                        />
                    )}
                </div>
            </div>

            {/* Table */}
            <div className="flex-1 overflow-auto border border-gray-200 dark:border-gray-700/50 rounded-lg">
                <table className="w-full text-xs">
                    <thead className="sticky top-0 bg-gray-50 dark:bg-gray-800 z-10">
                        <tr className="border-b border-gray-200 dark:border-gray-700">
                            {orderedVisibleColumns.map(renderHeaderCell)}
                        </tr>
                    </thead>
                    <tbody>
                        {filtered.length === 0 ? (
                            <tr>
                                <td colSpan={orderedVisibleColumns.length} className="text-center py-12 text-gray-400 dark:text-gray-500">
                                    <Search size={28} className="mx-auto mb-2 opacity-50" />
                                    <p className="text-sm">{t('introHub.noResults')}</p>
                                </td>
                            </tr>
                        ) : (
                            filtered.map((c) => {
                                const primary = c.protocols[0];
                                return (
                                    <tr
                                        key={c.company}
                                        onClick={primary ? () => onSelectProvider(primary.protocol, primary.providerId) : undefined}
                                        className="border-b border-gray-100 dark:border-gray-700/30 hover:bg-blue-50/40 dark:hover:bg-blue-900/10 transition-colors cursor-pointer"
                                        title={t('introHub.list.connectHint', { company: c.company })}
                                    >
                                        {orderedVisibleColumns.map((col) => renderBodyCell(col, c))}
                                    </tr>
                                );
                            })
                        )}
                    </tbody>
                </table>
            </div>

            {/* Footer: dynamic, labelled approximate */}
            <div className="mt-3 pt-2 flex items-center justify-between text-[11px] text-gray-400 dark:text-gray-500">
                <span>{t('introHub.list.footerSummary', { count: filtered.length, gb: totalGb })}</span>
                <span className="italic">{t('introHub.list.approximate')}</span>
            </div>
        </div>
    );
}
