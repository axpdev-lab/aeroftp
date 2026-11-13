// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../types';
import type { EditState } from './Preview/types';
import type { FileProperties } from './Dialogs';

type Handler = (args?: Record<string, unknown>) => Promise<unknown>;

const mocks = vi.hoisted(() => ({
    handlers: {} as Record<string, Handler>,
    invoke: vi.fn(),
    pickSave: vi.fn(),
    peerShareStart: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock('@tauri-apps/api/path', () => ({ tempDir: vi.fn(async () => '/sentinel/tmp/') }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn(async () => undefined) }));
vi.mock('@tauri-apps/api/webview', () => ({
    getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => () => undefined) }),
}));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key,
    translate: (key: string) => key,
    useI18n: () => ({ t: (key: string) => key, language: 'en' }),
}));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../hooks/useTauriListener', () => ({ useTauriListener: () => undefined }));
vi.mock('../hooks/useHumanizedLog', () => {
    const log = { logRaw: () => 'log-1', updateEntry: () => undefined };
    return { useHumanizedLog: () => log };
});
vi.mock('../utils/clipboard', () => ({ copyText: vi.fn(async () => undefined) }));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(async () => null), pickSave: mocks.pickSave }));
vi.mock('../utils/aeroShare', async (original) => ({
    ...(await original<typeof import('../utils/aeroShare')>()),
    peerIdentityGet: vi.fn(async () => ({ afid: 'afid-sentinel-me', created: false })),
    peerFriendsList: vi.fn(async () => []),
    peerShareStart: mocks.peerShareStart,
    // No profile store: the dialog only needs the call to resolve.
    upsertFriendProfile: vi.fn(async () => ({ id: 'peer_sentinel' })),
}));

import { CompressDialog, type CompressResult } from './CompressDialog';
import { ArchiveBrowser } from './ArchiveBrowser';
import { ArchivePasswordDialog, MasterPasswordSetupDialog, PropertiesDialog, MultiFilePropertiesDialog } from './Dialogs';
import { ExportImportDialog } from './ExportImportDialog';
import CryptomatorCreateDialog from './CryptomatorCreateDialog';
import { AeroShareDialog } from './AeroShare/AeroShareDialog';
import { FilenNotesPanel } from './FilenNotesPanel';
import { ImageSaveDialog } from './Preview/viewers/ImageSaveDialog';
import { UniversalPreview } from './Preview/UniversalPreview';
import { GitHubWriteModeIndicator } from './GitHubWriteModeIndicator';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: unknown) => void;
    const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
    return { promise, resolve, reject };
}

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
    mocks.handlers = {};
    mocks.invoke.mockReset().mockImplementation((cmd: string, args?: Record<string, unknown>) =>
        mocks.handlers[cmd] ? mocks.handlers[cmd](args) : Promise.resolve(null));
    mocks.pickSave.mockReset().mockResolvedValue('/sentinel/out/target.bin');
    mocks.peerShareStart.mockReset();
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.clearAllMocks();
});

const flush = () => act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const render = async (node: React.ReactNode) => {
    await act(async () => root.render(<>{node}</>));
    await flush();
};
// The outer element is the backdrop, unless it holds a separate backdrop layer first.
const backdrop = () => {
    const outer = host.firstElementChild as HTMLElement;
    return (outer.querySelector(':scope > .absolute.inset-0') as HTMLElement | null) ?? outer;
};
const clickBackdrop = () => act(async () => { backdrop().dispatchEvent(new MouseEvent('click', { bubbles: true })); });
// From an element, as a real key press: it reaches React handlers and window listeners alike.
const escape = (from: Element = document.body) => act(async () => {
    from.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
});
const buttons = () => [...host.querySelectorAll('button')];
const findButton = (label: string) => buttons().find(b =>
    b.textContent?.trim() === label || b.getAttribute('aria-label') === label || b.getAttribute('title') === label);
