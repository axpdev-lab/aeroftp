import { getProviderById } from '../providers';

/**
 * Blomp's Keystone, read from the preset rather than spelled a second time.
 *
 * The Swift form is reached through the Blomp tile, but Swift is a protocol and
 * the auth URL is editable, so the same form can be pointed at a private
 * OpenStack. That distinction has to survive into the saved profile: Blomp's
 * exemption from the cleartext-object-store guard is granted by its preset id,
 * and a different deployment must not inherit it just because it was configured
 * through the same form. So the id follows the auth URL, and a custom Keystone
 * opts in explicitly or is refused.
 */
export const BLOMP_AUTH_URL =
    getProviderById('blomp')?.defaults?.server || 'https://authenticate.blomp.com';

const canonical = (url: string): string => url.trim().replace(/\/+$/, '').toLowerCase();

/**
 * Whether an auth URL still points at the Blomp preset. Compared without a
 * trailing slash and case-insensitively, because a host typed by hand is the
 * expected input here and neither difference changes which service is reached.
 */
export const isBlompAuthUrl = (server: string | undefined): boolean =>
    canonical(server || '') === canonical(BLOMP_AUTH_URL);

/**
 * The options a Swift auth-URL edit should carry.
 *
 * The preset's cleartext exemption is granted by identity, so it has to follow
 * the auth URL: leaving Blomp drops it, returning to Blomp restores it. What
 * must NOT follow the auth URL is the opt-in a user gave for their own private
 * OpenStack. Recomputing on every keystroke conflates the two, and the way it
 * shows is that correcting one character in the URL silently unticks a box the
 * user ticked, with the tick still drawn until the next render reads the value
 * back. Typing is not a decision.
 *
 * So the flag is touched only when an edit CROSSES the preset boundary, which
 * is what "leaving" and "returning" meant in the first place.
 */
export const swiftOptionsForAuthUrl = <T extends { allowCleartextStorage?: boolean }>(
    previousServer: string | undefined,
    nextServer: string,
    options: T | undefined,
): T | undefined => {
    const wasPreset = isBlompAuthUrl(previousServer);
    const isPreset = isBlompAuthUrl(nextServer);
    if (wasPreset === isPreset) return options;
    return { ...(options as T), allowCleartextStorage: isPreset ? true : undefined };
};
