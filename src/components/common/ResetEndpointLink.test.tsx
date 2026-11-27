// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import React, { act, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getProviderById } from '../../providers';
import type { EndpointDefault } from '../../providers/presetEndpoint';
import { ResetEndpointLink } from './ResetEndpointLink';

vi.mock('../../i18n', () => ({
    useTranslation: () => (key: string) => key,
    translate: (key: string) => key,
    useI18n: () => ({ t: (key: string) => key, language: 'en' }),
}));

// The same wiring the WebDAV Advanced block uses: an endpoint input whose
// state lives in connectionParams, plus the reset link.
function Harness({ providerId, initialServer }: { providerId: string; initialServer: string }) {
    const provider = getProviderById(providerId);
    const [params, setParams] = useState({ server: initialServer, port: 443, username: 'alice' });
    return (
        <div>
            <input
                aria-label="endpoint"
                value={params.server}
                onChange={(e) => setParams({ ...params, server: e.target.value })}
            />
            <ResetEndpointLink
                provider={provider}
                params={params}
                fallbackPort={443}
                onReset={(def: EndpointDefault) => setParams({ ...params, server: def.server, port: def.port ?? params.port })}
            />
        </div>
    );
}

describe('ResetEndpointLink', () => {
    let container: HTMLDivElement;
    let root: Root;

    beforeEach(() => {
        container = document.createElement('div');
        document.body.appendChild(container);
        root = createRoot(container);
    });

    afterEach(() => {
        act(() => root.unmount());
        container.remove();
    });

    const render = async (ui: React.ReactElement) => {
        await act(async () => {
            root.render(ui);
        });
    };

    const link = () => container.querySelector('button');
    const input = () => container.querySelector('input') as HTMLInputElement;

    it('is hidden at the preset default and appears once the endpoint changes', async () => {
        await render(<Harness providerId="puter-webdav" initialServer="https://dav.puter.com/{username}" />);
        expect(link()).toBeNull();
        await act(async () => {
            // React reads the value through its own tracker, so set it with
            // the native setter before dispatching the input event.
            const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')!.set!;
            setter.call(input(), 'https://dav.example.com/alice');
            input().dispatchEvent(new Event('input', { bubbles: true }));
        });
        expect(link()).not.toBeNull();
        expect(link()!.textContent).toContain('connection.resetEndpointToDefault');
    });

    it('click restores the preset default and the link disappears', async () => {
        await render(<Harness providerId="puter-webdav" initialServer="https://dav.example.com/alice" />);
        expect(link()).not.toBeNull();
        await act(async () => {
            link()!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
        });
        expect(input().value).toBe('https://dav.puter.com/{username}');
        expect(link()).toBeNull();
    });

    it('renders nothing for a generic provider', async () => {
        await render(<Harness providerId="custom-webdav" initialServer="https://anything.example.com" />);
        expect(link()).toBeNull();
    });
});
