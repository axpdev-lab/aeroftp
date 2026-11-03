// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useMemo, useState } from 'react';
import { ArrowDownLeft, ArrowUpRight, Check, LucideIcon } from 'lucide-react';
import { ServerProfile } from '../../types';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { SearchBox } from '../SearchBox';

// Protocol gradient colors: same as SavedServers
const PROTOCOL_COLORS: Record<string, string> = {
    ftp: 'from-blue-500 to-cyan-400',
    ftps: 'from-green-500 to-emerald-400',
    sftp: 'from-purple-500 to-violet-400',
    scp: 'from-purple-500 to-violet-400',
    webdav: 'from-orange-500 to-amber-400',
    s3: 'from-amber-500 to-yellow-400',
    aerocloud: 'from-sky-400 to-blue-500',
    googledrive: 'from-red-500 to-red-400',
    dropbox: 'from-blue-600 to-blue-400',
    onedrive: 'from-sky-500 to-sky-400',
    mega: 'from-red-600 to-red-500',
    box: 'from-blue-500 to-blue-600',
    pcloud: 'from-green-500 to-teal-400',
    azure: 'from-blue-600 to-indigo-500',
    filen: 'from-emerald-500 to-green-400',
    internxt: 'from-blue-500 to-blue-400',
    kdrive: 'from-blue-500 to-sky-400',
    drime: 'from-green-500 to-emerald-400',
    filelu: 'from-sky-500 to-cyan-400',
    koofr: 'from-green-500 to-emerald-400',
    opendrive: 'from-cyan-500 to-sky-400',
    yandexdisk: 'from-yellow-500 to-amber-400',
    fourshared: 'from-blue-500 to-cyan-400',
    zohoworkdrive: 'from-yellow-500 to-orange-400',
    github: 'from-gray-700 to-gray-500',
    googlephotos: 'from-amber-500 to-amber-400',
    immich: 'from-indigo-500 to-purple-400',
    twake: 'from-sky-500 to-indigo-400',
    imagekit: 'from-blue-500 to-cyan-400',
    uploadcare: 'from-emerald-500 to-green-400',
};

