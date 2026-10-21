<script setup lang="ts">
import { computed } from 'vue'
import {
  AlertTriangle,
  Cloud,
  CloudOff,
  LogIn,
  RefreshCw,
  Upload,
} from '@lucide/vue'
import { formatRelativeTime } from '@/composables/useFormat'
import { useSyncStore } from '@/stores/sync'

/**
 * The sync state, in the title bar.
 *
 * Deliberately quiet: a local-only workspace is the normal case, not a
 * problem, so it is shown in muted text rather than as a warning.
 */
const emit = defineEmits<{ openSettings: []; openConflicts: [] }>()

const sync = useSyncStore()

const appearance = computed(() => {
  switch (sync.phase) {
    case 'local':
      return { icon: CloudOff, color: 'var(--text-muted)' }
    case 'signedOut':
      return { icon: LogIn, color: 'var(--warning)' }
    case 'syncing':
      return { icon: RefreshCw, color: 'var(--accent)' }
    case 'conflicts':
      return { icon: AlertTriangle, color: 'var(--warning)' }
    case 'error':
      return { icon: AlertTriangle, color: 'var(--danger)' }
    default:
      return sync.status.pendingChanges > 0
        ? { icon: Upload, color: 'var(--text-secondary)' }
        : { icon: Cloud, color: 'var(--success)' }
  }
})

const title = computed(() => {
  if (!sync.status.connected) return 'This workspace is stored only on this machine'
  const parts = [sync.status.serverUrl ?? '']
  if (sync.status.userEmail) parts.push(`as ${sync.status.userEmail}`)
  if (sync.status.lastSyncAt) {
    parts.push(`last synced ${formatRelativeTime(sync.status.lastSyncAt)}`)
  }
  if (sync.status.lastError) parts.push(sync.status.lastError)
  return parts.filter(Boolean).join(' · ')
})

function activate () {
  if (sync.phase === 'conflicts') emit('openConflicts')
  else if (sync.status.connected && sync.status.signedIn) void sync.syncNow()
  else emit('openSettings')
}
</script>

<template>
  <button
    type="button"
    class="btn btn-ghost gap-1.5 px-2 py-1 text-xs"
    :style="{ color: appearance.color }"
    :title="title"
    @click="activate"
  >
    <component
      :is="appearance.icon"
      :size="13"
      :class="sync.phase === 'syncing' && 'animate-spin'"
    />
    <span>{{ sync.label }}</span>
  </button>
</template>
