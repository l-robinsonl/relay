import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import type { SyncStatus } from '@/services/ipc'

const mocks = vi.hoisted(() => ({
  syncStatus: vi.fn(),
  syncConflicts: vi.fn(),
  syncNow: vi.fn(),
  syncConnect: vi.fn(),
  syncDisconnect: vi.fn(),
  syncResolve: vi.fn(),
  syncWatch: vi.fn(),
  syncUnwatch: vi.fn(),
}))

vi.mock('@/services/ipc', () => ({
  api: mocks,
  onSyncEvent: vi.fn().mockResolvedValue(() => {}),
  isRelayError: () => false,
  toRelayError: (e: unknown) => ({ kind: 'internal', message: String(e) }),
}))

const refreshTree = vi.fn()
const refreshEnvironments = vi.fn()

vi.mock('@/stores/workspace', () => ({
  useWorkspaceStore: () => ({
    current: { id: 'w1', name: 'Test' },
    refreshTree,
    refreshEnvironments,
  }),
}))

import { useSyncStore } from './sync'

function status (over: Partial<SyncStatus> = {}): SyncStatus {
  return {
    connected: true,
    serverUrl: 'https://relay.internal',
    userEmail: 'ada@example.com',
    signedIn: true,
    lastPulledSeq: 4,
    lastSyncAt: Date.now(),
    lastError: null,
    pendingChanges: 0,
    openConflicts: 0,
    ...over,
  }
}

beforeEach(() => {
  setActivePinia(createPinia())
  vi.clearAllMocks()
  mocks.syncConflicts.mockResolvedValue([])
  mocks.syncWatch.mockResolvedValue(true)
  mocks.syncUnwatch.mockResolvedValue(undefined)
})

describe('the sync indicator', () => {
  it('reports a workspace with no server as local, not broken', async () => {
    mocks.syncStatus.mockResolvedValue(status({ connected: false }))
    const sync = useSyncStore()
    await sync.refresh()

    expect(sync.phase).toBe('local')
    expect(sync.label).toBe('Local only')
  })

  it('reports being up to date', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    const sync = useSyncStore()
    await sync.refresh()

    expect(sync.phase).toBe('idle')
    expect(sync.label).toBe('Up to date')
  })

  it('counts changes waiting to upload', async () => {
    mocks.syncStatus.mockResolvedValue(status({ pendingChanges: 3 }))
    const sync = useSyncStore()
    await sync.refresh()

    expect(sync.phase).toBe('idle')
    expect(sync.label).toBe('3 to upload')
  })

  it('surfaces conflicts ahead of an older error', async () => {
    // A conflict is actionable; a stale error message is not.
    mocks.syncStatus.mockResolvedValue(
      status({ openConflicts: 2, lastError: 'something earlier' }),
    )
    mocks.syncConflicts.mockResolvedValue([])
    const sync = useSyncStore()
    await sync.refresh()

    expect(sync.phase).toBe('conflicts')
    expect(sync.label).toBe('2 conflicts')
  })

  it('puts signing in ahead of everything else', async () => {
    // A signed-out workspace cannot sync at all, so that is the thing to
    // tell the user about.
    mocks.syncStatus.mockResolvedValue(
      status({ signedIn: false, openConflicts: 1, lastError: 'expired' }),
    )
    const sync = useSyncStore()
    await sync.refresh()

    expect(sync.phase).toBe('signedOut')
  })

  it('reports a failure', async () => {
    mocks.syncStatus.mockResolvedValue(status({ lastError: 'could not reach the server' }))
    const sync = useSyncStore()
    await sync.refresh()

    expect(sync.phase).toBe('error')
    expect(sync.label).toBe('Sync failed')
  })

  it('uses the singular for one conflict', async () => {
    mocks.syncStatus.mockResolvedValue(status({ openConflicts: 1 }))
    const sync = useSyncStore()
    await sync.refresh()
    expect(sync.label).toBe('1 conflict')
  })
})

