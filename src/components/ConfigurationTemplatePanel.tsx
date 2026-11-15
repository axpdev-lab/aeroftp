// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useLayoutEffect, useMemo, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    AlertCircle,
    CheckCircle2,
    FileText,
    FolderInput,
    KeyRound,
    Server,
    Sparkles,
    Upload,
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { ServerProfile, type CryptSecretForm } from '../types';
import { pickFile } from '../utils/pickPath';
import { Checkbox } from './ui/Checkbox';
import { CryptSecretFormChoice } from './CryptSecretFormChoice';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { announceStoredServerProfiles } from '../utils/serverProfileStore';
import { accountChangeGeneration } from '../utils/userPartitions';
import { ImportSecurityReview, keptRelaxationId, type ImportedSecurity } from './ImportSecurityReview';
import { useActivityLog } from '../hooks/useActivityLog';

export type TemplateExistingMode = 'skip' | 'duplicate' | 'overwrite';
export type TemplatePlannedAction = 'add' | 'skip' | 'rename' | 'replace';
export type SecretDisposition = 'none' | 'newCredential' | 'keptExisting' | 'clearedExisting';

export interface IniParseIssue {
    line: number;
    blockName: string | null;
    message: string;
}

export interface TemplateSkippedBlock {
    name: string;
    backendType: string;
    reason: string;
    startLine: number;
}

export interface TemplateBlockWarning {
    name: string;
    reason: string;
    startLine: number;
}

export interface TemplatePreviewRow {
    fileIndex: number;
    blockName: string;
    targetName: string;
    startLine: number;
    protocol: string;
    providerId: string | null;
    host: string;
    port: number;
    username: string;
    hasCredential: boolean;
    hasSecretInput: boolean;
    looksObscured: boolean;
    secretForm: CryptSecretForm | null;
    needsSecretFormChoice: boolean;
    existingMatchId: string | null;
    existingMatchName: string | null;
    action: TemplatePlannedAction;
    secretDisposition: SecretDisposition;
    secretNote: string | null;
    duplicateInTemplate: boolean;
}

export interface TemplatePreviewReport {
    rows: TemplatePreviewRow[];
    skipped: TemplateSkippedBlock[];
    warnings: TemplateBlockWarning[];
    parseIssues: IniParseIssue[];
    security: ImportedSecurity[];
    secretChoiceBlocks: string[];
    totalBlocks: number;
    capped: boolean;
    sourceLabel: string;
    digestSha256: string;
    planDigestSha256: string;
}

export interface TemplateAppliedRow {
    blockName: string;
    targetName: string;
    profileId: string;
    protocol: string;
    action: TemplatePlannedAction;
    secretDisposition: SecretDisposition;
    secretNote: string | null;
    secretForm: CryptSecretForm | null;
    hasStoredCredential: boolean;
}

export interface TemplateImportResult {
    applied: TemplateAppliedRow[];
    skippedExisting: string[];
    skippedDuplicateInTemplate?: string[];
    skippedUnselected: string[];
    skippedInvalid: TemplateSkippedBlock[];
    warnings: TemplateBlockWarning[];
    parseIssues: IniParseIssue[];
    security: ImportedSecurity[];
    oauthAppsKept: string[];
    sharedSecretsFailed: string[];
    stored: Record<string, unknown>[];
    digestSha256: string;
}

type TemplateInputPayload =
    | { kind: 'text'; content: string }
    | { kind: 'file'; path: string };

const MAX_TEMPLATE_BYTES = 10 * 1024 * 1024;
const PREVIEW_DEBOUNCE_MS = 250;

const EXAMPLE_TEMPLATE = `[example-sftp]
type = sftp
host = files.example.com
port = 22
user = deploy

[example-webdav]
type = webdav
url = https://cloud.example.com/dav
user = team
`;

