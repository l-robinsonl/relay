<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  AlertTriangle,
  CheckCircle2,
  FileJson,
  FolderOpen,
  Loader2,
  X,
} from '@lucide/vue'
import type { ImportReport } from '@relay/shared-types'
import MethodBadge from '@/components/ui/MethodBadge.vue'
import { api } from '@/services/ipc'
import { useToastStore } from '@/stores/toast'
import { useWorkspaceStore } from '@/stores/workspace'

/**
 * The import flow: choose a file, see exactly what will be created and what
 * will not survive, then accept.
 *
 * The preview step exists because an import is hard to undo. Finding out
 * after the fact that the auth on 200 requests did not come across is far
 * worse than being told first.
 */
const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const workspace = useWorkspaceStore()
const toasts = useToastStore()

const report = ref<ImportReport | null>(null)
const fileName = ref('')
const loading = ref(false)
const applying = ref(false)
const error = ref<string | null>(null)

const counts = computed(() => {
  if (!report.value) return null
  return {
    collections: report.value.collections.length,
    requests: report.value.requests.length,
    environments: report.value.environments.length,
  }
})

const total = computed(() =>
  counts.value
    ? counts.value.collections + counts.value.requests + counts.value.environments
    : 0,
)

/** The first requests, as a preview of what is coming. */
const sampleRequests = computed(() => report.value?.requests.slice(0, 8) ?? [])

watch(
  () => props.open,
  (isOpen) => {
    if (!isOpen) return
    report.value = null
    fileName.value = ''
    error.value = null
  },
)

async function chooseFile () {
  if (!workspace.current) return

  const { open } = await import('@tauri-apps/plugin-dialog')
  const selected = await open({
    multiple: false,
    filters: [
      { name: 'Specifications and exports', extensions: ['json', 'yaml', 'yml'] },
    ],
  })
  if (typeof selected !== 'string') return

  fileName.value = selected.split(/[/\\]/).pop() ?? selected
  await preview(selected)
}

async function preview (path: string) {
  if (!workspace.current) return
  loading.value = true
  error.value = null

  try {
    // Rust reads the file. The webview holds no filesystem permission, and
    // the command hands back a parsed report rather than the file's text.
    report.value = await api.importPreviewPath(path, workspace.current.id)
  } catch (cause) {
    report.value = null
    error.value =
      cause && typeof cause === 'object' && 'message' in cause
        ? String((cause as { message: unknown }).message)
        : String(cause)
  } finally {
    loading.value = false
  }
}

/** Accepts a file dropped onto the dialog. */
async function onDrop (event: DragEvent) {
  const file = event.dataTransfer?.files?.[0]
  if (!file || !workspace.current) return

  fileName.value = file.name
  loading.value = true
  error.value = null
  try {
    const content = await file.text()
    report.value = await api.importPreview(content, workspace.current.id)
  } catch (cause) {
    report.value = null
    error.value =
      cause && typeof cause === 'object' && 'message' in cause
        ? String((cause as { message: unknown }).message)
        : String(cause)
  } finally {
    loading.value = false
  }
}