describe('syncing', () => {
  it('reloads the sidebar when changes arrive', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncNow.mockResolvedValue({
      pulled: 3,
      pushed: 0,
      conflicts: 0,
      rejected: 0,
      head: 7,
    })

    const sync = useSyncStore()
    await sync.refresh()
    await sync.syncNow()

    expect(refreshTree).toHaveBeenCalled()
    expect(refreshEnvironments).toHaveBeenCalled()
  })

  it('does not reload the sidebar when nothing came in', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncNow.mockResolvedValue({
      pulled: 0,
      pushed: 2,
      conflicts: 0,
      rejected: 0,
      head: 7,
    })

    const sync = useSyncStore()
    await sync.refresh()
    await sync.syncNow()

    expect(refreshTree).not.toHaveBeenCalled()
  })

  it('refuses to sync a workspace with no server', async () => {
    mocks.syncStatus.mockResolvedValue(status({ connected: false }))
    const sync = useSyncStore()
    await sync.refresh()
    await sync.syncNow()

    expect(mocks.syncNow).not.toHaveBeenCalled()
  })

  it('does not start a second pass while one is running', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    let release: (() => void) | undefined
    mocks.syncNow.mockImplementation(
      () =>
        new Promise((resolve) => {
          release = () =>
            resolve({ pulled: 0, pushed: 0, conflicts: 0, rejected: 0, head: 1 })
        }),
    )

    const sync = useSyncStore()
    await sync.refresh()

    const first = sync.syncNow()
    await sync.syncNow()
    release?.()
    await first

    expect(mocks.syncNow).toHaveBeenCalledTimes(1)
  })

  it('clears the running flag when a sync fails', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncNow.mockRejectedValue({ kind: 'sync', message: 'unreachable' })

    const sync = useSyncStore()
    await sync.refresh()
    await sync.syncNow()

    // A failure must not leave the indicator spinning forever.
    expect(sync.syncing).toBe(false)
  })

  it('does not let a status failure break the app', async () => {
    mocks.syncStatus.mockRejectedValue(new Error('database gone'))
    const sync = useSyncStore()
    await sync.refresh()

    expect(sync.phase).toBe('local')
  })
})

describe('resolving', () => {
  it('reloads the tree after a decision', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncResolve.mockResolvedValue(undefined)

    const sync = useSyncStore()
    await sync.resolve('conflict-1', true)

    expect(mocks.syncResolve).toHaveBeenCalledWith('conflict-1', true)
    expect(refreshTree).toHaveBeenCalled()
  })
})


describe('live updates', () => {
  it('reports when the live connection is open', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    const sync = useSyncStore()
    await sync.refresh()
    await sync.watch()

    expect(sync.live).toBe(true)
  })

  it('treats a watch failure as merely not-live, not an error', async () => {
    // Live updates are an optimisation; manual sync still works.
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncWatch.mockRejectedValue(new Error('no socket'))

    const sync = useSyncStore()
    await sync.refresh()
    await sync.watch()

    expect(sync.live).toBe(false)
    expect(sync.phase).toBe('idle')
  })

  it('a background sync says nothing when it merely pulls', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncNow.mockResolvedValue({
      pulled: 2,
      pushed: 0,
      conflicts: 0,
      rejected: 0,
      head: 9,
    })

    const sync = useSyncStore()
    await sync.refresh()
    await sync.syncQuietly()

    // Announcing every teammate's save would be unusable in a team.
    const toasts = (await import('@/stores/toast')).useToastStore()
    expect(toasts.items).toHaveLength(0)
    expect(refreshTree).toHaveBeenCalled()
  })

  it('a background sync does speak up about a conflict', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncNow.mockResolvedValue({
      pulled: 1,
      pushed: 0,
      conflicts: 1,
      rejected: 0,
      head: 9,
    })

    const sync = useSyncStore()
    await sync.refresh()
    await sync.syncQuietly()

    const toasts = (await import('@/stores/toast')).useToastStore()
    expect(toasts.items.length).toBeGreaterThan(0)
    expect(toasts.items[0]!.message).toContain('conflict')
  })

  it('a failed background sync stays silent and stops spinning', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncNow.mockRejectedValue({ kind: 'sync', message: 'unreachable' })

    const sync = useSyncStore()
    await sync.refresh()
    await sync.syncQuietly()

    const toasts = (await import('@/stores/toast')).useToastStore()
    expect(toasts.items).toHaveLength(0)
    expect(sync.syncing).toBe(false)
  })

  it('stops watching on disconnect', async () => {
    mocks.syncStatus.mockResolvedValue(status())
    mocks.syncDisconnect.mockResolvedValue(undefined)

    const sync = useSyncStore()
    await sync.refresh()
    await sync.watch()
    await sync.disconnect()

    expect(mocks.syncUnwatch).toHaveBeenCalled()
    expect(sync.live).toBe(false)
  })
})
