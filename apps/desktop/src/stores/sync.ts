import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import type { ConflictView, SyncEvent, SyncOutcome, SyncStatus } from '@/services/ipc'
import { api, onSyncEvent } from '@/services/ipc'
import { useToastStore } from '@/stores/toast'
import { useWorkspaceStore } from '@/stores/workspace'

/** The state of the sync indicator. */
export type SyncPhase = 'local' | 'idle' | 'syncing' | 'error' | 'conflicts' | 'signedOut'

const EMPTY: SyncStatus = {
  connected: false,
  serverUrl: null,
  userEmail: null,
  signedIn: false,
  lastPulledSeq: 0,
  lastSyncAt: null,
  lastError: null,
  pendingChanges: 0,
  openConflicts: 0,
}

/** Synchronisation state for the active workspace. */
export const useSyncStore = defineStore('sync', () => {
  const workspace = useWorkspaceStore()
  const toasts = useToastStore()

  const status = ref<SyncStatus>({ ...EMPTY })
  const conflicts = ref<ConflictView[]>([])
  const syncing = ref(false)
  const lastOutcome = ref<SyncOutcome | null>(null)
  const live = ref(false)

  /** Collapses a burst of notifications into one sync. */
  let pendingSync: ReturnType<typeof setTimeout> | undefined

  /**
   * What the indicator should show.
   *
   * Order matters: a signed-out workspace cannot sync at all, so that
   * takes precedence over an error from before the token expired.
   */
  const phase = computed<SyncPhase>(() => {
    if (!status.value.connected) return 'local'
    if (!status.value.signedIn) return 'signedOut'
    if (syncing.value) return 'syncing'
    if (status.value.openConflicts > 0) return 'conflicts'
    if (status.value.lastError) return 'error'
    return 'idle'
  })

  const label = computed(() => {
    switch (phase.value) {
      case 'local':
        return 'Local only'
      case 'signedOut':
        return 'Sign in to sync'
      case 'syncing':
        return 'Syncing…'
      case 'conflicts':
        return `${status.value.openConflicts} conflict${
          status.value.openConflicts === 1 ? '' : 's'
        }`
      case 'error':
        return 'Sync failed'
      default:
        return status.value.pendingChanges > 0
          ? `${status.value.pendingChanges} to upload`
          : 'Up to date'
    }
  })

  async function refresh () {
    if (!workspace.current) return
    try {
      status.value = await api.syncStatus(workspace.current.id)
      conflicts.value = status.value.openConflicts
        ? await api.syncConflicts(workspace.current.id)
        : []
    } catch {
      // The indicator must never be the thing that breaks the app.
      status.value = { ...EMPTY }
    }
  }

  async function syncNow () {
    if (!workspace.current || syncing.value || !status.value.connected) return
    syncing.value = true

    try {
      const outcome = await api.syncNow(workspace.current.id)
      lastOutcome.value = outcome

      // Incoming changes alter the sidebar, so it has to be reloaded.
      if (outcome.pulled > 0) {
        await workspace.refreshTree()
        await workspace.refreshEnvironments()
      }

      if (outcome.conflicts > 0) {
        toasts.warn(
          `${outcome.conflicts} conflict${outcome.conflicts === 1 ? '' : 's'} need resolving`,
          'Someone else changed the same thing. Nothing was overwritten.',
        )
      } else if (!outcome.pulled && !outcome.pushed) {
        toasts.info('Already up to date')
      } else {
        toasts.success(
          `Synced — ${outcome.pulled} in, ${outcome.pushed} out`,
        )
      }
    } catch (error) {
      toasts.error('Could not sync', error)
    } finally {
      syncing.value = false
      await refresh()
    }
  }

  async function connect (
    serverUrl: string,
    email: string,
    password: string,
    join: boolean,
  ) {
    if (!workspace.current) return false
    try {
      status.value = await api.syncConnect(
        workspace.current.id,
        serverUrl,
        email,
        password,
        join,
      )
      toasts.success('Connected', `Signed in to ${serverUrl} as ${email}.`)
      await syncNow()
      await watch()
      return true
    } catch (error) {
      toasts.error('Could not connect', error)
      return false
    }
  }

  async function disconnect () {
    if (!workspace.current) return
    try {
      await unwatch()
      await api.syncDisconnect(workspace.current.id)
      toasts.info('Disconnected', 'Your workspace is still here, on this machine.')
    } catch (error) {
      toasts.error('Could not disconnect', error)
    } finally {
      await refresh()
    }
  }

  /**
   * Syncs because the server said something changed.
   *
   * Quiet: an automatic sync that announced itself every time somebody
   * else saved a request would be unusable in a team.
   */
  async function syncQuietly () {
    if (!workspace.current || syncing.value || !status.value.connected) return
    syncing.value = true

    try {
      const outcome = await api.syncNow(workspace.current.id)
      lastOutcome.value = outcome

      if (outcome.pulled > 0) {
        await workspace.refreshTree()
        await workspace.refreshEnvironments()
      }
      // A conflict does need saying, because it needs a decision.
      if (outcome.conflicts > 0) {
        toasts.warn(
          `${outcome.conflicts} conflict${outcome.conflicts === 1 ? '' : 's'} need resolving`,
          'Someone else changed the same thing. Nothing was overwritten.',
        )
      }
    } catch {
      // A background sync that fails says nothing: the indicator already
      // shows the state, and an unprompted error toast is just noise.
    } finally {
      syncing.value = false
      await refresh()
    }
  }

  function handleEvent (event: SyncEvent) {
    if (event.workspaceId !== workspace.current?.id) return

    if (event.event === 'connected') {
      live.value = true
      return
    }
    if (event.event === 'disconnected') {
      live.value = false
      return
    }

    // Several people saving at once produces a burst; one sync covers all
    // of it, and the protocol only needs the latest revision anyway.
    clearTimeout(pendingSync)
    pendingSync = setTimeout(() => void syncQuietly(), 400)
  }

  /** Starts listening for server changes to the active workspace. */
  async function watch () {
    if (!workspace.current) return
    try {
      live.value = await api.syncWatch(workspace.current.id)
    } catch {
      // Live updates are an optimisation; manual sync still works.
      live.value = false
    }
  }

  /** Stops listening. */
  async function unwatch () {
    if (!workspace.current) return
    clearTimeout(pendingSync)
    live.value = false
    try {
      await api.syncUnwatch(workspace.current.id)
    } catch {
      // Nothing useful to do if it was already gone.
    }
  }

  /** Subscribes to the event channel for the lifetime of the app. */
  function listenForChanges () {
    return onSyncEvent(handleEvent)
  }

  async function resolve (conflictId: string, keepLocal: boolean) {
    try {
      await api.syncResolve(conflictId, keepLocal)
      await workspace.refreshTree()
      await refresh()
      toasts.success(keepLocal ? 'Kept your version' : 'Took the server version')
    } catch (error) {
      toasts.error('Could not resolve that conflict', error)
    }
  }

  return {
    status,
    conflicts,
    syncing,
    lastOutcome,
    live,
    phase,
    label,
    refresh,
    syncNow,
    syncQuietly,
    connect,
    disconnect,
    resolve,
    watch,
    unwatch,
    listenForChanges,
  }
})