async function apply () {
  if (!report.value) return
  applying.value = true
  try {
    const saved = await api.importApply(report.value)
    await workspace.refreshTree()
    await workspace.refreshEnvironments()
    toasts.success(`Imported ${saved} object${saved === 1 ? '' : 's'}`)
    emit('close')
  } catch (cause) {
    toasts.error('The import could not be saved', cause)
  } finally {
    applying.value = false
  }
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-center justify-center p-4"
      role="dialog"
      aria-modal="true"
      aria-labelledby="import-title"
    >
      <div class="absolute inset-0 bg-black/40 backdrop-blur-[2px]" @click="emit('close')" />

      <div
        class="animate-slide-up relative flex max-h-[80vh] w-full max-w-2xl flex-col
               overflow-hidden rounded-lg border border-default
               bg-[var(--surface-overlay)]"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
        @dragover.prevent
        @drop.prevent="onDrop"
      >
        <header class="flex shrink-0 items-center gap-2 border-b border-subtle px-4 py-3">
          <FileJson :size="16" class="text-muted" />
          <h2 id="import-title" class="text-sm font-medium text-primary">Import</h2>
          <button
            type="button"
            class="ml-auto rounded p-1 text-muted hover:text-primary"
            aria-label="Close"
            @click="emit('close')"
          >
            <X :size="14" />
          </button>
        </header>

        <div class="min-h-0 flex-1 overflow-y-auto p-4">
          <!-- Choose a file -->
          <div v-if="!report && !loading" class="space-y-3">
            <button
              type="button"
              class="flex w-full flex-col items-center gap-2 rounded-lg border border-dashed
                     border-default px-6 py-10 text-center transition-colors
                     hover:border-[var(--accent)] hover:bg-[var(--surface-hover)]"
              @click="chooseFile"
            >
              <FolderOpen :size="22" class="text-muted" />
              <span class="text-sm font-medium text-secondary">
                Choose a file, or drop one here
              </span>
              <span class="text-xs text-muted">
                OpenAPI 3 (JSON or YAML), a Postman collection or environment, or a Relay
                workspace export
              </span>
            </button>

            <div
              v-if="error"
              class="rounded-md border p-3"
              :style="{ borderColor: 'var(--danger)', backgroundColor: 'var(--danger-muted)' }"
            >
              <p class="text-xs font-medium" :style="{ color: 'var(--danger)' }">
                {{ fileName || 'That file' }} could not be read
              </p>
              <p class="selectable mt-1 text-xs text-secondary">{{ error }}</p>
            </div>
          </div>

          <div v-else-if="loading" class="flex items-center justify-center gap-2 py-16">
            <Loader2 :size="16" class="animate-spin" :style="{ color: 'var(--accent)' }" />
            <span class="text-sm text-muted">Reading {{ fileName }}…</span>
          </div>

          <!-- Preview -->
          <div v-else-if="report" class="space-y-4">
            <div>
              <p class="text-xs text-muted">{{ fileName }}</p>
              <p class="text-sm font-medium text-primary">{{ report.sourceFormat }}</p>
            </div>

            <div v-if="counts" class="grid grid-cols-3 gap-2">
              <div
                v-for="item in [
                  { label: 'Collections', value: counts.collections },
                  { label: 'Requests', value: counts.requests },
                  { label: 'Environments', value: counts.environments },
                ]"
                :key="item.label"
                class="rounded-md border border-subtle px-3 py-2"
              >
                <p class="text-lg font-semibold tabular-nums text-primary">{{ item.value }}</p>
                <p class="text-xxs uppercase tracking-wide text-muted">{{ item.label }}</p>
              </div>
            </div>

            <div v-if="sampleRequests.length">
              <p class="mb-1.5 text-xxs font-medium uppercase tracking-wide text-muted">
                Requests
              </p>
              <ul class="space-y-0.5 rounded-md border border-subtle p-1">
                <li
                  v-for="request in sampleRequests"
                  :key="request.id"
                  class="flex items-center gap-2 px-2 py-1"
                >
                  <MethodBadge :method="request.method" class="w-10 shrink-0 text-right" />
                  <span class="truncate text-xs text-secondary">{{ request.name }}</span>
                  <span class="ml-auto truncate font-mono text-xxs text-muted">
                    {{ request.url }}
                  </span>
                </li>
                <li
                  v-if="report.requests.length > sampleRequests.length"
                  class="px-2 py-1 text-xs text-muted"
                >
                  and {{ report.requests.length - sampleRequests.length }} more
                </li>
              </ul>
            </div>

            <!-- What will not come across -->
            <div v-if="report.warnings.length">
              <p
                class="mb-1.5 flex items-center gap-1.5 text-xxs font-medium uppercase
                       tracking-wide"
                :style="{ color: 'var(--warning)' }"
              >
                <AlertTriangle :size="12" />
                {{ report.warnings.length }} thing{{ report.warnings.length === 1 ? '' : 's' }}
                will not come across
              </p>
              <ul
                class="max-h-48 space-y-1.5 overflow-y-auto rounded-md border p-2"
                :style="{
                  borderColor: 'var(--warning)',
                  backgroundColor: 'var(--warning-muted)',
                }"
              >
                <li v-for="(warning, index) in report.warnings" :key="index" class="text-xs">
                  <p class="font-mono text-xxs text-muted">{{ warning.path }}</p>
                  <p class="text-secondary">{{ warning.message }}</p>
                </li>
              </ul>
            </div>

            <p
              v-else
              class="flex items-center gap-1.5 text-xs"
              :style="{ color: 'var(--success)' }"
            >
              <CheckCircle2 :size="13" />
              Everything in this file can be imported.
            </p>
          </div>
        </div>

        <footer
          v-if="report"
          class="flex shrink-0 items-center justify-end gap-2 border-t border-subtle px-4 py-3"
        >
          <button type="button" class="btn text-xs" @click="emit('close')">Cancel</button>
          <button
            type="button"
            class="btn btn-primary text-xs"
            :disabled="applying || total === 0"
            @click="apply"
          >
            <Loader2 v-if="applying" :size="13" class="animate-spin" />
            Import {{ total }} object{{ total === 1 ? '' : 's' }}
          </button>
        </footer>
      </div>
    </div>
  </Teleport>
</template>
