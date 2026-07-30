// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Move the item at `from` so it lands at index `to` (inherits that slot).
 *
 * This is NOT the classic "insert before" formula that does `to - 1` when
 * moving down. After the move, the dragged item's index is `to` (clamped).
 * Matches `aero profiles --tui` # and My Servers drop-on-card (#453).
 *
 * `to === list.length` means "after the last item" (append).
 */
export function reorderInheritingIndex<T>(list: readonly T[], from: number, to: number): T[] {
  if (from < 0 || from >= list.length) return list.slice();
  // Allow to === length (append sentinel). Clamp other out-of-range to ends.
  const target = Math.max(0, Math.min(to, list.length));
  if (from === target) return list.slice();
  // Moving to "after last" while already last is a no-op.
  if (from === list.length - 1 && target === list.length) return list.slice();

  const next = list.slice();
  const [moved] = next.splice(from, 1);
  // After removal the array is shorter. Inserting at the original `target`
  // (without the classic from < to decrement) is what makes the item land AT
  // the drop target's former index when dragging down/right (#453).
  const insertAt = Math.max(0, Math.min(target, next.length));
  next.splice(insertAt, 0, moved);
  return next;
}

/**
 * Reorder a visible subset (filter / search / group) and write it back into
 * the full list, keeping non-visible items in their relative places.
 *
 * The visible sequence is reordered with {@link reorderInheritingIndex}; each
 * visible slot in `full` (in full-list order) is then refilled from the new
 * visible order. Used so table+protocol-chip reorder moves what the user sees
 * (#453 residual: drag must work while a sidebar filter is active).
 */
export function reorderVisibleInFull<T extends { id: string }>(
  full: readonly T[],
  visible: readonly T[],
  fromVisible: number,
  toVisible: number,
): T[] {
  if (visible.length === 0) return full.slice();
  const nextVisible = reorderInheritingIndex(visible, fromVisible, toVisible);
  if (nextVisible.every((s, i) => s.id === visible[i]?.id)) {
    return full.slice();
  }

  const byId = new Map(full.map((s) => [s.id, s]));
  const visibleIds = new Set(visible.map((s) => s.id));
  // Slots in the full list that currently hold a visible profile, in full order.
  const slots: number[] = [];
  for (let i = 0; i < full.length; i++) {
    if (visibleIds.has(full[i].id)) slots.push(i);
  }
  if (slots.length !== nextVisible.length) {
    // Defensive: visible is not a subset of full (stale UI). Do not guess.
    return full.slice();
  }

  const result = full.slice();
  for (let k = 0; k < slots.length; k++) {
    const item = byId.get(nextVisible[k].id);
    if (item) result[slots[k]] = item;
  }
  return result;
}
