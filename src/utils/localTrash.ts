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
  /**
   * The panel gesture the permanent deletes belong to: its toast's Cancel
   * stops the delete in flight (`operation_scope_cancel`) and any later one.
   */
  operationId?: string;
}

export interface LocalTrashResult {
  /** Items that left their folder, by trash or by permanent delete. */
  removed: string[];
  /** Items left in place: the user chose to keep them, or the operation was cancelled first. */
  kept: string[];
  failed: { path: string; error: string }[];
}

/**
 * The one reader of a cancelled delete, in both shapes the native deletes
 * answer. A panel delete over FTP or on the local disk resolves with
 * `Deletion cancelled: <k> of <n> ...` (what it did, then the stop); a
 * provider delete, and any delete of a sync run, rejects with the typed
 * cancellation (`Transfer cancelled by user`, sync.rs `IpcError::Cancelled`)
 * or `Deletion cancelled` (a provider folder). Either way the item is not
 * deleted, or not all of it, and the gesture starts no further delete.
 */
export function isDeletionCancelled(outcome: unknown): boolean {
  const text = outcome instanceof Error ? outcome.message : typeof outcome === 'string' ? outcome : '';
  return text.startsWith('Deletion cancelled') || text.startsWith('Transfer cancelled by user');
}

export async function trashLocalPaths(
  paths: string[],
  deps: LocalTrashDeps,
): Promise<LocalTrashResult> {
  const result: LocalTrashResult = { removed: [], kept: [], failed: [] };
  const needsCopy: string[] = [];
  const refused: { path: string; error: string }[] = [];
  // One cancellation for the whole call (M7): the gesture's Stop, or a
  // delete that answered cancelled. Once it is set, nothing more is deleted
  // in either group, nothing more is asked, and every path not done is kept.
  let cancelled = false;
  const stopped = () => cancelled || deps.isCancelled?.() === true;
  const deletePermanently = async (path: string): Promise<'removed' | 'cancelled' | string> => {
    try {
      const answer = await deps.invoke('delete_local_file', deps.operationId ? { path, operationId: deps.operationId } : { path });
      return isDeletionCancelled(answer) ? 'cancelled' : 'removed';
    } catch (err) {
      return isDeletionCancelled(err) ? 'cancelled' : String(err);
    }
  };

  for (const [index, path] of paths.entries()) {
    if (stopped()) {
      result.kept.push(...paths.slice(index));
      break;
    }
    try {
      await deps.invoke('delete_to_trash', { path });
      result.removed.push(path);
    } catch (err) {
      const error = String(err);
      if (error.startsWith(`${TRASH_NEEDS_HOME_COPY}:`)) needsCopy.push(path);
      else refused.push({ path, error });
    }
  }

  if (needsCopy.length > 0) {
    const choice = stopped() ? 'cancel' : await deps.askHomeCopy(needsCopy);
    // Only the two exact choices act. Anything else (a dialog dismissed with
    // no value, a choice added later) keeps the items: the permanent delete
    // is never what an unexpected answer falls through to.
    if (choice !== 'copy' && choice !== 'permanent') {
      result.kept.push(...needsCopy);
    } else {
      for (const [index, path] of needsCopy.entries()) {
        if (stopped()) {
          result.kept.push(...needsCopy.slice(index));
          break;
        }
        if (choice === 'copy') {
          try {
            await deps.invoke('delete_to_trash', { path, allowHomeCopy: true });
            result.removed.push(path);
          } catch (err) {
            result.failed.push({ path, error: String(err) });
          }
          continue;
        }
        const outcome = await deletePermanently(path);
        if (outcome === 'cancelled') {
          cancelled = true;
          result.kept.push(...needsCopy.slice(index));
          break;
        }
        if (outcome === 'removed') result.removed.push(path);
        else result.failed.push({ path, error: outcome });
      }
    }
  }

  if (refused.length > 0 && stopped()) {
    // Cancelled before them: not deleted, and not asked about.
    result.kept.push(...refused.map(r => r.path));
  } else if (refused.length > 0 && deps.askPermanentAfterFailure) {
    const confirmed = await deps.askPermanentAfterFailure(refused.map(r => r.path));
    if (confirmed === true) {
      for (const [index, { path }] of refused.entries()) {
        if (stopped()) {
          result.kept.push(...refused.slice(index).map(r => r.path));
          break;
        }
        const outcome = await deletePermanently(path);
        if (outcome === 'cancelled') {
          cancelled = true;
          result.kept.push(...refused.slice(index).map(r => r.path));
          break;
        }
        if (outcome === 'removed') result.removed.push(path);
        else result.failed.push({ path, error: outcome });
      }
    } else {
      result.kept.push(...refused.map(r => r.path));
    }
  } else {
    result.failed.push(...refused);
  }

  return result;
}
