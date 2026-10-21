import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import type { ConflictView, SyncOutcome, SyncStatus } from '@/services/ipc'
import { api } from '@/services/ipc'
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
      return true
    } catch (error) {
      toasts.error('Could not connect', error)
      return false
    }
  }

  async function disconnect () {
    if (!workspace.current) return
    try {
      await api.syncDisconnect(workspace.current.id)
      toasts.info('Disconnected', 'Your workspace is still here, on this machine.')
    } catch (error) {
      toasts.error('Could not disconnect', error)
    } finally {
      await refresh()
    }
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
    phase,
    label,
    refresh,
    syncNow,
    connect,
    disconnect,
    resolve,
  }
})
