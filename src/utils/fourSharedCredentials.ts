// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { loadOAuthClientCredentials } from './oauthClientCredentials';

interface FourSharedCredentials {
    consumerKey: string;
    consumerSecret: string;
}

export async function loadFourSharedCredentials(): Promise<FourSharedCredentials> {
    const { clientId, clientSecret } = await loadOAuthClientCredentials('fourshared');
    return { consumerKey: clientId, consumerSecret: clientSecret };
}
