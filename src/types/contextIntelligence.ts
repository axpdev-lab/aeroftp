// Context Intelligence types for AeroFTP AI Agent
// Shared types used by project detection, smart context, and system prompt modules

// Project detection result (mirrors Rust ProjectContext)
export interface ProjectContext {
    project_type: string;        // "nodejs", "rust", "python", etc.
    name: string | null;
    version: string | null;
    scripts: string[];
    deps_count: number;
    dev_deps_count: number;
    entry_points: string[];
    config_files: string[];
}

// Smart context section with priority
export interface ContextSection {
    type: 'project' | 'git' | 'imports' | 'memory' | 'rag' | 'files' | 'app_knowledge';
    content: string;
    priority: number;       // 1 = highest priority
    estimatedTokens: number;
}

// Assembled smart context
export interface SmartContext {
    sections: ContextSection[];
    totalEstimatedTokens: number;
}

// Budget mode type
export type BudgetMode = 'full' | 'compact' | 'minimal';
