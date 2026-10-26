// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement as h } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { AIChat } from './AIChat';
import { getDefaultAISettings, type AISettings } from '../../types/ai';

const backend = vi.hoisted(() => ({ settings: '', readErrors: 0, configReads: 0 }));
const conversations = vi.hoisted(() => ({
    messages: [], conversations: [], activeConversationId: null,
    showHistory: false, showExportMenu: false, expandedMessages: new Set<string>(), activeBranchId: null,
    messagesRef: { current: [] }, conversationsRef: { current: [] },
    setMessages: vi.fn(), setConversations: vi.fn(), setShowHistory: vi.fn(), setShowExportMenu: vi.fn(),
    setExpandedMessages: vi.fn(), persistConversation: vi.fn(), startNewChat: vi.fn(), switchConversation: vi.fn(),
    handleDeleteConversation: vi.fn(), loadChatHistory: vi.fn(), exportConversation: vi.fn(), conversationUsageKey: () => 'test',
    forkConversation: vi.fn(), switchBranch: vi.fn(), deleteBranch: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async (command: string, args?: { account?: string }) => {
    if (command === 'get_credential' && args?.account === 'config_ai_settings') {
        backend.configReads++;
        if (backend.readErrors > 0) {
            backend.readErrors--;
            throw new Error('STORE_NOT_READY');
        }
        return backend.settings;
    }
    if (command === 'get_credential') return 'FIXTURE-KEY';
    if (command === 'list_plugins') return [];
    return undefined;
}) }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../../hooks/useTauriListener', () => ({ createTauriListener: () => () => {}, useTauriListener: () => {} }));
vi.mock('./useAIChatConversations', () => ({ useAIChatConversations: () => conversations }));
vi.mock('./useAIChatImages', () => ({ useAIChatImages: () => ({
    attachedImages: [], handleImagePick: vi.fn(), handlePaste: vi.fn(), removeImage: vi.fn(), clearImages: vi.fn(),
}) }));
vi.mock('./useAgentMemory', () => ({ useAgentMemory: () => ({ memory: null, searchMemory: vi.fn(), refreshMemory: vi.fn() }) }));
vi.mock('./useKeyboardShortcuts', () => ({ useKeyboardShortcuts: () => {}, getDefaultShortcuts: () => ({}) }));
vi.mock('./aiChatMcp', () => ({
    MCP_SERVERS_CHANGED: 'mcp-servers-changed', mcpSnapshotLoader: () => ({ load: vi.fn(), stop: vi.fn() }),
    notifyMcpServersChanged: vi.fn(), runMcpTool: vi.fn(),
}));
vi.mock('../AISettings', () => ({ AISettingsPanel: () => null }));
vi.mock('./AIChatHeader', () => ({ AIChatHeader: () => null }));
vi.mock('./ChatSearchOverlay', () => ({ ChatSearchOverlay: () => null }));
vi.mock('./ChatHistoryManager', () => ({ ChatHistoryManager: () => null }));
vi.mock('./PromptTemplateSelector', () => ({ default: () => null }));
vi.mock('./TokenBudgetIndicator', () => ({ TokenBudgetIndicator: () => null }));

function settingsWithModel(displayName: string): AISettings {
    const settings = getDefaultAISettings();
    settings.providers = [{ id: 'p', name: 'Fixture Provider', type: 'custom', baseUrl: 'http://127.0.0.1:1',
        isEnabled: true, isDefault: true, createdAt: new Date(0), updatedAt: new Date(0) }];
    settings.models = [{ id: 'm', providerId: 'p', name: 'fixture-model', displayName, isEnabled: true, isDefault: true,
        maxTokens: 4096, supportsStreaming: true, supportsTools: false, supportsVision: false }];
    return settings;
}

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => window.setTimeout(callback, 0));
    vi.stubGlobal('cancelAnimationFrame', (id: number) => window.clearTimeout(id));
    localStorage.clear();
    backend.readErrors = 0;
    backend.configReads = 0;
    backend.settings = '';
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.unstubAllGlobals();
});

it('reconciles fallback models through loadModels when the vault becomes ready', async () => {
    localStorage.setItem('aeroftp_ai_settings', JSON.stringify(settingsWithModel('Fallback Model')));
    backend.settings = JSON.stringify(settingsWithModel('Vault Model'));
    backend.readErrors = 99;
    await act(async () => root.render(h(AIChat)));

    expect(host.textContent).toContain('Fallback Model');
    expect(host.textContent).not.toContain('Vault Model');
    backend.readErrors = 0;
    await act(async () => { window.dispatchEvent(new Event('aeroftp-vault-ready')); });

    expect(host.textContent).toContain('Vault Model');
    expect(host.textContent).not.toContain('Fallback Model');
    expect(backend.configReads).toBe(2);
});
