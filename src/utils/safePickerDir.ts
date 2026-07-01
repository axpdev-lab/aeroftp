import { invoke } from '@tauri-apps/api/core';

/**
 * Resolve a directory that is safe to pass to a native folder picker as its
 * `defaultPath`.
 *
 * A profile's stored local path is machine-relative: after importing a profile
 * from another machine (or deleting the folder, or a manual typo) that path may
 * not exist here, and handing a non-existent `defaultPath` to the GTK file
 * chooser crashes the app (heap corruption). The backend walks up to the nearest
 * existing ancestor directory, or returns `null` when nothing valid is found so
 * the picker opens at the OS default location.
 *
 * Never throws: if the command itself fails for any reason we fall back to
 * `undefined` (OS default) rather than let the helper break the picker.
 */
export async function safePickerStartDir(
  path?: string | null,
): Promise<string | undefined> {
  try {
    const resolved = await invoke<string | null>('safe_picker_start_dir', {
      path: path ?? null,
    });
    return resolved ?? undefined;
  } catch {
    return undefined;
  }
}
