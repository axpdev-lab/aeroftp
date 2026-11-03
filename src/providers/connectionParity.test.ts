import { describe, expect, it } from 'vitest';
import { getDefaultPort, getProtocolInfo } from '../components/ProtocolSelector';
import { PROVIDERS } from './registry';
import { connectionManifest, connectionProfileDefaults, presetConnectionOptions } from './connectionManifest';

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
});
