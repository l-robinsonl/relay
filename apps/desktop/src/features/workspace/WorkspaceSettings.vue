<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Cookie, Key, Settings2, Trash2, X } from '@lucide/vue'
import type { Variable } from '@relay/shared-types'
import { api } from '@/services/ipc'
import { useDialogStore } from '@/stores/dialogs'
import { useToastStore } from '@/stores/toast'
import { useWorkspaceStore } from '@/stores/workspace'

/**
 * Workspace-level settings: the variables every request in the workspace
 * can see, and the shared cookie jar.
 *
 * Workspace variables sit at the bottom of the precedence chain, so this
 * is where a `baseUrl` that is the same in every environment belongs —
 * rather than being copied into each one.
 */
const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const workspace = useWorkspaceStore()
const dialogs = useDialogStore()
const toasts = useToastStore()

const draft = ref<Variable[]>([])
const saving = ref(false)

/** The stored rows plus a trailing blank, so adding is just typing. */
const rows = computed<Variable[]>(() => [
  ...draft.value,
  { key: '', value: '', enabled: true, secret: false },
])

const dirty = computed(
  () => JSON.stringify(draft.value) !== JSON.stringify(workspace.current?.variables ?? []),
)

watch(
  () => props.open,
  (isOpen) => {
    if (isOpen) draft.value = structuredClone(workspace.current?.variables ?? [])
  },
)

function commit (index: number, patch: Partial<Variable>) {
  const next = [...draft.value]
  const existing = next[index]
  if (existing) next[index] = { ...existing, ...patch }
  else next.push({ key: '', value: '', enabled: true, secret: false, ...patch })
  draft.value = next
}

function remove (index: number) {
  draft.value = draft.value.filter((_, i) => i !== index)
}

async function save () {
  saving.value = true
  try {
    await workspace.saveVariables(draft.value)
    toasts.success('Workspace variables saved')
  } finally {
    saving.value = false
  }
}

async function clearCookies () {
  const confirmed = await dialogs.confirm({
    title: 'Clear stored cookies?',
    description:
      'Any session you have established by signing in through Relay will be forgotten, and those requests will need signing in again.',
    confirmLabel: 'Clear cookies',
    destructive: true,
  })
  if (!confirmed) return

  try {
    await api.cookiesClear()
    toasts.success('Cookies cleared')
  } catch (error) {
    toasts.error('Could not clear cookies', error)
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
      aria-labelledby="workspace-settings-title"
    >
      <div class="absolute inset-0 bg-black/40 backdrop-blur-[2px]" @click="emit('close')" />

      <div
        class="animate-slide-up relative flex max-h-[80vh] w-full max-w-xl flex-col
               overflow-hidden rounded-lg border border-default
               bg-[var(--surface-overlay)]"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
      >
        <header class="flex shrink-0 items-center gap-2 border-b border-subtle px-4 py-3">
          <Settings2 :size="16" class="text-muted" />
          <h2 id="workspace-settings-title" class="text-sm font-medium text-primary">
            {{ workspace.current?.name ?? 'Workspace' }}
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

        <div class="min-h-0 flex-1 space-y-5 overflow-y-auto p-4">
          <section>
            <h3 class="mb-1 text-xs font-medium text-secondary">Variables</h3>
            <p class="mb-2 text-xs text-muted">
              Available to every request in this workspace. An environment of the same
              name wins, so put the values that do not change between environments here.
            </p>

            <div class="space-y-1">
              <div
                v-for="(row, index) in rows"
                :key="index"
                class="grid grid-cols-[2rem_minmax(0,1fr)_minmax(0,1.4fr)_2rem] items-center
                       gap-x-2"
              >
                <input
                  type="checkbox"
                  class="h-3.5 w-3.5 cursor-pointer accent-[var(--accent)]"
                  :checked="row.enabled"
                  :disabled="index >= draft.length"
                  :aria-label="row.key ? `Enable ${row.key}` : 'Enable this row'"
                  @change="commit(index, { enabled: ($event.target as HTMLInputElement).checked })"
                />
                <input
                  class="input input-mono py-1 text-xs"
                  :value="row.key"
                  placeholder="name"
                  spellcheck="false"
                  @change="commit(index, { key: ($event.target as HTMLInputElement).value })"
                />
                <input
                  class="input input-mono py-1 text-xs"
                  :value="row.value"
                  placeholder="value"
                  spellcheck="false"
                  @change="commit(index, { value: ($event.target as HTMLInputElement).value })"
                />
                <button
                  v-if="index < draft.length"
                  type="button"
                  class="rounded p-1 text-muted hover:text-[var(--danger)]"
                  :aria-label="`Remove ${row.key || 'row'}`"
                  @click="remove(index)"
                >
                  <Trash2 :size="13" />
                </button>
                <span v-else />
              </div>
            </div>

            <p class="mt-2 flex items-start gap-1.5 text-xs text-muted">
              <Key :size="12" class="mt-0.5 shrink-0" />
              <span>
                Secrets belong in an environment, not here — only environment variables
                can be marked secret and kept in your credential store.
              </span>
            </p>

            <button
              type="button"
              class="btn btn-primary mt-3 text-xs"
              :disabled="!dirty || saving"
              @click="save"
            >
              {{ dirty ? 'Save variables' : 'Saved' }}
            </button>
          </section>

          <section class="border-t border-subtle pt-4">
            <h3 class="mb-1 text-xs font-medium text-secondary">Cookies</h3>
            <p class="mb-2 text-xs text-muted">
              Relay keeps cookies across requests, so signing in and then calling an
              endpoint works. A stale session cookie breaks later requests in a way that
              is hard to spot — this throws them all away.
            </p>
            <button type="button" class="btn text-xs" @click="clearCookies">
              <Cookie :size="13" />
              Clear stored cookies
            </button>
          </section>
        </div>
      </div>
    </div>
  </Teleport>
</template>
