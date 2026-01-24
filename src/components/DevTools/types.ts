// DevTools Types - Extensible architecture for future phases

export type DevToolsTab = 'preview' | 'editor' | 'terminal' | 'chat';

export interface DevToolsState {
    isOpen: boolean;
    height: number;  // Resizable height in pixels
    activeTab: DevToolsTab;
    previewFile: PreviewFile | null;
}

export interface PreviewFile {
    name: string;
    path: string;
    content: string;
    mimeType: string;
    size: number;
    isRemote: boolean;  // true = from FTP server, false = local
}

// File type detection helpers
export const getFileLanguage = (filename: string): string => {
    const ext = filename.split('.').pop()?.toLowerCase() || '';
    const langMap: Record<string, string> = {
        'html': 'html',
        'htm': 'html',
        'php': 'php',
        'js': 'javascript',
        'jsx': 'jsx',
        'ts': 'typescript',
        'tsx': 'tsx',
        'css': 'css',
        'scss': 'scss',
        'sass': 'sass',
        'less': 'less',
        'json': 'json',
        'xml': 'xml',
        'svg': 'svg',
        'md': 'markdown',
        'markdown': 'markdown',
        'py': 'python',
        'rb': 'ruby',
        'java': 'java',
        'c': 'c',
        'cpp': 'cpp',
        'h': 'c',
        'rs': 'rust',
        'go': 'go',
        'sh': 'bash',
        'bash': 'bash',
        'zsh': 'bash',
        'sql': 'sql',
        'yaml': 'yaml',
        'yml': 'yaml',
        'toml': 'toml',
        'ini': 'ini',
        'conf': 'ini',
        'htaccess': 'ini',
        'env': 'ini',
        'txt': 'text',
        'log': 'text',
        'webmanifest': 'json',
        'vue': 'html',
        'svelte': 'html',
        'astro': 'html',
    };
    return langMap[ext] || 'text';
};

export const isImageFile = (filename: string): boolean => {
    const ext = filename.split('.').pop()?.toLowerCase() || '';
    return ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'ico', 'bmp'].includes(ext);
};

export const isMarkdownFile = (filename: string): boolean => {
    const ext = filename.split('.').pop()?.toLowerCase() || '';
    return ['md', 'markdown', 'mdx'].includes(ext);
};

export const isPdfFile = (filename: string): boolean => {
    const ext = filename.split('.').pop()?.toLowerCase() || '';
    return ext === 'pdf';
};

export const isPreviewable = (filename: string): boolean => {
    const ext = filename.split('.').pop()?.toLowerCase() || '';
    const previewableExts = [
        // Code
        'html', 'htm', 'php', 'js', 'jsx', 'ts', 'tsx', 'css', 'scss', 'sass', 'less',
        'json', 'xml', 'svg', 'md', 'markdown', 'py', 'rb', 'java', 'c', 'cpp', 'h',
        'rs', 'go', 'sh', 'bash', 'sql', 'yaml', 'yml', 'toml', 'ini', 'conf', 'txt', 'log',
        'htaccess', 'env', 'webmanifest', 'vue', 'svelte', 'astro',
        // Images
        'png', 'jpg', 'jpeg', 'gif', 'webp', 'ico', 'bmp',
    ];
    return previewableExts.includes(ext);
};
