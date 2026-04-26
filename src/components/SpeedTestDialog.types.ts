import { ServerProfile } from '../types';

export const SPEEDTEST_SUPPORTED_PROTOCOLS = ['ftp', 'ftps', 'sftp', 's3', 'webdav'] as const;

export type SpeedTestSupportedProtocol = typeof SPEEDTEST_SUPPORTED_PROTOCOLS[number];

export type SpeedTestPhase =
    | 'idle'
    | 'connecting'
    | 'uploading'
    | 'downloading'
    | 'cleaning_up'
    | 'done';

export interface SpeedTestProgress {
    phase: Exclude<SpeedTestPhase, 'idle'>;
    transferred_bytes: number;
    total_bytes: number;
    bytes_per_sec: number | null;
}

export interface SpeedTestResult {
    server_name: string | null;
    protocol: string;
    remote_path: string;
    temp_file_name: string;
    size_bytes: number;
    upload_duration_ms: number;
    download_duration_ms: number;
    upload_bytes_per_sec: number;
    download_bytes_per_sec: number;
    upload_mbps: number;
    download_mbps: number;
    integrity_verified: boolean;
    upload_sha256: string;
    download_sha256: string;
    temp_file_cleaned: boolean;
    cleanup_error: string | null;
}

export interface SpeedTestProviderConnectionParams {
    protocol: string;
    server: string;
    port?: number | null;
    username: string;
    password: string;
    initial_path?: string | null;
    bucket?: string | null;
    region?: string | null;
    endpoint?: string | null;
    path_style?: boolean | null;
    storage_class?: string | null;
    sse_mode?: string | null;
    sse_kms_key_id?: string | null;
    private_key_path?: string | null;
    key_passphrase?: string | null;
    timeout?: number | null;
    tls_mode?: string | null;
    verify_cert?: boolean | null;
}

export interface SpeedTestRunRequest {
    connection: SpeedTestProviderConnectionParams;
    size_bytes: number;
    remote_dir: string;
    server_name?: string | null;
}

export interface SpeedTestDialogProps {
    servers: ServerProfile[];
    initialServerId?: string;
    onClose: () => void;
}
