/**
 * Moving local items to the trash, with the two outcomes the backend can
 * refuse on the user's behalf turned into explicit questions.
 *
 * `delete_to_trash` answers `TRASH_NEEDS_HOME_COPY` when the item's drive has
 * no trash this user can write, because the trash crate would otherwise copy
 * the item into the home trash (possibly gigabytes, with no progress). The
 * user then chooses: copy it anyway, delete permanently, or keep it.
 *
 * A plain trash failure is never turned into a permanent delete without
 * asking: the Duplicate Finder used to do exactly that, silently.
 */

/** The marker `delete_to_trash` puts at the start of that refusal. */
export const TRASH_NEEDS_HOME_COPY = 'TRASH_NEEDS_HOME_COPY';

export type HomeCopyChoice = 'copy' | 'permanent' | 'cancel';

export interface LocalTrashDeps {
  invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  /** Asked once per call, with every item that would need the copy. */
  askHomeCopy: (paths: string[]) => Promise<HomeCopyChoice>;
  /**
   * Optional: asked once per call with the items the trash refused for any
   * other reason. Without it those items are reported as failed.
   */
  askPermanentAfterFailure?: (paths: string[]) => Promise<boolean>;
  isCancelled?: () => boolean;
}

export interface LocalTrashResult {
  /** Items that left their folder, by trash or by permanent delete. */
  removed: string[];
  /** Items the user chose to keep when asked. */
  kept: string[];
  failed: { path: string; error: string }[];
}

export async function trashLocalPaths(
  paths: string[],
  deps: LocalTrashDeps,
): Promise<LocalTrashResult> {
  const result: LocalTrashResult = { removed: [], kept: [], failed: [] };
  const needsCopy: string[] = [];
  const refused: { path: string; error: string }[] = [];

  for (const path of paths) {
    if (deps.isCancelled?.()) break;
    try {
      await deps.invoke('delete_to_trash', { path });
      result.removed.push(path);
    } catch (err) {
      const error = String(err);
      if (error.includes(TRASH_NEEDS_HOME_COPY)) needsCopy.push(path);
      else refused.push({ path, error });
    }
  }

  if (needsCopy.length > 0) {
    const choice = await deps.askHomeCopy(needsCopy);
    if (choice === 'cancel') {
      result.kept.push(...needsCopy);
    } else {
      for (const path of needsCopy) {
        try {
          if (choice === 'copy') {
            await deps.invoke('delete_to_trash', { path, allowHomeCopy: true });
          } else {
            await deps.invoke('delete_local_file', { path });
          }
          result.removed.push(path);
        } catch (err) {
          result.failed.push({ path, error: String(err) });
        }
      }
    }
  }

  if (refused.length > 0 && deps.askPermanentAfterFailure) {
    const confirmed = await deps.askPermanentAfterFailure(refused.map(r => r.path));
    if (confirmed) {
      for (const { path } of refused) {
        try {
          await deps.invoke('delete_local_file', { path });
          result.removed.push(path);
        } catch (err) {
          result.failed.push({ path, error: String(err) });
        }
      }
    } else {
      result.kept.push(...refused.map(r => r.path));
    }
  } else {
    result.failed.push(...refused);
  }

  return result;
}
