<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { Clock, Trash2 } from '@lucide/vue'
import type { HistoryEntry } from '@relay/shared-types'
import EmptyState from '@/components/ui/EmptyState.vue'
import MethodBadge from '@/components/ui/MethodBadge.vue'
import { formatDuration, formatRelativeTime, statusTone } from '@/composables/useFormat'
import { api } from '@/services/ipc'
import { useDialogStore } from '@/stores/dialogs'
import { useTabsStore } from '@/stores/tabs'
import { useToastStore } from '@/stores/toast'
import { useWorkspaceStore } from '@/stores/workspace'

const workspace = useWorkspaceStore()
const tabs = useTabsStore()
const toasts = useToastStore()
const dialogs = useDialogStore()

const entries = ref<HistoryEntry[]>([])
const loading = ref(false)

const toneColor: Record<ReturnType<typeof statusTone>, string> = {
  success: 'var(--success)',
  info: 'var(--info)',
  warning: 'var(--warning)',
  danger: 'var(--danger)',
}

async function load () {
  if (!workspace.current) return
  loading.value = true
  try {
    entries.value = await api.historyList(workspace.current.id, 200, 0)
  } catch (error) {
    toasts.error('Could not load history', error)
  } finally {
    loading.value = false
  }
}

async function clear () {
  if (!workspace.current) return

  const confirmed = await dialogs.confirm({
    title: 'Clear request history?',
    description: `${entries.value.length} entries will be removed from this machine. History is never synced, so this cannot be recovered from a server.`,
    confirmLabel: 'Clear history',
    destructive: true,
  })
  if (!confirmed) return

  await api.historyClear(workspace.current.id)
  await load()
}

function open (entry: HistoryEntry) {
  if (entry.requestId) void tabs.openRequest(entry.requestId)
}

onMounted(load)
watch(() => workspace.current?.id, load)
</script>

<template>
  <div class="flex min-h-0 flex-col">
    <div class="flex shrink-0 items-center gap-1 px-2 py-2">
      <button type="button" class="btn btn-ghost flex-1 py-1 text-xs" @click="load">
        Refresh
      </button>
      <button
        type="button"
        class="btn btn-ghost py-1 text-xs"
        :disabled="!entries.length"
        @click="clear"
      >
        <Trash2 :size="12" />
        Clear
      </button>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
      <EmptyState
        v-if="!entries.length && !loading"
        title="No history yet"
        description="Requests you send appear here. History is local and never synchronised."
      >
        <template #icon><Clock :size="22" /></template>
      </EmptyState>

      <ul v-else role="list" class="space-y-0.5">
        <li v-for="entry in entries" :key="entry.id">
          <button
            type="button"
            class="row flex-col items-start gap-0.5 py-1.5"
            :disabled="!entry.requestId"
            @click="open(entry)"
          >
            <span class="flex w-full items-center gap-2">
              <MethodBadge :method="entry.method" class="w-10 shrink-0 text-right" />
              <span class="truncate text-xs">{{ entry.name || entry.url }}</span>
              <span
                v-if="entry.status"
                class="ml-auto shrink-0 font-mono text-xxs tabular-nums"
                :style="{ color: toneColor[statusTone(entry.status)] }"
              >{{ entry.status }}</span>
              <span
                v-else
                class="ml-auto shrink-0 text-xxs"
                :style="{ color: 'var(--danger)' }"
              >failed</span>
            </span>
            <span class="flex w-full items-center gap-2 pl-12 text-xxs text-muted">
              <span>{{ formatRelativeTime(entry.executedAt) }}</span>
              <span class="tabular-nums">{{ formatDuration(entry.durationMs) }}</span>
            </span>
          </button>
        </li>
      </ul>
    </div>
  </div>
</template>
