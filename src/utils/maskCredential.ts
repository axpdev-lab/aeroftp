// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Masks sensitive credentials for safe display in activity logs.
 *
 * Rules:
 * - HTTP(S) URL: shown as host + path unmasked (the URL is public
 *   information; e.g. ImageKit's "URL Endpoint ID" is what the
 *   end-user copy-pastes from a public dashboard, and naive masking
 *   would render it as the unhelpful "htt***")
 * - S3 access key (starts with AKIA, 20+ chars): AKIAD...ICIYF (first 5 + last 4)
 * - Email (contains @): ale***@proton.me (first 3 before @ + *** + @domain)
 * - Short value (<=3 chars): *** (fully masked)
 * - Generic username: ale*** (first 3 + ***)
 */
export function maskCredential(value: string): string {
    if (!value) return value;

    const trimmed = value.trim();
    if (trimmed.length === 0) return value;

    // HTTP(S) URL: not a secret, render host + path so providers that
    // model their account identifier as a URL (ImageKit, Uploadcare,
    // self-hosted WebDAV ...) still log a useful Authenticated-as line.
    if (/^https?:\/\//i.test(trimmed)) {
        try {
            const url = new URL(trimmed);
            const path = url.pathname.replace(/\/+$/, '');
            return `${url.host}${path}`;
        } catch {
            // Malformed URL: fall through to the generic rule.
        }
    }

    // S3 access key: starts with AKIA and is 20+ chars
    if (/^AKIA[A-Z0-9]{16,}$/i.test(trimmed)) {
        return `${trimmed.slice(0, 5)}...${trimmed.slice(-4)}`;
    }

    // Email: show first 3 chars of local part + *** + @domain
    const atIdx = trimmed.indexOf('@');
    if (atIdx > 0) {
        const local = trimmed.slice(0, atIdx);
        const domain = trimmed.slice(atIdx);
        const visible = Math.min(3, local.length);
        return `${local.slice(0, visible)}***${domain}`;
    }

    // Short username (<=3 chars): fully mask
    if (trimmed.length <= 3) {
        return '***';
    }

    // Generic username: first 3 chars + ***
    return `${trimmed.slice(0, 3)}***`;
}