const EXISTING_MODES: Array<{ mode: TemplateExistingMode; labelKey: string; hintKey: string }> = [
    {
        mode: 'skip',
        labelKey: 'settings.templateExistingSkip',
        hintKey: 'settings.templateExistingSkipHint',
    },
    {
        mode: 'duplicate',
        labelKey: 'settings.templateExistingRename',
        hintKey: 'settings.templateExistingRenameHint',
    },
    {
        mode: 'overwrite',
        labelKey: 'settings.templateExistingReplace',
        hintKey: 'settings.templateExistingReplaceHint',
    },
];

interface Props {
    droppedFilePath?: string | null;
    onConsumeDroppedFile?: () => void;
    onProfilesStored: (stored: ServerProfile[]) => void;
    onBack: () => void;
    onClose: () => void;
    onBusyChange?: (busy: boolean) => void;
}

/** What a consent to keep a security-relaxing option is given for: the
 *  block's name, the option and the value reviewed. A preview that moves the
 *  block keeps it; one that changes the block or the value drops it. */
function relaxationIdentity(sec: ImportedSecurity, relaxation: { key: string; value: string }): string {
    return JSON.stringify([sec.name, relaxation.key, relaxation.value]);
}

function actionBadgeClass(action: TemplatePlannedAction): string {
    switch (action) {
        case 'add':
            return 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300';
        case 'rename':
            return 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300';
        case 'replace':
            return 'bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300';
        case 'skip':
        default:
            return 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-300';
    }
}

