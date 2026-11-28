// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import { PROVIDER_LOGOS, S3BucketLogo, WebDavLogo, OpenStackSwiftLogo } from './ProviderLogos';
import { getServerIcon } from './IntroHub/ServerCard';
import type { ServerProfile } from '../types';

// The My Servers icon rule: a preset profile wears its brand logo (keyed by
// providerId), a generic profile wears the protocol's category glyph (keyed
// by protocol) that matches the sidebar category chip. Swift must never fall
// through to an empty square.
const profile = (partial: Partial<ServerProfile>): ServerProfile => partial as ServerProfile;

describe('generic protocol logos in PROVIDER_LOGOS', () => {
    it('maps the generic s3 / webdav / swift protocols to their category glyphs', () => {
        expect(PROVIDER_LOGOS['s3']).toBe(S3BucketLogo);
        expect(PROVIDER_LOGOS['webdav']).toBe(WebDavLogo);
        expect(PROVIDER_LOGOS['swift']).toBe(OpenStackSwiftLogo);
    });

    it('keeps preset brand logos distinct from the generic glyphs', () => {
        expect(PROVIDER_LOGOS['minio']).toBeDefined();
        expect(PROVIDER_LOGOS['minio']).not.toBe(PROVIDER_LOGOS['s3']);
        expect(PROVIDER_LOGOS['nextcloud']).toBeDefined();
        expect(PROVIDER_LOGOS['nextcloud']).not.toBe(PROVIDER_LOGOS['webdav']);
        expect(PROVIDER_LOGOS['blomp']).toBeDefined();
        expect(PROVIDER_LOGOS['blomp']).not.toBe(PROVIDER_LOGOS['swift']);
    });
});

describe('getServerIcon: generic profiles vs presets', () => {
    it('a generic S3 profile wears the bucket glyph, not a brand logo', () => {
        const html = renderToStaticMarkup(createElement(() => <>{getServerIcon(profile({ protocol: 's3' }))}</>));
        expect(html).toContain('<svg');
        expect(html).not.toContain('<img');
        expect(html).toBe(renderToStaticMarkup(createElement(S3BucketLogo, { size: 20 })));
    });

    it('a generic WebDAV profile wears the globe glyph', () => {
        const html = renderToStaticMarkup(createElement(() => <>{getServerIcon(profile({ protocol: 'webdav' }))}</>));
        expect(html).toContain('<svg');
        expect(html).toContain('<circle cx="12" cy="12" r="10"');
        expect(html).toBe(renderToStaticMarkup(createElement(WebDavLogo, { size: 20 })));
    });

    it('a generic Swift profile wears the OpenStack Swift logo instead of an empty square', () => {
        const html = renderToStaticMarkup(createElement(() => <>{getServerIcon(profile({ protocol: 'swift' }))}</>));
        // Regression: before the swift logo entry this fell through to
        // ProtocolIcon, which has no swift entry and rendered null.
        expect(html).not.toBe('');
        expect(html).toContain('#DA4E31');
        expect(html).toBe(renderToStaticMarkup(createElement(OpenStackSwiftLogo, { size: 20 })));
    });

    it('preset profiles keep their brand logos over the generic protocol glyph', () => {
        const minio = renderToStaticMarkup(createElement(() => <>{getServerIcon(profile({ protocol: 's3', providerId: 'minio' }))}</>));
        expect(minio).toContain('minio.png');
        const nextcloud = renderToStaticMarkup(createElement(() => <>{getServerIcon(profile({ protocol: 'webdav', providerId: 'nextcloud' }))}</>));
        expect(nextcloud).toContain('#0082C9');
        expect(nextcloud).not.toBe(renderToStaticMarkup(createElement(WebDavLogo, { size: 20 })));
        const blomp = renderToStaticMarkup(createElement(() => <>{getServerIcon(profile({ protocol: 'swift', providerId: 'blomp' }))}</>));
        expect(blomp).toContain('blomp.png');
    });

    it('a preset without its own logo (custom-s3) still gets the generic S3 bucket', () => {
        const html = renderToStaticMarkup(createElement(() => <>{getServerIcon(profile({ protocol: 's3', providerId: 'custom-s3' }))}</>));
        expect(html).toBe(renderToStaticMarkup(createElement(S3BucketLogo, { size: 20 })));
    });
});
