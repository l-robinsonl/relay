<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { AlertTriangle, ArrowLeftRight, Check, X } from '@lucide/vue'
import type { ConflictView } from '@/services/ipc'
import { formatRelativeTime } from '@/composables/useFormat'
import { useSyncStore } from '@/stores/sync'

/**
 * Resolving conflicts.
 *
 * Both versions are shown side by side with the differing fields marked,
 * because "keep mine or theirs" is impossible to answer without seeing
 * what actually differs.
 */
const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const sync = useSyncStore()
const selectedId = ref<string | null>(null)

const selected = computed<ConflictView | null>(
  () => sync.conflicts.find((c) => c.id === selectedId.value) ?? sync.conflicts[0] ?? null,
)

watch(
  () => props.open,
  (isOpen) => {
    if (isOpen) selectedId.value = sync.conflicts[0]?.id ?? null
  },
)

/** Fields that differ between the two versions, for the comparison table. */
const differences = computed(() => {
  const conflict = selected.value
  if (!conflict) return []

  const local = conflict.local ?? {}
  const server = conflict.server ?? {}

  // Metadata changes on every save and tells the user nothing.
  const ignored = new Set(['updatedAt', 'createdAt', 'rev', 'dirty', 'deletedAt'])
  const keys = new Set([...Object.keys(local), ...Object.keys(server)])

  return [...keys]
    .filter((key) => !ignored.has(key))
    .map((key) => ({
      key,
      local: format((local as Record<string, unknown>)[key]),
      server: format((server as Record<string, unknown>)[key]),
    }))
    .filter((row) => row.local !== row.server)
})

function format (value: unknown): string {
  if (value === undefined || value === null) return '—'
  if (typeof value === 'string') return value || '(empty)'
  return JSON.stringify(value)
}

async function resolve (keepLocal: boolean) {
  const conflict = selected.value
  if (!conflict) return

  await sync.resolve(conflict.id, keepLocal)
  selectedId.value = sync.conflicts[0]?.id ?? null
  if (sync.conflicts.length === 0) emit('close')
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-center justify-center p-4"
      role="dialog"
      aria-modal="true"
      aria-labelledby="conflicts-title"
    >
      <div class="absolute inset-0 bg-black/40 backdrop-blur-[2px]" @click="emit('close')" />

      <div
        class="animate-slide-up relative flex max-h-[80vh] w-full max-w-3xl flex-col
               overflow-hidden rounded-lg border border-default
               bg-[var(--surface-overlay)]"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
      >
        <header class="flex shrink-0 items-center gap-2 border-b border-subtle px-4 py-3">
          <AlertTriangle :size="16" :style="{ color: 'var(--warning)' }" />
          <h2 id="conflicts-title" class="text-sm font-medium text-primary">
            {{ sync.conflicts.length }} conflict{{ sync.conflicts.length === 1 ? '' : 's' }}
          </h2>
          <button
            type="button"
            class="ml-auto rounded p-1 text-muted hover:text-primary"
            aria-label="Close"
            @click="emit('close')"
          >
            <X :size="14" />
          </button>
        </header>

        <p class="shrink-0 border-b border-subtle px-4 py-2 text-xs text-muted">
          Someone else changed these while you were working on them. Nothing has been
          overwritten — both versions are here, and you choose.
        </p>

        <div class="flex min-h-0 flex-1">
          <!-- The list -->
          <ul
            v-if="sync.conflicts.length > 1"
            class="w-56 shrink-0 overflow-y-auto border-r border-subtle p-1.5"
          >
            <li v-for="conflict in sync.conflicts" :key="conflict.id">
              <button
                type="button"
                class="row flex-col items-start gap-0.5"
                :class="selected?.id === conflict.id && 'is-selected'"
                @click="selectedId = conflict.id"
              >
                <span class="truncate text-xs">
                  {{ conflict.name ?? conflict.entityId }}
                </span>
                <span class="text-xxs text-muted">
                  {{ conflict.entity }} · {{ formatRelativeTime(conflict.detectedAt) }}
                </span>
              </button>
            </li>
          </ul>

          <!-- The comparison -->
          <div v-if="selected" class="min-h-0 flex-1 overflow-y-auto p-4">
            <div class="mb-3">
              <p class="text-sm font-medium text-primary">
                {{ selected.name ?? selected.entityId }}
              </p>
              <p class="text-xs text-muted">
                {{ selected.entity }} · noticed
                {{ formatRelativeTime(selected.detectedAt) }}
              </p>
            </div>

            <div
              v-if="selected.serverDeleted"
              class="mb-3 rounded-md border p-3 text-xs"
              :style="{ borderColor: 'var(--warning)', backgroundColor: 'var(--warning-muted)' }"
            >
              Someone deleted this on the server while you were editing it. Keeping your
              version will restore it for everyone.
            </div>

            <table v-else-if="differences.length" class="w-full text-xs">
              <thead>
                <tr class="border-b border-subtle text-left text-xxs uppercase
                           tracking-wide text-muted">
                  <th class="w-28 py-1.5 pr-3 font-medium">Field</th>
                  <th class="py-1.5 pr-3 font-medium">Yours</th>
                  <th class="py-1.5 font-medium">Theirs</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="row in differences"
                  :key="row.key"
                  class="border-b border-subtle align-top"
                >
                  <td class="py-1.5 pr-3 font-mono text-muted">{{ row.key }}</td>
                  <td class="selectable break-all py-1.5 pr-3 font-mono text-primary">
                    {{ row.local }}
                  </td>
                  <td class="selectable break-all py-1.5 font-mono text-secondary">
                    {{ row.server }}
                  </td>
                </tr>
              </tbody>
            </table>

            <p v-else class="text-xs text-muted">
              The two versions look the same in every field Relay compares. Either choice
              is safe.
            </p>
          </div>
        </div>

        <footer
          v-if="selected"
          class="flex shrink-0 items-center gap-2 border-t border-subtle px-4 py-3"
        >
          <ArrowLeftRight :size="13" class="shrink-0 text-muted" />
          <span class="text-xs text-muted">Which version should win?</span>
          <div class="ml-auto flex gap-2">
            <button type="button" class="btn text-xs" @click="resolve(false)">
              Take theirs
            </button>
            <button type="button" class="btn btn-primary text-xs" @click="resolve(true)">
              <Check :size="13" />
              Keep mine
            </button>
          </div>
        </footer>
      </div>
    </div>
  </Teleport>
</template>
