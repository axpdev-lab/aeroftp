import { describe, expect, it } from 'vitest';
import { transformWithOxc } from 'vite';
import APP from '../App.tsx?raw';
import { getDefaultPort, getProtocolInfo } from '../components/ProtocolSelector';
import { PROVIDERS } from './registry';
import { connectionManifest, connectionProfileDefaults, getConnectionPreset, presetConnectionOptions } from './connectionManifest';

describe('connection metadata used by saved profiles', () => {
    it('native tile defaults cannot select a preset belonging to a different transport', () => {
        for (const protocol of connectionManifest.protocols) {
            const defaults = connectionProfileDefaults(protocol.token);
            const preset = PROVIDERS.find(provider => provider.id === defaults.providerId);
            if (preset) expect(preset.protocol, protocol.token).toBe(protocol.token);
        }
    });
    it('selecting any preset preserves all of its material connection options', () => {
        for (const provider of PROVIDERS) {
            for (const [key, value] of Object.entries(provider.defaults ?? {})) {
                if (['server', 'port', 'basePath', 'endpointTemplate'].includes(key)) continue;
                expect(presetConnectionOptions(provider.id)[key === 'tls_mode' ? 'tlsMode' : key], `${provider.id}.${key}`).toEqual(value);
            }
        }
    });
    it('catalog-only GitLab uses its own API metadata and HTTPS port', () => {
        expect(getProtocolInfo('gitlab')?.type).toBe('gitlab');
        expect(getDefaultPort('gitlab')).toBe(443);
    });

    it('implicit FTPS has its own metadata and port instead of the FTP fallback', () => {
        expect(getProtocolInfo('ftps')?.type).toBe('ftps');
        expect(getDefaultPort('ftps')).toBe(990);
    });

    it('SCP is a profile protocol of its own on the SSH port, drawn like SFTP', () => {
        expect(connectionManifest.protocols.find(p => p.token === 'scp')).toMatchObject({ frontend: true, creation: 'profile' });
        expect(getProtocolInfo('scp')).toMatchObject({ type: 'scp', name: 'SCP', badge: 'SSH' });
        expect(getProtocolInfo('scp')?.icon).toEqual(getProtocolInfo('sftp')?.icon);
        expect(getDefaultPort('scp')).toBe(22);
        expect(connectionProfileDefaults('scp')).toMatchObject({ port: 22, server: '', options: {} });
    });

    it('puter-webdav documents 2FA expiration and token auth guidance in credential helpText', () => {
        const puter = PROVIDERS.find(p => p.id === 'puter-webdav');
        expect(puter).toBeDefined();
        const userField = puter?.fields?.find(f => f.key === 'username');
        const passField = puter?.fields?.find(f => f.key === 'password');
        expect(userField?.helpText).toContain('-token');
        expect(passField?.helpText).toMatch(/appended verification codes expire and may stop authenticating/i);
        expect(passField?.helpText).toMatch(/-token/);
        expect(passField?.helpText).toMatch(/persistent connections/i);
    });

    it('LifetimeCloud WebDAV preset is declared in manifest and registry as a free plan with token onboarding', () => {
        const manifestPreset = connectionManifest.presets.find(p => p.id === 'lifetimecloud');
        expect(manifestPreset).toMatchObject({
            id: 'lifetimecloud',
            protocol: 'webdav',
            generic: false,
            defaults: {
                server: 'https://lifetimecloud.me/dav/{username}/',
                port: 443,
            },
        });

        const provider = PROVIDERS.find(p => p.id === 'lifetimecloud');
        expect(provider).toBeDefined();
        expect(provider!.protocol).toBe('webdav');
        expect(provider!.category).toBe('webdav');
        expect(provider!.features).toEqual({ shareLink: false, sync: true });
        // A free plan with no time limit and usage limits:
        // the description says free, never trial, and the setup notes give the
        // limits before the user saves the profile.
        expect((provider!.description ?? '').toLowerCase()).toContain('free');
        expect((provider!.description ?? '').toLowerCase()).not.toContain('trial');
        expect((provider!.setupInstructions ?? []).join(' ')).toMatch(/50 MB per file and 50 files/);
        const pwField = provider!.fields.find(f => f.key === 'password');
        expect(pwField?.label).toBe('WebDAV token');
        expect(pwField?.placeholder?.toLowerCase()).toContain('not your lifetimecloud account password');
        expect(provider!.passwordGenUrl).toBe('https://lifetimecloud.me/profile');
        expect(provider!.helpUrl).toBe('https://lifetimecloud.me/articles/rclone-webdav-guide.html');
        expect(provider!.signupUrl).toBe('https://lifetimecloud.me/register');
        expect(provider!.healthCheckUrl).toBe('https://lifetimecloud.me/');
    });
});

