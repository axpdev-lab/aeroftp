// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import * as Flags from 'country-flag-icons/react/3x2';

/**
 * SVG country flag keyed by ISO 3166-1 alpha-2 code (or 'EU'). Uses
 * `country-flag-icons`, which renders correctly on Windows (regional
 * indicator emoji do not). Returns null for empty/unknown codes so
 * self-hosted / generic catalog rows render no flag rather than a
 * broken placeholder.
 */
export const CountryFlag: React.FC<{ code: string; className?: string; title?: string }> = ({
    code,
    className = 'w-5 h-3.5 rounded-sm shadow-sm',
    title,
}) => {
    const normalized = (code || '').toUpperCase();
    if (!normalized) return null;
    const FlagComponent = (Flags as Record<string, React.FC<React.SVGProps<SVGSVGElement>>>)[normalized];
    if (!FlagComponent) return null;
    return <FlagComponent className={className} aria-label={title || normalized} />;
};
