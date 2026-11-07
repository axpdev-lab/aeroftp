// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const settings = vi.hoisted(() => ({ patch: vi.fn(async () => undefined) }));
const store = vi.hoisted(() => ({ secureStore: vi.fn(() => new Promise<void>(() => {})) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../utils/aeroShare', async (original) => ({
    ...(await original<typeof import('../utils/aeroShare')>()),
    patchAeroShareSettings: settings.patch,
}));
vi.mock('../utils/secureStorage', () => ({
    secureGetWithFallback: vi.fn(async () => ({})),
    secureStore: store.secureStore,
}));
vi.mock('../utils/serverProfileStore', () => ({
    addMigratedServerProfiles: vi.fn(),
    readSavedServerProfilesForWrite: vi.fn(async () => ({ userId: 1 })),
}));

import { DeltaEligibilityDialog } from './AeroSync/DeltaEligibilityDialog';
import { OverwriteDialog } from './OverwriteDialog';
import { FolderOverwriteDialog } from './FolderOverwriteDialog';
import { HostKeyDialog } from './HostKeyDialog';
import { AeroShareActivationPrompt } from './AeroShare/AeroShareActivationPrompt';
import { KeystoreMigrationWizard } from './KeystoreMigrationWizard';
import { GitHubLocalSyncWarning } from './GitHubLocalSyncWarning';
import { AERO_SHARE_ACTIVATED_EVENT } from '../utils/aeroShare';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Discussion #1527, class E: prompts whose backdrop TOOK a decision. A click
 * outside started a queued plan, cancelled a transfer, rejected a host key,
 * recorded a permanent decline or skipped a credential migration. The
 * backdrop is now inert, and ESC gives each prompt's safe answer.
 */
