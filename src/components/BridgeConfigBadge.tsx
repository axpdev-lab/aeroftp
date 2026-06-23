// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Small inline badge shown next to a local file in AeroFile when that file is
// recognized as a third-party client config (rclone, WinSCP, FileZilla, ...).
// Signals that the file can be imported into AeroFTP; the right-click menu's
// "Import to AeroFTP" entry runs the actual import flow.

import * as React from 'react';
import { FolderInput } from 'lucide-react';
import type { BridgeSourceDescriptor } from './bridge/bridgeSources';

interface Props {
    source: BridgeSourceDescriptor;
    /** Tooltip text, e.g. "rclone - Import to AeroFTP". */
    title?: string;
}

export const BridgeConfigBadge: React.FC<Props> = ({ source, title }) => (
    <span
        title={title}
        className={`inline-flex items-center gap-1 px-1.5 py-px rounded text-[9px] font-medium leading-none flex-shrink-0 ${source.accentBg} ${source.accent}`}
    >
        <FolderInput size={9} className="flex-shrink-0" />
        {source.label}
    </span>
);

export default BridgeConfigBadge;
