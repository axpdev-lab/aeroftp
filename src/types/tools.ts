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
}

// Provider-agnostic tool definitions (works with all 13 protocols)
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
        name: 'terminal_execute',
        description: 'Execute a command in the integrated terminal',
        parameters: [
            { name: 'command', type: 'string', description: 'Shell command to execute', required: true },
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
        dangerLevel: 'safe',
    },
];

// Backwards compatibility
export const FTP_TOOLS = AGENT_TOOLS;

// Get tool by name
export const getToolByName = (name: string): AITool | undefined =>
    AGENT_TOOLS.find(t => t.name === name);

// Check if tool requires approval
export const requiresApproval = (toolName: string): boolean => {
    const tool = getToolByName(toolName);
    return tool ? tool.dangerLevel !== 'safe' : true;
};

// Generate tool description for AI system prompt
export const generateToolsPrompt = (): string => {
    return `AVAILABLE TOOLS:
${AGENT_TOOLS.map(t => `- ${t.name}: ${t.description}
  Parameters: ${t.parameters.map(p => `${p.name} (${p.type}${p.required ? ', required' : ''})`).join(', ')}`).join('\n\n')}

RULES:
1. Safe tools (remote_list, remote_read, remote_info, remote_search, rag_search) execute automatically.
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
