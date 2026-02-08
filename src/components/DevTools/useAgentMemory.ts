import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

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
                if (mountedRef.current) setMemory(raw);
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
            if (mountedRef.current) setMemory(raw);
        } catch {
            // Silent failure
        }
    }, [projectPath]);

    return { memory, appendMemory };
}
