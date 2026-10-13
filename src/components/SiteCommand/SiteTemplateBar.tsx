// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../../i18n';
import { loadSavedServerProfiles, mergeSavedServerProfile } from '../../utils/serverProfileStore';

interface SiteTemplate {
    id: string;
    name: string;
    line: string;
}

interface SiteSlot {
    name: string;
    kind: 'text' | 'number' | 'secret';
}

interface SiteRender {
    line: string;
    echo: string;
    carries_secret: boolean;
}

interface SiteImport {
    templates: SiteTemplate[];
    added: string[];
    skipped: string[];
}

interface SiteTemplateBarProps {
    /** Saved profile that owns the templates. Absent on Quick Connect. */
    profileId: string | null;
    /** The free-form line, offered as a template when the user saves it. */
    draft: string;
    disabled: boolean;
    onRun: (line: string, echo: string, remember: boolean) => void;
}

function errorText(error: unknown): string {
    if (typeof error === 'string') return error;
    if (error instanceof Error) return error.message;
    return String(error);
}

/**
 * Templates saved on the open profile. Filling and checking happen in the
 * backend; this bar only stores the line with its holes, then asks the
 * dialog to send the filled command once.
 */
export const SiteTemplateBar: React.FC<SiteTemplateBarProps> = ({ profileId, draft, disabled, onRun }) => {
    const t = useTranslation();
    const [templates, setTemplates] = useState<SiteTemplate[]>([]);
    const [selectedId, setSelectedId] = useState('');
    const [slots, setSlots] = useState<SiteSlot[]>([]);
    const [values, setValues] = useState<Record<string, string>>({});
    const [saving, setSaving] = useState(false);
    const [saveName, setSaveName] = useState('');
    const [notice, setNotice] = useState('');
    const [error, setError] = useState('');
    const [revision, setRevision] = useState(0);

    useEffect(() => {
        if (!profileId) {
            setTemplates([]);
            setSelectedId('');
            return;
        }
        let current = true;
        loadSavedServerProfiles()
            .then(profiles => {
                if (!current) return;
                setTemplates(profiles.find(profile => profile.id === profileId)?.siteTemplates ?? []);
            })
            .catch(err => { if (current) setError(errorText(err)); });
        return () => { current = false; };
    }, [profileId, revision]);

    const selected = templates.find(template => template.id === selectedId) ?? null;

    useEffect(() => {
        setValues({});
        setSlots([]);
        if (!selected) return;
        let current = true;
        invoke<SiteSlot[]>('site_template_slots', { line: selected.line })
            .then(next => { if (current) setSlots(next); })
            .catch(err => { if (current) setError(errorText(err)); });
        return () => { current = false; };
    }, [selected]);

    if (!profileId) {
        return <p className="px-5 pt-2 text-[11px] text-gray-500 dark:text-gray-400">{t('siteCommand.templateQuickConnect')}</p>;
    }

    const persist = async (next: SiteTemplate[]) => {
        await mergeSavedServerProfile(profileId, profile => ({ ...profile, siteTemplates: next }));
        setRevision(revision + 1);
    };

    const run = async () => {
        if (!selected || disabled) return;
        setError('');
        try {
            const rendered = await invoke<SiteRender>('site_template_render', {
                line: selected.line,
                values: slots.map(slot => ({ name: slot.name, value: values[slot.name] ?? '' })),
            });
            setValues(current => {
                const cleared = { ...current };
                for (const slot of slots) if (slot.kind === 'secret') cleared[slot.name] = '';
                return cleared;
            });
            onRun(rendered.line, rendered.echo, !rendered.carries_secret);
        } catch (err) {
            setError(errorText(err));
        }
    };

    const importGlftpd = async () => {
        setError('');
        try {
            // The built-in set comes from its own command, then the import merges
            // that payload. Same names stay as the profile saved them.
            const catalog = await invoke<SiteTemplate[]>('site_template_catalog');
            const source = JSON.stringify({
                catalog: 'glftpd',
                version: 1,
                templates: catalog.map(template => ({ name: template.name, line: template.line })),
            });
            const imported = await invoke<SiteImport>('site_template_import', { existing: templates, source });
            await persist(imported.templates);
            setNotice(t('siteCommand.importDone', { added: imported.added.length, skipped: imported.skipped.length }));
        } catch (err) {
            setError(errorText(err));
        }
    };

    const save = async () => {
        const name = saveName.trim();
        if (!name || !draft.trim()) return;
        if (templates.some(template => template.name.toLowerCase() === name.toLowerCase())) {
            setError(t('siteCommand.duplicateName', { name }));
            return;
        }
        setError('');
        try {
            const prepared = await invoke<SiteTemplate>('site_template_prepare', { name, line: draft });
            await persist([...templates, prepared]);
            setSaveName('');
            setSaving(false);
            setSelectedId(prepared.id);
        } catch (err) {
            setError(errorText(err));
        }
    };

    const remove = async () => {
        if (!selected) return;
        setError('');
        try {
            await persist(templates.filter(template => template.id !== selected.id));
            setSelectedId('');
        } catch (err) {
            setError(errorText(err));
        }
    };

    return (
        <div className="px-5 pt-2 space-y-2">
            <div className="flex items-center gap-2">
                <label className="text-xs text-gray-500 dark:text-gray-400 shrink-0" htmlFor="site-template">{t('siteCommand.templatesLabel')}</label>
                <select
                    id="site-template"
                    value={selectedId}
                    onChange={event => { setSelectedId(event.target.value); setError(''); setNotice(''); }}
                    className="flex-1 min-w-0 px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                >
                    <option value="">{t('siteCommand.templateNone')}</option>
                    {templates.map(template => <option key={template.id} value={template.id}>{template.name}</option>)}
                </select>
                <button type="button" onClick={() => void importGlftpd()} disabled={disabled} className="px-2 py-1 text-xs rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-300 disabled:opacity-40">{t('siteCommand.importGlftpd')}</button>
                <button type="button" onClick={() => { setSaving(open => !open); setError(''); }} disabled={disabled || draft.trim() === ''} className="px-2 py-1 text-xs rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-300 disabled:opacity-40">{t('siteCommand.saveLine')}</button>
                <button type="button" onClick={() => void remove()} disabled={disabled || !selected} className="px-2 py-1 text-xs rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-300 disabled:opacity-40">{t('siteCommand.removeTemplate')}</button>
            </div>
            {saving && (
                <div className="flex items-center gap-2">
                    <input
                        value={saveName}
                        onChange={event => setSaveName(event.target.value)}
                        placeholder={t('siteCommand.saveName')}
                        aria-label={t('siteCommand.saveName')}
                        maxLength={64}
                        className="flex-1 px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                    />
                    <button type="button" onClick={() => void save()} disabled={saveName.trim() === ''} className="px-2 py-1 text-xs rounded bg-blue-500 text-white disabled:opacity-40">{t('siteCommand.saveConfirm')}</button>
                </div>
            )}
            {selected && (
                <div className="flex flex-wrap items-end gap-2">
                    {slots.map(slot => (
                        <label key={slot.name} className="flex flex-col text-[11px] text-gray-500 dark:text-gray-400">
                            {slot.name}
                            <input
                                type={slot.kind === 'secret' ? 'password' : 'text'}
                                inputMode={slot.kind === 'number' ? 'numeric' : undefined}
                                value={values[slot.name] ?? ''}
                                autoComplete="off"
                                spellCheck={false}
                                aria-label={slot.name}
                                onChange={event => setValues(current => ({ ...current, [slot.name]: event.target.value }))}
                                className="mt-0.5 px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm font-mono w-36"
                            />
                        </label>
                    ))}
                    <button type="button" onClick={() => void run()} disabled={disabled} className="px-3 py-1 text-xs rounded bg-blue-500 text-white disabled:opacity-40">{t('siteCommand.runTemplate')}</button>
                </div>
            )}
            {notice && <p className="text-[11px] text-gray-500 dark:text-gray-400">{notice}</p>}
            {error && <p className="text-[11px] text-red-600 dark:text-red-400" role="alert">{error}</p>}
            <p className="text-[11px] text-gray-500 dark:text-gray-400">{t('siteCommand.templateStored')}</p>
        </div>
    );
};
