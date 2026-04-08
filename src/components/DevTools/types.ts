// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

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
    const baseName = filename.split('/').pop()?.toLowerCase() || filename.toLowerCase();
    const langMap: Record<string, string> = {
        // Web
        'html': 'html', 'htm': 'html', 'xhtml': 'html', 'vue': 'html', 'svelte': 'html', 'astro': 'html',
        'css': 'css', 'scss': 'scss', 'sass': 'scss', 'less': 'less',
        'js': 'javascript', 'mjs': 'javascript', 'cjs': 'javascript', 'jsx': 'javascript',
        'ts': 'typescript', 'mts': 'typescript', 'cts': 'typescript', 'tsx': 'typescript',
        'json': 'json', 'jsonc': 'json', 'json5': 'json', 'webmanifest': 'json',
        'eslintrc': 'json', 'prettierrc': 'json', 'babelrc': 'json',
        'xml': 'xml', 'xsl': 'xml', 'xslt': 'xml', 'xsd': 'xml', 'svg': 'xml', 'manifest': 'xml', 'plist': 'xml',
        'php': 'php', 'phtml': 'php',
        'graphql': 'graphql', 'gql': 'graphql',
        'mdx': 'mdx',
        'pug': 'pug', 'jade': 'pug',
        'hbs': 'handlebars', 'handlebars': 'handlebars',
        'liquid': 'liquid',
        'twig': 'twig',
        // Markdown / docs
        'md': 'markdown', 'markdown': 'markdown',
        'rst': 'restructuredtext', 'rest': 'restructuredtext',
        // Systems programming
        'c': 'cpp', 'h': 'cpp', 'cpp': 'cpp', 'cc': 'cpp', 'cxx': 'cpp', 'hpp': 'cpp', 'hh': 'cpp', 'hxx': 'cpp',
        'rs': 'rust',
        'go': 'go',
        'swift': 'swift',
        'm': 'objective-c', 'mm': 'objective-c',
        // JVM
        'java': 'java',
        'kt': 'kotlin', 'kts': 'kotlin',
        'scala': 'scala', 'sc': 'scala',
        'clj': 'clojure', 'cljs': 'clojure', 'cljc': 'clojure', 'edn': 'clojure',
        // .NET
        'cs': 'csharp',
        'fs': 'fsharp', 'fsx': 'fsharp', 'fsi': 'fsharp',
        'vb': 'vb',
        // Scripting
        'py': 'python', 'pyw': 'python', 'pyi': 'python',
        'rb': 'ruby', 'erb': 'ruby', 'rake': 'ruby', 'gemspec': 'ruby',
        'lua': 'lua',
        'pl': 'perl', 'pm': 'perl',
        'r': 'r', 'rmd': 'r',
        'jl': 'julia',
        'tcl': 'tcl',
        'coffee': 'coffee', 'litcoffee': 'coffee',
        'ex': 'elixir', 'exs': 'elixir',
        'dart': 'dart',
        // Shell
        'sh': 'shell', 'bash': 'shell', 'zsh': 'shell', 'fish': 'shell', 'ksh': 'shell',
        'bat': 'bat', 'cmd': 'bat',
        'ps1': 'powershell', 'psm1': 'powershell', 'psd1': 'powershell',
        // Database
        'sql': 'sql',
        'mysql': 'mysql',
        'pgsql': 'pgsql',
        // Config / data
        'yaml': 'yaml', 'yml': 'yaml',
        'toml': 'toml',
        'ini': 'ini', 'cfg': 'ini', 'conf': 'ini', 'properties': 'ini',
        'htaccess': 'ini', 'env': 'ini', 'npmrc': 'ini',
        'gitignore': 'ini', 'gitattributes': 'ini', 'dockerignore': 'ini', 'editorconfig': 'ini',
        'production': 'ini', 'development': 'ini', 'staging': 'ini', 'local': 'ini',
        // Infrastructure
        'tf': 'hcl', 'tfvars': 'hcl', 'hcl': 'hcl',
        'bicep': 'bicep',
        'proto': 'protobuf',
        // Blockchain / niche
        'sol': 'solidity',
        'qs': 'qsharp',
        'wgsl': 'wgsl',
        'sparql': 'sparql', 'rq': 'sparql',
        'cypher': 'cypher',
        // Pascal / Delphi
        'pas': 'pascal', 'pp': 'pascal', 'inc': 'pascal',
        // Scheme / Lisp
        'scm': 'scheme', 'ss': 'scheme', 'rkt': 'scheme',
        // Other
        'abap': 'abap',
        'apex': 'apex', 'cls': 'apex', 'trigger': 'apex',
        'azcli': 'azcli',
        'ecl': 'ecl',
        'st': 'st',
        'sb': 'sb',
        'pq': 'powerquery', 'pqm': 'powerquery',
        'redis': 'redis',
        'razor': 'razor', 'cshtml': 'razor',
        'sv': 'systemverilog', 'svh': 'systemverilog', 'v': 'systemverilog',
        'mips': 'mips', 'asm': 'mips', 's': 'mips',
        // Text / fallback
        'txt': 'text', 'log': 'text', 'nvmrc': 'text', 'browserslistrc': 'text',
    };
    // Check known filenames without extensions
    const filenameMap: Record<string, string> = {
        'makefile': 'makefile',
        'dockerfile': 'dockerfile',
        'containerfile': 'dockerfile',
        'gemfile': 'ruby',
        'rakefile': 'ruby',
        'vagrantfile': 'ruby',
        'procfile': 'text',
        'license': 'text',
        'licence': 'text',
        'changelog': 'markdown',
        'readme': 'markdown',
    };
    return langMap[ext] || filenameMap[baseName] || 'text';
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
    const baseName = filename.split('/').pop()?.toLowerCase() || filename.toLowerCase();
    // If getFileLanguage recognizes it (returns anything other than 'text'), it's previewable
    const hasLanguage = getFileLanguage(filename) !== 'text' || ext === 'txt' || ext === 'log';
    if (hasLanguage) return true;
    const previewableExts = [
        'example', 'sample', 'bak',
        // Images
        'png', 'jpg', 'jpeg', 'gif', 'webp', 'ico', 'bmp',
    ];
    const knownFilenames = [
        'makefile', 'dockerfile', 'containerfile', 'vagrantfile', 'gemfile',
        'rakefile', 'procfile', 'brewfile', 'justfile',
        'license', 'licence', 'authors', 'contributors',
        'changelog', 'changes', 'readme', 'todo',
    ];
    return previewableExts.includes(ext) || knownFilenames.includes(baseName);
};