export const ConfigurationTemplatePanel: React.FC<Props> = ({
    droppedFilePath,
    onConsumeDroppedFile,
    onProfilesStored,
    onBack,
    onClose,
    onBusyChange,
}) => {
    const t = useTranslation();
    const tRef = useRef(t);
    useLayoutEffect(() => {
        tRef.current = t;
    });
    const activityLog = useActivityLog();

    const [sourceTab, setSourceTab] = useState<'paste' | 'file'>('paste');
    const [textContent, setTextContent] = useState('');
    const [filePath, setFilePath] = useState<string | null>(null);
    const [existingMode, setExistingMode] = useState<TemplateExistingMode>('skip');
    const [secretForms, setSecretForms] = useState<Record<string, CryptSecretForm>>({});
    const [selectedBlocks, setSelectedBlocks] = useState<Set<string>>(() => new Set());
    const [keptRelaxed, setKeptRelaxed] = useState<Set<string>>(() => new Set());

    const [preview, setPreview] = useState<TemplatePreviewReport | null>(null);
    const [previewing, setPreviewing] = useState(false);
    const [importing, setImporting] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [warningNote, setWarningNote] = useState<string | null>(null);

    const requestSeqRef = useRef(0);
    // Whether the selection was set by a preview of this source: only the
    // first one selects every importable block, so an explicit None survives
    // the previews that follow (a secret form, a mode change).
    const selectionInitializedRef = useRef(false);
    const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const lastDebouncedTextRef = useRef<string>('');

    useEffect(() => {
        onBusyChange?.(importing);
    }, [importing, onBusyChange]);

    useEffect(() => {
        return () => {
            if (closeTimerRef.current) clearTimeout(closeTimerRef.current);
        };
    }, []);

    /** A pending close of an earlier import, cancelled when the source
     *  changes or another import starts. */
    const cancelPendingClose = useCallback(() => {
        if (closeTimerRef.current) clearTimeout(closeTimerRef.current);
        closeTimerRef.current = null;
    }, []);

    const resetSelectionAndPolicies = useCallback(() => {
        cancelPendingClose();
        selectionInitializedRef.current = false;
        setSecretForms({});
        setSelectedBlocks(new Set());
        setKeptRelaxed(new Set());
    }, [cancelPendingClose]);

    // An earlier import's pending close never ends a later source's work.
    useEffect(() => {
        cancelPendingClose();
    }, [textContent, filePath, sourceTab, cancelPendingClose]);

    // Route dropped template file into File mode directly.
    useEffect(() => {
        if (droppedFilePath) {
            resetSelectionAndPolicies();
            setSourceTab('file');
            setFilePath(droppedFilePath);
            setSuccess(null);
            setWarningNote(null);
            onConsumeDroppedFile?.();
        }
    }, [droppedFilePath, onConsumeDroppedFile, resetSelectionAndPolicies]);

    const currentInput = useMemo<TemplateInputPayload | null>(() => {
        if (sourceTab === 'paste') {
            return textContent.trim().length > 0 ? { kind: 'text', content: textContent } : null;
        }
        return filePath ? { kind: 'file', path: filePath } : null;
    }, [sourceTab, textContent, filePath]);

    const runPreview = useCallback(async (
        payload: TemplateInputPayload,
        mode: TemplateExistingMode,
        forms: Record<string, CryptSecretForm>,
    ) => {
        if (payload.kind === 'text') {
            const byteLen = new TextEncoder().encode(payload.content).length;
            if (byteLen > MAX_TEMPLATE_BYTES) {
                setPreview(null);
                setError(tRef.current('settings.templateTooLarge'));
                return;
            }
        }
        const seq = ++requestSeqRef.current;
        setPreviewing(true);
        setError(null);
        try {
            const report = await invoke<TemplatePreviewReport>('preview_configuration_template', {
                input: payload,
                options: {
                    existing: mode,
                    secretForms: forms,
                },
            });
            if (seq !== requestSeqRef.current) return;
            setPreview(report);
            const initialized = selectionInitializedRef.current;
            selectionInitializedRef.current = true;
            setSelectedBlocks(prev => {
                const next = new Set<string>();
                for (const row of report.rows) {
                    if (row.action === 'skip') continue;
                    if (!initialized || prev.has(row.blockName)) next.add(row.blockName);
                }
                return next;
            });
            // A consent stays only on the option it was given for: the same
            // block, key and value (`relaxationIdentity`), wherever the block
            // now sits.
            setKeptRelaxed(prev => {
                const allowed = new Set(report.security.flatMap(sec =>
                    [...sec.reset, ...sec.kept].map(r => relaxationIdentity(sec, r))));
                return new Set([...prev].filter(identity => allowed.has(identity)));
            });
        } catch (err) {
            if (seq !== requestSeqRef.current) return;
            setPreview(null);
            setError(String(err));
        } finally {
            if (seq === requestSeqRef.current) {
                setPreviewing(false);
            }
        }
    }, []);

    // Trigger live preview: debounced when editing pasted text, immediate on toggles/files.
    useEffect(() => {
        if (!currentInput) {
            lastDebouncedTextRef.current = '';
            requestSeqRef.current += 1;
            setPreview(null);
            setError(null);
            setPreviewing(false);
            return;
        }
        const textChanged = sourceTab === 'paste' && lastDebouncedTextRef.current !== textContent;
        if (textChanged) {
            const timer = setTimeout(() => {
                lastDebouncedTextRef.current = textContent;
                void runPreview(currentInput, existingMode, secretForms);
            }, PREVIEW_DEBOUNCE_MS);
            return () => clearTimeout(timer);
        }
        void runPreview(currentInput, existingMode, secretForms);
    }, [currentInput, sourceTab, textContent, existingMode, secretForms, runPreview]);

    const handleBrowseFile = async () => {
        const picked = await pickFile({
            title: t('settings.templatePickTitle'),
            filters: [
                { name: t('settings.templatePickFilterName'), extensions: ['ini', 'conf', 'cfg', 'txt'] },
            ],
        });
        if (!picked) return;
        resetSelectionAndPolicies();
        setSourceTab('file');
        setFilePath(picked);
        setSuccess(null);
        setWarningNote(null);
    };

    const switchSourceTab = (nextTab: 'paste' | 'file') => {
        if (nextTab === sourceTab) return;
        resetSelectionAndPolicies();
        setSourceTab(nextTab);
    };

    const toggleBlockSelected = (blockName: string) => {
        setSelectedBlocks(prev => {
            const next = new Set(prev);
            if (next.has(blockName)) next.delete(blockName);
            else next.add(blockName);
            return next;
        });
    };

    const setBlockSecretForm = (blockName: string, form: CryptSecretForm) => {
        setSecretForms(prev => ({
            ...prev,
            [blockName]: form,
        }));
    };

    // The review's checkbox ids (by position) and the consents (by identity).
    const relaxationIdentityById = useMemo(() => new Map((preview?.security ?? []).flatMap(sec =>
        [...sec.reset, ...sec.kept].map(r => [keptRelaxationId(sec.fileIndex, r.key), relaxationIdentity(sec, r)] as const))),
    [preview]);
    const keptRelaxedIds = useMemo(
        () => new Set([...relaxationIdentityById].filter(([, identity]) => keptRelaxed.has(identity)).map(([id]) => id)),
        [relaxationIdentityById, keptRelaxed],
    );

    const selectableRows = useMemo(
        () => (preview ? preview.rows.filter(r => r.action !== 'skip') : []),
        [preview],
    );

    const activeSelectedCount = useMemo(
        () => selectableRows.filter(r => selectedBlocks.has(r.blockName)).length,
        [selectableRows, selectedBlocks],
    );

    const missingSecretFormCount = useMemo(
        () => selectableRows.filter(r => selectedBlocks.has(r.blockName) && r.needsSecretFormChoice).length,
        [selectableRows, selectedBlocks],
    );

    const handleImport = async () => {
        if (!currentInput || !preview || activeSelectedCount === 0 || missingSecretFormCount > 0 || importing) return;
        cancelPendingClose();
        setImporting(true);
        setError(null);
        setSuccess(null);
        setWarningNote(null);
        const accountBefore = accountChangeGeneration();
        try {
            const keepRelaxedList = preview.security.flatMap(sec =>
                [...sec.reset, ...sec.kept]
                    .filter(r => keptRelaxed.has(relaxationIdentity(sec, r)))
                    .map(r => ({ fileIndex: sec.fileIndex, key: r.key })),
            );
            const activeSecretForms = Object.fromEntries(
                Object.entries(secretForms).filter(([blockName]) =>
                    preview.secretChoiceBlocks.includes(blockName),
                ),
            );
            const result = await invoke<TemplateImportResult>('import_configuration_template', {
                input: currentInput,
                options: {
                    existing: existingMode,
                    secretForms: activeSecretForms,
                    selectedBlocks: selectableRows
                        .filter(r => selectedBlocks.has(r.blockName))
                        .map(r => r.blockName),
                    keepRelaxed: keepRelaxedList,
                    expectedDigestSha256: preview.digestSha256,
                    expectedPlanDigestSha256: preview.planDigestSha256,
                },
            });

            const storedProfiles = result.stored as unknown as ServerProfile[];
            announceStoredServerProfiles(storedProfiles, accountBefore);
            if (accountChangeGeneration() === accountBefore) {
                onProfilesStored(storedProfiles);
            }

            const blockNames = result.applied.map(r => r.blockName).join(', ');
            const skippedCount = result.skippedExisting.length + (result.skippedDuplicateInTemplate?.length ?? 0);
            activityLog.log(
                'PROFILE_SAVE',
                t('settings.templateLogImported', {
                    imported: result.applied.length,
                    skipped: skippedCount,
                    invalid: result.skippedInvalid.length,
                }),
                'success',
                blockNames ? t('settings.templateLogBlocks', { blocks: blockNames }) : undefined,
            );

            const notices: string[] = [];
            if (result.oauthAppsKept.length > 0) {
                notices.push(
                    t('settings.templateOAuthAppsKept', { names: result.oauthAppsKept.join(', ') }),
                );
            }
            if (result.sharedSecretsFailed.length > 0) {
                notices.push(
                    t('settings.templateSharedSecretsFailed', { names: result.sharedSecretsFailed.join(', ') }),
                );
            }

            const msg = t('settings.templateImportSuccess', { count: result.applied.length });
            if (notices.length > 0) {
                setWarningNote(`${msg}. ${notices.join(' ')}`);
            } else {
                setSuccess(msg);
                closeTimerRef.current = setTimeout(() => onClose(), 1400);
            }
        } catch (err) {
            setError(String(err));
        } finally {
            setImporting(false);
        }
    };

    const actionLabel = (action: TemplatePlannedAction): string => {
        switch (action) {
            case 'add':
                return t('settings.templateActionAdd');
            case 'rename':
                return t('settings.templateActionRename');
            case 'replace':
                return t('settings.templateActionReplace');
            case 'skip':
                return t('settings.templateActionSkip');
        }
    };

    const renderSecretNote = (row: TemplatePreviewRow): string => {
        if (row.action === 'skip' && row.duplicateInTemplate) {
            return t('settings.templateDuplicateInTemplate');
        }
        if (row.secretDisposition === 'keptExisting') {
            return t('settings.templateSecretKept');
        }
        if (row.secretDisposition === 'clearedExisting') {
            return t('settings.templateSecretCleared');
        }
        if (row.secretDisposition === 'newCredential') {
            return t('settings.templateSecretNew');
        }
        if (row.needsSecretFormChoice) {
            return t('settings.templateSecretFormPending');
        }
        return t('settings.templateSecretNone');
    };

    const activeHintKey = EXISTING_MODES.find(m => m.mode === existingMode)?.hintKey;

    return (
        <div className="space-y-4">
            {/* Source tabs: Paste vs File */}
            <div className="flex gap-1 p-1 rounded-lg bg-gray-100 dark:bg-gray-900/40">
                <button
                    type="button"
                    data-testid="template-tab-paste"
                    onClick={() => switchSourceTab('paste')}
                    className={`flex-1 px-3 py-1.5 text-xs rounded-md flex items-center justify-center gap-1.5 transition-colors ${
                        sourceTab === 'paste'
                            ? 'bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 shadow-sm font-medium'
                            : 'text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200'
                    }`}
                >
                    <FileText size={14} />
                    {t('settings.templateTabPaste')}
                </button>
                <button
                    type="button"
                    data-testid="template-tab-file"
                    onClick={() => switchSourceTab('file')}
                    className={`flex-1 px-3 py-1.5 text-xs rounded-md flex items-center justify-center gap-1.5 transition-colors ${
                        sourceTab === 'file'
                            ? 'bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 shadow-sm font-medium'
                            : 'text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200'
                    }`}
                >
                    <FolderInput size={14} />
                    {t('settings.templateTabFile')}
                </button>
            </div>

            {sourceTab === 'paste' ? (
                <div className="space-y-1.5">
                    <div className="flex items-center justify-between text-xs">
                        <span className="text-gray-600 dark:text-gray-400">
                            {t('settings.templatePasteHint')}
                        </span>
                        <div className="flex items-center gap-2">
                            <button
                                type="button"
                                data-testid="template-show-example"
                                onClick={() => {
                                    resetSelectionAndPolicies();
                                    setTextContent(EXAMPLE_TEMPLATE);
                                    setSuccess(null);
                                    setWarningNote(null);
                                }}
                                className="text-blue-600 dark:text-blue-400 hover:underline flex items-center gap-1 font-medium"
                            >
                                <Sparkles size={12} />
                                {t('settings.templateShowExample')}
                            </button>
                            {textContent.length > 0 && (
                                <button
                                    type="button"
                                    onClick={() => {
                                        resetSelectionAndPolicies();
                                        setTextContent('');
                                        setSuccess(null);
                                        setWarningNote(null);
                                    }}
                                    className="text-gray-500 hover:text-gray-700 dark:hover:text-gray-300"
                                >
                                    {t('settings.templateClear')}
                                </button>
                            )}
                        </div>
                    </div>
                    <textarea
                        data-testid="template-textarea"
                        value={textContent}
                        onChange={(e) => {
                            setTextContent(e.target.value);
                            setSuccess(null);
                            setWarningNote(null);
                        }}
                        rows={6}
                        spellCheck={false}
                        placeholder={t('settings.templateTextareaPlaceholder')}
                        className="w-full px-3 py-2 text-xs font-mono border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-900/60 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
                    />
                </div>
            ) : (
                <div className="space-y-2">
                    {filePath ? (
                        <div className="flex items-center justify-between gap-2 p-2.5 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900/40">
                            <div className="flex items-center gap-2 min-w-0">
                                <FileText size={16} className="text-blue-500 flex-shrink-0" />
                                <span className="text-sm text-gray-800 dark:text-gray-100 truncate" title={filePath}>
                                    {filePath.split(/[\\/]/).pop()}
                                </span>
                            </div>
                            <button
                                type="button"
                                onClick={handleBrowseFile}
                                className="text-xs text-blue-500 hover:text-blue-600 font-medium flex-shrink-0"
                            >
                                {t('settings.importChangeFile')}
                            </button>
                        </div>
                    ) : (
                        <button
                            type="button"
                            data-testid="template-choose-file"
                            onClick={handleBrowseFile}
                            className="w-full py-6 px-4 border-2 border-dashed border-gray-300 dark:border-gray-600 rounded-lg hover:border-blue-400 dark:hover:border-blue-500 hover:bg-gray-50 dark:hover:bg-gray-800/60 flex flex-col items-center gap-1.5 transition-colors text-xs text-gray-600 dark:text-gray-400"
                        >
                            <FolderInput size={20} className="text-blue-500" />
                            <span className="font-medium text-gray-800 dark:text-gray-200">
                                {t('settings.templateChooseFile')}
                            </span>
                            <span>{t('settings.templateFileTypes')}</span>
                        </button>
                    )}
                </div>
            )}

            {/* Existing profile policy */}
            <div role="radiogroup" aria-label={t('settings.importExistingLabel')} className="space-y-1.5">
                <div className="text-xs font-medium text-gray-600 dark:text-gray-400">
                    {t('settings.importExistingLabel')}
                </div>
                <div className="flex gap-1 p-1 rounded-lg bg-gray-100 dark:bg-gray-900/40">
                    {EXISTING_MODES.map(({ mode, labelKey }) => (
                        <button
                            key={mode}
                            type="button"
                            role="radio"
                            aria-checked={existingMode === mode}
                            data-testid={`template-existing-${mode}`}
                            onClick={() => setExistingMode(mode)}
                            className={`flex-1 px-2 py-1 text-xs rounded-md transition-colors ${
                                existingMode === mode
                                    ? 'bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 shadow-sm font-medium'
                                    : 'text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200'
                            }`}
                        >
                            {t(labelKey)}
                        </button>
                    ))}
                </div>
                {activeHintKey && (
                    <p className="text-xs text-gray-500 dark:text-gray-400">{t(activeHintKey)}</p>
                )}
            </div>

            {/* Live Preview */}
            {previewing && (
                <div className="text-xs text-gray-500 dark:text-gray-400 flex items-center gap-2">
                    <span className="animate-spin w-3.5 h-3.5 border-2 border-blue-500 border-t-transparent rounded-full" />
                    {t('settings.templatePreviewing')}
                </div>
            )}

            {preview && (
                <div className="space-y-2.5">
                    {/* Syntax issues */}
                    {preview.parseIssues.length > 0 && (
                        <div className="p-2.5 rounded-lg bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800/50 space-y-1">
                            <div className="text-xs font-medium text-amber-800 dark:text-amber-300">
                                {t('settings.templateParseIssuesTitle')}
                            </div>
                            <ul className="text-xs text-amber-700 dark:text-amber-300 space-y-0.5 max-h-24 overflow-y-auto">
                                {preview.parseIssues.map((issue, idx) => (
                                    <li key={`${issue.line}-${idx}`}>
                                        {`L${issue.line}${issue.blockName ? ` [${issue.blockName}]` : ''}: ${issue.message}`}
                                    </li>
                                ))}
                            </ul>
                        </div>
                    )}

                    {/* Rows table */}
                    {preview.rows.length > 0 && (
                        <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
                            <div className="flex items-center justify-between px-3 py-1.5 bg-gray-50 dark:bg-gray-900/40 border-b border-gray-200 dark:border-gray-700 text-xs text-gray-600 dark:text-gray-400">
                                <span>
                                    {t('settings.templateFoundSummary', {
                                        count: preview.rows.length,
                                        selected: activeSelectedCount,
                                    })}
                                </span>
                                {selectableRows.length > 1 && (
                                    <div className="flex items-center gap-2">
                                        <button
                                            type="button"
                                            onClick={() => setSelectedBlocks(new Set(selectableRows.map(r => r.blockName)))}
                                            className="text-blue-600 dark:text-blue-400 hover:underline"
                                        >
                                            {t('settings.templateSelectAll')}
                                        </button>
                                        <button
                                            type="button"
                                            onClick={() => setSelectedBlocks(new Set())}
                                            className="text-gray-500 hover:underline"
                                        >
                                            {t('settings.templateSelectNone')}
                                        </button>
                                    </div>
                                )}
                            </div>
                            <div className="max-h-60 overflow-y-auto divide-y divide-gray-100 dark:divide-gray-700/70">
                                {preview.rows.map(row => {
                                    const Logo =
                                        (row.providerId && PROVIDER_LOGOS[row.providerId])
                                        || PROVIDER_LOGOS[row.protocol.toLowerCase()]
                                        || PROVIDER_LOGOS[row.protocol];
                                    const checked = row.action !== 'skip' && selectedBlocks.has(row.blockName);
                                    const chosenForm = secretForms[row.blockName] ?? row.secretForm ?? undefined;
                                    return (
                                        <div key={`${row.blockName}-${row.fileIndex}`} className="px-3 py-2 text-xs space-y-1">
                                            <div className="flex items-center gap-2">
                                                <Checkbox
                                                    checked={checked}
                                                    disabled={row.action === 'skip'}
                                                    onChange={() => toggleBlockSelected(row.blockName)}
                                                />
                                                <div className="w-5 h-5 flex items-center justify-center flex-shrink-0">
                                                    {Logo ? <Logo size={18} /> : <Server size={16} className="text-gray-400" />}
                                                </div>
                                                <div className="min-w-0 flex-1">
                                                    <div className="flex items-center gap-1.5">
                                                        <span className="font-medium text-gray-900 dark:text-gray-100 truncate">
                                                            {row.targetName}
                                                        </span>
                                                        {row.targetName !== row.blockName && (
                                                            <span className="text-[10px] text-gray-400 truncate">
                                                                {`(${row.blockName})`}
                                                            </span>
                                                        )}
                                                        <span className="text-[10px] text-gray-400">{`L${row.startLine}`}</span>
                                                    </div>
                                                    <div className="text-[11px] text-gray-500 dark:text-gray-400 truncate">
                                                        {`${row.protocol.toUpperCase()} • ${row.host || '-'}${row.port ? `:${row.port}` : ''}${row.username ? ` • ${row.username}` : ''}`}
                                                    </div>
                                                </div>
                                                <div className="flex flex-col items-end gap-0.5 flex-shrink-0">
                                                    <span className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${actionBadgeClass(row.action)}`}>
                                                        {actionLabel(row.action)}
                                                    </span>
                                                    <span className="text-[10px] text-gray-500 dark:text-gray-400">
                                                        {renderSecretNote(row)}
                                                    </span>
                                                </div>
                                            </div>
                                            {row.hasSecretInput && row.action !== 'skip' && (
                                                <div
                                                    data-testid={`template-secret-choice-${row.blockName}`}
                                                    className="ml-7 mt-1 p-2 rounded bg-gray-50 dark:bg-gray-900/40 border border-gray-200 dark:border-gray-700"
                                                >
                                                    <div className="flex items-center gap-1.5 text-[11px] font-medium text-gray-700 dark:text-gray-300">
                                                        <KeyRound size={12} className="text-amber-500 flex-shrink-0" />
                                                        <span>
                                                            {row.looksObscured
                                                                ? t('settings.templateSecretLooksObscured')
                                                                : t('settings.templateSecretLooksPlain')}
                                                        </span>
                                                    </div>
                                                    <CryptSecretFormChoice
                                                        legend={row.blockName}
                                                        value={chosenForm}
                                                        onChange={(form) => setBlockSecretForm(row.blockName, form)}
                                                        missing={checked && row.needsSecretFormChoice}
                                                        clearLabel={t('settings.templateSecretFormClear')}
                                                        obscuredLabel={t('settings.templateSecretFormObscured')}
                                                        unrecordedHint={t('settings.templateSecretFormRequired')}
                                                        recordedHint={t('settings.templateSecretFormSelected')}
                                                    />
                                                </div>
                                            )}
                                        </div>
                                    );
                                })}
                            </div>
                        </div>
                    )}

                    {/* Skipped invalid blocks / warnings */}
                    {(preview.skipped.length > 0 || preview.warnings.length > 0) && (
                        <div className="p-2.5 rounded-lg bg-gray-50 dark:bg-gray-900/40 border border-gray-200 dark:border-gray-700 space-y-1 text-xs max-h-28 overflow-y-auto">
                            {preview.skipped.map(item => (
                                <div key={`skip-${item.name}-${item.startLine}`} className="text-red-600 dark:text-red-400">
                                    {`L${item.startLine} [${item.name}] (${item.backendType}): ${item.reason}`}
                                </div>
                            ))}
                            {preview.warnings.map((w, idx) => (
                                <div key={`warn-${w.name}-${idx}`} className="text-amber-600 dark:text-amber-400">
                                    {`L${w.startLine} [${w.name}]: ${w.reason}`}
                                </div>
                            ))}
                        </div>
                    )}

                    {preview.security.length > 0 && (
                        <ImportSecurityReview
                            entries={preview.security}
                            kept={keptRelaxedIds}
                            onToggle={id => {
                                const identity = relaxationIdentityById.get(id);
                                if (identity === undefined) return;
                                setKeptRelaxed(prev => {
                                    const next = new Set(prev);
                                    if (next.has(identity)) next.delete(identity);
                                    else next.add(identity);
                                    return next;
                                });
                            }}
                        />
                    )}
                </div>
            )}

            {error && (
                <div className="text-red-500 text-xs flex items-start gap-2">
                    <AlertCircle size={14} className="mt-0.5 flex-shrink-0" />
                    <span>{error}</span>
                </div>
            )}
            {success && (
                <div className="text-green-600 dark:text-green-400 text-xs flex items-center gap-2">
                    <CheckCircle2 size={14} />
                    <span>{success}</span>
                </div>
            )}
            {warningNote && (
                <div className="text-amber-600 dark:text-amber-400 text-xs flex items-start gap-2">
                    <AlertCircle size={14} className="mt-0.5 flex-shrink-0" />
                    <span>{warningNote}</span>
                </div>
            )}

            <div className="flex gap-2">
                <button
                    type="button"
                    onClick={onBack}
                    disabled={importing}
                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-50"
                >
                    {t('common.back')}
                </button>
                <button
                    type="button"
                    data-testid="template-import-submit"
                    onClick={handleImport}
                    disabled={
                        !preview
                        || activeSelectedCount === 0
                        || missingSecretFormCount > 0
                        || importing
                        || previewing
                        || success !== null
                    }
                    className="flex-1 px-4 py-2 text-sm bg-blue-500 text-white rounded-lg hover:bg-blue-600 disabled:opacity-50 flex items-center justify-center gap-2"
                >
                    {importing ? (
                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                    ) : (
                        <Upload size={16} />
                    )}
                    {importing
                        ? t('settings.importing')
                        : t('settings.templateImportButton', { count: activeSelectedCount })}
                </button>
            </div>
        </div>
    );
};
