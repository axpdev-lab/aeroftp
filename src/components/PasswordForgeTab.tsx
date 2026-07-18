// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertTriangle, Check, Copy, Eye, EyeOff, Loader2, Shuffle } from 'lucide-react';
import { useTranslation } from '../i18n';
import { Checkbox } from './ui/Checkbox';
import {
    PASSWORD_PRESETS,
    entropyCommandArgs,
    passwordCommandArgs,
    type PasswordForgeSettings,
    type PasswordPresetId,
    type SymbolGroupId,
} from '../utils/passwordForge';

const SYMBOL_GROUPS: Array<{ id: SymbolGroupId; sample: string }> = [
    { id: 'punctuation', sample: '!@#$%^&*' },
    { id: 'brackets', sample: '()[]{}<>' },
    { id: 'separators', sample: '-_=+' },
    { id: 'special', sample: '|;:,.?/~' },
];

export const PasswordForgeTab: React.FC = () => {
    const t = useTranslation();
    const [mode, setMode] = useState<'random' | 'passphrase'>('random');
    const [settings, setSettings] = useState<PasswordForgeSettings>({ ...PASSWORD_PRESETS.balanced });
    const [activePreset, setActivePreset] = useState<PasswordPresetId | null>('balanced');
    const [wordCount, setWordCount] = useState(5);
    const [separator, setSeparator] = useState('-');
    const [capitalize, setCapitalize] = useState(true);
    const [batchCount, setBatchCount] = useState(1);
    const [passwords, setPasswords] = useState<string[]>([]);
    const [entropy, setEntropy] = useState(0);
    const [copiedIdx, setCopiedIdx] = useState<number | null>(null);
    /** Per-row reveal state; passwords stay masked by default (KeePass parity). */
    const [revealed, setRevealed] = useState<Record<number, boolean>>({});
    const [generating, setGenerating] = useState(false);
    const [error, setError] = useState('');
    const clearTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    const updateSettings = useCallback((patch: Partial<PasswordForgeSettings>) => {
        setSettings(current => ({ ...current, ...patch }));
        setActivePreset(null);
        setPasswords([]);
        setError('');
    }, []);

    const applyPreset = useCallback((preset: PasswordPresetId) => {
        setMode('random');
        setSettings({ ...PASSWORD_PRESETS[preset], symbolGroups: [...PASSWORD_PRESETS[preset].symbolGroups] });
        setActivePreset(preset);
        setPasswords([]);
        setError('');
    }, []);

    useEffect(() => {
        if (mode === 'random') {
            invoke<number>('calculate_entropy', entropyCommandArgs(settings)).then(setEntropy).catch(() => setEntropy(0));
        } else {
            setEntropy(wordCount * Math.log2(1133));
        }
    }, [mode, settings, wordCount]);

    const generate = useCallback(async () => {
        setGenerating(true);
        setError('');
        try {
            const result = mode === 'random'
                ? await invoke<string[]>('generate_password', passwordCommandArgs(settings, batchCount))
                : await invoke<string[]>('generate_passphrase', { wordCount, separator, capitalize, count: batchCount });
            setPasswords(result);
            setRevealed({});
        } catch (reason) {
            setPasswords([]);
            setRevealed({});
            setError(String(reason));
        } finally {
            setGenerating(false);
        }
    }, [batchCount, capitalize, mode, separator, settings, wordCount]);

    const copyPassword = useCallback(async (password: string, index: number) => {
        try {
            await invoke('copy_to_clipboard', { text: password });
            setCopiedIdx(index);
            setTimeout(() => setCopiedIdx(null), 2000);
            if (clearTimerRef.current) clearTimeout(clearTimerRef.current);
            clearTimerRef.current = setTimeout(() => {
                invoke('copy_to_clipboard', { text: '' }).catch(() => undefined);
            }, 30000);
        } catch { /* clipboard access is best-effort */ }
    }, []);

    useEffect(() => () => {
        if (clearTimerRef.current) clearTimeout(clearTimerRef.current);
    }, []);

    const entropyColor = entropy < 40 ? 'bg-red-500' : entropy < 60 ? 'bg-orange-500' : entropy < 80 ? 'bg-yellow-500' : entropy < 100 ? 'bg-green-500' : 'bg-cyan-500';
    const entropyLabel = entropy < 40 ? t('cyberTools.pwdWeak') : entropy < 60 ? t('cyberTools.pwdFair') : entropy < 80 ? t('cyberTools.pwdGood') : entropy < 100 ? t('cyberTools.pwdStrong') : t('cyberTools.pwdExcellent');
    const entropyPct = Math.min(100, (entropy / 128) * 100);

    return (
        <div className="space-y-4">
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.pwdDescription')}</p>

            <div className="grid grid-cols-2 rounded-md bg-gray-100 p-1 dark:bg-gray-900/60">
                {(['random', 'passphrase'] as const).map(value => (
                    <button key={value} type="button" onClick={() => setMode(value)} className={`rounded px-3 py-1.5 text-xs font-medium transition-all ${mode === value ? 'bg-white text-cyan-700 shadow-sm dark:bg-gray-700 dark:text-cyan-300' : 'text-gray-500 hover:text-gray-800 dark:hover:text-gray-200'}`}>
                        {t(value === 'random' ? 'cyberTools.pwdModeRandom' : 'cyberTools.pwdModePassphrase')}
                    </button>
                ))}
            </div>

            {mode === 'random' ? (
                <div className="space-y-4">
                    <div>
                        <label className="mb-1.5 block text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.pwdPresets')}</label>
                        <div className="grid grid-cols-3 gap-1.5">
                            {(['balanced', 'maximum', 'compatible'] as const).map(preset => (
                                <button key={preset} type="button" onClick={() => applyPreset(preset)} className={`rounded-md border px-2 py-2 text-xs font-medium transition-all ${activePreset === preset ? 'border-cyan-500 bg-cyan-500/10 text-cyan-700 dark:text-cyan-300' : 'border-gray-200 text-gray-600 hover:border-cyan-300 dark:border-gray-700 dark:text-gray-300'}`}>
                                    {t(`cyberTools.pwdPreset${preset[0].toUpperCase()}${preset.slice(1)}`)}
                                </button>
                            ))}
                        </div>
                    </div>

                    <div>
                        <div className="mb-1 flex justify-between text-xs text-gray-500 dark:text-gray-400">
                            <span>{t('cyberTools.pwdLength')}</span>
                            <span className="font-mono">{settings.length}</span>
                        </div>
                        <input type="range" min={8} max={128} value={settings.length} onChange={event => updateSettings({ length: Number(event.target.value) })} className="w-full accent-cyan-500" />
                    </div>

                    <div className="grid grid-cols-2 gap-2">
                        {([
                            ['uppercase', 'pwdUppercase'],
                            ['lowercase', 'pwdLowercase'],
                            ['digits', 'pwdDigits'],
                            ['symbols', 'pwdSymbols'],
                        ] as const).map(([key, label]) => (
                            <Checkbox key={key} checked={settings[key]} onChange={checked => updateSettings({ [key]: checked })} label={<span className="text-xs text-gray-700 dark:text-gray-300">{t(`cyberTools.${label}`)}</span>} />
                        ))}
                    </div>

                    {settings.symbols && (
                        <div>
                            <label className="mb-1.5 block text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.pwdSymbolGroups')}</label>
                            <div className="grid grid-cols-2 gap-1.5">
                                {SYMBOL_GROUPS.map(group => {
                                    const checked = settings.symbolGroups.includes(group.id);
                                    return (
                                        <button key={group.id} type="button" onClick={() => updateSettings({ symbolGroups: checked ? settings.symbolGroups.filter(id => id !== group.id) : [...settings.symbolGroups, group.id] })} className={`rounded-md border px-2 py-1.5 text-left font-mono text-xs transition-colors ${checked ? 'border-cyan-500/60 bg-cyan-500/10 text-cyan-700 dark:text-cyan-300' : 'border-gray-200 text-gray-500 dark:border-gray-700'}`}>
                                            {group.sample}
                                        </button>
                                    );
                                })}
                            </div>
                        </div>
                    )}

                    <div className="grid grid-cols-2 gap-2">
                        <div>
                            <label className="mb-1 block text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.pwdCustomCharacters')}</label>
                            <input value={settings.customCharacters} onChange={event => updateSettings({ customCharacters: event.target.value })} className="w-full rounded-md border border-gray-300 bg-white px-2.5 py-2 font-mono text-xs dark:border-gray-600 dark:bg-gray-900" spellCheck={false} />
                        </div>
                        <div>
                            <label className="mb-1 block text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.pwdExcludedCharacters')}</label>
                            <input value={settings.excludedCharacters} onChange={event => updateSettings({ excludedCharacters: event.target.value })} className="w-full rounded-md border border-gray-300 bg-white px-2.5 py-2 font-mono text-xs dark:border-gray-600 dark:bg-gray-900" spellCheck={false} />
                        </div>
                    </div>

                    <div className="space-y-2">
                        <Checkbox checked={settings.requireEachGroup} onChange={checked => updateSettings({ requireEachGroup: checked })} label={<span className="text-xs text-gray-600 dark:text-gray-300">{t('cyberTools.pwdRequireEachGroup')}</span>} />
                        <Checkbox checked={settings.excludeAmbiguous} onChange={checked => updateSettings({ excludeAmbiguous: checked })} label={<span className="text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.pwdExcludeAmbiguous')}</span>} />
                    </div>
                </div>
            ) : (
                <div className="space-y-3">
                    <div>
                        <div className="mb-1 flex justify-between text-xs text-gray-500 dark:text-gray-400"><span>{t('cyberTools.pwdWordCount')}</span><span className="font-mono">{wordCount}</span></div>
                        <input type="range" min={3} max={24} value={wordCount} onChange={event => setWordCount(Number(event.target.value))} className="w-full accent-cyan-500" />
                    </div>
                    <div className="flex gap-3">
                        <div className="flex-1"><label className="mb-1 block text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.pwdSeparator')}</label><input value={separator} onChange={event => setSeparator(event.target.value)} maxLength={3} className="w-full rounded-md border border-gray-300 bg-white px-3 py-1.5 text-center font-mono text-sm dark:border-gray-600 dark:bg-gray-900" /></div>
                        <div className="flex items-center pt-5"><Checkbox checked={capitalize} onChange={setCapitalize} label={<span className="text-xs text-gray-700 dark:text-gray-300">{t('cyberTools.pwdCapitalize')}</span>} /></div>
                    </div>
                    {wordCount >= 12 && <p className="text-[10px] text-amber-500/80">{t('cyberTools.pwdNotBip39')}</p>}
                </div>
            )}

            <div>
                <div className="mb-1 flex justify-between text-xs text-gray-500 dark:text-gray-400"><span>{t('cyberTools.pwdBatchCount')}</span><span className="font-mono">{batchCount}</span></div>
                <input type="range" min={1} max={5} value={batchCount} onChange={event => setBatchCount(Number(event.target.value))} className="w-full accent-cyan-500" />
            </div>

            <div>
                <div className="mb-1 flex justify-between gap-3 text-xs text-gray-500 dark:text-gray-400"><span>{t('cyberTools.pwdEntropy')}</span><span className="font-mono">{Math.round(entropy)} {t('cyberTools.pwdBits')}: {entropyLabel}</span></div>
                <div className="h-2 overflow-hidden rounded-full bg-gray-200 dark:bg-gray-700"><div className={`h-full rounded-full transition-all duration-500 ${entropyColor}`} style={{ width: `${entropyPct}%` }} /></div>
            </div>

            {error && <div className="flex items-start gap-1.5 rounded-md bg-red-500/10 p-2 text-xs text-red-600 dark:text-red-300"><AlertTriangle size={13} className="mt-0.5 shrink-0" />{error}</div>}

            <button onClick={generate} disabled={generating} className="flex w-full cursor-pointer items-center justify-center gap-2 rounded-md bg-cyan-500 py-2 text-sm font-medium text-white transition-all hover:bg-cyan-600 active:scale-[0.99] disabled:cursor-wait disabled:opacity-70">
                {generating ? <Loader2 size={14} className="animate-spin" /> : <Shuffle size={14} />}
                {generating ? t('cyberTools.pwdGenerating') : t('cyberTools.pwdGenerate')}
            </button>

            {passwords.length > 0 && (
                <div className="space-y-2">
                    {passwords.map((password, index) => {
                        const isRevealed = !!revealed[index];
                        return (
                        <div key={`${index}-${password}`} className="flex items-center gap-2 animate-scale-in">
                            <div className="relative min-w-0 flex-1">
                                <input
                                    type={isRevealed ? 'text' : 'password'}
                                    readOnly
                                    value={password}
                                    className="w-full select-all rounded-md border border-gray-200 bg-gray-50 px-3 py-2 font-mono text-xs text-gray-800 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-200"
                                    spellCheck={false}
                                    autoComplete="off"
                                />
                            </div>
                            <button
                                type="button"
                                onClick={() => setRevealed(prev => ({ ...prev, [index]: !prev[index] }))}
                                className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-gray-100 text-gray-500 transition-colors hover:bg-gray-200 hover:text-gray-700 dark:bg-gray-700 dark:text-gray-300 dark:hover:bg-gray-600 dark:hover:text-gray-100"
                                title={isRevealed ? t('cyberTools.pwdHide') : t('cyberTools.pwdShow')}
                                aria-label={isRevealed ? t('cyberTools.pwdHide') : t('cyberTools.pwdShow')}
                            >
                                {isRevealed ? <EyeOff size={13} /> : <Eye size={13} />}
                            </button>
                            <button
                                type="button"
                                onClick={() => copyPassword(password, index)}
                                className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-gray-100 transition-colors hover:bg-gray-200 dark:bg-gray-700 dark:hover:bg-gray-600"
                                title={copiedIdx === index ? t('cyberTools.pwdAutoClear') : t('cyberTools.pwdCopy')}
                            >
                                {copiedIdx === index ? <Check size={13} className="text-green-500" /> : <Copy size={13} />}
                            </button>
                        </div>
                        );
                    })}
                </div>
            )}
        </div>
    );
};

export default PasswordForgeTab;
