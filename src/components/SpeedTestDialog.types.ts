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
    test_id: string;
    server_name: string | null;
    phase: Exclude<SpeedTestPhase, 'idle'>;
    transferred_bytes: number;
    total_bytes: number;
    bytes_per_sec: number | null;
}

export interface SpeedTestResult {
    test_id: string;
    server_name: string | null;
    protocol: string;
    remote_path: string;
    temp_file_name: string;
    size_bytes: number;
    upload_duration_ms: number;
    download_duration_ms: number;
    /// Time from download issue to first transferred byte (TTFB), in ms. May be null on very fast/local providers.
    download_ttfb_ms: number | null;
    upload_bytes_per_sec: number;
    download_bytes_per_sec: number;
    upload_mbps: number;
    download_mbps: number;
    /// True when SHA-256 integrity check actually ran. False = explicitly skipped.
    integrity_checked: boolean;
    /// True only when the check ran AND hashes matched. Always read with integrity_checked.
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
    test_id?: string;
    expert_confirmed?: boolean;
    /// Default true. Set false to skip SHA-256 verification (matches CLI --no-integrity).
    verify_integrity?: boolean;
}

export interface SpeedTestRunOutcome {
    test_id: string;
    server_name: string | null;
    result: SpeedTestResult | null;
    error: string | null;
}

export interface SpeedTestCompareRequest {
    tests: SpeedTestRunRequest[];
    max_parallel?: number;
}

export interface SpeedTestCompareResult {
    size_bytes: number;
    started_at_ms: number;
    finished_at_ms: number;
    results: SpeedTestRunOutcome[];
}

// History persistence
export interface SpeedTestHistoryRecordRequest {
    server_id: string | null;
    server_name: string | null;
    host_hash: string | null;
    protocol: string;
    size_bytes: number;
    upload_bytes_per_sec: number;
    download_bytes_per_sec: number;
    upload_duration_ms: number;
    download_duration_ms: number;
    integrity_verified: boolean;
    cleanup_ok: boolean;
}

export interface SpeedTestHistoryEntry {
    id: number;
    server_id: string | null;
    server_name: string | null;
    host_hash: string | null;
    protocol: string;
    size_bytes: number;
    upload_bytes_per_sec: number;
    download_bytes_per_sec: number;
    upload_duration_ms: number;
    download_duration_ms: number;
    integrity_verified: boolean;
    cleanup_ok: boolean;
    created_at: number;
}

export interface SpeedTestHistorySummary {
    server_id: string | null;
    samples: number;
    last: SpeedTestHistoryEntry | null;
    best_download: SpeedTestHistoryEntry | null;
    best_upload: SpeedTestHistoryEntry | null;
    median_download_bps: number | null;
    median_upload_bps: number | null;
    regression_warning: boolean;
}

export type SpeedTestMode = 'single' | 'compare';

export interface SpeedTestDialogProps {
    servers: ServerProfile[];
    initialServerId?: string;
    initialMode?: SpeedTestMode;
    onClose: () => void;
}
