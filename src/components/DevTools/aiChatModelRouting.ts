// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { AISettings } from '../../types/ai';
import { SelectedModel } from './aiChatTypes';
import { detectTaskType } from './aiChatUtils';

/** Resolve a stored model id into the shape the send path needs, or null when
 *  the model or its provider no longer exists. */
export function resolveModelById(settings: AISettings, modelId?: string): SelectedModel | null {
    if (!modelId) return null;
    const model = settings.models.find(m => m.id === modelId);
    if (!model) return null;
    const provider = settings.providers.find(p => p.id === model.providerId);
    if (!provider) return null;
    return {
        providerId: provider.id,
        providerName: provider.name,
        providerType: provider.type,
        modelId: model.id,
        modelName: model.name,
        displayName: model.displayName,
    };
}

export interface RoutedModels {
    /** The model the request is attempted with first. */
    primary: SelectedModel | null;
    /** The routing rule's fallback, tried once if `primary` fails with a
     *  failover-worthy error. Null unless auto-routing picked the primary. */
    fallback: SelectedModel | null;
}

/**
 * Resolve which model handles this prompt.
 *
 * An explicit pick in the chat header always wins and carries no fallback:
 * the user chose that model and a silent switch would contradict the choice.
 * Otherwise auto-routing classifies the prompt, and a matching rule supplies
 * both the preferred model and its optional fallback. With no matching rule
 * the default model answers, again with no fallback, because the fallback is
 * a property of the rule rather than of the conversation.
 */
export function resolveRoutedModels(
    selectedModel: SelectedModel | null,
    settings: AISettings,
    prompt: string,
): RoutedModels {
    if (selectedModel) {
        return { primary: selectedModel, fallback: null };
    }
    if (!settings.autoRouting?.enabled) {
        return { primary: resolveModelById(settings, settings.defaultModelId ?? undefined), fallback: null };
    }

    const rule = settings.autoRouting.rules.find(r => r.taskType === detectTaskType(prompt));
    const preferred = resolveModelById(settings, rule?.preferredModelId);
    if (preferred) {
        const fallback = resolveModelById(settings, rule?.fallbackModelId);
        // A fallback identical to the preferred model would retry the same
        // endpoint that just failed, so drop it rather than burn a second call.
        return {
            primary: preferred,
            fallback: fallback && fallback.modelId !== preferred.modelId ? fallback : null,
        };
    }

    // The rule pointed at a model that no longer resolves: its fallback is the
    // next thing the user asked for, ahead of the global default.
    const ruleFallback = resolveModelById(settings, rule?.fallbackModelId);
    if (ruleFallback) {
        return { primary: ruleFallback, fallback: null };
    }

    return { primary: resolveModelById(settings, settings.defaultModelId ?? undefined), fallback: null };
}
