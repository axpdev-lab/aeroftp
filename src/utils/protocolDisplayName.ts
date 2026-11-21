// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The brand name of a service reached by protocol alone, with no preset
 * (`providerId`) to name it: `dropbox` reads "Dropbox", not DROPBOX (#369) or
 * dropbox. MEGA stays all-caps because that is its real style. MTP is not
 * here: its name is translated (`protocol.mtpName`).
 */
const PROTOCOL_DISPLAY_NAMES: Record<string, string> = {
    dropbox: 'Dropbox', onedrive: 'OneDrive', googledrive: 'Google Drive',
    googlephotos: 'Google Photos', box: 'Box', pcloud: 'pCloud Drive',
    jottacloud: 'Jottacloud', filen: 'Filen', internxt: 'Internxt', proton: 'Proton Drive',
    kdrive: 'kDrive', zohoworkdrive: 'Zoho WorkDrive', yandexdisk: 'Yandex Disk',
    drime: 'Drime', mega: 'MEGA', backblaze: 'Backblaze B2', fourshared: '4shared',
    imagekit: 'ImageKit', uploadcare: 'Uploadcare', cloudinary: 'Cloudinary', twake: 'Twake Drive',
    filelu: 'FileLu', github: 'GitHub', gitlab: 'GitLab',
};

/** The brand name of `protocol`, or `undefined` when it has none here. */
export const protocolDisplayName = (protocol: string): string | undefined => PROTOCOL_DISPLAY_NAMES[protocol];
