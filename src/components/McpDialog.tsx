// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { X, Copy, Check, ExternalLink } from 'lucide-react';
import { useTranslation } from '../i18n';
import { openUrl } from '../utils/openUrl';
import { useDraggableModal } from '../hooks/useDraggableModal';

interface McpDialogProps {
    isOpen: boolean;
    onClose: () => void;
}

const McpDialog: React.FC<McpDialogProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [copiedField, setCopiedField] = React.useState<string | null>(null);

    React.useEffect(() => {
        if (!isOpen) return;
        const handleKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        document.addEventListener('keydown', handleKey);
        return () => document.removeEventListener('keydown', handleKey);
    }, [isOpen, onClose]);

    if (!isOpen) return null;

    const copyToClipboard = async (text: string, field: string) => {
        try {
            await navigator.clipboard.writeText(text);
            setCopiedField(field);
            setTimeout(() => setCopiedField(null), 2000);
        } catch { /* ignore */ }
    };

    const mcpConfig = `{
  "mcpServers": {
    "aeroftp": {
      "command": "aeroftp-cli",
      "args": ["mcp"]
    }
  }
}`;

    const targets = [
        { tool: 'Claude Code', path: '~/.claude/.mcp.json' },
        { tool: 'Claude Desktop (Win)', path: '%APPDATA%\\Claude\\claude_desktop_config.json' },
        { tool: 'Claude Desktop (Mac)', path: '~/Library/Application Support/Claude/claude_desktop_config.json' },
        { tool: 'Cursor', path: '~/.cursor/mcp.json' },
        { tool: 'Windsurf', path: '~/.codeium/windsurf/mcp_config.json' },
    ];

    return (
        <div
            className="fixed inset-0 z-[9999] flex items-start justify-center pt-[5vh]"
            onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
        >
            <div className="absolute inset-0 bg-black/50" onClick={onClose} />
            <div
                {...modalDrag.panelProps}
                className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-full max-w-lg overflow-hidden flex flex-col animate-scale-in"
                style={{ maxHeight: '85vh', ...modalDrag.panelProps.style }}
                role="dialog"
                aria-modal="true"
            >
                {/* Header */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 shrink-0 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2.5 pointer-events-none">
                        <img src="/icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP" className="w-6 h-6 object-contain" />
                        <div>
                            <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">
                                {t('mcp.title')}
                            </h2>
                            <p className="text-[10px] text-gray-400">
                                Model Context Protocol
                            </p>
                        </div>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 cursor-pointer"
                        title={t('common.close')}
                    >
                        <X size={16} className="text-gray-500 dark:text-gray-400" />
                    </button>
                </div>

                {/* Content */}
                <div className="px-5 py-4 overflow-y-auto flex-1">
                    <p className="text-xs text-gray-500 dark:text-gray-400 mb-4">
                        {t('mcp.description')}
                    </p>

                    {/* Step 1: VS Code Extension */}
                    <div className="mb-4">
                        <h3 className="text-xs font-semibold text-gray-900 dark:text-gray-100 mb-2 flex items-center gap-2">
                            <span className="w-5 h-5 rounded-full bg-blue-500 text-white text-[10px] flex items-center justify-center font-bold">1</span>
                            {t('mcp.stepExtension')}
                        </h3>
                        <div className="flex items-center gap-2">
                            <code className="flex-1 text-xs bg-gray-100 dark:bg-gray-700 px-3 py-2 rounded-lg text-gray-900 dark:text-gray-100 font-mono">
                                ext install axpdev-lab.aeroftp-mcp
                            </code>
                            <button
                                onClick={() => copyToClipboard('ext install axpdev-lab.aeroftp-mcp', 'ext')}
                                className="w-8 h-8 flex items-center justify-center rounded-lg hover:bg-gray-100 dark:bg-gray-700 transition-colors cursor-pointer shrink-0"
                                title={t('common.copy')}
                            >
                                {copiedField === 'ext' ? <Check size={14} className="text-green-500" /> : <Copy size={14} className="text-gray-400 dark:text-gray-500" />}
                            </button>
                        </div>
                    </div>

                    {/* Step 2: Command Palette */}
                    <div className="mb-4">
                        <h3 className="text-xs font-semibold text-gray-900 dark:text-gray-100 mb-2 flex items-center gap-2">
                            <span className="w-5 h-5 rounded-full bg-blue-500 text-white text-[10px] flex items-center justify-center font-bold">2</span>
                            {t('mcp.stepConfigure')}
                        </h3>
                        <div className="flex items-center gap-2">
                            <code className="flex-1 text-xs bg-gray-100 dark:bg-gray-700 px-3 py-2 rounded-lg text-gray-900 dark:text-gray-100 font-mono">
                                Ctrl+Shift+P &rarr; AeroFTP: Install MCP Server
                            </code>
                            <button
                                onClick={() => copyToClipboard('AeroFTP: Install MCP Server', 'cmd')}
                                className="w-8 h-8 flex items-center justify-center rounded-lg hover:bg-gray-100 dark:bg-gray-700 transition-colors cursor-pointer shrink-0"
                                title={t('common.copy')}
                            >
                                {copiedField === 'cmd' ? <Check size={14} className="text-green-500" /> : <Copy size={14} className="text-gray-400 dark:text-gray-500" />}
                            </button>
                        </div>
                    </div>

                    {/* Separator */}
                    <div className="border-t border-gray-200 dark:border-gray-700 my-4" />

                    {/* Manual Config */}
                    <div className="mb-4">
                        <h3 className="text-xs font-semibold text-gray-900 dark:text-gray-100 mb-2">
                            {t('mcp.manualConfig')}
                        </h3>
                        <div className="relative">
                            <pre className="text-xs bg-gray-100 dark:bg-gray-700 px-3 py-2.5 rounded-lg text-gray-900 dark:text-gray-100 font-mono overflow-x-auto">
                                {mcpConfig}
                            </pre>
                            <button
                                onClick={() => copyToClipboard(mcpConfig, 'json')}
                                className="absolute top-2 right-2 w-7 h-7 flex items-center justify-center rounded-md hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
                                title={t('common.copy')}
                            >
                                {copiedField === 'json' ? <Check size={12} className="text-green-500" /> : <Copy size={12} className="text-gray-400 dark:text-gray-500" />}
                            </button>
                        </div>
                    </div>

                    {/* Config Paths */}
                    <div>
                        <h3 className="text-xs font-semibold text-gray-900 dark:text-gray-100 mb-2">
                            {t('mcp.configPaths')}
                        </h3>
                        <div className="space-y-1.5">
                            {targets.map(({ tool, path }) => (
                                <div key={tool} className="flex items-center justify-between text-xs gap-2">
                                    <span className="text-gray-500 dark:text-gray-400 shrink-0">{tool}</span>
                                    <code className="text-gray-400 dark:text-gray-500 font-mono text-[10px] truncate">{path}</code>
                                </div>
                            ))}
                        </div>
                    </div>
                </div>

                {/* Footer */}
                <div className="px-6 py-3 border-t border-gray-200 dark:border-gray-700 flex items-center justify-between shrink-0">
                    <div className="flex items-center gap-3">
                        <button
                            onClick={() => openUrl('https://marketplace.visualstudio.com/items?itemName=axpdev-lab.aeroftp-mcp')}
                            className="text-xs text-blue-500 hover:text-blue-400 flex items-center gap-1 cursor-pointer"
                        >
                            VS Code Marketplace <ExternalLink size={10} />
                        </button>
                        <button
                            onClick={() => openUrl('https://docs.aeroftp.app/mcp/overview')}
                            className="text-xs text-blue-500 hover:text-blue-400 flex items-center gap-1 cursor-pointer"
                        >
                            {t('mcp.docs')} <ExternalLink size={10} />
                        </button>
                    </div>
                    <button
                        onClick={onClose}
                        className="px-3 py-1.5 text-xs rounded-lg bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-900 dark:text-gray-100 transition-colors cursor-pointer"
                    >
                        {t('common.close')}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default McpDialog;
