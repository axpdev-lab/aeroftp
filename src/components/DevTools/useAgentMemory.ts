import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

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

        invoke<string>('read_agent_memory', { projectPath })
            .then(raw => {
                if (mountedRef.current) setMemory(sanitizeAgentMemory(raw));
            })
            .catch(() => {
                if (mountedRef.current) setMemory('');
            });

        return () => { mountedRef.current = false; };
    }, [projectPath]);

    // Append new entry
    const appendMemory = useCallback(async (entry: string, category: string = 'general') => {
        if (!projectPath) return;
        const formatted = `\n[${new Date().toISOString().slice(0, 16).replace('T', ' ')}] [${category}] ${entry}`;
        try {
            await invoke('write_agent_memory', { projectPath, content: formatted });
            // Reload after write
            const raw = await invoke<string>('read_agent_memory', { projectPath });
            if (mountedRef.current) setMemory(sanitizeAgentMemory(raw));
        } catch {
            // Silent failure
        }
    }, [projectPath]);

    return { memory, appendMemory };
}
