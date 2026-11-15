// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ActivityLogProvider, useActivityLog } from '../hooks/useActivityLog';
import { PROFILES_CHANGED_EVENT, type ProfilesChangedDetail } from '../utils/serverProfileStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../utils/pickPath', () => ({
    pickFile: vi.fn(async () => '/templates/batch.ini'),
}));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string, params?: Record<string, string | number>) =>
        params ? `${key}:${Object.values(params).join(':')}` : key,
}));
vi.mock('../utils/userPartitions', () => ({
    accountChangeGeneration: () => 0,
}));

import { invoke } from '@tauri-apps/api/core';
import { pickFile } from '../utils/pickPath';
import { ConfigurationTemplatePanel } from './ConfigurationTemplatePanel';

let root: Root;
let host: HTMLDivElement;

function LogObserver({ onEntries }: { onEntries: (msgs: string[]) => void }) {
    const { entries } = useActivityLog();
    onEntries(entries.map(e => `${e.operation}:${e.message}:${e.details ?? ''}`));
    return null;
}

beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.useRealTimers();
    vi.mocked(invoke).mockReset();
    vi.mocked(pickFile).mockClear();
});

async function flush() {
    await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
    });
}

describe('ConfigurationTemplatePanel', () => {
    it('evaluates pasted template live, handles per-block secret form choice and records counts/block-names only in activity log', async () => {
        const loggedMessages: string[] = [];
        const storedEvents: ProfilesChangedDetail[] = [];
        const onProfilesStored = vi.fn();
        const listener = (ev: Event) => storedEvents.push((ev as CustomEvent<ProfilesChangedDetail>).detail);
        window.addEventListener(PROFILES_CHANGED_EVENT, listener);

        vi.mocked(invoke).mockImplementation(async (cmd, args) => {
            if (cmd === 'preview_configuration_template') {
                const payload = args as { options: { secretForms?: Record<string, string>; existing: string } };
                const chosenForm = payload.options.secretForms?.alpha ?? null;
                return {
                    rows: [
                        {
                            fileIndex: 0,
                            blockName: 'alpha',
                            targetName: 'alpha',
                            startLine: 1,
                            protocol: 'sftp',
                            providerId: null,
                            host: 'alpha.example.com',
                            port: 22,
                            username: 'deploy',
                            hasCredential: chosenForm !== null,
                            hasSecretInput: true,
                            looksObscured: true,
                            secretForm: chosenForm,
                            needsSecretFormChoice: chosenForm === null,
                            existingMatchId: payload.options.existing === 'overwrite' ? 'srv_1' : null,
                            existingMatchName: payload.options.existing === 'overwrite' ? 'alpha' : null,
                            action: payload.options.existing === 'overwrite' ? 'replace' : 'add',
                            duplicateInTemplate: false,
                            secretDisposition: chosenForm !== null ? 'newCredential' : 'none',
                            secretNote: chosenForm !== null ? 'new credential' : 'password form not selected',
                        },
                    ],
                    skipped: [],
                    warnings: [],
                    parseIssues: [],
                    security: [],
                    secretChoiceBlocks: ['alpha'],
                    totalBlocks: 1,
                    capped: false,
                    sourceLabel: 'template',
                    digestSha256: 'abc123',
                    planDigestSha256: 'plan-sha-1',
                };
            }
            if (cmd === 'import_configuration_template') {
                return {
                    applied: [
                        {
                            blockName: 'alpha',
                            targetName: 'alpha',
                            profileId: 'tpl-alpha-1',
                            protocol: 'sftp',
                            action: 'add',
                            secretDisposition: 'newCredential',
                            secretNote: 'new credential',
                            secretForm: 'clear',
                            hasStoredCredential: true,
                        },
                    ],
                    skippedExisting: [],
                    skippedUnselected: [],
                    skippedInvalid: [],
                    warnings: [],
                    parseIssues: [],
                    security: [],
                    oauthAppsKept: [],
                    sharedSecretsFailed: [],
                    stored: [
                        {
                            id: 'tpl-alpha-1',
                            name: 'alpha',
                            host: 'alpha.example.com',
                            port: 22,
                            username: 'deploy',
                            protocol: 'sftp',
                            hasStoredCredential: true,
                        },
                    ],
                    digestSha256: 'abc123',
                };
            }
            throw new Error(`unexpected command: ${cmd}`);
        });

        await act(async () => {
            root.render(
                createElement(
                    ActivityLogProvider,
                    null,
                    createElement(LogObserver, { onEntries: (msgs) => { loggedMessages.splice(0, loggedMessages.length, ...msgs); } }),
                    createElement(ConfigurationTemplatePanel, {
                        onProfilesStored,
                        onBack: vi.fn(),
                        onClose: vi.fn(),
                    }),
                ),
            );
        });

        // Show example populates textarea cleanly without mentioning "rclone".
        await act(async () => {
            host.querySelector<HTMLButtonElement>('[data-testid="template-show-example"]')!.click();
        });
        const textarea = host.querySelector<HTMLTextAreaElement>('[data-testid="template-textarea"]')!;
        expect(textarea.value.toLowerCase()).not.toContain('rclone');

        // Type custom template with 23-char alphanumeric secret password.
        await act(async () => {
            const secretIni = '[alpha]\ntype = sftp\nhost = alpha.example.com\nuser = deploy\npass = AbcdefghijKlmnoPqrs1234\n';
            Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(textarea, secretIni);
            textarea.dispatchEvent(new Event('input', { bubbles: true }));
        });

        // Debounce fires after 250ms.
        await act(async () => {
            vi.advanceTimersByTime(300);
        });
        await flush();

        expect(vi.mocked(invoke)).toHaveBeenCalledWith(
            'preview_configuration_template',
            expect.objectContaining({
                options: expect.objectContaining({ secretForms: {} }),
            }),
        );

        // Submit stays disabled until explicit Clear/Obscured choice is made.
        const submitBtn = host.querySelector<HTMLButtonElement>('[data-testid="template-import-submit"]')!;
        expect(submitBtn.disabled).toBe(true);
        const secretGroup = host.querySelector('[data-testid="template-secret-choice-alpha"]')!;
        const radios = secretGroup.querySelectorAll<HTMLInputElement>('input[type="radio"]');
        expect(radios.length).toBe(2);
        expect(secretGroup.textContent).toContain('settings.templateSecretLooksObscured');
        expect(secretGroup.textContent?.toLowerCase()).not.toContain('rclone');

        // Pick "Plain text" (first radio).
        await act(async () => {
            radios[0].click();
        });
        await flush();

        expect(vi.mocked(invoke)).toHaveBeenLastCalledWith(
            'preview_configuration_template',
            expect.objectContaining({
                options: expect.objectContaining({ secretForms: { alpha: 'clear' } }),
            }),
        );
        expect(submitBtn.disabled).toBe(false);

        // Submit import.
        await act(async () => {
            submitBtn.click();
        });
        await flush();

        expect(vi.mocked(invoke)).toHaveBeenCalledWith(
            'import_configuration_template',
            expect.objectContaining({
                options: expect.objectContaining({
                    secretForms: { alpha: 'clear' },
                    selectedBlocks: ['alpha'],
                    expectedDigestSha256: 'abc123',
                    expectedPlanDigestSha256: 'plan-sha-1',
                }),
            }),
        );
        expect(onProfilesStored).toHaveBeenCalledTimes(1);
        expect(storedEvents).toHaveLength(1);
        expect(loggedMessages.some(m => m.includes('PROFILE_SAVE') && m.includes('settings.templateLogImported:1:0:0') && m.includes('settings.templateLogBlocks:alpha'))).toBe(true);
        expect(loggedMessages.join('\n')).not.toContain('AbcdefghijKlmnoPqrs1234');
        expect(host.textContent?.toLowerCase()).not.toContain('rclone');

        window.removeEventListener(PROFILES_CHANGED_EVENT, listener);
    });

    it('rejects oversized pasted text (>10 MB) on client before calling IPC and supports file browse', async () => {
        vi.mocked(invoke).mockResolvedValue({
            rows: [],
            skipped: [],
            warnings: [],
            parseIssues: [],
            security: [],
            secretChoiceBlocks: [],
            totalBlocks: 0,
            capped: false,
            sourceLabel: 'batch.ini',
            digestSha256: 'def456',
            planDigestSha256: 'plan-empty',
        });

        await act(async () => {
            root.render(
                createElement(
                    ActivityLogProvider,
                    null,
                    createElement(ConfigurationTemplatePanel, {
                        onProfilesStored: vi.fn(),
                        onBack: vi.fn(),
                        onClose: vi.fn(),
                    }),
                ),
            );
        });

        const textarea = host.querySelector<HTMLTextAreaElement>('[data-testid="template-textarea"]')!;
        // 4-byte UTF-8 emoji × 2,621,441 = 10,485,764 UTF-8 bytes (> 10 MB) without 10M-char jsdom slowdown.
        const oversized = '🔑'.repeat(2_621_441);
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(textarea, oversized);
            textarea.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => {
            vi.advanceTimersByTime(300);
        });
        await flush();

        expect(vi.mocked(invoke)).not.toHaveBeenCalled();
        expect(host.textContent).toContain('settings.templateTooLarge');

        // Switch to file tab & browse.
        await act(async () => {
            host.querySelector<HTMLButtonElement>('[data-testid="template-tab-file"]')!.click();
        });
        await act(async () => {
            host.querySelector<HTMLButtonElement>('[data-testid="template-choose-file"]')!.click();
        });
        await flush();

        expect(vi.mocked(pickFile)).toHaveBeenCalledTimes(1);
        expect(vi.mocked(invoke)).toHaveBeenCalledWith(
            'preview_configuration_template',
            expect.objectContaining({
                input: { kind: 'file', path: '/templates/batch.ini' },
            }),
        );
    });

    it('resets secretForms, selectedBlocks, and keptRelaxed on tab switch while keeping them while typing', async () => {
        vi.mocked(invoke).mockImplementation(async (cmd, args) => {
            if (cmd !== 'preview_configuration_template') throw new Error(`unexpected ${cmd}`);
            const payload = args as { options: { secretForms?: Record<string, string> } };
            const alphaForm = payload.options.secretForms?.alpha ?? null;
            return {
                rows: [
                    {
                        fileIndex: 0,
                        blockName: 'alpha',
                        targetName: 'alpha',
                        startLine: 1,
                        protocol: 'webdav',
                        providerId: 'custom-webdav',
                        host: 'dav.example.test',
                        port: 443,
                        username: 'alice',
                        hasCredential: alphaForm !== null,
                        hasSecretInput: true,
                        looksObscured: false,
                        secretForm: alphaForm,
                        needsSecretFormChoice: alphaForm === null,
                        existingMatchId: null,
                        existingMatchName: null,
                        action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: alphaForm !== null ? 'newCredential' : 'none',
                        secretNote: alphaForm !== null ? 'new credential' : 'password form not selected',
                    },
                    {
                        fileIndex: 1,
                        blockName: 'beta',
                        targetName: 'beta',
                        startLine: 6,
                        protocol: 'sftp',
                        providerId: null,
                        host: 'beta.example.test',
                        port: 22,
                        username: 'bob',
                        hasCredential: false,
                        hasSecretInput: false,
                        looksObscured: false,
                        secretForm: null,
                        needsSecretFormChoice: false,
                        existingMatchId: null,
                        existingMatchName: null,
                        action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: 'none',
                        secretNote: null,
                    },
                ],
                skipped: [],
                warnings: [],
                parseIssues: [],
                security: [
                    {
                        fileIndex: 0,
                        name: 'alpha',
                        reset: [{ key: 'verifyCert', value: 'false' }],
                        kept: [],
                    },
                ],
                secretChoiceBlocks: ['alpha'],
                totalBlocks: 2,
                capped: false,
                sourceLabel: 'pasted',
                digestSha256: 'digest-1',
                planDigestSha256: 'plan-1',
            };
        });

        await act(async () => {
            root.render(
                createElement(
                    ActivityLogProvider,
                    null,
                    createElement(ConfigurationTemplatePanel, {
                        onProfilesStored: vi.fn(),
                        onBack: vi.fn(),
                        onClose: vi.fn(),
                    }),
                ),
            );
        });

        const textarea = host.querySelector<HTMLTextAreaElement>('[data-testid="template-textarea"]')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(
                textarea,
                '[alpha]\ntype = webdav\nurl = https://dav.example.test/dav/\npass = p1\n[beta]\ntype = sftp\nhost = beta.example.test\n',
            );
            textarea.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => { vi.advanceTimersByTime(300); });
        await flush();

        // Pick secret form, uncheck alpha, and check relaxation toggle.
        await act(async () => {
            host.querySelector<HTMLInputElement>('[data-testid="template-secret-choice-alpha"] input[type="radio"]')!.click();
        });
        await flush();
        const getBlockBoxes = () => Array.from(host.querySelectorAll<HTMLElement>('[role="checkbox"]'));
        await act(async () => {
            getBlockBoxes()[0].click();
            host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.click();
        });
        await flush();

        // Typing keeps secretForms, selectedBlocks, and keptRelaxed intact.
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(
                textarea,
                '[alpha]\ntype = webdav\nurl = https://dav.example.test/dav/\npass = p1\n[beta]\ntype = sftp\nhost = beta2.example.test\n',
            );
            textarea.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => { vi.advanceTimersByTime(300); });
        await flush();
        expect(vi.mocked(invoke)).toHaveBeenLastCalledWith(
            'preview_configuration_template',
            expect.objectContaining({ options: expect.objectContaining({ secretForms: { alpha: 'clear' } }) }),
        );
        expect(getBlockBoxes()[0].getAttribute('aria-checked')).toBe('false');
        expect(getBlockBoxes()[1].getAttribute('aria-checked')).toBe('true');
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.checked).toBe(true);

        // Switching to File tab and picking a file clears all three states.
        vi.mocked(pickFile).mockResolvedValueOnce('/templates/other.ini');
        await act(async () => {
            host.querySelector<HTMLButtonElement>('[data-testid="template-tab-file"]')!.click();
        });
        await act(async () => {
            host.querySelector<HTMLButtonElement>('[data-testid="template-choose-file"]')!.click();
        });
        await flush();

        expect(vi.mocked(invoke)).toHaveBeenLastCalledWith(
            'preview_configuration_template',
            expect.objectContaining({
                input: { kind: 'file', path: '/templates/other.ini' },
                options: expect.objectContaining({ secretForms: {} }),
            }),
        );
        expect(getBlockBoxes()[0].getAttribute('aria-checked')).toBe('true');
        expect(getBlockBoxes()[1].getAttribute('aria-checked')).toBe('true');
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.checked).toBe(false);
    });

    it('resets secretForms, selectedBlocks, and keptRelaxed when choosing another file', async () => {
        vi.mocked(invoke).mockImplementation(async (cmd, args) => {
            if (cmd !== 'preview_configuration_template') throw new Error(`unexpected ${cmd}`);
            const payload = args as { options: { secretForms?: Record<string, string> } };
            const alphaForm = payload.options.secretForms?.alpha ?? null;
            return {
                rows: [
                    {
                        fileIndex: 0, blockName: 'alpha', targetName: 'alpha', startLine: 1,
                        protocol: 'webdav', providerId: null, host: 'dav.example.test', port: 443, username: 'alice',
                        hasCredential: alphaForm !== null, hasSecretInput: true, looksObscured: false,
                        secretForm: alphaForm, needsSecretFormChoice: alphaForm === null,
                        existingMatchId: null, existingMatchName: null, action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: alphaForm !== null ? 'newCredential' : 'none',
                        secretNote: null,
                    },
                    {
                        fileIndex: 1, blockName: 'beta', targetName: 'beta', startLine: 5,
                        protocol: 'sftp', providerId: null, host: 'beta.example.test', port: 22, username: 'bob',
                        hasCredential: false, hasSecretInput: false, looksObscured: false,
                        secretForm: null, needsSecretFormChoice: false,
                        existingMatchId: null, existingMatchName: null, action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: 'none', secretNote: null,
                    },
                ],
                skipped: [], warnings: [], parseIssues: [],
                security: [{ fileIndex: 0, name: 'alpha', reset: [{ key: 'verifyCert', value: 'false' }], kept: [] }],
                secretChoiceBlocks: ['alpha'], totalBlocks: 2, capped: false,
                sourceLabel: 'file', digestSha256: 'd', planDigestSha256: 'p',
            };
        });

        await act(async () => {
            root.render(
                createElement(
                    ActivityLogProvider,
                    null,
                    createElement(ConfigurationTemplatePanel, {
                        onProfilesStored: vi.fn(),
                        onBack: vi.fn(),
                        onClose: vi.fn(),
                    }),
                ),
            );
        });

        vi.mocked(pickFile).mockResolvedValueOnce('/templates/first.ini');
        await act(async () => { host.querySelector<HTMLButtonElement>('[data-testid="template-tab-file"]')!.click(); });
        await act(async () => { host.querySelector<HTMLButtonElement>('[data-testid="template-choose-file"]')!.click(); });
        await flush();

        const getBlockBoxes = () => Array.from(host.querySelectorAll<HTMLElement>('[role="checkbox"]'));

        await act(async () => {
            host.querySelector<HTMLInputElement>('[data-testid="template-secret-choice-alpha"] input[type="radio"]')!.click();
            getBlockBoxes()[0].click();
            host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.click();
        });
        await flush();

        // Pick another file.
        vi.mocked(pickFile).mockResolvedValueOnce('/templates/second.ini');
        const changeFileBtn = Array.from(host.querySelectorAll<HTMLButtonElement>('button'))
            .find(b => b.textContent?.includes('settings.importChangeFile'))!;
        await act(async () => { changeFileBtn.click(); });
        await flush();

        expect(vi.mocked(invoke)).toHaveBeenLastCalledWith(
            'preview_configuration_template',
            expect.objectContaining({
                input: { kind: 'file', path: '/templates/second.ini' },
                options: expect.objectContaining({ secretForms: {} }),
            }),
        );
        expect(getBlockBoxes()[0].getAttribute('aria-checked')).toBe('true');
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.checked).toBe(false);
    });

    it('resets secretForms, selectedBlocks, and keptRelaxed when clicking Example', async () => {
        vi.mocked(invoke).mockImplementation(async (cmd, args) => {
            if (cmd !== 'preview_configuration_template') throw new Error(`unexpected ${cmd}`);
            const payload = args as { options: { secretForms?: Record<string, string> } };
            const alphaForm = payload.options.secretForms?.alpha ?? null;
            return {
                rows: [
                    {
                        fileIndex: 0, blockName: 'alpha', targetName: 'alpha', startLine: 1,
                        protocol: 'webdav', providerId: null, host: 'dav.example.test', port: 443, username: 'alice',
                        hasCredential: alphaForm !== null, hasSecretInput: true, looksObscured: false,
                        secretForm: alphaForm, needsSecretFormChoice: alphaForm === null,
                        existingMatchId: null, existingMatchName: null, action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: alphaForm !== null ? 'newCredential' : 'none',
                        secretNote: null,
                    },
                    {
                        fileIndex: 1, blockName: 'beta', targetName: 'beta', startLine: 5,
                        protocol: 'sftp', providerId: null, host: 'beta.example.test', port: 22, username: 'bob',
                        hasCredential: false, hasSecretInput: false, looksObscured: false,
                        secretForm: null, needsSecretFormChoice: false,
                        existingMatchId: null, existingMatchName: null, action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: 'none', secretNote: null,
                    },
                ],
                skipped: [], warnings: [], parseIssues: [],
                security: [{ fileIndex: 0, name: 'alpha', reset: [{ key: 'verifyCert', value: 'false' }], kept: [] }],
                secretChoiceBlocks: ['alpha'], totalBlocks: 2, capped: false,
                sourceLabel: 'pasted', digestSha256: 'd', planDigestSha256: 'p',
            };
        });

        await act(async () => {
            root.render(
                createElement(
                    ActivityLogProvider,
                    null,
                    createElement(ConfigurationTemplatePanel, {
                        onProfilesStored: vi.fn(),
                        onBack: vi.fn(),
                        onClose: vi.fn(),
                    }),
                ),
            );
        });

        const textarea = host.querySelector<HTMLTextAreaElement>('[data-testid="template-textarea"]')!;
        const setInput = async (value: string) => {
            await act(async () => {
                Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(textarea, value);
                textarea.dispatchEvent(new Event('input', { bubbles: true }));
            });
            await act(async () => { vi.advanceTimersByTime(300); });
            await flush();
        };
        const getBlockBoxes = () => Array.from(host.querySelectorAll<HTMLElement>('[role="checkbox"]'));

        await setInput('[alpha]\ntype = webdav\nurl = https://dav.example.test\npass = x\n[beta]\ntype = sftp\nhost = b\n');
        await act(async () => {
            host.querySelector<HTMLInputElement>('[data-testid="template-secret-choice-alpha"] input[type="radio"]')!.click();
            getBlockBoxes()[0].click();
            host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.click();
        });
        await flush();

        await act(async () => {
            host.querySelector<HTMLButtonElement>('[data-testid="template-show-example"]')!.click();
        });
        await act(async () => { vi.advanceTimersByTime(300); });
        await flush();
        expect(vi.mocked(invoke)).toHaveBeenLastCalledWith(
            'preview_configuration_template',
            expect.objectContaining({ options: expect.objectContaining({ secretForms: {} }) }),
        );
        expect(getBlockBoxes()[0].getAttribute('aria-checked')).toBe('true');
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.checked).toBe(false);
    });

    it('resets secretForms, selectedBlocks, and keptRelaxed when clicking Clear', async () => {
        vi.mocked(invoke).mockImplementation(async (cmd, args) => {
            if (cmd !== 'preview_configuration_template') throw new Error(`unexpected ${cmd}`);
            const payload = args as { options: { secretForms?: Record<string, string> } };
            const alphaForm = payload.options.secretForms?.alpha ?? null;
            return {
                rows: [
                    {
                        fileIndex: 0, blockName: 'alpha', targetName: 'alpha', startLine: 1,
                        protocol: 'webdav', providerId: null, host: 'dav.example.test', port: 443, username: 'alice',
                        hasCredential: alphaForm !== null, hasSecretInput: true, looksObscured: false,
                        secretForm: alphaForm, needsSecretFormChoice: alphaForm === null,
                        existingMatchId: null, existingMatchName: null, action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: alphaForm !== null ? 'newCredential' : 'none',
                        secretNote: null,
                    },
                    {
                        fileIndex: 1, blockName: 'beta', targetName: 'beta', startLine: 5,
                        protocol: 'sftp', providerId: null, host: 'beta.example.test', port: 22, username: 'bob',
                        hasCredential: false, hasSecretInput: false, looksObscured: false,
                        secretForm: null, needsSecretFormChoice: false,
                        existingMatchId: null, existingMatchName: null, action: 'add',
                        duplicateInTemplate: false,
                        secretDisposition: 'none', secretNote: null,
                    },
                ],
                skipped: [], warnings: [], parseIssues: [],
                security: [{ fileIndex: 0, name: 'alpha', reset: [{ key: 'verifyCert', value: 'false' }], kept: [] }],
                secretChoiceBlocks: ['alpha'], totalBlocks: 2, capped: false,
                sourceLabel: 'pasted', digestSha256: 'd', planDigestSha256: 'p',
            };
        });

        await act(async () => {
            root.render(
                createElement(
                    ActivityLogProvider,
                    null,
                    createElement(ConfigurationTemplatePanel, {
                        onProfilesStored: vi.fn(),
                        onBack: vi.fn(),
                        onClose: vi.fn(),
                    }),
                ),
            );
        });

        const textarea = host.querySelector<HTMLTextAreaElement>('[data-testid="template-textarea"]')!;
        const setInput = async (value: string) => {
            await act(async () => {
                Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(textarea, value);
                textarea.dispatchEvent(new Event('input', { bubbles: true }));
            });
            await act(async () => { vi.advanceTimersByTime(300); });
            await flush();
        };
        const getBlockBoxes = () => Array.from(host.querySelectorAll<HTMLElement>('[role="checkbox"]'));

        await setInput('[alpha]\ntype = webdav\nurl = https://dav.example.test\npass = x\n[beta]\ntype = sftp\nhost = b\n');
        await act(async () => {
            host.querySelector<HTMLInputElement>('[data-testid="template-secret-choice-alpha"] input[type="radio"]')!.click();
            getBlockBoxes()[0].click();
            host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.click();
        });
        await flush();

        const clearBtn = Array.from(host.querySelectorAll<HTMLButtonElement>('button'))
            .find(b => b.textContent?.includes('settings.templateClear'))!;
        await act(async () => { clearBtn.click(); });
        await flush();

        await setInput('[alpha]\ntype = webdav\nurl = https://dav.example.test\npass = y\n[beta]\ntype = sftp\nhost = b\n');
        expect(vi.mocked(invoke)).toHaveBeenLastCalledWith(
            'preview_configuration_template',
            expect.objectContaining({ options: expect.objectContaining({ secretForms: {} }) }),
        );
        expect(getBlockBoxes()[0].getAttribute('aria-checked')).toBe('true');
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-review"] input[type="checkbox"]')!.checked).toBe(false);
    });

    it('uses typed duplicateInTemplate boolean rather than secretNote or existingMatchId to label skipped rows', async () => {
        vi.mocked(invoke).mockImplementation(async (cmd) => {
            if (cmd !== 'preview_configuration_template') throw new Error(`unexpected ${cmd}`);
            return {
                rows: [
                    {
                        fileIndex: 0, blockName: 'dup-row', targetName: 'dup-row', startLine: 1,
                        protocol: 'sftp', providerId: null, host: 'dup.example.test', port: 22, username: 'alice',
                        hasCredential: false, hasSecretInput: false, looksObscured: false,
                        secretForm: null, needsSecretFormChoice: false,
                        existingMatchId: 'srv_existing', existingMatchName: 'dup-row', action: 'skip',
                        duplicateInTemplate: true,
                        secretDisposition: 'none', secretNote: null,
                    },
                    {
                        fileIndex: 1, blockName: 'existing-row', targetName: 'existing-row', startLine: 5,
                        protocol: 'sftp', providerId: null, host: 'old.example.test', port: 22, username: 'bob',
                        hasCredential: false, hasSecretInput: false, looksObscured: false,
                        secretForm: null, needsSecretFormChoice: false,
                        existingMatchId: null, existingMatchName: null, action: 'skip',
                        duplicateInTemplate: false,
                        secretDisposition: 'none', secretNote: 'duplicate in template',
                    },
                ],
                skipped: [], warnings: [], parseIssues: [], security: [],
                secretChoiceBlocks: [], totalBlocks: 2, capped: false,
                sourceLabel: 'pasted', digestSha256: 'd', planDigestSha256: 'p',
            };
        });

        await act(async () => {
            root.render(
                createElement(
                    ActivityLogProvider,
                    null,
                    createElement(ConfigurationTemplatePanel, {
                        onProfilesStored: vi.fn(),
                        onBack: vi.fn(),
                        onClose: vi.fn(),
                    }),
                ),
            );
        });

        const textarea = host.querySelector<HTMLTextAreaElement>('[data-testid="template-textarea"]')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(
                textarea,
                '[dup-row]\ntype = sftp\nhost = dup.example.test\n[existing-row]\ntype = sftp\nhost = old.example.test\n',
            );
            textarea.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => { vi.advanceTimersByTime(300); });
        await flush();

        const rows = Array.from(host.querySelectorAll<HTMLElement>('.divide-y > div'));
        expect(rows).toHaveLength(2);
        expect(rows[0].textContent).toContain('settings.templateDuplicateInTemplate');
        expect(rows[1].textContent).toContain('settings.templateSecretNone');
        expect(rows[1].textContent).not.toContain('settings.templateDuplicateInTemplate');
    });
});