const button = (label: string) => {
    const found = findButton(label);
    if (!found) throw new Error(`no button "${label}"`);
    return found;
};
/** A button whose text starts with `prefix` (labels followed by a count or a format). */
const buttonStartingWith = (prefix: string) => {
    const found = buttons().find(b => b.textContent?.trim().startsWith(prefix));
    if (!found) throw new Error(`no button starting with "${prefix}"`);
    return found;
};
const press = (el: HTMLElement) => act(async () => { el.click(); });
const click = (label: string) => press(button(label));
const xButton = () => {
    const found = buttons().find(b => b.querySelector('svg.lucide-x'));
    if (!found) throw new Error('no X button');
    return found;
};
const clickX = () => press(xButton());
const confirmOpen = () => host.querySelector('[role="alertdialog"]') !== null;
const input = (selector: string) => {
    const found = host.querySelector(selector) as HTMLInputElement | null;
    if (!found) throw new Error(`no input "${selector}"`);
    return found;
};
const typeInto = (el: HTMLInputElement | HTMLTextAreaElement, value: string) => act(async () => {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value')!.set!.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
});

/** The X asked with the "typed input" copy; keep editing leaves it open, discard closes. */
const expectDirtyConfirmThenDiscard = async (onClose: ReturnType<typeof vi.fn>, openConfirm: () => Promise<void> = clickX) => {
    await openConfirm();
    expect(onClose).not.toHaveBeenCalled();
    expect(confirmOpen()).toBe(true);
    expect(host.textContent).toContain('modalGuard.dirtyBody');
    await click('modalGuard.keepEditing');
    expect(confirmOpen()).toBe(false);
    expect(onClose).not.toHaveBeenCalled();
    await openConfirm();
    await click('modalGuard.discardAndClose');
    expect(onClose).toHaveBeenCalledTimes(1);
};

/** The X asked with the "keeps running" copy: there is no stop, only wait or close anyway. */
const expectBusyConfirmThenCloseAnyway = async (onClose: ReturnType<typeof vi.fn>, openConfirm: () => Promise<void> = clickX) => {
    await openConfirm();
    expect(onClose).not.toHaveBeenCalled();
    expect(confirmOpen()).toBe(true);
    expect(host.textContent).toContain('modalGuard.busyBodyKeepsRunning');
    expect(findButton('modalGuard.stopAndClose')).toBeFalsy();
    await click('modalGuard.wait');
    expect(confirmOpen()).toBe(false);
    expect(onClose).not.toHaveBeenCalled();
    await openConfirm();
    await click('modalGuard.closeAnyway');
    expect(onClose).toHaveBeenCalledTimes(1);
};

// ─── CompressDialog ───────────────────────────────────────────────────────

describe('CompressDialog (Discussion #1527)', () => {
    const files = [{ name: 'sentinel-a.txt', path: '/sentinel/sentinel-a.txt', size: 0, isDir: false }];
    const open = async (onClose: () => void, onConfirm: (o: unknown) => Promise<CompressResult | void> = async () => undefined) => {
        await render(<CompressDialog files={files} defaultName="sentinel-archive" outputDir="/sentinel/out" onConfirm={onConfirm} onClose={onClose} />);
    };
    const passwordInput = () => input('input[placeholder="compress.passwordHint"]');
    const confirmInput = () => input('input[aria-label="password.confirm"]');
    const nameInput = () => input('input[type="text"]');
    const formatCard = (label: string) => {
        const found = buttons().find(b => b.querySelector('span.font-semibold')?.textContent === label);
        if (!found) throw new Error(`no format card "${label}"`);
        return found;
    };
    const startCompress = async (onClose: () => void) => {
        const run = deferred<CompressResult | void>();
        const onConfirm = vi.fn(() => run.promise);
        await open(onClose, onConfirm);
        return { run, onConfirm };
    };

    it('closes from the backdrop and the X at once with every option at its default', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        onClose.mockClear();
        await clickX();
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
    });

    it('keeps a typed password from the backdrop, and the X offers to discard it', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps a typed confirmation from the backdrop after the password is cleared', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await typeInto(confirmInput(), 'sentinel-pass-1');
        await typeInto(passwordInput(), '');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await clickX();
        expect(confirmOpen()).toBe(true);
    });

    const optionChanges: [string, () => Promise<void>][] = [
        ['the archive name', () => typeInto(nameInput(), 'sentinel-renamed')],
        ['the format', () => press(formatCard('TAR'))],
        ['the compression level', () => click('compress.fastest')],
        ['a 7z advanced option', async () => {
            await press(formatCard('7z'));
            await click('compress.advanced');
            await click('PPMd');
            // Back to the default format: only the advanced choice is left changed.
            await press(formatCard('ZIP'));
        }],
    ];
    it.each(optionChanges)('keeps a changed option (%s) from the backdrop, and the X offers to discard it', async (_name, change) => {
        const onClose = vi.fn();
        await open(onClose);
        await change();
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps a running compression from the backdrop, and the X offers to close anyway, not to stop', async () => {
        const onClose = vi.fn();
        const { run, onConfirm } = await startCompress(onClose);
        await press(buttonStartingWith('compress.compress'));
        expect(onConfirm).toHaveBeenCalledTimes(1);
        expect(host.textContent).toContain('compress.compressing');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectBusyConfirmThenCloseAnyway(onClose);
        await act(async () => run.resolve());
    });

    it('lifts the guard once a result is shown: the backdrop closes it even after a password', async () => {
        const onClose = vi.fn();
        const { run } = await startCompress(onClose);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await typeInto(confirmInput(), 'sentinel-pass-1');
        await press(buttonStartingWith('compress.compress'));
        await act(async () => run.resolve({ inputBytes: 2048, outputBytes: 1024 }));
        await flush();
        expect(findButton('compress.done')).toBeTruthy();
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
    });
});

