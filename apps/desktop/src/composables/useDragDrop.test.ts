import { describe, expect, it } from 'vitest'
import type { TreeNode } from '@relay/shared-types'
import { dropPositionFor, isInvalidDrop, orderBetween } from './useDragDrop'

function folder (id: string): TreeNode {
  return { kind: 'folder', id, parentId: null, name: id, sortOrder: 0, childCount: 0 }
}

function request (id: string): TreeNode {
  return {
    kind: 'request',
    id,
    parentId: null,
    name: id,
    method: 'GET',
    url: '',
    sortOrder: 0,
  }
}

describe('orderBetween', () => {
  it('places an item between its neighbours', () => {
    expect(orderBetween(1, 2)).toBe(1.5)
    expect(orderBetween(0, 10)).toBe(5)
  })

  it('handles the ends of a list', () => {
    expect(orderBetween(null, 5)).toBe(4)
    expect(orderBetween(5, null)).toBe(6)
    expect(orderBetween(null, null)).toBe(0)
  })

  it('stays strictly between its neighbours', () => {
    // Repeated insertion in the same gap must keep producing distinct,
    // correctly ordered values rather than colliding.
    let before = 0
    const after = 1
    for (let i = 0; i < 20; i += 1) {
      const next = orderBetween(before, after)
      expect(next).toBeGreaterThan(before)
      expect(next).toBeLessThan(after)
      before = next
    }
  })

  it('falls back to appending once the gap is too small to split', () => {
    // Two adjacent floats have no representable midpoint.
    const a = 1
    const b = 1 + Number.EPSILON
    const result = orderBetween(a, b)
    expect(result).toBeGreaterThan(a)
  })
})

describe('dropPositionFor', () => {
  it('gives a folder three zones', () => {
    expect(dropPositionFor(folder('f'), 2, 24)).toBe('before')
    expect(dropPositionFor(folder('f'), 12, 24)).toBe('inside')
    expect(dropPositionFor(folder('f'), 22, 24)).toBe('after')
  })

  it('gives a request two zones, because there is no inside', () => {
    expect(dropPositionFor(request('r'), 5, 24)).toBe('before')
    expect(dropPositionFor(request('r'), 20, 24)).toBe('after')
    expect(dropPositionFor(request('r'), 12, 24)).not.toBe('inside')
  })

  it('does not divide by a zero height', () => {
    expect(dropPositionFor(folder('f'), 0, 0)).toBe('after')
  })
})

describe('isInvalidDrop', () => {
  const noAncestors = () => false

  it('refuses dropping a node onto itself', () => {
    expect(isInvalidDrop(folder('a'), folder('a'), noAncestors)).toBe(true)
  })

  it('refuses dropping a folder into its own subtree', () => {
    // Dropping a parent inside its own child would detach the whole subtree
    // from the root and make it unreachable.
    const isAncestor = (ancestorId: string, nodeId: string) =>
      ancestorId === 'parent' && nodeId === 'child'
    expect(isInvalidDrop(folder('parent'), folder('child'), isAncestor)).toBe(true)
  })

  it('allows an ordinary move', () => {
    expect(isInvalidDrop(request('r'), folder('f'), noAncestors)).toBe(false)
    expect(isInvalidDrop(folder('a'), folder('b'), noAncestors)).toBe(false)
  })
})
