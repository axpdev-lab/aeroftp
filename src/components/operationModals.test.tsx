// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../types';
import type { UnifiedTransferPlan } from '../utils/unifiedTransferPlanner';

type Handler = (args?: Record<string, unknown>) => Promise<unknown>;

const mocks = vi.hoisted(() => ({
    handlers: {} as Record<string, Handler>,
    invoke: vi.fn(),
    profiles: [] as unknown[],
    vaultState: {} as Record<string, unknown>,
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => undefined) }));
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
vi.mock('../hooks/useActivityLog', () => {
    const value = { log: () => undefined };
    return { useActivityLog: () => value };
});
vi.mock('../utils/clipboard', () => ({ copyText: vi.fn(async () => undefined) }));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(async () => '/sync/local-dir'), pickSave: vi.fn(async () => null) }));
vi.mock('../utils/serverProfileStore', () => ({ loadSavedServerProfiles: vi.fn(async () => mocks.profiles) }));
vi.mock('../utils/speedTest', async (original) => ({
    ...(await original<typeof import('../utils/speedTest')>()),
    // No keyring lookup and no host hashing: the dialog only needs a payload.
    buildSpeedTestConnection: vi.fn(async () => ({ protocol: 'ftp', server: 'ftp.example.com', username: 'tester', password: '' })),
    hashHost: vi.fn(async () => ''),
}));
vi.mock('./vault/useVaultState', () => ({
    useVaultState: () => mocks.vaultState,
    securityLevels: {},
    mapVaultError: (e: unknown) => String(e),
}));
vi.mock('./vault/VaultHome', () => ({ VaultHome: () => null }));
vi.mock('./vault/VaultCreate', () => ({ VaultCreate: () => null }));
vi.mock('./vault/VaultOpen', () => ({ VaultOpen: () => null }));
vi.mock('./vault/VaultBrowse', () => ({ VaultBrowse: () => null }));
vi.mock('./vault/VaultViewTabs', () => ({ VaultViewTabs: () => null }));
vi.mock('./vault/VaultRecentList', () => ({ VaultRecentList: () => null }));
vi.mock('./vault/VaultCreateReceipt', () => ({ VaultCreateReceipt: () => null }));
vi.mock('./vault/VaultReceipt', () => ({ VaultReceipt: () => null }));
vi.mock('./vault/ZipCompressionReport', () => ({ ZipCompressionReport: () => null }));

import { SpeedTestDialog } from './SpeedTestDialog';
import { useKeyboardShortcuts } from '../hooks/useKeyboardShortcuts';
import { buildSpeedTestConnection } from '../utils/speedTest';
import { isEscapeClaimed, useGuardedClose } from '../hooks/useGuardedClose';
import { TwoFactorPromptDialog } from './TwoFactorPromptDialog';
import { CrossProfilePanel } from './CrossProfile/CrossProfilePanel';
import { ShareLinkModal } from './ShareLinkModal';
import { TotpSetup } from './TotpSetup';
import { ServerHealthCheck } from './ServerHealthCheck';
import VaultSyncDialog from './VaultSyncDialog';
import { UnifiedTransferPlanDialog } from './UnifiedTransferPlanDialog';
import { ConfirmDialog, InputDialog } from './Dialogs';
import { VaultPanel } from './VaultPanel';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function deferred<T>() {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((res) => { resolve = res; });
    return { promise, resolve };
}

const server = (id: string): ServerProfile => ({
    id, name: `lab-${id}`, host: `${id}.example.com`, port: 21, username: 'tester', protocol: 'ftp',
}) as unknown as ServerProfile;

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
    mocks.handlers = {};
    mocks.profiles = [];
    mocks.vaultState = {};
    mocks.invoke.mockReset().mockImplementation((cmd: string, args?: Record<string, unknown>) =>
        mocks.handlers[cmd] ? mocks.handlers[cmd](args) : Promise.resolve(null));
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.clearAllMocks();
});

const render = async (node: React.ReactNode) => {
    await act(async () => root.render(<>{node}</>));
    await flush();
};
const flush = () => act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
// The outer element is the backdrop, unless it holds a separate backdrop layer first.
const backdrop = () => {
    const outer = host.firstElementChild as HTMLElement;
    return (outer.querySelector(':scope > .absolute.inset-0') as HTMLElement | null) ?? outer;
};
const clickBackdrop = () => act(async () => { backdrop().dispatchEvent(new MouseEvent('click', { bubbles: true })); });
// From an element, as a real key press: it reaches document and window listeners alike.
const escape = () => act(async () => {
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
});
const buttons = () => [...host.querySelectorAll('button')];
const findButton = (label: string) => buttons().find(b =>
    b.textContent?.trim() === label || b.getAttribute('aria-label') === label || b.getAttribute('title') === label);
