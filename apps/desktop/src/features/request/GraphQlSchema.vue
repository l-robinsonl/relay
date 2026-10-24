<script setup lang="ts">
import { computed, ref } from 'vue'
import { ChevronRight, Loader2, RefreshCw, TriangleAlert } from '@lucide/vue'
import type { Field, RequestDoc, Schema } from '@relay/shared-types'
import { api } from '@/services/ipc'

/**
 * What the endpoint accepts, read from the endpoint.
 *
 * Fetched rather than cached between sessions: a schema is a live
 * fact about a running server, and showing a stale one is worse than
 * showing none — somebody writes a query against a field that was
 * removed last week and cannot see why it fails.
 */
const props = defineProps<{ doc: RequestDoc, environment: string | null }>()

const schema = ref<Schema | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)
const filter = ref('')
const open = ref<Set<string>>(new Set())

function messageOf (value: unknown): string {
  if (value && typeof value === 'object' && 'message' in value) {
    const detail = 'detail' in value ? (value as { detail?: string | null }).detail : null
    const message = String((value as { message: unknown }).message)
    return detail ? `${message} — ${detail}` : message
  }
  return String(value)
}

async function load () {
  loading.value = true
  error.value = null
  try {
    schema.value = await api.graphqlSchema(props.doc, props.environment)
  } catch (e) {
    schema.value = null
    error.value = messageOf(e)
  } finally {
    loading.value = false
  }
}

function toggle (key: string) {
  const next = new Set(open.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  open.value = next
}

/** The operations, filtered, with their group kept. */
const groups = computed(() => {
  const needle = filter.value.trim().toLowerCase()
  const match = (fields: Field[]) =>
    needle
      ? fields.filter(
        (f) =>
          f.name.toLowerCase().includes(needle)
            || (f.description ?? '').toLowerCase().includes(needle),
      )
      : fields

  const current = schema.value
  if (!current) return []

  return [
    { label: 'Queries', fields: match(current.queries) },
    { label: 'Mutations', fields: match(current.mutations) },
    { label: 'Subscriptions', fields: match(current.subscriptions) },
  ].filter((g) => g.fields.length > 0)
})

const nothingMatched = computed(
  () => schema.value !== null && groups.value.length === 0 && filter.value.trim() !== '',
)
</script>

<template>
  <div class="flex h-full flex-col">
    <div class="flex shrink-0 items-center gap-2 border-b border-subtle px-3 py-1.5">
      <input
        v-model="filter"
        class="input py-1 text-xs"
        placeholder="Filter operations"
        spellcheck="false"
        aria-label="Filter operations"
        :disabled="!schema"
      />
      <button
        type="button"
        class="btn shrink-0 gap-1.5 py-1 text-xs"
        :disabled="loading"
        @click="load"
      >
        <Loader2 v-if="loading" :size="12" class="animate-spin" />
        <RefreshCw v-else :size="12" />
        {{ schema ? 'Refresh' : 'Read schema' }}
      </button>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto">
      <!-- Nothing fetched yet. Not an empty state so much as an
           unasked question: reading a schema is a real request to
           somebody's server, so it waits to be asked. -->
      <p v-if="!schema && !error && !loading" class="px-3 py-3 text-xs text-muted">
        Ask the endpoint what it accepts. The lookup goes out as this request does —
        same URL, same headers, same authentication — so a private API works.
      </p>

      <p v-else-if="loading" class="px-3 py-3 text-xs text-muted">
        Asking the endpoint…
      </p>

      <div
        v-else-if="error"
        class="m-3 flex items-start gap-2 rounded border p-2.5 text-xs"
        :style="{
          borderColor: 'var(--danger)',
          backgroundColor: 'var(--danger-muted)',
          color: 'var(--danger)',
        }"
      >
        <TriangleAlert :size="13" class="mt-px shrink-0" />
        <span>{{ error }}</span>
      </div>

      <p v-else-if="nothingMatched" class="px-3 py-3 text-xs text-muted">
        Nothing matches “{{ filter }}”.
      </p>

      <div v-for="group in groups" :key="group.label" class="border-b border-subtle">
        <p class="px-3 py-1.5 text-xxs font-medium uppercase tracking-wide text-muted">
          {{ group.label }}
        </p>
        <ul>
          <li v-for="field in group.fields" :key="field.name">
            <button
              type="button"
              class="flex w-full items-start gap-1.5 px-3 py-1.5 text-left
                     hover:bg-[var(--surface-hover)]"
              @click="toggle(group.label + field.name)"
            >
              <ChevronRight
                :size="12"
                class="mt-0.5 shrink-0 text-muted transition-transform"
                :class="open.has(group.label + field.name) ? 'rotate-90' : ''"
              />
              <span class="min-w-0 flex-1">
                <span class="font-mono text-xs text-primary">{{ field.name }}</span>
                <span class="ml-1.5 font-mono text-xs" :style="{ color: 'var(--accent-text)' }">
                  {{ field.typeName }}
                </span>
                <span
                  v-if="field.description"
                  class="mt-0.5 block truncate text-xs text-muted"
                >{{ field.description }}</span>
              </span>
            </button>

            <div
              v-if="open.has(group.label + field.name)"
              class="border-t border-subtle px-3 py-2 pl-7"
              :style="{ backgroundColor: 'var(--surface-sunken)' }"
            >
              <p v-if="field.description" class="mb-1.5 text-xs text-secondary">
                {{ field.description }}
              </p>
              <p v-if="!field.args.length" class="text-xs text-muted">Takes no arguments.</p>
              <ul v-else class="space-y-0.5">
                <li v-for="arg in field.args" :key="arg.name" class="font-mono text-xs">
                  <span class="text-primary">{{ arg.name }}</span>
                  <span :style="{ color: 'var(--accent-text)' }">: {{ arg.typeName }}</span>
                  <span v-if="arg.description" class="ml-1.5 font-sans text-muted">
                    {{ arg.description }}
                  </span>
                </li>
              </ul>
            </div>
          </li>
        </ul>
      </div>
    </div>
  </div>
</template>
