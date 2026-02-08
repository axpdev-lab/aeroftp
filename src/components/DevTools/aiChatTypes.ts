import { AIProviderType } from '../../types/ai';

// Vision constants
export const MAX_IMAGE_SIZE = 20 * 1024 * 1024; // 20 MB
export const MAX_IMAGES = 5;
export const SUPPORTED_IMAGE_TYPES = ['image/jpeg', 'image/png', 'image/gif', 'image/webp'];
export const MAX_DIMENSION = 2048;

export interface VisionImage {
    data: string;       // base64 (no data URI prefix)
    mediaType: string;  // "image/jpeg" etc.
    preview: string;    // "data:image/jpeg;base64,..." for local display
}

export interface Message {
    id: string;
    role: 'user' | 'assistant';
    content: string;
    timestamp: Date;
    images?: VisionImage[];
    thinking?: string;
    thinkingDuration?: number;
    webSearchUsed?: boolean;
    modelInfo?: {
        modelName: string;
        providerName: string;
        providerType: AIProviderType;
    };
    tokenInfo?: {
        inputTokens?: number;
        outputTokens?: number;
        totalTokens?: number;
        cost?: number;
        cacheCreationTokens?: number;  // Anthropic: tokens to create cache entry
        cacheReadTokens?: number;      // Anthropic: tokens read from cache (90% cheaper)
        cacheSavings?: number;         // Estimated USD savings from caching
    };
}

export interface AIChatProps {
    className?: string;
    remotePath?: string;
    localPath?: string;
    /** Theme hint - AI Chat stays dark but may use for future enhancements */
    isLightTheme?: boolean;
    /** Active protocol type (e.g. 'sftp', 'ftp', 'googledrive') */
    providerType?: string;
    /** Whether currently connected to remote */
    isConnected?: boolean;
    /** Currently selected files in the file panel */
    selectedFiles?: string[];
    /** Server hostname for connection context */
    serverHost?: string;
    /** Server port for connection context */
    serverPort?: number;
    /** Username for connection context */
    serverUser?: string;
    /** Callback to refresh file panels after AI tool mutations */
    onFileMutation?: (target: 'remote' | 'local' | 'both') => void;
    /** Currently open file name in the code editor */
    editorFileName?: string;
    /** Currently open file path in the code editor */
    editorFilePath?: string;
}

// Selected model state
export interface SelectedModel {
    providerId: string;
    providerName: string;
    providerType: AIProviderType;
    modelId: string;
    modelName: string;
    displayName: string;
}

// Tool names that mutate the filesystem and should trigger a panel refresh
export const MUTATION_TOOLS: Record<string, 'remote' | 'local' | 'both'> = {
    remote_delete: 'remote', remote_rename: 'remote', remote_mkdir: 'remote',
    remote_upload: 'remote', remote_edit: 'remote', upload_files: 'remote',
    download_files: 'local', remote_download: 'local',
    local_write: 'local', local_delete: 'local', local_rename: 'local',
    local_mkdir: 'local', local_edit: 'local',
    archive_create: 'both', archive_extract: 'both',
};
