// SPDX-License-Identifier: GPL-3.0-or-later

import { useState } from 'react';
import { AlertTriangle, ChevronDown, Eye, EyeOff, Settings } from 'lucide-react';
import { useTranslation } from '../i18n';
import { TotpLivePreview } from './TotpLivePreview';

interface StoredTotpSecretDisclosureProps {
    value: string;
    onChange: (value: string) => void;
    accent?: 'emerald' | 'red';
}

export const StoredTotpSecretDisclosure: React.FC<StoredTotpSecretDisclosureProps> = ({
    value,
    onChange,
    accent = 'emerald',
}) => {
    const t = useTranslation();
    const [expanded, setExpanded] = useState(false);
    const [revealed, setRevealed] = useState(false);
    const focusClass = accent === 'red'
        ? 'focus:ring-red-500 focus:border-red-500'
        : 'focus:ring-emerald-500 focus:border-emerald-500';

    return (
        <div className="rounded-md border border-gray-200 dark:border-gray-700">
            <button
                type="button"
                onClick={() => setExpanded(!expanded)}
                className="flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-xs text-gray-600 transition-colors hover:bg-gray-50 dark:text-gray-300 dark:hover:bg-gray-700/40"
                aria-expanded={expanded}
            >
                <span className="flex items-center gap-2">
                    <Settings size={13} />
                    <span className="font-medium">{t('connection.storedTotpAdvanced')}</span>
                    {value && <span className="h-1.5 w-1.5 rounded-full bg-amber-500" />}
                </span>
                <ChevronDown size={14} className={`transition-transform duration-200 ${expanded ? 'rotate-180' : ''}`} />
            </button>
            {expanded && (
                <div className="animate-scale-in overflow-hidden border-t border-gray-200 dark:border-gray-700">
                    <div className="space-y-3 p-3">
                        <div className="flex gap-2 rounded-md border border-amber-200 bg-amber-50 p-2.5 text-xs leading-relaxed text-amber-900 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200">
                            <AlertTriangle size={14} className="mt-0.5 shrink-0" />
                            <span>{t('connection.storedTotpWarning')}</span>
                        </div>
                        <div>
                            <div className="mb-1.5 flex min-h-[1.5rem] items-center justify-between gap-2">
                                <label className="block text-sm font-medium">{t('connection.totpSecret')}</label>
                                <TotpLivePreview secret={value} t={t} inline />
                            </div>
                            <div className="relative">
                                <input
                                    type={revealed ? 'text' : 'password'}
                                    value={value}
                                    onChange={(event) => onChange(event.target.value)}
                                    className={`w-full rounded-lg border border-gray-300 bg-gray-50 px-4 py-2.5 pr-12 font-mono text-sm dark:border-gray-600 dark:bg-gray-700 ${focusClass}`}
                                    placeholder={t('connection.totpSecretPlaceholder')}
                                    autoComplete="off"
                                    spellCheck={false}
                                />
                                <button type="button" tabIndex={-1} onClick={() => setRevealed(!revealed)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                    {revealed ? <EyeOff size={18} /> : <Eye size={18} />}
                                </button>
                            </div>
                            <p className="mt-1.5 text-xs text-gray-500 dark:text-gray-400">{t('connection.totpSecretHelp')}</p>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
};

export default StoredTotpSecretDisclosure;
