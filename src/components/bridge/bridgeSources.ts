// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Descriptor table for every profile-bridge source surfaced in the
// Export/Import dialog. All 15 sources are handled generically by
// BridgeSourcePanel + the bridge_* Tauri commands.

export interface BridgeSourceDescriptor {
    id: string;
    label: string;
    accent: string;   // tailwind text color for the icon
    accentBg: string; // tailwind bg for the icon chip
}

// Order chosen to group conceptually: full-fidelity clients first
// (rclone, SSH/FTP clients), then cloud CLIs, site/IDE, backup tools.
export const GENERIC_BRIDGE_SOURCES: BridgeSourceDescriptor[] = [
    { id: 'rclone', label: 'rclone', accent: 'text-orange-500', accentBg: 'bg-orange-100 dark:bg-orange-900/30' },
    { id: 'winscp', label: 'WinSCP', accent: 'text-purple-500', accentBg: 'bg-purple-100 dark:bg-purple-900/30' },
    { id: 'filezilla', label: 'FileZilla', accent: 'text-emerald-500', accentBg: 'bg-emerald-100 dark:bg-emerald-900/30' },
    { id: 'aws', label: 'AWS CLI', accent: 'text-yellow-600', accentBg: 'bg-yellow-100 dark:bg-yellow-900/30' },
    { id: 'mc', label: 'MinIO Client', accent: 'text-red-500', accentBg: 'bg-red-100 dark:bg-red-900/30' },
    { id: 's3cmd', label: 's3cmd', accent: 'text-amber-500', accentBg: 'bg-amber-100 dark:bg-amber-900/30' },
    { id: 'ssh', label: 'OpenSSH', accent: 'text-sky-500', accentBg: 'bg-sky-100 dark:bg-sky-900/30' },
    { id: 'putty', label: 'PuTTY', accent: 'text-indigo-500', accentBg: 'bg-indigo-100 dark:bg-indigo-900/30' },
    { id: 'mobaxterm', label: 'MobaXterm', accent: 'text-green-600', accentBg: 'bg-green-100 dark:bg-green-900/30' },
    { id: 'lftp', label: 'lftp', accent: 'text-teal-500', accentBg: 'bg-teal-100 dark:bg-teal-900/30' },
    { id: 'cyberduck', label: 'Cyberduck', accent: 'text-yellow-500', accentBg: 'bg-yellow-100 dark:bg-yellow-900/30' },
    { id: 'dreamweaver', label: 'Dreamweaver', accent: 'text-pink-500', accentBg: 'bg-pink-100 dark:bg-pink-900/30' },
    { id: 'kopia', label: 'Kopia', accent: 'text-blue-500', accentBg: 'bg-blue-100 dark:bg-blue-900/30' },
    { id: 'restic', label: 'restic', accent: 'text-cyan-600', accentBg: 'bg-cyan-100 dark:bg-cyan-900/30' },
    { id: 'duplicacy', label: 'Duplicacy', accent: 'text-fuchsia-500', accentBg: 'bg-fuchsia-100 dark:bg-fuchsia-900/30' },
];

export interface BridgeSourceMeta {
    source: string;
    supportedProtocols: string[];
    importFilterName: string;
    importExtensions: string[];
    exportExt: string;
    exportLabel: string;
    secretPolicy: 'full' | 'limited' | 'metadata';
}
