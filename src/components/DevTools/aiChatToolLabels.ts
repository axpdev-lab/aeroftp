/**
 * Shared helper for getting localized tool labels across ToolApproval,
 * BatchToolApproval, and MarkdownRenderer.
 *
 * This eliminates 3 duplicated toolLabels maps and centralizes i18n.
 */

/**
 * Get the localized label for a tool call.
 *
 * @param toolName - The tool name (e.g., "remote_list", "local_edit")
 * @param t - The translation function from useI18n().t
 * @returns Localized tool label (e.g., "List Files", "Local Edit")
 */
export function getToolLabel(
    toolName: string,
    t: (key: string, params?: Record<string, string | number>) => string
): string {
    const key = `ai.toolLabels.${toolName}`;
    const translated = t(key);

    // If translation key doesn't exist, t() returns the key itself.
    // Fallback to the toolName as-is for unknown tools.
    return translated !== key ? translated : toolName;
}
