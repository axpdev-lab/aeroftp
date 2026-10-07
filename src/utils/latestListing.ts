// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Only the latest listing may commit its result, error or loading completion. */
export class LatestListing {
    private generation = 0;

    async run<T>(load: (isCurrent: () => boolean) => Promise<T | null>, commit: (value: T) => void,
        loading: (value: boolean) => void): Promise<boolean> {
        const generation = ++this.generation;
        const isCurrent = () => generation === this.generation;
        loading(true);
        try {
            const value = await load(isCurrent);
            if (!isCurrent() || value === null) return false;
            commit(value);
            return true;
        } catch (error) {
            if (!isCurrent()) return false;
            throw error;
        } finally {
            if (isCurrent()) loading(false);
        }
    }
}