// ─── ArchiveBrowser ───────────────────────────────────────────────────────

describe('ArchiveBrowser (Discussion #1527)', () => {
    const entry = { name: 'sentinel-a.txt', size: 4, compressedSize: 2, isDir: false, isEncrypted: true, modified: null };
    const passwordInput = () => input('input[placeholder="archive.enterPassword"]');
    const unlock = async (onClose: () => void) => {
        mocks.handlers.list_zip = async () => [entry];
        await render(<ArchiveBrowser archivePath="/sentinel/locked.zip" archiveType="zip" isEncrypted onClose={onClose} />);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await click('archive.unlock');
        await flush();
        expect(mocks.invoke).toHaveBeenCalledWith('list_zip', { archivePath: '/sentinel/locked.zip', password: 'sentinel-pass-1' });
    };

    it('keeps a password typed in the prompt from the backdrop, and the X offers to discard it', async () => {
        const onClose = vi.fn();
        await render(<ArchiveBrowser archivePath="/sentinel/locked.zip" archiveType="zip" isEncrypted onClose={onClose} />);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps an archive opened with a password from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        await unlock(onClose);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
    });

    it('keeps a running extraction from the backdrop, and the X offers to close anyway', async () => {
        const onClose = vi.fn();
        const extraction = deferred<unknown>();
        mocks.handlers.list_zip = async () => [{ ...entry, isEncrypted: false }];
        mocks.handlers.extract_zip_entry = () => extraction.promise;
        await render(<ArchiveBrowser archivePath="/sentinel/plain.zip" archiveType="zip" isEncrypted={false} onClose={onClose} />);
        await click('archive.extract');
        await flush();
        expect(mocks.invoke).toHaveBeenCalledWith('extract_zip_entry', expect.objectContaining({ entryName: 'sentinel-a.txt' }));
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectBusyConfirmThenCloseAnyway(onClose, () => click('common.close'));
        await act(async () => extraction.resolve(null));
    });

    it('closes from the backdrop on an unencrypted archive at rest', async () => {
        const onClose = vi.fn();
        mocks.handlers.list_zip = async () => [{ ...entry, isEncrypted: false }];
        await render(<ArchiveBrowser archivePath="/sentinel/plain.zip" archiveType="zip" isEncrypted={false} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

// ─── Dialogs/index.tsx ────────────────────────────────────────────────────

describe('ArchivePasswordDialog (Discussion #1527)', () => {
    const open = async (onClose: () => void, onSubmit: (p: string) => Promise<void> = async () => undefined) => {
        await render(<ArchivePasswordDialog archiveName="sentinel.7z" format="7z" cipher="AES-256" onSubmit={onSubmit} onClose={onClose} />);
    };
    const passwordInput = () => input('input[placeholder="contextMenu.enterArchivePassword"]');

    it('keeps a typed password from the backdrop, and the X offers to discard it', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose, () => click('common.close'));
    });

    it('keeps a password check in flight from the backdrop, with the X disabled', async () => {
        const onClose = vi.fn();
        const check = deferred<void>();
        await open(onClose, () => check.promise);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await click('contextMenu.decrypt');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        expect(button('common.close').disabled).toBe(true);
        await act(async () => check.resolve());
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('closes from the backdrop and the X at once with nothing typed', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        await click('common.close');
        expect(onClose).toHaveBeenCalledTimes(2);
        expect(confirmOpen()).toBe(false);
    });
});

describe('MasterPasswordSetupDialog (Discussion #1527)', () => {
    const fields = () => [...host.querySelectorAll('input[placeholder="••••••••"]')] as HTMLInputElement[];
    const open = async (onClose: () => void) => {
        await render(<MasterPasswordSetupDialog onComplete={() => undefined} onClose={onClose} />);
    };

    it('keeps typed passwords from the backdrop, and the X offers to discard them', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await typeInto(fields()[1], 'sentinel-pass-1');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps a setup in flight from the backdrop, and the X offers to close anyway', async () => {
        const onClose = vi.fn();
        const setup = deferred<unknown>();
        mocks.handlers.enable_master_password = () => setup.promise;
        await open(onClose);
        await typeInto(fields()[0], 'sentinel-pass-1');
        await typeInto(fields()[1], 'sentinel-pass-1');
        await click('masterPassword.enable');
        expect(mocks.invoke).toHaveBeenCalledWith('enable_master_password', expect.anything());
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectBusyConfirmThenCloseAnyway(onClose);
        await act(async () => setup.resolve(null));
    });

    it('closes from the backdrop with nothing typed', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

describe('PropertiesDialog (Discussion #1527)', () => {
    const file = (extra: Partial<FileProperties> = {}): FileProperties => ({
        name: 'sentinel-a.txt', path: '/sentinel/sentinel-a.txt', size: 4, is_dir: false, modified: null,
        permissions: 'private', isRemote: true, protocol: 'opendrive', ...extra,
    });

    it('keeps a privacy change in flight from the backdrop, and the X offers to close anyway', async () => {
        const onClose = vi.fn();
        const change = deferred<void>();
        const onPrivacyChange = vi.fn(() => change.promise);
        await render(<PropertiesDialog file={file()} onClose={onClose} initialTab="permissions" onPrivacyChange={onPrivacyChange} />);
        await press(host.querySelectorAll('input[name="opendrive-privacy"]')[1] as HTMLInputElement);
        await click('properties.applyPrivacy');
        expect(onPrivacyChange).toHaveBeenCalledWith('public');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectBusyConfirmThenCloseAnyway(onClose);
        await act(async () => change.resolve());
    });

    it('keeps a checksum being calculated from the backdrop, and the footer Close asks first', async () => {
        const onClose = vi.fn();
        await render(<PropertiesDialog file={file({ checksum: { calculating: true } })} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(confirmOpen()).toBe(true);
        expect(onClose).not.toHaveBeenCalled();
    });

    it('closes from the backdrop and the X at once at rest', async () => {
        const onClose = vi.fn();
        await render(<PropertiesDialog file={file()} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        await clickX();
        expect(onClose).toHaveBeenCalledTimes(2);
        expect(confirmOpen()).toBe(false);
    });
});

describe('MultiFilePropertiesDialog (Discussion #1527)', () => {
    const selection = {
        isRemote: true,
        protocol: 'opendrive',
        files: ['a', 'b'].map((n): FileProperties => ({
            name: `sentinel-${n}.txt`, path: `/sentinel/sentinel-${n}.txt`, size: 1, is_dir: false, modified: null,
            permissions: 'private', isRemote: true, protocol: 'opendrive',
        })),
    };

    it('keeps a privacy change in flight from the backdrop, and ESC and the X ask first', async () => {
        const onClose = vi.fn();
        const change = deferred<void>();
        await render(<MultiFilePropertiesDialog selection={selection} onClose={onClose} onPrivacyChange={() => change.promise} />);
        await click('properties.applyPrivacyAll');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await escape();
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        // A second ESC answers the confirm "wait".
        await escape();
        expect(confirmOpen()).toBe(false);
        expect(onClose).not.toHaveBeenCalled();
        await expectBusyConfirmThenCloseAnyway(onClose, () => click('common.close'));
        await act(async () => change.resolve());
    });

    it('closes from the backdrop and ESC at rest', async () => {
        const onClose = vi.fn();
        await render(<MultiFilePropertiesDialog selection={selection} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        await escape();
        expect(onClose).toHaveBeenCalledTimes(2);
    });
});

// ─── ExportImportDialog ───────────────────────────────────────────────────

describe('ExportImportDialog (Discussion #1527)', () => {
    const servers = [{ id: 'a', name: 'lab-a', host: 'a.example.com', port: 21, username: 'tester', protocol: 'ftp' }] as unknown as ServerProfile[];
    const open = async (onClose: () => void, initialMode?: 'export') => {
        await render(<ExportImportDialog servers={servers} onImport={() => undefined} onProfilesStored={() => undefined} onClose={onClose} initialMode={initialMode} />);
    };
    const typePasswords = async () => {
        await typeInto(input('input[placeholder="settings.encryptionPassword"]'), 'sentinel-pass-1');
        await typeInto(input('input[placeholder="settings.confirmPassword"]'), 'sentinel-pass-1');
    };

    it('keeps a typed export password from the backdrop, and the X offers to discard it', async () => {
        const onClose = vi.fn();
        await open(onClose, 'export');
        await typeInto(input('input[placeholder="settings.encryptionPassword"]'), 'sentinel-pass-1');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps an export in flight from the backdrop, and lifts the guard once it succeeded', async () => {
        const onClose = vi.fn();
        const run = deferred<unknown>();
        mocks.handlers.export_server_profiles = () => run.promise;
        await open(onClose, 'export');
        await typePasswords();
        await press(buttonStartingWith('settings.exportServers'));
        await flush();
        expect(mocks.invoke).toHaveBeenCalledWith('export_server_profiles', expect.objectContaining({ filePath: '/sentinel/out/target.bin' }));
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await clickX();
        expect(host.textContent).toContain('modalGuard.busyBodyKeepsRunning');
        expect(findButton('modalGuard.stopAndClose')).toBeFalsy();
        await click('modalGuard.wait');
        await act(async () => run.resolve(null));
        await flush();
        expect(host.textContent).toContain('settings.exportSuccess');
        // Done: the typed password no longer holds the dialog.
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('closes from the backdrop on the landing page', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

// ─── CryptomatorCreateDialog ──────────────────────────────────────────────

describe('CryptomatorCreateDialog (Discussion #1527)', () => {
    const open = async (onClose: () => void) => {
        await render(<CryptomatorCreateDialog outputDir="/sentinel/out" onClose={onClose} />);
    };
    const passwordInput = () => input('input[aria-label="common.password"]');
    const confirmInput = () => input('input[aria-label="cryptomator.confirmPassword"]');

    it('keeps a typed password from the backdrop, and ESC and the X offer to discard it', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await escape(passwordInput());
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        await click('modalGuard.keepEditing');
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps a vault being created from the backdrop, and the X and Cancel offer to close anyway', async () => {
        const onClose = vi.fn();
        const create = deferred<unknown>();
        mocks.handlers.cryptomator_create = () => create.promise;
        await open(onClose);
        await typeInto(passwordInput(), 'sentinel-pass-1');
        await typeInto(confirmInput(), 'sentinel-pass-1');
        await press(buttonStartingWith('common.create'));
        expect(mocks.invoke).toHaveBeenCalledWith('cryptomator_create', expect.objectContaining({ vaultPath: '/sentinel/out/cryptomator.defaultVaultName' }));
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.cancel');
        expect(confirmOpen()).toBe(true);
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.wait');
        await expectBusyConfirmThenCloseAnyway(onClose);
        await act(async () => create.resolve(null));
    });

    it('closes from the backdrop with no password typed', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

// ─── ImageSaveDialog ──────────────────────────────────────────────────────

describe('ImageSaveDialog (Discussion #1527)', () => {
    const editState: EditState = {
        crop: null, resize: null, rotation: 90, flipH: false, flipV: false,
        brightness: 0, contrast: 0, hue: 0, blur: 0, sharpen: 0, grayscale: false, invert: false,
    };
    const open = async (onClose: () => void) => {
        await render(<ImageSaveDialog isOpen filePath="/sentinel/img/sentinel.png" fileName="sentinel.png" editState={editState}
            originalDimensions={{ width: 4, height: 2 }} onSaved={() => undefined} onClose={onClose} />);
    };

    it('keeps a save in flight from the backdrop, and ESC and the X offer to close anyway', async () => {
        const onClose = vi.fn();
        const save = deferred<unknown>();
        mocks.handlers.process_image = () => save.promise;
        await open(onClose);
        await click('preview.image.edit.saveAsCopy');
        expect(mocks.invoke).toHaveBeenCalledWith('process_image', expect.objectContaining({ outputPath: '/sentinel/img/sentinel_edited.png' }));
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await escape();
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        await click('modalGuard.wait');
        await expectBusyConfirmThenCloseAnyway(onClose);
        await act(async () => save.resolve({ path: '/sentinel/img/sentinel_edited.png', width: 2, height: 4, size: 1, format: 'png' }));
    });

    it('closes from the backdrop and ESC at rest', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        await escape();
        expect(onClose).toHaveBeenCalledTimes(2);
    });

    it('takes ESC before the preview it was opened from', async () => {
        // The preview listens on document, before the dialog's window
        // listener: one ESC closed the dialog and asked to close the preview.
        const onPreviewClose = vi.fn();
        const onSaveClose = vi.fn();
        const file = { name: 'sentinel.bin', path: '/sentinel/sentinel.bin', size: 1, isRemote: false };
        const preview = <UniversalPreview isOpen file={file} onClose={onPreviewClose} />;
        await render(preview);
        await render(<>{preview}<ImageSaveDialog isOpen filePath="/sentinel/img/sentinel.png" fileName="sentinel.png" editState={editState}
            originalDimensions={{ width: 4, height: 2 }} onSaved={() => undefined} onClose={onSaveClose} /></>);
        await escape();
        expect(onSaveClose).toHaveBeenCalledTimes(1);
        expect(onPreviewClose).not.toHaveBeenCalled();
    });
});

// ─── AeroShareDialog ──────────────────────────────────────────────────────

describe('AeroShareDialog (Discussion #1527)', () => {
    const started = {
        namespace: 'ns-sentinel', ticket: 'ticket-sentinel', token: 'token-sentinel',
        link: 'aeroftp-share://v1/sentinel-link', drive_name: 'sentinel-drive',
    };
    const openShare = async (onClose: () => void) => {
        await render(<AeroShareDialog initialMode="share" prefillAfid="afid-sentinel-friend" prefillShareFolder="/sentinel/share" onClose={onClose} />);
    };

    it('keeps a share in flight from the backdrop, then keeps the share link while the X closes at once', async () => {
        const onClose = vi.fn();
        const run = deferred<typeof started>();
        mocks.peerShareStart.mockImplementation(() => run.promise);
        await openShare(onClose);
        await click('aeroShare.dialog.shareSubmit');
        expect(mocks.peerShareStart).toHaveBeenCalledWith(expect.objectContaining({ dir: '/sentinel/share', recipientAfid: 'afid-sentinel-friend' }));
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await clickX();
        expect(host.textContent).toContain('modalGuard.busyBodyKeepsRunning');
        expect(findButton('modalGuard.stopAndClose')).toBeFalsy();
        await click('modalGuard.wait');
        expect(onClose).not.toHaveBeenCalled();

        await act(async () => run.resolve(started));
        await flush();
        expect(host.textContent).toContain('aeroShare.dialog.shareReadyTitle');
        // The link still has to reach the receiver: a stray click keeps it.
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await clickX();
        expect(confirmOpen()).toBe(false);
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('keeps a pasted share link from the backdrop, and ESC and the X offer to discard it', async () => {
        const onClose = vi.fn();
        await render(<AeroShareDialog initialMode="receive" onClose={onClose} />);
        await typeInto(input('input[placeholder="aeroShare.dialog.linkPlaceholder"]'), 'aeroftp-share://v1/sentinel-link');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await escape();
        expect(confirmOpen()).toBe(true);
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.keepEditing');
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps a typed recipient from the backdrop', async () => {
        const onClose = vi.fn();
        await render(<AeroShareDialog initialMode="share" onClose={onClose} />);
        await typeInto(input('input[placeholder="aeroShare.dialog.recipientAfidLabel"]'), 'afid-sentinel-typed');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await clickX();
        expect(confirmOpen()).toBe(true);
    });

    it('keeps a changed share folder from the backdrop while the recipient stays untouched', async () => {
        const onClose = vi.fn();
        await openShare(onClose);
        const folderInput = button('aeroShare.dialog.chooseFolder')
            .parentElement!.querySelector('input') as HTMLInputElement;
        await typeInto(folderInput, '/sentinel/other');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('asks before the share success Close discards a dirty receive form', async () => {
        const onClose = vi.fn();
        const run = deferred<typeof started>();
        mocks.peerShareStart.mockImplementation(() => run.promise);
        await openShare(onClose);
        await click('aeroShare.dialog.receiveTab');
        await typeInto(input('input[placeholder="aeroShare.dialog.linkPlaceholder"]'), 'aeroftp-share://v1/sentinel-link');
        await click('aeroShare.dialog.shareTab');
        await click('aeroShare.dialog.shareSubmit');
        await act(async () => run.resolve(started));
        await flush();
        expect(host.textContent).toContain('aeroShare.dialog.shareReadyTitle');
        await click('aeroShare.dialog.close');
        expect(confirmOpen()).toBe(true);
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.keepEditing');
        expect(onClose).not.toHaveBeenCalled();
    });

    it('closes from the backdrop and ESC with nothing typed', async () => {
        const onClose = vi.fn();
        await openShare(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        await escape();
        expect(onClose).toHaveBeenCalledTimes(2);
    });
});

// ─── FilenNotesPanel ──────────────────────────────────────────────────────

describe('FilenNotesPanel (Discussion #1527)', () => {
    const note = {
        uuid: 'note-sentinel', title: 'sentinel-note', preview: 'sentinel body', noteType: 'text',
        favorite: false, pinned: false, trash: false, archive: false, createdTimestamp: 1, editedTimestamp: 1,
        tags: [], participants: [],
    };
    const openNote = async (onClose: () => void) => {
        mocks.handlers.filen_notes_list = async () => [note];
        mocks.handlers.filen_notes_tags_list = async () => [];
        mocks.handlers.filen_notes_get_content = async () => ({ content: 'sentinel body', preview: 'sentinel body', noteType: 'text', editedTimestamp: 1, editorId: 1 });
        await render(<FilenNotesPanel isOpen onClose={onClose} />);
        const row = [...host.querySelectorAll('div.cursor-pointer')].find(d => d.textContent?.includes('sentinel-note')) as HTMLElement;
        await press(row);
        await flush();
        expect(mocks.invoke).toHaveBeenCalledWith('filen_notes_get_content', { uuid: 'note-sentinel' });
    };
    const titleInput = () => input('input[placeholder="filenNotes.untitled"]');

    it('keeps an edited note from the backdrop, and the X offers to discard it', async () => {
        const onClose = vi.fn();
        await openNote(onClose);
        await typeInto(titleInput(), 'sentinel-note-edited');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectDirtyConfirmThenDiscard(onClose);
    });

    it('keeps the close guard up for a title edit after the content auto-save completes', async () => {
        const onClose = vi.fn();
        await openNote(onClose);
        vi.useFakeTimers();
        try {
            await typeInto(input('textarea'), 'sentinel body edited');
            await typeInto(titleInput(), 'sentinel-note-retitled');
            mocks.handlers.filen_notes_edit_content = async () => null;
            await act(async () => { await vi.advanceTimersByTimeAsync(2100); });
            expect(mocks.invoke).toHaveBeenCalledWith('filen_notes_edit_content', expect.objectContaining({ uuid: 'note-sentinel' }));
            expect(mocks.invoke).not.toHaveBeenCalledWith('filen_notes_edit_title', expect.anything());
            await clickX();
            expect(confirmOpen()).toBe(true);
            expect(onClose).not.toHaveBeenCalled();
        } finally {
            vi.useRealTimers();
        }
    });

    it('answers Escape on the close confirm with keep editing: no save, editor and edits stay', async () => {
        const onClose = vi.fn();
        await openNote(onClose);
        mocks.handlers.filen_notes_edit_content = async () => { throw new Error('sentinel save failure'); };
        await typeInto(titleInput(), 'sentinel-note-edited');
        await clickX();
        expect(confirmOpen()).toBe(true);
        await escape();
        expect(confirmOpen()).toBe(false);
        expect(onClose).not.toHaveBeenCalled();
        expect(input('input[placeholder="filenNotes.untitled"]').value).toBe('sentinel-note-edited');
        expect(mocks.invoke).not.toHaveBeenCalledWith('filen_notes_edit_content', expect.anything());
        expect(mocks.invoke).not.toHaveBeenCalledWith('filen_notes_edit_title', expect.anything());
    });

    it('keeps a save in flight from the backdrop, and the X offers to close anyway', async () => {
        const onClose = vi.fn();
        const save = deferred<unknown>();
        await openNote(onClose);
        mocks.handlers.filen_notes_edit_content = () => save.promise;
        await typeInto(titleInput(), 'sentinel-note-edited');
        await click('filenNotes.saveNow');
        expect(mocks.invoke).toHaveBeenCalledWith('filen_notes_edit_content', expect.objectContaining({ uuid: 'note-sentinel' }));
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await expectBusyConfirmThenCloseAnyway(onClose);
        await act(async () => save.resolve(null));
    });

    it('closes from the backdrop and ESC on the note list', async () => {
        const onClose = vi.fn();
        mocks.handlers.filen_notes_list = async () => [note];
        mocks.handlers.filen_notes_tags_list = async () => [];
        await render(<FilenNotesPanel isOpen onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        await escape();
        expect(onClose).toHaveBeenCalledTimes(2);
    });
});

// ─── GitHubWriteModeIndicator ─────────────────────────────────────────────

describe('GitHubWriteModeIndicator PR prompt (Discussion #1527)', () => {
    // The badge sits before the prompt, so the prompt's backdrop is looked up by class.
    const prompt = () => host.querySelector('.fixed.inset-0') as HTMLElement | null;
    const clickPromptBackdrop = () => act(async () => { prompt()!.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
    const titleInput = () => input('input[placeholder="github.prTitle"]');
    const open = async () => {
        await render(<GitHubWriteModeIndicator writeMode="branch" workingBranch="sentinel-branch" />);
        // The badge button carries the label as a title, the prompt's button as text.
        await press(buttons().find(b => b.getAttribute('title') === 'github.createPr')!);
        expect(prompt()).not.toBeNull();
    };

    it('keeps a request being created from the backdrop, and ESC offers to close anyway', async () => {
        const create = deferred<string>();
        mocks.handlers.github_create_pr = () => create.promise;
        await open();
        await typeInto(titleInput(), 'sentinel PR title');
        await press(buttons().find(b => b.textContent?.trim() === 'github.createPr')!);
        expect(mocks.invoke).toHaveBeenCalledWith('github_create_pr', { title: 'sentinel PR title', body: '' });
        await clickPromptBackdrop();
        expect(prompt()).not.toBeNull();
        await escape(titleInput());
        expect(confirmOpen()).toBe(true);
        expect(host.textContent).toContain('modalGuard.busyBodyKeepsRunning');
        expect(findButton('modalGuard.stopAndClose')).toBeFalsy();
        await click('modalGuard.closeAnyway');
        expect(prompt()).toBeNull();
        await act(async () => create.resolve(''));
    });

    it('hides the prompt from the backdrop and ESC when nothing runs', async () => {
        await open();
        await typeInto(titleInput(), 'sentinel PR title');
        await clickPromptBackdrop();
        expect(prompt()).toBeNull();
        await press(buttons().find(b => b.getAttribute('title') === 'github.createPr')!);
        // The typed title survives the hide.
        expect(titleInput().value).toBe('sentinel PR title');
        await escape(titleInput());
        expect(prompt()).toBeNull();
        expect(confirmOpen()).toBe(false);
    });
});
