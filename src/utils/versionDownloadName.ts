// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The name a downloaded version is offered under: the file's name with `_v`
 * and the first eight characters of the version id before its extension.
 * The id itself is sent unchanged with every request; only its part in the
 * name is made safe on every file system. Windows refuses `<>:"/\|?*`,
 * control characters and a trailing dot, and MEGAcmd ids (`H:vAAAAAA2`) and
 * Nextcloud ones (`42:1700000000`) carry a colon.
 */
export function versionDownloadName(fileName: string, versionId: string): string {
    const dot = fileName.lastIndexOf('.');
    const [stem, ext] = dot > 0 ? [fileName.slice(0, dot), fileName.slice(dot)] : [fileName, ''];
    const tag = versionId
        .slice(0, 8)
        .replace(/[^A-Za-z0-9._-]/g, '_')
        .replace(/\.+$/, '');
    return `${stem}_v${tag}${ext}`;
}
