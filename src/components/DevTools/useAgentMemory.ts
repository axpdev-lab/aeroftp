// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface AgentMemoryEntry {
    id: number;
    category: string;
    content: string;
    created_at: number;
}

/**
 * Sanitize agent memory content to prevent prompt injection.
 * Strips lines that contain common system prompt override patterns.
 * AA-SEC-007: Agent memory prompt injection prevention.
 */
export function sanitizeAgentMemory(raw: string): string {
    if (!raw) return raw;
    const injectionLinePatterns = [
        /^\s*(SYSTEM|IMPORTANT|OVERRIDE|INSTRUCTION)\s*:/i,
        /ignore\s+(all\s+)?previous/i,
        /ignore\s+(all\s+)?above/i,
        /disregard\s+(all\s+)?previous/i,
        /disregard\s+(all\s+)?above/i,
        /you\s+are\s+now\s+/i,
        /new\s+instructions?\s*:/i,
        /system\s+override/i,
    ];

    return raw
        .split('\n')
        .filter(line => !injectionLinePatterns.some(p => p.test(line)))
        .join('\n');
}

export function useAgentMemory(projectPath: string | undefined) {
    const [memory, setMemory] = useState<string>('');
    const memoryLoadedRef = useRef(false);
    const lastPathRef = useRef<string | undefined>(undefined);
    const mountedRef = useRef(true);

    const formatEntries = useCallback((entries: AgentMemoryEntry[]) => {
        return sanitizeAgentMemory(
            entries
                .map(entry => `[${new Date(entry.created_at * 1000).toISOString().slice(0, 16).replace('T', ' ')}] [${entry.category}] ${entry.content}`)
                .join('\n'),
        );
    }, []);

    // Auto-load on mount/path change
    useEffect(() => {
        mountedRef.current = true;

        if (!projectPath) {
            memoryLoadedRef.current = false;
            return () => { mountedRef.current = false; };
        }
        if (lastPathRef.current === projectPath && memoryLoadedRef.current) {
            return () => { mountedRef.current = false; };
        }

        lastPathRef.current = projectPath;
        memoryLoadedRef.current = true;

        invoke<AgentMemoryEntry[]>('agent_memory_search', { projectPath, query: null, limit: 10 })
            .then(entries => {
                if (mountedRef.current) setMemory(formatEntries(entries));
            })
            .catch(() => {
                if (mountedRef.current) setMemory('');
            });

        return () => { mountedRef.current = false; };
    }, [projectPath]);

    // Append new entry
    const appendMemory = useCallback(async (entry: string, category: string = 'general') => {
        if (!projectPath) return;
        try {
            await invoke('agent_memory_store', { projectPath, category, content: entry, serverHost: null });
            // Reload after write
            const entries = await invoke<AgentMemoryEntry[]>('agent_memory_search', { projectPath, query: null, limit: 10 });
            if (mountedRef.current) setMemory(formatEntries(entries));
        } catch {
            // Silent failure
        }
    }, [formatEntries, projectPath]);

    const searchMemory = useCallback(async (query: string, limit = 5) => {
        if (!projectPath) return '';
        try {
            const entries = await invoke<AgentMemoryEntry[]>('agent_memory_search', { projectPath, query, limit });
            return formatEntries(entries);
        } catch {
            return '';
        }
    }, [formatEntries, projectPath]);

    return { memory, appendMemory, searchMemory };
}