describe('a decision prompt', () => {
    let host: HTMLDivElement;
    let root: Root;
    beforeEach(() => {
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
    });
    afterEach(() => {
        act(() => root.unmount());
        host.remove();
        localStorage.clear();
        vi.clearAllMocks();
    });
    const render = (node: React.ReactNode) => act(() => root.render(<>{node}</>));
    // The outermost element of the prompt is its backdrop, or holds it first.
    const backdrop = () => {
        const outer = document.querySelector('[role="dialog"], .fixed.inset-0') as HTMLElement;
        const inner = outer.querySelector(':scope > .absolute.inset-0') as HTMLElement | null;
        return inner ?? outer;
    };
    const clickBackdrop = () => act(() => { backdrop().dispatchEvent(new MouseEvent('click', { bubbles: true })); });
    const escape = () => act(() => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })); });

    it('about delta sync: the backdrop starts nothing, ESC and the X cancel the plan, Continue runs it', () => {
        const onContinue = vi.fn();
        const onCancel = vi.fn();
        render(<DeltaEligibilityDialog isOpen reason="no rsync" serverLabel="lab" onContinue={onContinue} onCancel={onCancel} />);
        clickBackdrop();
        expect(onContinue).not.toHaveBeenCalled();
        expect(onCancel).not.toHaveBeenCalled();
        escape();
        expect(onCancel).toHaveBeenCalledTimes(1);
        expect(onContinue).not.toHaveBeenCalled();
        const continueButton = [...document.querySelectorAll('button')].find(b => b.textContent === 'aerosync.deltaEligibility.continue')!;
        act(() => { continueButton.click(); });
        expect(onContinue).toHaveBeenCalledWith(false);
    });

    it.each([
        ['a file', () => {
            const onDecision = vi.fn(); const onCancel = vi.fn();
            const file = { name: 'a.txt', size: 1, isRemote: false };
            return { node: <OverwriteDialog isOpen source={file} destination={{ ...file, isRemote: true }} onDecision={onDecision} onCancel={onCancel} />, onDecision, onCancel };
        }],
        ['a folder', () => {
            const onDecision = vi.fn(); const onCancel = vi.fn();
            return { node: <FolderOverwriteDialog isOpen folderName="docs" direction="upload" onDecision={onDecision} onCancel={onCancel} />, onDecision, onCancel };
        }],
    ])('about overwriting %s: the backdrop decides nothing, ESC cancels without writing', (_name, make) => {
        const { node, onDecision, onCancel } = make();
        render(node);
        clickBackdrop();
        expect(onDecision).not.toHaveBeenCalled();
        expect(onCancel).not.toHaveBeenCalled();
        escape();
        expect(onDecision).toHaveBeenCalledWith('cancel', false);
        expect(onCancel).toHaveBeenCalledTimes(1);
    });

    it('about a host key: the backdrop trusts or rejects nothing, ESC rejects', () => {
        const onAccept = vi.fn(); const onReject = vi.fn();
        render(<HostKeyDialog visible info={{ status: 'unknown', fingerprint: 'SHA256:x', algorithm: 'ed25519' }} host="h" port={22} onAccept={onAccept} onReject={onReject} />);
        clickBackdrop();
        expect(onReject).not.toHaveBeenCalled();
        expect(onAccept).not.toHaveBeenCalled();
        escape();
        expect(onReject).toHaveBeenCalledTimes(1);
        expect(onAccept).not.toHaveBeenCalled();
    });

    it('about a host key being saved: ESC and Cancel wait until the accept settles', async () => {
        let settle!: () => void;
        const onAccept = vi.fn(() => new Promise<void>((resolve) => { settle = resolve; }));
        const onReject = vi.fn();
        render(<HostKeyDialog visible info={{ status: 'unknown', fingerprint: 'SHA256:x', algorithm: 'ed25519' }} host="h" port={22} onAccept={onAccept} onReject={onReject} />);
        const buttons = () => [...document.querySelectorAll('button')];
        const trust = buttons().find(b => b.textContent === 'protocol.hostKeyTrustAction')!;
        act(() => { trust.click(); });
        expect(onAccept).toHaveBeenCalledTimes(1);
        escape();
        act(() => { buttons().find(b => b.textContent === 'common.cancel')!.click(); });
        act(() => { trust.click(); });
        expect(onReject).not.toHaveBeenCalled();
        expect(onAccept).toHaveBeenCalledTimes(1);
        await act(async () => { settle(); });
        escape();
        expect(onReject).toHaveBeenCalledTimes(1);
    });

    it('about receiving with AeroShare: the backdrop and ESC record no decline', async () => {
        render(<AeroShareActivationPrompt />);
        await act(async () => { window.dispatchEvent(new Event(AERO_SHARE_ACTIVATED_EVENT)); });
        expect(document.querySelector('[role="dialog"]')).not.toBeNull();
        clickBackdrop();
        expect(document.querySelector('[role="dialog"]')).not.toBeNull();
        escape();
        expect(document.querySelector('[role="dialog"]')).toBeNull();
        expect(settings.patch).not.toHaveBeenCalled();
    });

    it('about migrating the keystore: nothing skips it from the backdrop, and nothing skips it mid-migration', async () => {
        localStorage.setItem('aeroftp_ai_settings', '{"provider":"x"}');
        const onSkip = vi.fn(); const onComplete = vi.fn();
        render(<KeystoreMigrationWizard isOpen onSkip={onSkip} onComplete={onComplete} />);
        clickBackdrop();
        escape();
        expect(onSkip).not.toHaveBeenCalled();
        // Through to the migration, which the store holds open.
        const next = () => [...document.querySelectorAll('button')].find(b => b.textContent?.includes('common.next') || b.textContent?.includes('settings.migrationFinish'))!;
        act(() => { next().click(); });
        act(() => { next().click(); });
        await act(async () => { await new Promise(resolve => setTimeout(resolve, 150)); });
        expect(store.secureStore).toHaveBeenCalled();
        const skip = [...document.querySelectorAll('button')].find(b => b.textContent === 'settings.migrationSkip') as HTMLButtonElement;
        expect(skip.disabled).toBe(true);
        clickBackdrop();
        escape();
        expect(onSkip).not.toHaveBeenCalled();
    });

    it('about unpushed GitHub commits: the backdrop decides nothing, ESC cancels', () => {
        const onPushFirst = vi.fn(); const onContinue = vi.fn(); const onCancel = vi.fn();
        render(<GitHubLocalSyncWarning unpushedCount={2} branch="main" onPushFirst={onPushFirst} onContinue={onContinue} onCancel={onCancel} />);
        clickBackdrop();
        expect(onCancel).not.toHaveBeenCalled();
        expect(onContinue).not.toHaveBeenCalled();
        escape();
        expect(onCancel).toHaveBeenCalledTimes(1);
        expect(onContinue).not.toHaveBeenCalled();
    });
});