describe('ConfigurationTemplatePanel review state across previews', () => {
    type Block = { name: string; relaxed?: boolean; secret?: boolean };
    /** A preview report of `blocks`, in that order; `forms` are the chosen
     *  secret forms. */
    const report = (blocks: Block[], forms: Record<string, string> = {}) => ({
        rows: blocks.map((block, fileIndex) => ({
            fileIndex, blockName: block.name, targetName: block.name, startLine: 1 + fileIndex * 4,
            protocol: 'webdav', providerId: null, host: `${block.name}.example.com`, port: 443, username: 'u',
            hasCredential: false, hasSecretInput: !!block.secret, looksObscured: false,
            secretForm: forms[block.name] ?? null, needsSecretFormChoice: !!block.secret && !forms[block.name],
            existingMatchId: null, existingMatchName: null, action: 'add', duplicateInTemplate: false,
            secretDisposition: 'none', secretNote: '',
        })),
        skipped: [], warnings: [], parseIssues: [],
        security: blocks.flatMap((block, fileIndex) => block.relaxed
            ? [{ fileIndex, name: block.name, reset: [{ key: 'verifyCert', value: 'false' }], kept: [] }]
            : []),
        secretChoiceBlocks: blocks.filter(block => block.secret).map(block => block.name),
        totalBlocks: blocks.length, capped: false, sourceLabel: 'template',
        digestSha256: 'd', planDigestSha256: 'p',
    });
    const importResult = {
        applied: [], skippedExisting: [], skippedUnselected: [], skippedInvalid: [], warnings: [], parseIssues: [],
        security: [], oauthAppsKept: [], sharedSecretsFailed: [], stored: [], digestSha256: 'd',
    };

    async function renderPanel(onClose = vi.fn()) {
        await act(async () => {
            root.render(createElement(ActivityLogProvider, null, createElement(ConfigurationTemplatePanel, {
                onProfilesStored: vi.fn(), onBack: vi.fn(), onClose,
            })));
        });
        return onClose;
    }
    async function type(text: string) {
        const textarea = host.querySelector<HTMLTextAreaElement>('[data-testid="template-textarea"]')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(textarea, text);
            textarea.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => { vi.advanceTimersByTime(300); });
        await flush();
    }
    const buttonWithText = (text: string) =>
        [...host.querySelectorAll('button')].find(button => button.textContent?.includes(text))!;
    const lastImportOptions = () => vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'import_configuration_template').slice(-1)[0]?.[1] as
        { options: { selectedBlocks: string[]; keepRelaxed: { fileIndex: number; key: string }[] } } | undefined;

    it('keeps an explicit None through the preview a secret form change runs', async () => {
        vi.mocked(invoke).mockImplementation(async (cmd, args) => {
            if (cmd === 'preview_configuration_template') {
                const forms = (args as { options: { secretForms?: Record<string, string> } }).options.secretForms ?? {};
                return report([{ name: 'alpha', secret: true }, { name: 'beta' }], forms);
            }
            throw new Error(`unexpected command: ${cmd}`);
        });
        await renderPanel();
        await type('[alpha]\n[beta]\n');
        await act(async () => { buttonWithText('settings.templateSelectNone').click(); });
        const radios = host.querySelectorAll<HTMLInputElement>('[data-testid="template-secret-choice-alpha"] input[type="radio"]');
        await act(async () => { radios[0].click(); });
        await flush();
        expect(vi.mocked(invoke)).toHaveBeenLastCalledWith('preview_configuration_template', expect.objectContaining({
            options: expect.objectContaining({ secretForms: { alpha: 'clear' } }),
        }));
        expect(host.querySelector<HTMLButtonElement>('[data-testid="template-import-submit"]')!.disabled).toBe(true);
    });

    it("keeps a consent on alpha's option when the blocks are reordered, and never moves it to beta", async () => {
        let order: Block[] = [{ name: 'alpha', relaxed: true }, { name: 'beta', relaxed: true }];
        vi.mocked(invoke).mockImplementation(async (cmd) => {
            if (cmd === 'preview_configuration_template') return report(order);
            if (cmd === 'import_configuration_template') return importResult;
            throw new Error(`unexpected command: ${cmd}`);
        });
        await renderPanel();
        await type('[alpha]\n[beta]\n');
        await act(async () => { host.querySelector<HTMLInputElement>('[data-testid="import-security-keep-0:verifyCert"]')!.click(); });
        order = [{ name: 'beta', relaxed: true }, { name: 'alpha', relaxed: true }];
        await type('[beta]\n[alpha]\n');
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-keep-0:verifyCert"]')!.checked).toBe(false);
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-keep-1:verifyCert"]')!.checked).toBe(true);
        await act(async () => { host.querySelector<HTMLButtonElement>('[data-testid="template-import-submit"]')!.click(); });
        await flush();
        expect(lastImportOptions()!.options.keepRelaxed).toEqual([{ fileIndex: 1, key: 'verifyCert' }]);
    });

    it('drops the consent when the reviewed value changes', async () => {
        let value = 'false';
        vi.mocked(invoke).mockImplementation(async (cmd) => {
            if (cmd === 'preview_configuration_template') {
                const r = report([{ name: 'alpha', relaxed: true }]);
                r.security[0].reset[0].value = value;
                return r;
            }
            throw new Error(`unexpected command: ${cmd}`);
        });
        await renderPanel();
        await type('[alpha]\n');
        await act(async () => { host.querySelector<HTMLInputElement>('[data-testid="import-security-keep-0:verifyCert"]')!.click(); });
        value = 'no';
        await type('[alpha]\nverifyCert = no\n');
        expect(host.querySelector<HTMLInputElement>('[data-testid="import-security-keep-0:verifyCert"]')!.checked).toBe(false);
    });

    it('names each profile checkbox by the profile it imports', async () => {
        vi.mocked(invoke).mockImplementation(async (cmd) => {
            if (cmd === 'preview_configuration_template') return report([{ name: 'alpha' }, { name: 'beta' }]);
            throw new Error(`unexpected command: ${cmd}`);
        });
        await renderPanel();
        await type('[alpha]\n[beta]\n');
        // The test translator renders `key:params`: the label names the target.
        const names = ['alpha', 'beta'].map(name =>
            host.querySelector(`[role="checkbox"][aria-label="settings.templateSelectProfile:${name}"]`));
        expect(names.every(Boolean)).toBe(true);
        expect(names[0]).not.toBe(names[1]);
    });

    it('does not close the panel when the source changes before the first import resolves', async () => {
        let resolveImport!: (value: unknown) => void;
        vi.mocked(invoke).mockImplementation(async (cmd) => {
            if (cmd === 'preview_configuration_template') return report([{ name: 'alpha' }]);
            if (cmd === 'import_configuration_template') return new Promise(resolve => { resolveImport = resolve; });
            throw new Error(`unexpected command: ${cmd}`);
        });
        const onClose = await renderPanel();
        await type('[alpha]\n');
        await act(async () => { host.querySelector<HTMLButtonElement>('[data-testid="template-import-submit"]')!.click(); });
        // The source changes while the first import is still pending.
        await type('[alpha]\nport = 8443\n');
        await act(async () => { resolveImport(importResult); });
        await flush();
        await act(async () => { vi.advanceTimersByTime(3000); });
        expect(onClose).not.toHaveBeenCalled();
    });

    it('does not let an earlier import close the dialog while a later one is pending', async () => {
        let imports = 0;
        vi.mocked(invoke).mockImplementation(async (cmd) => {
            if (cmd === 'preview_configuration_template') return report([{ name: 'alpha' }]);
            if (cmd === 'import_configuration_template') {
                imports += 1;
                if (imports === 1) return importResult;
                return new Promise(() => undefined);
            }
            throw new Error(`unexpected command: ${cmd}`);
        });
        const onClose = await renderPanel();
        await type('[alpha]\n');
        const submit = () => host.querySelector<HTMLButtonElement>('[data-testid="template-import-submit"]')!;
        await act(async () => { submit().click(); });
        await flush();
        // A new source inside the close delay, then a second import.
        await type('[alpha]\nport = 8443\n');
        await act(async () => { submit().click(); });
        await flush();
        await act(async () => { vi.advanceTimersByTime(3000); });
        expect(imports).toBe(2);
        expect(onClose).not.toHaveBeenCalled();
    });

    it('keeps resolved secretForms for import after preview drops answered blocks from secretChoiceBlocks', async () => {
        let importedSecretForms: Record<string, string> | undefined;
        vi.mocked(invoke).mockImplementation(async (cmd, args) => {
            if (cmd === 'preview_configuration_template') {
                const payload = args as { options: { secretForms?: Record<string, string> } };
                const alphaForm = (payload.options.secretForms?.alpha ?? null) as 'clear' | 'obscured' | null;
                return {
                    rows: [
                        {
                            fileIndex: 0, blockName: 'alpha', targetName: 'alpha', startLine: 1,
                            protocol: 'sftp', providerId: null, host: 'alpha.example.test', port: 22, username: 'alice',
                            hasCredential: alphaForm !== null, hasSecretInput: true, looksObscured: false,
                            secretForm: alphaForm, needsSecretFormChoice: alphaForm === null,
                            existingMatchId: null, existingMatchName: null, action: 'add',
                            duplicateInTemplate: false,
                            secretDisposition: alphaForm !== null ? 'newCredential' : 'none',
                            secretNote: null,
                        },
                    ],
                    skipped: [], warnings: [], parseIssues: [], security: [],
                    // Real backend removes answered blocks from secretChoiceBlocks once resolved.
                    secretChoiceBlocks: alphaForm === null ? ['alpha'] : [],
                    totalBlocks: 1, capped: false, sourceLabel: 'paste',
                    digestSha256: 'digest-alpha', planDigestSha256: 'plan-alpha',
                };
            }
            if (cmd === 'import_configuration_template') {
                const payload = args as { options: { secretForms?: Record<string, string> } };
                importedSecretForms = payload.options.secretForms;
                return importResult;
            }
            throw new Error(`unexpected command: ${cmd}`);
        });

        await renderPanel();
        await type('[alpha]\ntype = sftp\nhost = alpha.example.test\nuser = alice\npass = PlainPw!\n');

        const clearRadio = host.querySelector<HTMLInputElement>(
            '[data-testid="template-secret-choice-alpha"] input[type="radio"]',
        )!;
        await act(async () => { clearRadio.click(); });
        await flush();

        // Updated preview now has secretChoiceBlocks: [], yet Clear choice must still be sent on import.
        await act(async () => {
            host.querySelector<HTMLButtonElement>('[data-testid="template-import-submit"]')!.click();
        });
        await flush();

        expect(importedSecretForms).toEqual({ alpha: 'clear' });
    });
});
