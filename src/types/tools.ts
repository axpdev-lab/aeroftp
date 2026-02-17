// AI Tool Types for AeroFTP Agent

export type DangerLevel = 'safe' | 'medium' | 'high';

export interface AITool {
    name: string;
    description: string;
    parameters: AIToolParameter[];
    dangerLevel: DangerLevel;
}

export interface AIToolParameter {
    name: string;
    type: 'string' | 'number' | 'boolean' | 'array';
    description: string;
    required: boolean;
}

export interface AgentToolCall {
    id: string;
    toolName: string;
    args: Record<string, unknown>;
    status: 'pending' | 'approved' | 'rejected' | 'executing' | 'completed' | 'error';
    result?: unknown;
    error?: string;
    preview?: string;
    validation?: { valid: boolean; errors: string[]; warnings: string[] };
}

// Provider-agnostic tool definitions (works with all 14 protocols)
export const AGENT_TOOLS: AITool[] = [
    // Safe — auto-execute
    {
        name: 'remote_list',
        description: 'List files and folders in a remote directory',
        parameters: [
            { name: 'path', type: 'string', description: 'Remote directory path', required: true },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'remote_read',
        description: 'Read a remote text file (max 5KB)',
        parameters: [
            { name: 'path', type: 'string', description: 'Remote file path', required: true },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'remote_info',
        description: 'Get file/directory info (size, modified, permissions)',
        parameters: [
            { name: 'path', type: 'string', description: 'Remote path', required: true },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'remote_search',
        description: 'Search for files by name pattern on remote',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory to search', required: true },
            { name: 'pattern', type: 'string', description: 'Search pattern (e.g. "*.txt")', required: true },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'preview_edit',
        description: 'Preview a find/replace edit without modifying the file. Returns original and modified content for diff display.',
        parameters: [
            { name: 'path', type: 'string', description: 'File path to preview edit', required: true },
            { name: 'find', type: 'string', description: 'String to find', required: true },
            { name: 'replace', type: 'string', description: 'Replacement string', required: true },
            { name: 'replace_all', type: 'boolean', description: 'Replace all occurrences (default true)', required: false },
            { name: 'remote', type: 'boolean', description: 'If true, read from remote server', required: false },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'local_list',
        description: 'List files and folders in a local directory',
        parameters: [
            { name: 'path', type: 'string', description: 'Local directory path', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_read',
        description: 'Read a local text file (max 5KB)',
        parameters: [
            { name: 'path', type: 'string', description: 'Local file path', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_search',
        description: 'Search for files by name pattern in a local directory',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory to search', required: true },
            { name: 'pattern', type: 'string', description: 'Search pattern (e.g. "*.txt")', required: true },
        ],
        dangerLevel: 'medium',
    },

    // Medium — requires confirmation
    {
        name: 'local_mkdir',
        description: 'Create a local directory (including parents)',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory path to create', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_write',
        description: 'Write content to a local text file',
        parameters: [
            { name: 'path', type: 'string', description: 'Local file path', required: true },
            { name: 'content', type: 'string', description: 'File content', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_rename',
        description: 'Rename/move a local file or folder',
        parameters: [
            { name: 'from', type: 'string', description: 'Current path', required: true },
            { name: 'to', type: 'string', description: 'New path', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_move_files',
        description: 'Move multiple local files into a destination directory in one batch operation',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of source file absolute paths to move (relative names auto-resolved to local path)', required: true },
            { name: 'destination', type: 'string', description: 'Destination directory absolute path (relative names auto-resolved to local path)', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_edit',
        description: 'Find and replace text in a local file (literal match, not regex)',
        parameters: [
            { name: 'path', type: 'string', description: 'Local file path', required: true },
            { name: 'find', type: 'string', description: 'Exact text to find (literal string, no regex)', required: true },
            { name: 'replace', type: 'string', description: 'Replacement text', required: true },
            { name: 'replace_all', type: 'boolean', description: 'Replace all occurrences (default: true)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'remote_edit',
        description: 'Find and replace text in a remote file (literal match, not regex)',
        parameters: [
            { name: 'path', type: 'string', description: 'Remote file path', required: true },
            { name: 'find', type: 'string', description: 'Exact text to find (literal string, no regex)', required: true },
            { name: 'replace', type: 'string', description: 'Replacement text', required: true },
            { name: 'replace_all', type: 'boolean', description: 'Replace all occurrences (default: true)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'upload_files',
        description: 'Upload multiple local files to a remote directory',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of local file paths to upload', required: true },
            { name: 'remote_dir', type: 'string', description: 'Remote destination directory', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'download_files',
        description: 'Download multiple remote files to a local directory',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of remote file paths to download', required: true },
            { name: 'local_dir', type: 'string', description: 'Local destination directory', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'remote_download',
        description: 'Download file from remote to local',
        parameters: [
            { name: 'remote_path', type: 'string', description: 'Remote file path', required: true },
            { name: 'local_path', type: 'string', description: 'Local destination', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'remote_upload',
        description: 'Upload file from local to remote',
        parameters: [
            { name: 'local_path', type: 'string', description: 'Local file path', required: true },
            { name: 'remote_path', type: 'string', description: 'Remote destination', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'remote_mkdir',
        description: 'Create a directory on remote',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory path to create', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'remote_rename',
        description: 'Rename/move a file or folder on remote',
        parameters: [
            { name: 'from', type: 'string', description: 'Current path', required: true },
            { name: 'to', type: 'string', description: 'New path', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'sync_preview',
        description: 'Preview sync differences between local and remote directories',
        parameters: [
            { name: 'local_path', type: 'string', description: 'Local directory', required: true },
            { name: 'remote_path', type: 'string', description: 'Remote directory', required: true },
        ],
        dangerLevel: 'medium',
    },

    // Batch file operations
    {
        name: 'local_batch_rename',
        description: 'Rename multiple files using patterns: find/replace, add prefix, add suffix, or sequential numbering',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of file absolute paths to rename (relative names auto-resolved to local path)', required: true },
            { name: 'mode', type: 'string', description: 'Rename mode: find_replace, add_prefix, add_suffix, or sequential', required: true },
            { name: 'find', type: 'string', description: 'Text to find (find_replace mode only)', required: false },
            { name: 'replace', type: 'string', description: 'Replacement text (find_replace mode only)', required: false },
            { name: 'prefix', type: 'string', description: 'Prefix to add (add_prefix mode only)', required: false },
            { name: 'suffix', type: 'string', description: 'Suffix to add before extension (add_suffix mode only)', required: false },
            { name: 'base_name', type: 'string', description: 'Base name for sequential mode (default: file)', required: false },
            { name: 'start_number', type: 'number', description: 'Starting number for sequential mode (default: 1)', required: false },
            { name: 'padding', type: 'number', description: 'Digit padding for sequential mode (default: 2)', required: false },
            { name: 'case_sensitive', type: 'boolean', description: 'Case-sensitive find/replace (default: false)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_copy_files',
        description: 'Copy multiple local files or folders into a destination directory',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of source file/folder absolute paths to copy (relative names auto-resolved to local path)', required: true },
            { name: 'destination', type: 'string', description: 'Destination directory absolute path (relative names auto-resolved to local path)', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_trash',
        description: 'Move files to system trash/recycle bin (safe alternative to permanent delete)',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of file absolute paths to move to trash (relative names auto-resolved to local path)', required: true },
        ],
        dangerLevel: 'medium',
    },

    // Archive operations
    {
        name: 'archive_compress',
        description: 'Compress files into an archive (ZIP, 7z, TAR, TAR.GZ, TAR.BZ2, TAR.XZ) with optional AES-256 encryption',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of file/folder paths to compress', required: true },
            { name: 'output_path', type: 'string', description: 'Output archive file path', required: true },
            { name: 'format', type: 'string', description: 'Archive format: zip, 7z, tar, tar.gz, tar.bz2, tar.xz (default: zip)', required: false },
            { name: 'password', type: 'string', description: 'Encryption password for ZIP (AES-256) or 7z', required: false },
            { name: 'compression_level', type: 'number', description: 'Compression level 0-9 (default: 6)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'archive_decompress',
        description: 'Extract an archive (ZIP, 7z, TAR, TAR.GZ, TAR.BZ2, TAR.XZ) with optional password',
        parameters: [
            { name: 'archive_path', type: 'string', description: 'Path to the archive file', required: true },
            { name: 'output_dir', type: 'string', description: 'Output directory for extracted files', required: true },
            { name: 'password', type: 'string', description: 'Decryption password (if encrypted)', required: false },
            { name: 'create_subfolder', type: 'boolean', description: 'Create subfolder with archive name (default: true)', required: false },
        ],
        dangerLevel: 'medium',
    },

    // Content inspection tools
    {
        name: 'local_grep',
        description: 'Search file contents using regex pattern. Recursively searches text files in a directory, returning matching lines with context.',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory to search in', required: true },
            { name: 'pattern', type: 'string', description: 'Regex pattern to search for', required: true },
            { name: 'glob', type: 'string', description: 'File filter pattern (e.g. "*.ts", "*.rs")', required: false },
            { name: 'max_results', type: 'number', description: 'Maximum matches to return (default: 50)', required: false },
            { name: 'context_lines', type: 'number', description: 'Lines of context around each match (default: 2)', required: false },
            { name: 'case_sensitive', type: 'boolean', description: 'Case-sensitive search (default: true)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_head',
        description: 'Read the first N lines of a local file (default: 20 lines)',
        parameters: [
            { name: 'path', type: 'string', description: 'File path', required: true },
            { name: 'lines', type: 'number', description: 'Number of lines to read (default: 20, max: 500)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_tail',
        description: 'Read the last N lines of a local file (default: 20 lines)',
        parameters: [
            { name: 'path', type: 'string', description: 'File path', required: true },
            { name: 'lines', type: 'number', description: 'Number of lines to read (default: 20, max: 500)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_stat_batch',
        description: 'Get file metadata for multiple paths at once: size, modified date, type, permissions',
        parameters: [
            { name: 'paths', type: 'array', description: 'Array of file/directory paths to stat (max 100)', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'local_tree',
        description: 'Display a recursive directory tree with file sizes, filtered by depth and glob pattern',
        parameters: [
            { name: 'path', type: 'string', description: 'Root directory path', required: true },
            { name: 'max_depth', type: 'number', description: 'Maximum depth to recurse (default: 3, max: 10)', required: false },
            { name: 'show_hidden', type: 'boolean', description: 'Show hidden files/directories (default: false)', required: false },
            { name: 'glob', type: 'string', description: 'File filter pattern (e.g. "*.ts")', required: false },
        ],
        dangerLevel: 'medium',
    },

    // Clipboard
    {
        name: 'clipboard_read',
        description: 'Read current text content from the system clipboard',
        parameters: [],
        dangerLevel: 'medium',
    },
    {
        name: 'clipboard_write',
        description: 'Write text content to the system clipboard',
        parameters: [
            { name: 'content', type: 'string', description: 'Text to copy to clipboard', required: true },
        ],
        dangerLevel: 'medium',
    },

    // Read-only analysis tools (safe)
    {
        name: 'local_diff',
        description: 'Compare two local files and show unified diff output with additions and deletions',
        parameters: [
            { name: 'path_a', type: 'string', description: 'First file path', required: true },
            { name: 'path_b', type: 'string', description: 'Second file path', required: true },
            { name: 'context_lines', type: 'number', description: 'Lines of context around changes (default: 3)', required: false },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'local_file_info',
        description: 'Get detailed file properties: size, permissions, timestamps, MIME type, owner (Unix)',
        parameters: [
            { name: 'path', type: 'string', description: 'File or directory path', required: true },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'local_disk_usage',
        description: 'Calculate total size of a directory (recursive): total bytes, file count, directory count',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory path', required: true },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'local_find_duplicates',
        description: 'Find duplicate files in a directory using MD5 hash comparison, sorted by wasted space',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory to scan', required: true },
            { name: 'min_size', type: 'number', description: 'Minimum file size in bytes (default: 1024)', required: false },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'hash_file',
        description: 'Compute cryptographic hash of a file (MD5, SHA-1, SHA-256, SHA-512, BLAKE3)',
        parameters: [
            { name: 'path', type: 'string', description: 'File path to hash', required: true },
            { name: 'algorithm', type: 'string', description: 'Hash algorithm: md5, sha1, sha256, sha512, blake3 (default: sha256)', required: false },
        ],
        dangerLevel: 'safe',
    },

    // High — explicit confirmation
    {
        name: 'remote_delete',
        description: 'Delete a file or directory on remote',
        parameters: [
            { name: 'path', type: 'string', description: 'Path to delete', required: true },
        ],
        dangerLevel: 'high',
    },
    {
        name: 'local_delete',
        description: 'Delete a local file or directory',
        parameters: [
            { name: 'path', type: 'string', description: 'Path to delete', required: true },
        ],
        dangerLevel: 'high',
    },
    {
        name: 'shell_execute',
        description: 'Execute a shell command and capture output. Returns stdout, stderr, and exit code. Use this to run system commands, scripts, build tools, git, npm, etc.',
        parameters: [
            { name: 'command', type: 'string', description: 'Shell command to execute', required: true },
            { name: 'working_dir', type: 'string', description: 'Working directory (default: user home)', required: false },
            { name: 'timeout_secs', type: 'number', description: 'Timeout in seconds (default: 30, max: 120)', required: false },
        ],
        dangerLevel: 'high',
    },

    // RAG — file indexing and content search
    {
        name: 'rag_index',
        description: 'Index files in a directory for AI context. Returns file listing with types, sizes, and text previews for understanding the workspace structure.',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory path to index', required: true },
            { name: 'recursive', type: 'boolean', description: 'Recurse into subdirectories (default: true)', required: false },
            { name: 'max_files', type: 'number', description: 'Maximum files to index (default: 200)', required: false },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'rag_search',
        description: 'Full-text search across files in a directory. Finds lines matching a query string in all text files, returning file paths, line numbers, and context.',
        parameters: [
            { name: 'query', type: 'string', description: 'Search string (case-insensitive)', required: true },
            { name: 'path', type: 'string', description: 'Directory to search (default: current)', required: false },
            { name: 'max_results', type: 'number', description: 'Maximum results (default: 20)', required: false },
        ],
        dangerLevel: 'medium',
    },

    // Agent memory — persistent project notes
    {
        name: 'agent_memory_write',
        description: 'Save a note to persistent project memory (.aeroagent file) for future reference across sessions',
        parameters: [
            { name: 'entry', type: 'string', description: 'Content to remember', required: true },
            { name: 'category', type: 'string', description: 'Category: convention, preference, issue, pattern', required: false },
        ],
        dangerLevel: 'medium',
    },

    // App control tools
    {
        name: 'set_theme',
        description: 'Change the application theme. Available themes: light, dark, tokyo (Tokyo Night), cyber (Cyber)',
        parameters: [
            { name: 'theme', type: 'string', description: 'Theme name: light, dark, tokyo, or cyber', required: true },
        ],
        dangerLevel: 'safe',
    },
    {
        name: 'app_info',
        description: 'Get information about the current application state: version, platform, connection status, and current working directory',
        parameters: [],
        dangerLevel: 'safe',
    },
    {
        name: 'sync_control',
        description: 'Control the AeroSync background synchronization service. Actions: start (begin sync), stop (halt sync), status (check if running)',
        parameters: [
            { name: 'action', type: 'string', description: 'Action to perform: start, stop, or status', required: true },
        ],
        dangerLevel: 'medium',
    },
    {
        name: 'vault_peek',
        description: 'Peek at an AeroVault encrypted container header without requiring the password. Shows encryption info, file count, and creation date',
        parameters: [
            { name: 'path', type: 'string', description: 'Path to the .aerovault file', required: true },
        ],
        dangerLevel: 'safe',
    },
];

// Get tool by name (searches built-in AGENT_TOOLS only)
export const getToolByName = (name: string): AITool | undefined =>
    AGENT_TOOLS.find(t => t.name === name);

// Get tool by name from a provided array (includes plugin tools)
export const getToolByNameFromAll = (name: string, allTools: AITool[]): AITool | undefined =>
    allTools.find(t => t.name === name);

// Check if tool is safe (auto-execute without approval)
// When allTools is provided, searches that array (includes plugin tools)
export const isSafeTool = (toolName: string, allTools?: AITool[]): boolean => {
    const tool = allTools
        ? allTools.find(t => t.name === toolName)
        : getToolByName(toolName);
    return tool ? tool.dangerLevel === 'safe' : false;
};

// Generate tool description for AI system prompt
export const generateToolsPrompt = (extraTools?: Array<{name: string; description: string; parameters?: Record<string, unknown>}>): string => {
    const builtInSection = AGENT_TOOLS.map(t => `- ${t.name}: ${t.description}
  Parameters: ${t.parameters.map(p => `${p.name} (${p.type}${p.required ? ', required' : ''})`).join(', ')}`).join('\n\n');

    let extraSection = '';
    if (extraTools && extraTools.length > 0) {
        extraSection = '\n\n' + extraTools.map(t => {
            const params = t.parameters;
            const props = (params?.properties ?? {}) as Record<string, { type?: string; description?: string }>;
            const required = (params?.required ?? []) as string[];
            const paramList = Object.entries(props).map(([name, spec]) =>
                `${name} (${spec.type || 'string'}${required.includes(name) ? ', required' : ''})`
            ).join(', ');
            return `- ${t.name}: ${t.description}${paramList ? `\n  Parameters: ${paramList}` : ''}`;
        }).join('\n\n');
    }

    return `AVAILABLE TOOLS:
${builtInSection}${extraSection}

RULES:
1. Safe tools (remote_list, remote_read, remote_info, remote_search) execute automatically.
2. Medium/high risk tools need user approval — the system shows an approval prompt automatically. Do NOT ask for confirmation yourself — just call the tool directly and the UI will handle approval.
3. Never delete files without explicit user request.

When using a tool, respond with:
TOOL: tool_name
ARGS: {"param1": "value1"}

Example:
TOOL: remote_list
ARGS: {"path": "/var/www"}`;
};

// Convert AITool to JSON Schema format (for native function calling)
export const toJSONSchema = (tool: AITool): Record<string, unknown> => ({
    type: 'object',
    properties: Object.fromEntries(
        tool.parameters.map(p => [p.name, {
            type: p.type === 'array' ? 'array' : p.type,
            description: p.description,
            ...(p.type === 'array' ? { items: { type: 'string' } } : {}),
        }])
    ),
    required: tool.parameters.filter(p => p.required).map(p => p.name),
});

// Convert all tools to native function definitions for AI providers
export const toNativeDefinitions = (tools: AITool[]): Array<{
    name: string;
    description: string;
    parameters: Record<string, unknown>;
}> => tools.map(t => ({
    name: t.name,
    description: t.description,
    parameters: toJSONSchema(t),
}));
