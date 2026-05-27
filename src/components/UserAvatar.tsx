// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface UserAvatarProps {
    name: string;
    avatarEmoji?: string | null;
    avatarColor?: string | null;
    size?: 'sm' | 'md' | 'lg';
    className?: string;
}

const SIZE_CLASS: Record<NonNullable<UserAvatarProps['size']>, string> = {
    sm: 'w-6 h-6 text-[11px]',
    md: 'w-8 h-8 text-sm',
    lg: 'w-10 h-10 text-base',
};

const HEX_COLOR_RE = /^#[0-9a-fA-F]{6}$/;

const safeAvatarColor = (color?: string | null): string =>
    color && HEX_COLOR_RE.test(color) ? color : '#3b82f6';

const fallbackInitial = (name: string): string => {
    const trimmed = name.trim();
    return (trimmed[0] || 'U').toUpperCase();
};

const isImageAvatar = (avatar?: string | null): boolean =>
    !!avatar && /^data:image\/(png|jpe?g|gif|webp|svg\+xml);/i.test(avatar);

export const UserAvatar: React.FC<UserAvatarProps> = ({
    name,
    avatarEmoji,
    avatarColor,
    size = 'md',
    className = '',
}) => {
    const imageAvatar = isImageAvatar(avatarEmoji);

    return (
        <span
            className={`${SIZE_CLASS[size]} inline-flex shrink-0 items-center justify-center overflow-hidden rounded-full font-semibold text-white shadow-sm ${className}`}
            style={{ backgroundColor: safeAvatarColor(avatarColor) }}
            aria-hidden="true"
        >
            {imageAvatar ? (
                <img src={avatarEmoji ?? undefined} alt="" className="h-full w-full object-cover" />
            ) : (
                avatarEmoji || fallbackInitial(name)
            )}
        </span>
    );
};

export default UserAvatar;
