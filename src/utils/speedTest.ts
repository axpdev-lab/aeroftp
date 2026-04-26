import { invoke } from '@tauri-apps/api/core';
import { ServerProfile } from '../types';
import { resolveS3Endpoint } from '../providers/registry';
import {
    SPEEDTEST_SUPPORTED_PROTOCOLS,
    SpeedTestProviderConnectionParams,
} from '../components/SpeedTestDialog.types';

export const SPEEDTEST_SIZES = [
    { bytes: 1024 * 1024, labelKey: 'speedTest.size1MB' },
    { bytes: 10 * 1024 * 1024, labelKey: 'speedTest.size10MB' },
    { bytes: 100 * 1024 * 1024, labelKey: 'speedTest.size100MB' },
] as const;

export function supportsSpeedTest(server: ServerProfile): boolean {
    return (SPEEDTEST_SUPPORTED_PROTOCOLS as readonly string[]).includes(server.protocol || 'ftp');
}

export function formatBytes(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
    if (bytes >= 1024 * 1024) return `${Math.round(bytes / (1024 * 1024))} MB`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${bytes} B`;
}

export function formatDuration(ms: number): string {
    if (ms < 1000) return `${Math.round(ms)} ms`;
    return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
}

export function formatMbps(mbps: number): string {
    if (!Number.isFinite(mbps)) return '0.00';
    if (mbps >= 1000) return mbps.toFixed(0);
    if (mbps >= 100) return mbps.toFixed(1);
    return mbps.toFixed(2);
}

export function formatMBps(bytesPerSec: number): string {
    return `${(bytesPerSec / (1024 * 1024)).toFixed(2)} MB/s`;
}

async function getCredentialWithRetry(account: string, maxRetries = 3): Promise<string> {
    for (let attempt = 0; attempt < maxRetries; attempt++) {
        try {
            return await invoke<string>('get_credential', { account });
        } catch (err) {
            if (String(err).includes('STORE_NOT_READY') && attempt < maxRetries - 1) {
                await new Promise(resolve => setTimeout(resolve, 200 * (attempt + 1)));
                continue;
            }
            throw err;
        }
    }
    return '';
}

export async function buildSpeedTestConnection(server: ServerProfile): Promise<SpeedTestProviderConnectionParams> {
    const protocol = server.protocol || 'ftp';
    if (!supportsSpeedTest(server)) {
        throw new Error(`Speed Test is not available for ${protocol}`);
    }

    let password = '';
    try {
        password = await getCredentialWithRetry(`server_${server.id}`);
    } catch {
        password = '';
    }

    const options = server.options || {};
    const region = options.region || (server.providerId === 'filelu-s3' ? 'global' : 'us-east-1');
    const endpoint = options.endpoint
        || resolveS3Endpoint(server.providerId, region as string, options.accountId ? { accountId: options.accountId } : undefined)
        || (protocol === 's3' && server.host && !server.host.includes('amazonaws.com') ? server.host : null);

    return {
        protocol,
        server: server.host,
        port: server.port ?? null,
        username: server.username || '',
        password,
        initial_path: server.initialPath || null,
        bucket: options.bucket || null,
        region,
        endpoint,
        path_style: options.pathStyle ?? null,
        storage_class: options.storage_class || null,
        sse_mode: options.sse_mode || null,
        sse_kms_key_id: options.sse_kms_key_id || null,
        private_key_path: options.private_key_path || null,
        key_passphrase: options.key_passphrase || null,
        timeout: options.timeout || 30,
        tls_mode: options.tlsMode || (protocol === 'ftps' ? 'implicit' : protocol === 'ftp' ? 'explicit' : null),
        verify_cert: options.verifyCert !== undefined ? options.verifyCert : true,
    };
}