export const ProfileIcon: React.FC<{ profile: ServerProfile }> = ({ profile }) => {
    const logoKey = profile.providerId || profile.protocol || '';
    const LogoComponent = PROVIDER_LOGOS[logoKey];
    const hasLogo = !!LogoComponent;
    const hasCustomIcon = !!profile.customIconUrl;
    const hasFavicon = !!profile.faviconUrl;
    const hasIcon = hasCustomIcon || hasFavicon;

    return (
        <span className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-lg ${
            hasIcon || hasLogo
                ? 'bg-[#FFFFF0] dark:bg-gray-600 border border-gray-200 dark:border-gray-500'
                : `bg-gradient-to-br ${PROTOCOL_COLORS[profile.protocol || 'ftp'] || PROTOCOL_COLORS.ftp} text-white`
        }`}>
            {hasCustomIcon ? (
                <img src={profile.customIconUrl} alt="" className="w-6 h-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
            ) : hasFavicon ? (
                <img src={profile.faviconUrl} alt="" className="w-6 h-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
            ) : hasLogo ? (
                <LogoComponent size={20} />
            ) : (
                <span className="font-bold">{(profile.name || profile.host || '?').charAt(0).toUpperCase()}</span>
            )}
        </span>
    );
};

/**
 * What a selection means, which sets the accent of the selected card: indigo
 * with an outgoing arrow for a transfer source, emerald with an incoming arrow
 * for a destination (the colors MyServersPanel/ServerCard use), cyan with a
 * check for a plain single choice.
 */
export type ProfileRailRole = 'source' | 'destination' | 'choice';

const ROLE_STYLE: Record<ProfileRailRole, { ring: string; badge: string; Icon: LucideIcon }> = {
    source: {
        ring: 'bg-indigo-50 ring-1 ring-indigo-500 dark:bg-indigo-900/30 dark:ring-indigo-400',
        badge: 'bg-indigo-500 text-white ring-1 ring-indigo-400/60',
        Icon: ArrowUpRight,
    },
    destination: {
        ring: 'bg-emerald-50 ring-1 ring-emerald-500 dark:bg-emerald-900/30 dark:ring-emerald-400',
        badge: 'bg-emerald-500 text-white ring-1 ring-emerald-400/60',
        Icon: ArrowDownLeft,
    },
    choice: {
        ring: 'bg-cyan-50 ring-1 ring-cyan-500 dark:bg-cyan-900/30 dark:ring-cyan-400',
        badge: 'bg-cyan-500 text-white ring-1 ring-cyan-400/60',
        Icon: Check,
    },
};

/** Whether `profile` matches a search query: name, host, user or protocol. */
export function profileMatchesQuery(profile: ServerProfile, query: string): boolean {
    const needle = query.trim().toLowerCase();
    if (!needle) return true;
    return [profile.name, profile.host, profile.username, profile.protocol, profile.providerId]
        .some(field => !!field && field.toLowerCase().includes(needle));
}

interface ProfileRailProps {
    title: string;
    profiles: ServerProfile[];
    selected: ServerProfile | null;
    onSelect: (profile: ServerProfile) => void;
    emptyMessage: string;
    role: ProfileRailRole;
    /** Renders a search field above the list; the placeholder is its label. */
    searchPlaceholder?: string;
    /** Shown in place of the list when the search matches nothing. */
    noMatchMessage?: string;
    /** Extra marks next to the protocol tag (e.g. a maturity badge). */
    renderBadges?: (profile: ServerProfile) => React.ReactNode;
    /** Height cap of the scrolling list. */
    listClassName?: string;
}

export const ProfileRail: React.FC<ProfileRailProps> = ({
    title, profiles, selected, onSelect, emptyMessage, role,
    searchPlaceholder, noMatchMessage, renderBadges, listClassName = 'max-h-60',
}) => {
    const [query, setQuery] = useState('');
    const style = ROLE_STYLE[role];
    const Icon = style.Icon;
    // Promote the selected profile to the top of the rail so it is always
    // visible without scrolling, both when the modal opens with an
    // auto-prefilled selection and after a manual click in the rail itself.
    const orderedProfiles = useMemo(() => {
        const visible = profiles.filter(p => profileMatchesQuery(p, query));
        if (!selected) return visible;
        const idx = visible.findIndex(p => p.id === selected.id);
        if (idx <= 0) return visible;
        return [visible[idx], ...visible.slice(0, idx), ...visible.slice(idx + 1)];
    }, [profiles, selected, query]);
    return (
        <div className="rounded-lg border border-gray-200 dark:border-gray-700">
            <div className="flex items-center justify-between border-b border-gray-200 px-3 py-2 dark:border-gray-700">
                <span className="text-xs font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                    {title}
                </span>
                {selected && (
                    <span className="text-xs text-gray-500 dark:text-gray-400 truncate max-w-[140px]">
                        {selected.name}
                    </span>
                )}
            </div>
            {searchPlaceholder && profiles.length > 0 && (
                <SearchBox
                    value={query}
                    onChange={setQuery}
                    onClear={() => setQuery('')}
                    placeholder={searchPlaceholder}
                    containerClassName="px-3 pt-2"
                    className="w-full h-8 pl-3 pr-7 text-sm rounded-lg bg-gray-100 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:border-cyan-400 dark:focus:border-cyan-500 focus:ring-1 focus:ring-cyan-400/25 transition-colors"
                />
            )}
            <div className={`${listClassName} overflow-y-auto p-2 space-y-1`}>
                {orderedProfiles.map((profile) => {
                    const isSelected = selected?.id === profile.id;
                    return (
                        <button
                            key={profile.id}
                            type="button"
                            onClick={() => onSelect(profile)}
                            aria-pressed={isSelected}
                            className={`relative w-full rounded-lg px-2 py-2 text-left transition flex items-center gap-3 ${
                                isSelected ? style.ring : 'hover:bg-gray-50 dark:hover:bg-gray-700/50'
                            }`}
                        >
                            <ProfileIcon profile={profile} />
                            {isSelected && (
                                <span
                                    className={`absolute -top-1.5 -left-1.5 flex items-center justify-center w-5 h-5 rounded-full shadow ${style.badge}`}
                                    aria-hidden="true"
                                >
                                    <Icon size={12} strokeWidth={2.5} />
                                </span>
                            )}
                            <div className="min-w-0 flex-1">
                                <div className="font-medium flex items-center gap-2">
                                    <span className="truncate text-sm text-gray-900 dark:text-white">{profile.name}</span>
                                    <span className="text-xs px-1.5 py-0.5 rounded font-medium uppercase bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300">
                                        {profile.protocol || 'ftp'}
                                    </span>
                                    {renderBadges?.(profile)}
                                </div>
                                {profile.initialPath && (
                                    <div className="text-xs text-gray-500 dark:text-gray-400 truncate mt-0.5">
                                        {profile.initialPath}
                                    </div>
                                )}
                            </div>
                        </button>
                    );
                })}
                {profiles.length === 0 && (
                    <div className="rounded-lg border border-dashed border-gray-300 px-3 py-4 text-center text-sm text-gray-500 dark:border-gray-600 dark:text-gray-400">
                        {emptyMessage}
                    </div>
                )}
                {profiles.length > 0 && orderedProfiles.length === 0 && (
                    <div className="px-3 py-4 text-center text-sm text-gray-500 dark:text-gray-400">
                        {noMatchMessage ?? emptyMessage}
                    </div>
                )}
            </div>
        </div>
    );
};
