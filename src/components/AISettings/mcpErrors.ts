// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { TranslationFunction } from '../../i18n/types';

// Backend MCP commands reject with stable codes; transport detail never reaches the UI.
const ERROR_KEYS: Record<string, string> = {
    MCP_DIRECTORY_INVALID: 'ai.mcpClient.errors.directoryInvalid',
    MCP_DIRECTORY_CHANGED: 'ai.mcpClient.errors.directoryChanged',
    MCP_PERMISSION_COMMAND_REQUIRED: 'ai.mcpClient.errors.permissionCommand',
    MCP_NETWORK_UNDECLARED: 'ai.mcpClient.networkBlocked',
    MCP_INSTALL_UNKNOWN: 'ai.mcpClient.errors.installInvalid',
    MCP_INSTALL_INVALID: 'ai.mcpClient.errors.installInvalid',
    MCP_INSTALL_INTEGRITY: 'ai.mcpClient.errors.installIntegrity',
    MCP_INSTALL_ARCHIVE: 'ai.mcpClient.errors.installArchive',
    MCP_INSTALL_RUNTIME: 'ai.mcpClient.errors.installRuntime',
    MCP_INSTALL_DOWNLOAD: 'ai.mcpClient.errors.installDownload',
    MCP_INSTALL_LIMIT: 'ai.mcpClient.errors.installLimit',
    MCP_INSTALL_EXISTS: 'ai.mcpClient.errors.installExists',
    MCP_INSTALL_IO: 'ai.mcpClient.errors.installIo',
    MCP_INSTALL_BUSY: 'ai.mcpClient.errors.busy',
    MCP_INSTALL_CANCELLED: 'ai.mcpClient.errors.cancelled',
    MCP_OAUTH_DENIED: 'ai.mcpClient.errors.denied',
    MCP_OAUTH_CANCELLED: 'ai.mcpClient.errors.cancelled',
    MCP_OAUTH_TIMEOUT: 'ai.mcpClient.errors.timeout',
    MCP_OAUTH_LOCKED: 'ai.mcpClient.errors.locked',
    MCP_OAUTH_USER_CHANGED: 'ai.mcpClient.errors.userChanged',
    MCP_USER_UNAVAILABLE: 'ai.mcpClient.errors.userChanged',
    MCP_OAUTH_STALE: 'ai.mcpClient.errors.stale',
    MCP_OAUTH_BUSY: 'ai.mcpClient.errors.busy',
    MCP_OAUTH_REAUTHORIZE: 'ai.mcpClient.errors.reauthorize',
    MCP_OAUTH_METADATA: 'ai.mcpClient.errors.metadata',
    MCP_OAUTH_CLIENT: 'ai.mcpClient.errors.client',
    MCP_OAUTH_CALLBACK: 'ai.mcpClient.errors.callback',
    MCP_HTTP_UNREACHABLE: 'ai.mcpClient.errors.unreachable',
    MCP_HTTP_FAILED: 'ai.mcpClient.errors.httpFailed',
    MCP_STORE_UNAVAILABLE: 'ai.mcpClient.errors.store',
    MCP_CONFIG_STALE_REVISION: 'ai.mcpClient.errors.staleRevision',
    MCP_CONFIG_DUPLICATE_ID: 'ai.mcpClient.errors.duplicateId',
    MCP_HTTP_INVALID_ENDPOINT: 'ai.mcpClient.errors.invalidEndpoint',
    MCP_HTTP_SECRET_INVALID: 'ai.mcpClient.errors.invalidSecret',
    MCP_HTTP_INVALID_OAUTH_CLIENT: 'ai.mcpClient.errors.invalidClient',
    MCP_CONFIG_NOT_FOUND: 'ai.mcpClient.errors.notFound',
    MCP_CONFIG_UNAVAILABLE: 'ai.mcpClient.errors.notFound',
    MCP_CONFIG_DISABLED: 'ai.mcpClient.errors.configDisabled',
    MCP_SECRET_UNAVAILABLE: 'ai.mcpClient.errors.secretUnavailable',
    MCP_CALL_INVALID_REQUEST: 'ai.mcpClient.errors.invalidCall',
    MCP_APPROVAL_REQUIRED: 'ai.mcpClient.errors.approvalRequired',
    MCP_OAUTH_REQUIRED: 'ai.mcpClient.errors.oauthRequired',
    MCP_TOOL_SCHEMA_CHANGED: 'ai.mcpClient.errors.schemaChanged',
    MCP_TOOL_ARGUMENTS: 'ai.mcpClient.errors.toolArguments',
    MCP_TOOLS_UNSUPPORTED: 'ai.mcpClient.errors.toolsUnsupported',
    MCP_TOOL_CANCELLED: 'ai.mcpClient.errors.toolCancelled',
    MCP_SERVER_TIMEOUT: 'ai.mcpClient.errors.serverTimeout',
    MCP_STDIO_SANDBOX_UNAVAILABLE: 'ai.mcpClient.errors.sandboxUnavailable',
    MCP_STDIO_START_FAILED: 'ai.mcpClient.errors.startFailed',
    MCP_SERVER_FAILED: 'ai.mcpClient.errors.serverFailed',
    MCP_HTTP_UNAUTHORIZED: 'ai.mcpClient.errors.httpUnauthorized',
    MCP_HTTP_UNSUPPORTED_VERSION: 'ai.mcpClient.errors.unsupportedVersion',
};

export function describeMcpError(t: TranslationFunction, cause: unknown): string {
    const code = typeof cause === 'string' ? cause : cause instanceof Error ? cause.message : String(cause);
    const key = ERROR_KEYS[code];
    return key ? t(key) : t('ai.mcpClient.errors.generic', { code });
}