const button = (label: string) => {
    const found = findButton(label);
    if (!found) throw new Error(`no button "${label}"`);
    return found;
};
const click = (label: string) => act(async () => { button(label).click(); });
const xButton = () => buttons().find(b => b.querySelector('svg.lucide-x'))!;
const confirmOpen = () => host.querySelector('[role="alertdialog"]') !== null;
const typeInto = (input: HTMLInputElement, value: string) => act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
});

describe('SpeedTestDialog (Discussion #1527)', () => {
    const result = {
        test_id: 't-1', server_name: 'lab-a', protocol: 'ftp', remote_path: '/', temp_file_name: 'probe.bin',
        size_bytes: 10, upload_duration_ms: 10, download_duration_ms: 10, download_ttfb_ms: null,
        upload_bytes_per_sec: 1, download_bytes_per_sec: 1, upload_mbps: 1, download_mbps: 1,
        integrity_checked: true, integrity_verified: true, upload_sha256: 'x', download_sha256: 'x',
        temp_file_cleaned: true, cleanup_error: null,
    };
    const start = async (onClose: () => void) => {
        const run = deferred<unknown>();
        mocks.handlers.speedtest_history_summary = async () => ({ samples: 0 });
        mocks.handlers.speedtest_run = () => run.promise;
        await render(<SpeedTestDialog servers={[server('a')]} onClose={onClose} />);
        await click('speedTest.start');
        await flush();
        return run;
    };

    it('keeps a running test from the backdrop, and ESC asks before it stops the test and closes', async () => {
        const onClose = vi.fn();
        await start(onClose);
        expect(mocks.invoke).toHaveBeenCalledWith('speedtest_run', expect.anything());
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        expect(mocks.invoke).not.toHaveBeenCalledWith('speedtest_cancel');
        // ESC used to cancel the test outright.
        await escape();
        expect(mocks.invoke).not.toHaveBeenCalledWith('speedtest_cancel');
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        await click('modalGuard.stopAndClose');
        expect(mocks.invoke).toHaveBeenCalledWith('speedtest_cancel');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('asks from the X too while a test runs, instead of cancelling it outright', async () => {
        const onClose = vi.fn();
        await start(onClose);
        await click('common.close');
        expect(mocks.invoke).not.toHaveBeenCalledWith('speedtest_cancel');
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        await click('modalGuard.wait');
        expect(confirmOpen()).toBe(false);
        expect(onClose).not.toHaveBeenCalled();
    });

    it('starts no test after a stop and close that landed while the connection was prepared', async () => {
        const onClose = vi.fn();
        const connection = deferred<Awaited<ReturnType<typeof buildSpeedTestConnection>>>();
        vi.mocked(buildSpeedTestConnection).mockImplementationOnce(() => connection.promise);
        mocks.handlers.speedtest_history_summary = async () => ({ samples: 0 });
        await render(<SpeedTestDialog servers={[server('a')]} onClose={onClose} />);
        await click('speedTest.start');
        await escape();
        await click('modalGuard.stopAndClose');
        expect(onClose).toHaveBeenCalledTimes(1);
        await act(async () => connection.resolve({ protocol: 'ftp', server: 'ftp.example.com', username: 'tester', password: '' } as never));
        await flush();
        expect(mocks.invoke).not.toHaveBeenCalledWith('speedtest_run', expect.anything());
    });

    it('claims no ESC while its tab hides it', async () => {
        const onClose = vi.fn();
        mocks.handlers.speedtest_history_summary = async () => ({ samples: 0 });
        await render(<SpeedTestDialog servers={[server('a')]} onClose={onClose} active={false} />);
        expect(isEscapeClaimed()).toBe(false);
        await escape();
        expect(onClose).not.toHaveBeenCalled();
        await render(<SpeedTestDialog servers={[server('a')]} onClose={onClose} active />);
        expect(isEscapeClaimed()).toBe(true);
    });

    it('leaves ESC to the app while a running test is hidden with its tab, and claims it again when shown', async () => {
        // The tab switch hides My Servers without unmounting it: the hidden
        // dialog must not take the ESC meant for what is in front (the app
        // shortcut closes the shortcuts dialog), nor open a confirm unseen.
        const appEscape = vi.fn();
        const onClose = vi.fn();
        const run = deferred<unknown>();
        mocks.handlers.speedtest_history_summary = async () => ({ samples: 0 });
        mocks.handlers.speedtest_run = () => run.promise;
        const Shell = ({ active }: { active: boolean }) => {
            useKeyboardShortcuts({ Escape: appEscape }, []);
            return <SpeedTestDialog servers={[server('a')]} onClose={onClose} active={active} />;
        };
        await render(<Shell active />);
        await click('speedTest.start');
        await flush();
        await render(<Shell active={false} />);
        await escape();
        expect(appEscape).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
        expect(onClose).not.toHaveBeenCalled();
        await render(<Shell active />);
        await escape();
        expect(appEscape).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(true);
    });

    it('is told it is hidden by My Servers, the hub and the app', () => {
        const read = (path: string) => readFileSync(resolve(process.cwd(), path), 'utf8');
        expect(read('src/App.tsx')).toMatch(/<IntroHub\s+visible=\{showConnectionScreen\}/);
        expect(read('src/components/IntroHub/IntroHub.tsx')).toMatch(/<MyServersPanel\s+visible=\{props\.visible !== false && activeTab === 'my-servers'\}/);
        expect(read('src/components/IntroHub/MyServersPanel.tsx')).toMatch(/<SpeedTestDialog[\s\S]{0,300}?active=\{visible\}/);
    });

    it('keeps a finished result from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        const run = await start(onClose);
        await act(async () => run.resolve(result));
        await flush();
        expect(findButton('speedTest.runAgain')).toBeTruthy();
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
    });

    it('closes from the backdrop before any test ran', async () => {
        const onClose = vi.fn();
        mocks.handlers.speedtest_history_summary = async () => ({ samples: 0 });
        await render(<SpeedTestDialog servers={[server('a')]} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

describe('CrossProfilePanel (Discussion #1527)', () => {
    const plan = {
        plan_id: 'plan-1', source_profile_id: 'src', dest_profile_id: 'dst', source_profile: 'lab-src', dest_profile: 'lab-dst',
        entries: [{ source_path: '/a.txt', dest_path: '/a.txt', display_name: 'a.txt', size: 10, is_dir: false }],
        directories: [], total_files: 1, total_bytes: 10,
    };
    const open = async (onClose: () => void) => {
        mocks.profiles = [server('src'), server('dst')];
        await render(<CrossProfilePanel onClose={onClose} initialSourceProfileId="src" initialDestProfileId="dst" />);
    };
    const planReady = async (onClose: () => void) => {
        mocks.handlers.cross_profile_plan = async () => plan;
        await open(onClose);
        await click('transfer.crossProfile.previewPlan');
        await flush();
        expect(findButton('transfer.crossProfile.executeTransfer')).toBeTruthy();
    };

    it('keeps a plan being built from the backdrop, and the X asks first', async () => {
        const onClose = vi.fn();
        const pending = deferred<unknown>();
        mocks.handlers.cross_profile_plan = () => pending.promise;
        await open(onClose);
        await click('transfer.crossProfile.previewPlan');
        expect(mocks.invoke).toHaveBeenCalledWith('cross_profile_plan', expect.anything());
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('transfer.crossProfile.closeAria');
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        // Building a plan cannot be stopped: the confirm offers to close, not to stop.
        expect(findButton('modalGuard.stopAndClose')).toBeFalsy();
        await click('modalGuard.closeAnyway');
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(mocks.invoke).not.toHaveBeenCalledWith('cross_profile_cancel', expect.anything());
    });

    it('keeps a ready plan from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        await planReady(onClose);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('transfer.crossProfile.closeAria');
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
    });

    it('keeps a running transfer from the backdrop, and the confirm stops it and closes', async () => {
        const onClose = vi.fn();
        await planReady(onClose);
        const execution = deferred<unknown>();
        mocks.handlers.cross_profile_execute = () => execution.promise;
        await click('transfer.crossProfile.executeTransfer');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        expect(mocks.invoke).not.toHaveBeenCalledWith('cross_profile_cancel', expect.anything());
        await click('transfer.crossProfile.closeAria');
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.stopAndClose');
        expect(mocks.invoke).toHaveBeenCalledWith('cross_profile_cancel', { transferId: 'plan-1' });
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('closes from the backdrop while only the setup is shown', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

describe('ShareLinkModal (Discussion #1527)', () => {
    const noCaps = { supports_expiration: false, supports_password: false, supports_permissions: false, available_permissions: [], supports_list_links: false, supports_revoke: false };
    const link = { url: 'https://share.example.com/s/SENTINEL-LINK', password: 'SENTINEL-ONE-TIME', expires_at: null };
    const open = async (onClose: () => void, caps: Partial<typeof noCaps> = {}) => {
        mocks.handlers.provider_share_link_capabilities = async () => ({ ...noCaps, ...caps });
        await render(<ShareLinkModal path="/docs/a.txt" fileName="a.txt" providerName="Lab" onClose={onClose} />);
    };
    const creating = async (onClose: () => void) => {
        const pending = deferred<unknown>();
        mocks.handlers.provider_create_share_link = () => pending.promise;
        await open(onClose);
        expect(mocks.invoke).toHaveBeenCalledWith('provider_create_share_link', expect.anything());
        return pending;
    };

    it('keeps a link being created from the backdrop, and the X says the request goes on', async () => {
        const onClose = vi.fn();
        await creating(onClose);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { xButton().click(); });
        expect(onClose).not.toHaveBeenCalled();
        // No way to stop the provider: the confirm offers to close, not to stop.
        expect(host.textContent).toContain('modalGuard.busyBodyKeepsRunning');
        expect(findButton('modalGuard.stopAndClose')).toBeUndefined();
        await click('modalGuard.closeAnyway');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('asks on ESC while a link is being created', async () => {
        const onClose = vi.fn();
        await creating(onClose);
        await escape();
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
    });

    it('keeps a generated link and its password from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        const pending = await creating(onClose);
        await act(async () => pending.resolve(link));
        await flush();
        expect(host.textContent).toContain('shareLinkModal.linkReady');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { xButton().click(); });
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
    });

    it('asks before dropping options typed in, and closes from the backdrop when none were', async () => {
        const onClose = vi.fn();
        await open(onClose, { supports_expiration: true });
        await click('shareLinkModal.showAdvanced');
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        onClose.mockClear();
        await click('shareLinkModal.expirationHoursOne');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { xButton().click(); });
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.discardAndClose');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('keeps a revoke in flight from the backdrop, and ESC on the revoke confirm only answers it', async () => {
        const onClose = vi.fn();
        const revoke = deferred<unknown>();
        mocks.handlers.provider_list_share_links = async () => [
            { id: 'link-1', url: link.url, created_at: null, expires_at: null, password_protected: false, permissions: null },
        ];
        mocks.handlers.provider_remove_share_link = () => revoke.promise;
        await open(onClose, { supports_expiration: true, supports_list_links: true, supports_revoke: true });
        await click('shareLinkModal.tabManage');
        await flush();
        await click('shareLinkModal.manageRevoke');
        await escape();
        expect(onClose).not.toHaveBeenCalled();
        await click('shareLinkModal.manageRevoke');
        // The confirm's own button comes after the row's.
        const revokeButtons = buttons().filter(b => b.textContent?.trim() === 'shareLinkModal.manageRevoke');
        await act(async () => { revokeButtons[revokeButtons.length - 1].click(); });
        expect(mocks.invoke).toHaveBeenCalledWith('provider_remove_share_link', { path: '/docs/a.txt', linkId: 'link-1' });
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { xButton().click(); });
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
    });
});

describe('TotpSetup (Discussion #1527)', () => {
    const secret = { secret: 'SENTINELSECRETBASE32', uri: 'otpauth://totp/AeroFTP:tester@example.com?secret=SENTINELSECRETBASE32' };
    const open = async (onClose: () => void) => {
        mocks.handlers.totp_setup_start = async () => secret;
        await render(<TotpSetup isOpen onClose={onClose} onEnabled={vi.fn()} />);
        expect(host.textContent).toContain(secret.secret);
    };

    it('keeps the secret being set up from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(confirmOpen()).toBe(false);
    });

    it('asks on ESC and on the X while the code is checked, and closes only on "close anyway"', async () => {
        const onClose = vi.fn();
        const verify = deferred<boolean>();
        mocks.handlers.totp_setup_verify = () => verify.promise;
        await open(onClose);
        await click('security.totp.next');
        await typeInto(host.querySelector('input') as HTMLInputElement, '123456');
        await click('security.totp.verify');
        expect(mocks.invoke).toHaveBeenCalledWith('totp_setup_verify', { code: '123456' });
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await escape();
        expect(onClose).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        await click('modalGuard.wait');
        expect(confirmOpen()).toBe(false);
        await click('common.close');
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.closeAnyway');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('shows no stale confirm when it is opened again after a verification that ended under the confirm', async () => {
        const onClose = vi.fn();
        const verify = deferred<boolean>();
        mocks.handlers.totp_setup_verify = () => verify.promise;
        mocks.handlers.totp_enable = async () => secret.secret;
        await open(onClose);
        await click('security.totp.next');
        await typeInto(host.querySelector('input') as HTMLInputElement, '123456');
        await click('security.totp.verify');
        await escape();
        expect(confirmOpen()).toBe(true);
        // The code checks out while the confirm is open: setup closes itself.
        await act(async () => verify.resolve(true));
        await flush();
        expect(onClose).toHaveBeenCalledTimes(1);
        await render(<TotpSetup isOpen={false} onClose={onClose} onEnabled={vi.fn()} />);
        await render(<TotpSetup isOpen onClose={onClose} onEnabled={vi.fn()} />);
        await flush();
        expect(confirmOpen()).toBe(false);
    });

    it('closes from the backdrop while no secret is shown yet', async () => {
        const onClose = vi.fn();
        mocks.handlers.totp_setup_start = () => new Promise(() => undefined);
        await render(<TotpSetup isOpen onClose={onClose} onEnabled={vi.fn()} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

describe('ServerHealthCheck (Discussion #1527)', () => {
    const report = { server_id: 'a', host: 'a.example.com', status: 'healthy', score: 100, checks: [], checked_at: '2026-01-01T00:00:00Z', error: null };

    it('keeps a check in flight from the backdrop, and the X asks first', async () => {
        const onClose = vi.fn();
        const batch = deferred<unknown>();
        mocks.handlers.server_health_check_batch = () => batch.promise;
        await render(<ServerHealthCheck servers={[server('a')]} onClose={onClose} />);
        await click('healthCheck.checkAll');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { xButton().click(); });
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.closeAnyway');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('keeps the results from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        mocks.handlers.server_health_check_batch = async () => [report];
        await render(<ServerHealthCheck servers={[server('a')]} onClose={onClose} />);
        await click('healthCheck.checkAll');
        await flush();
        expect(findButton('healthCheck.copyResults')).toBeTruthy();
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { xButton().click(); });
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('closes from the backdrop before any check ran', async () => {
        const onClose = vi.fn();
        await render(<ServerHealthCheck servers={[server('a')]} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

describe('VaultSyncDialog (Discussion #1527)', () => {
    const comparison = { vault_only: ['a.txt'], local_only: [], conflicts: [], unchanged: 0 };
    const compare = async (onClose: () => void) => {
        await render(<VaultSyncDialog vaultPath="/vaults/sample.aerovault" password="SENTINEL-PW" onClose={onClose} />);
        await click('vaultSync.selectDirectory');
        await flush();
        await click('vaultSync.compare');
    };

    it('keeps a compare in flight from the backdrop, and the X asks first', async () => {
        const onClose = vi.fn();
        const pending = deferred<unknown>();
        mocks.handlers.vault_v2_sync_compare = () => pending.promise;
        await compare(onClose);
        expect(host.textContent).toContain('vaultSync.comparing');
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(onClose).not.toHaveBeenCalled();
        await click('modalGuard.closeAnyway');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('keeps a comparison to review from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        mocks.handlers.vault_v2_sync_compare = async () => comparison;
        await compare(onClose);
        await flush();
        expect(findButton('vaultSync.applySync')).toBeTruthy();
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('closes from the backdrop before anything was compared', async () => {
        const onClose = vi.fn();
        await render(<VaultSyncDialog vaultPath="/vaults/sample.aerovault" password="SENTINEL-PW" onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

describe('UnifiedTransferPlanDialog (Discussion #1527)', () => {
    const plan = {
        id: 'plan-1', pairKind: 'local-remote', mode: 'copy',
        source: { kind: 'local', panelId: 'left', path: '/src' },
        destination: { kind: 'remote', profileId: 'p1', profileName: 'lab', protocol: 'sftp', path: '/dst' },
        engine: 'provider-upload', canExecute: true, destructive: false, requiresPreview: true,
        maxParallel: 2, entryCount: 1, totalBytes: 10, warnings: [],
    } as unknown as UnifiedTransferPlan;

    it('keeps the plan from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        await render(<UnifiedTransferPlanDialog plan={plan} onExecute={vi.fn()} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { xButton().click(); });
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('keeps an executing plan from the backdrop', async () => {
        const onClose = vi.fn();
        await render(<UnifiedTransferPlanDialog plan={plan} executing onExecute={vi.fn()} onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        expect(xButton().disabled).toBe(true);
    });

    it('lets ESC decline an overwrite question asked over the executing plan, and leaves the plan open', async () => {
        const onClose = vi.fn();
        const onCancel = vi.fn();
        const executing = <UnifiedTransferPlanDialog plan={plan} executing onExecute={vi.fn()} onClose={onClose} />;
        await render(executing);
        await render(<>{executing}<ConfirmDialog message="sentinel.txt exists. Overwrite?" onConfirm={vi.fn()} onCancel={onCancel} /></>);
        await escape();
        expect(onCancel).toHaveBeenCalledTimes(1);
        expect(onClose).not.toHaveBeenCalled();
    });

    it('lets ESC cancel a name asked over the plan', async () => {
        const onClose = vi.fn();
        const onCancel = vi.fn();
        const idle = <UnifiedTransferPlanDialog plan={plan} onExecute={vi.fn()} onClose={onClose} />;
        await render(idle);
        await render(<>{idle}<InputDialog title="sentinel name" defaultValue="" onConfirm={vi.fn()} onCancel={onCancel} /></>);
        await escape();
        expect(onCancel).toHaveBeenCalledTimes(1);
        expect(onClose).not.toHaveBeenCalled();
    });

    it('closes on ESC like the X, does nothing on ESC while it executes, and keeps ESC from the app shortcut', async () => {
        const onClose = vi.fn();
        await render(<UnifiedTransferPlanDialog plan={plan} executing onExecute={vi.fn()} onClose={onClose} />);
        expect(isEscapeClaimed()).toBe(true);
        await escape();
        expect(onClose).not.toHaveBeenCalled();
        await render(<UnifiedTransferPlanDialog plan={plan} onExecute={vi.fn()} onClose={onClose} />);
        await escape();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});

describe('VaultPanel (Discussion #1527)', () => {
    const vault = (mode: string, loading = false) => {
        mocks.vaultState = {
            mode, loading, vaultPath: '/vaults/sample.aerovault', error: '', success: '', zipReport: null,
            isPlaintextZip: false, vaultSecurity: null, lastReport: null, password: '', currentDir: '',
            remoteVaultPath: '', remoteLocalPath: '', clearReport: () => undefined,
        };
    };

    it('keeps a vault being opened from the backdrop, and the X closes at once', async () => {
        const onClose = vi.fn();
        vault('open');
        await render(<VaultPanel onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('closes from the backdrop on the home screen, unless an operation runs', async () => {
        const onClose = vi.fn();
        vault('home');
        await render(<VaultPanel onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).toHaveBeenCalledTimes(1);
        onClose.mockClear();
        vault('home', true);
        await render(<VaultPanel onClose={onClose} />);
        await clickBackdrop();
        expect(onClose).not.toHaveBeenCalled();
        await click('common.close');
        expect(confirmOpen()).toBe(true);
    });
});

describe('TwoFactorPromptDialog over a modal (Discussion #1527)', () => {
    const Underneath = ({ onClose }: { onClose: () => void }) => {
        useGuardedClose({ guard: null, onClose, escape: true });
        return null;
    };
    const prompt = (onCancel: () => void, loading = false) => (
        <TwoFactorPromptDialog isOpen providerKey="mega" accountLabel="tester@example.com" onSubmit={vi.fn()} onCancel={onCancel} loading={loading} />
    );

    it('takes one ESC for itself and leaves the dialog underneath open, and none while a retry runs', async () => {
        const onUnder = vi.fn();
        const onCancel = vi.fn();
        await render(<Underneath onClose={onUnder} />);
        await render(<><Underneath onClose={onUnder} />{prompt(onCancel, true)}</>);
        await escape();
        expect(onCancel).not.toHaveBeenCalled();
        expect(onUnder).not.toHaveBeenCalled();
        await render(<><Underneath onClose={onUnder} />{prompt(onCancel)}</>);
        await escape();
        expect(onCancel).toHaveBeenCalledTimes(1);
        expect(onUnder).not.toHaveBeenCalled();
    });
});
