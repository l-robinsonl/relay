<script setup lang="ts">
import { computed, ref } from 'vue'
import { Eye, EyeOff, Key, Plus, Settings2, Trash2 } from '@lucide/vue'
import type { Environment, Variable } from '@relay/shared-types'
import EmptyState from '@/components/ui/EmptyState.vue'
import { api } from '@/services/ipc'
import { useToastStore } from '@/stores/toast'
import { useWorkspaceStore } from '@/stores/workspace'

const workspace = useWorkspaceStore()
const toasts = useToastStore()

const editingId = ref<string | null>(null)
const secretDraft = ref<Record<string, string>>({})
const revealed = ref<Set<string>>(new Set())

const editing = computed<Environment | null>(
  () => workspace.environments.find((e) => e.id === editingId.value) ?? null,
)

/** The variable rows, plus a blank one so adding is just typing. */
const rows = computed<Variable[]>(() => [
  ...(editing.value?.variables ?? []),
  { key: '', value: '', enabled: true, secret: false },
])

async function create () {
  const name = window.prompt('Environment name', 'Development')
  if (!name?.trim()) return
  const environment = await workspace.createEnvironment(name.trim())
  if (environment) editingId.value = environment.id
}

async function remove (id: string, name: string) {
  if (!window.confirm(`Delete the environment "${name}"?`)) return
  await workspace.deleteEnvironment(id)
  if (editingId.value === id) editingId.value = null
}

async function commit (index: number, patch: Partial<Variable>) {
  const environment = editing.value
  if (!environment) return

  const variables = [...environment.variables]
  const existing = variables[index]
  if (existing) variables[index] = { ...existing, ...patch }
  else variables.push({ key: '', value: '', enabled: true, secret: false, ...patch })

  await workspace.saveEnvironment({ ...environment, variables })
}

async function removeVariable (index: number) {
  const environment = editing.value
  if (!environment) return
  await workspace.saveEnvironment({
    ...environment,
    variables: environment.variables.filter((_, i) => i !== index),
  })
}

/** Writes a secret value to the credential store, never to the database. */
async function saveSecret (key: string) {
  const environment = editing.value
  if (!environment) return
  const value = secretDraft.value[key]
  if (value === undefined) return

  try {
    if (value) await api.secretSet(environment.id, key, value)
    else await api.secretDelete(environment.id, key)
    secretDraft.value = { ...secretDraft.value, [key]: '' }
    toasts.success(value ? `Stored ${key}` : `Cleared ${key}`)
  } catch (error) {
    toasts.error('Could not store the secret', error)
  }
}

function toggleReveal (key: string) {
  const next = new Set(revealed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  revealed.value = next
}
</script>

<template>
  <div class="flex min-h-0 flex-col">
    <div class="shrink-0 px-2 py-2">
      <button type="button" class="btn btn-ghost w-full py-1 text-xs" @click="create">
        <Plus :size="13" />
        New environment
      </button>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
      <EmptyState
        v-if="!workspace.environments.length"
        title="No environments"
        description="Environments hold the variables that change between Development, Staging and Production."
      >
        <template #icon><Settings2 :size="22" /></template>
      </EmptyState>

      <ul v-else role="list" class="space-y-0.5">
        <li v-for="environment in workspace.environments" :key="environment.id">
          <div class="group flex items-center">
            <button
              type="button"
              class="row flex-1"
              :class="editingId === environment.id && 'is-selected'"
              @click="editingId = editingId === environment.id ? null : environment.id"
            >
              <span
                class="h-2 w-2 shrink-0 rounded-full"
                :style="{
                  backgroundColor:
                    workspace.activeEnvironmentId === environment.id
                      ? 'var(--success)'
                      : 'var(--border-strong)',
                }"
              />
              <span class="truncate text-sm">{{ environment.name }}</span>
              <span class="ml-auto shrink-0 text-xxs text-muted tabular-nums">
                {{ environment.variables.length }}
              </span>
            </button>
            <button
              type="button"
              class="btn-ghost rounded p-1 text-muted opacity-0 transition
                     hover:text-[var(--danger)] group-hover:opacity-100"
              :aria-label="`Delete ${environment.name}`"
              @click="remove(environment.id, environment.name)"
            >
              <Trash2 :size="13" />
            </button>
          </div>

          <!-- Variable editor -->
          <div v-if="editingId === environment.id" class="animate-in mt-1 space-y-1 pb-2 pl-2">
            <div
              v-for="(row, index) in rows"
              :key="index"
              class="space-y-1 rounded border border-subtle p-1.5"
            >
              <div class="flex items-center gap-1.5">
                <input
                  type="checkbox"
                  class="h-3 w-3 shrink-0 accent-[var(--accent)]"
                  :checked="row.enabled"
                  :disabled="index >= (editing?.variables.length ?? 0)"
                  :aria-label="`Enable ${row.key || 'variable'}`"
                  @change="commit(index, { enabled: ($event.target as HTMLInputElement).checked })"
                />
                <input
                  class="input-mono min-w-0 flex-1 border-0 bg-transparent px-0 py-0.5 text-xs
                         outline-none"
                  :value="row.key"
                  placeholder="name"
                  spellcheck="false"
                  @change="commit(index, { key: ($event.target as HTMLInputElement).value })"
                />
                <button
                  type="button"
                  class="shrink-0 rounded p-0.5 transition-colors"
                  :style="{ color: row.secret ? 'var(--warning)' : 'var(--text-muted)' }"
                  :aria-pressed="row.secret"
                  :aria-label="`Mark ${row.key || 'variable'} as secret`"
                  title="Secret — stored in your OS credential store, never synced or exported"
                  @click="commit(index, { secret: !row.secret, value: '' })"
                >
                  <Key :size="12" />
                </button>
                <button
                  v-if="index < (editing?.variables.length ?? 0)"
                  type="button"
                  class="shrink-0 rounded p-0.5 text-muted hover:text-[var(--danger)]"
                  :aria-label="`Remove ${row.key || 'variable'}`"
                  @click="removeVariable(index)"
                >
                  <Trash2 :size="12" />
                </button>
              </div>

              <!-- Non-secret value -->
              <input
                v-if="!row.secret"
                class="input input-mono py-0.5 text-xs"
                :value="row.value"
                placeholder="value"
                spellcheck="false"
                @change="commit(index, { value: ($event.target as HTMLInputElement).value })"
              />

              <!-- Secret value: write-only -->
              <div v-else-if="row.key" class="flex items-center gap-1">
                <input
                  :type="revealed.has(row.key) ? 'text' : 'password'"
                  class="input input-mono flex-1 py-0.5 text-xs"
                  :value="secretDraft[row.key] ?? ''"
                  placeholder="Enter to store in the credential store"
                  autocomplete="off"
                  @input="secretDraft = { ...secretDraft, [row.key]: ($event.target as HTMLInputElement).value }"
                  @keydown.enter="saveSecret(row.key)"
                  @blur="saveSecret(row.key)"
                />
                <button
                  type="button"
                  class="shrink-0 rounded p-1 text-muted"
                  :aria-label="revealed.has(row.key) ? 'Hide' : 'Show'"
                  @click="toggleReveal(row.key)"
                >
                  <component :is="revealed.has(row.key) ? EyeOff : Eye" :size="12" />
                </button>
              </div>
            </div>

            <p class="px-1 pt-1 text-xxs leading-relaxed text-muted">
              Secret values live in your operating system's credential store. They are
              never written to the database, included in an export, or sent to a sync
              server — teammates see the name and supply their own value.
            </p>
          </div>
        </li>
      </ul>
    </div>
  </div>
</template>
