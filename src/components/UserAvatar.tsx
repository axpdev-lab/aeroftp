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

/** A self-contained image the picker produced from an inline SVG logo. */
const DATA_IMAGE_RE = /^data:image\/(png|jpe?g|gif|webp|svg\+xml)[;,]/i;

/**
 * A logo that ships with the app, addressed by its path under `public/icons/`.
 *
 * The picker cannot always produce a data URL: providers whose logo is a PNG
 * (Hetzner, FileLu, AWS, MinIO, Koofr, Blomp, OpenDrive...) render as `<img>`
 * rather than as an inline `<svg>`, so what gets stored is the asset's own
 * path. Those avatars used to fall through to the text branch and render the
 * path as a string, which is issue #550: some provider icons "did not render".
 *
 * Deliberately a strict allowlist rather than a general URL test. The value
 * reaches us from stored user data, so anything with a scheme or a
 * protocol-relative prefix stays rejected: an avatar must never be able to
 * fetch from a host we did not ship, which would turn a user record into a
 * tracking pixel.
 */
const APP_ICON_PATH_RE = /^\/icons\/[A-Za-z0-9._/-]+\.(png|jpe?g|gif|webp|svg)$/i;

export const isImageAvatar = (avatar?: string | null): boolean => {
    if (!avatar) return false;
    if (DATA_IMAGE_RE.test(avatar)) return true;
    return APP_ICON_PATH_RE.test(avatar) && !avatar.includes('..');
};

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
                // `contain`, not `cover`: an avatar is a logo, and `cover` crops
                // whatever does not fit the square. On a logo with content at the
                // edge that removes part of the mark rather than some background
                // — AeroFTP's own rocket lost its red tip (#550).
                <img src={avatarEmoji ?? undefined} alt="" className="h-full w-full object-contain" />
            ) : (
                avatarEmoji || fallbackInitial(name)
            )}
        </span>
    );
};

export default UserAvatar;