describe('the connect of a WebDAV preset saved with an empty host', () => {
    /** App's normalizeProviderConnectionParams, run on the real manifest. */
    async function normalizer(presetOf: typeof getConnectionPreset = getConnectionPreset) {
        const start = APP.indexOf('  const normalizeProviderConnectionParams = (');
        const end = APP.indexOf('\n  };\n', start);
        expect(start, 'normalizeProviderConnectionParams in App.tsx').toBeGreaterThan(-1);
        const { code } = await transformWithOxc(APP.slice(start, end + '\n  };'.length), 'normalize.ts');
        const ctx = { isFilenDesktopBridge: () => false, withFilenBridgeCredentialDefaults: (p: unknown) => p, normalizeMegaOptions: (o: unknown) => o, getConnectionPreset: presetOf };
        return new Function(...Object.keys(ctx), `${code}; return normalizeProviderConnectionParams;`)(...Object.values(ctx)) as
            (params: { protocol: string; server: string; providerId?: string; username: string; password: string }) => { server: string };
    }

    it('connects to the preset server, as the CLI and the bridges do', async () => {
        const normalize = await normalizer();
        // Every exit before the "fill in the endpoint" guard goes through it:
        // the quick connect, a saved profile, a tab switch, the resume.
        for (const server of ['', '   ']) {
            expect(normalize({ protocol: 'webdav', server, providerId: 'lifetimecloud', username: 'alice', password: 'x' }).server)
                .toBe('https://lifetimecloud.me/dav/{username}/');
        }
        // A host that is set always wins.
        expect(normalize({ protocol: 'webdav', server: 'https://lifetimecloud.me/dav/bob/', providerId: 'lifetimecloud', username: 'alice', password: 'x' }).server)
            .toBe('https://lifetimecloud.me/dav/bob/');
        // Every named WebDAV preset with a server in the manifest fills it.
        for (const preset of connectionManifest.presets.filter(p => p.protocol === 'webdav' && !p.generic && String(p.defaults.server ?? '').trim())) {
            expect(normalize({ protocol: 'webdav', server: '', providerId: preset.id, username: 'u', password: 'x' }).server, preset.id)
                .toBe(String(preset.defaults.server).trim());
        }
    });

    it('leaves a blank host blank without a preset server of its own', async () => {
        const normalize = await normalizer();
        for (const providerId of [undefined, 'custom-webdav']) {
            expect(normalize({ protocol: 'webdav', server: '', providerId, username: 'u', password: 'x' }).server, String(providerId)).toBe('');
        }
        // A preset of another transport never fills a WebDAV host.
        expect(normalize({ protocol: 'webdav', server: '', providerId: 'neon-s3', username: 'u', password: 'x' }).server).toBe('');
        // Nor does a generic preset, whatever its defaults hold.
        const generic = await normalizer(() => ({ id: 'g', protocol: 'webdav', generic: true, defaults: { server: 'https://dav.example.test/' }, firstRegion: null }));
        expect(generic({ protocol: 'webdav', server: '', providerId: 'g', username: 'u', password: 'x' }).server).toBe('');
        const s3 = await normalizer(() => ({ id: 's', protocol: 's3', generic: false, defaults: { server: 'https://s3.example.test' }, firstRegion: null }));
        expect(s3({ protocol: 'webdav', server: '', providerId: 's', username: 'u', password: 'x' }).server).toBe('');
    });
});
