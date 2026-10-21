import { ref } from 'vue'
import type { TreeNode } from '@relay/shared-types'

/** Where a drop would place the dragged node. */
export type DropPosition = 'before' | 'after' | 'inside'

/** The node being dragged, and where it is hovering. */
export interface DragState {
  dragging: TreeNode | null
  overId: string | null
  position: DropPosition | null
}

/**
 * Computes a `sortOrder` that places an item between two neighbours.
 *
 * Orders are floats precisely so a reorder rewrites one row rather than
 * renumbering a whole folder. Repeated insertions between the same pair
 * halve the gap each time, so this falls back to appending once the gap
 * stops being representable, rather than silently producing duplicates.
 */
export function orderBetween (before: number | null, after: number | null): number {
  if (before === null && after === null) return 0
  if (before === null) return (after as number) - 1
  if (after === null) return before + 1

  const midpoint = (before + after) / 2
  // Guard against the gap collapsing below float precision.
  if (midpoint <= before || midpoint >= after) return before + 1
  return midpoint
}

/**
 * Works out where a drop should land, given the pointer's position within
 * the row.
 *
 * A folder gets three zones — above, inside, below — because dropping *into*
 * a folder and dropping *next to* it are different intentions. A request has
 * only two.
 */
export function dropPositionFor (
  target: TreeNode,
  offsetY: number,
  height: number,
): DropPosition {
  if (height <= 0) return 'after'
  const ratio = offsetY / height

  if (target.kind === 'folder') {
    if (ratio < 0.25) return 'before'
    if (ratio > 0.75) return 'after'
    return 'inside'
  }

  return ratio < 0.5 ? 'before' : 'after'
}

/** True when dropping `dragged` onto `target` would be invalid. */
export function isInvalidDrop (
  dragged: TreeNode,
  target: TreeNode,
  isAncestor: (ancestorId: string, nodeId: string) => boolean,
): boolean {
  if (dragged.id === target.id) return true
  // Dropping a folder inside its own subtree would detach that subtree.
  if (dragged.kind === 'folder' && isAncestor(dragged.id, target.id)) return true
  return false
}

/** Shared drag state for the sidebar tree. */
export function useDragDrop () {
  const state = ref<DragState>({ dragging: null, overId: null, position: null })

  function start (node: TreeNode) {
    state.value = { dragging: node, overId: null, position: null }
  }

  function over (nodeId: string, position: DropPosition) {
    if (!state.value.dragging) return
    state.value = { ...state.value, overId: nodeId, position }
  }

  function clear () {
    state.value = { dragging: null, overId: null, position: null }
  }

  return { state, start, over, clear }
}
