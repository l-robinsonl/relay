<script setup lang="ts">
import { computed, ref } from 'vue'
import { ArrowRightLeft, Loader2, RefreshCw, TriangleAlert } from '@lucide/vue'
import type { GrpcSchema, RequestDoc } from '@relay/shared-types'
import { api } from '@/services/ipc'

/**
 * What the server serves, read from the server.
 *
 * gRPC has no schema document to point at: reflection is how a
 * client finds out, and it is a live fact about a running process.
 * Caching one between sessions would mean offering a method that was
 * removed last week.
 */
const props = defineProps<{
  doc: RequestDoc
  environment: string | null
  /** The method currently chosen, as `package.Service/Method`. */
  selected: string
}>()

const emit = defineEmits<{
  /** A method was picked. The example body comes with it. */
  choose: [method: string, example: string]
}>()

const schema = ref<GrpcSchema | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)
const filter = ref('')

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
    schema.value = await api.grpcSchema(props.doc, props.environment)
  } catch (e) {
    schema.value = null
    error.value = messageOf(e)
  } finally {
    loading.value = false
  }
}

/** Every method, flattened, because that is what people search. */
const methods = computed(() => {
  const needle = filter.value.trim().toLowerCase()
  const all = (schema.value?.services ?? []).flatMap((service) =>
    service.methods.map((method) => ({
      path: `${service.name}/${method.name}`,
      service: service.name,
      ...method,
    })),
  )
  if (!needle) return all
  return all.filter((m) => m.path.toLowerCase().includes(needle))
})

/** What kind of call it is, in words rather than two booleans. */
function shape (method: { clientStreaming: boolean, serverStreaming: boolean }) {
  if (method.clientStreaming && method.serverStreaming) return 'bidirectional streaming'
  if (method.serverStreaming) return 'server streaming'
  if (method.clientStreaming) return 'client streaming'
  return 'unary'
}

/**
 * Whether Relay can call it yet.
 *
 * Only unary, for now. Listing the streaming ones and letting them be
 * picked would mean a method that fails the moment you press send,
 * with the reason arriving too late to be useful; listing them greyed
 * out says the same thing before the click.
 */
function callable (method: { clientStreaming: boolean, serverStreaming: boolean }) {
  return !method.clientStreaming && !method.serverStreaming
}

const streamingCount = computed(() => methods.value.filter((m) => !callable(m)).length)
</script>

<template>
  <div class="space-y-2 p-4">
    <div class="flex items-center gap-2">
      <button type="button" class="btn py-1 text-xs" :disabled="loading" @click="load">
        <Loader2 v-if="loading" :size="12" class="animate-spin" />
        <RefreshCw v-else :size="12" />
        {{ schema ? 'Read again' : 'Read schema' }}
      </button>
      <input
        v-if="schema"
        v-model="filter"
        class="input min-w-0 flex-1 py-1 text-xs"
        placeholder="Filter methods"
        aria-label="Filter methods"
      />
    </div>

    <p v-if="!schema && !loading && !error" class="text-xs text-muted">
      Ask the server which methods it has. It answers if reflection is switched on —
      most development servers have it, and many production ones do not.
    </p>

    <div
      v-if="error"
      class="flex items-start gap-2 rounded border p-2.5 text-xs"
      :style="{
        borderColor: 'var(--danger)',
        backgroundColor: 'var(--danger-muted)',
        color: 'var(--danger)',
      }"
    >
      <TriangleAlert :size="13" class="mt-0.5 shrink-0" />
      <span class="selectable">{{ error }}</span>
    </div>

    <!-- Said once, under the list, rather than on every greyed row. -->
    <p v-if="streamingCount" class="text-xxs text-muted">
      {{ streamingCount }} streaming
      {{ streamingCount === 1 ? 'method is' : 'methods are' }}
      listed but cannot be called yet — Relay does unary gRPC so far.
    </p>

    <p v-if="schema && !methods.length" class="text-xs text-muted">
      {{ filter ? 'Nothing matches.' : 'The server reported no services.' }}
    </p>

    <ul v-if="methods.length" class="divide-y divide-[var(--border-subtle)] text-xs">
      <li v-for="method in methods" :key="method.path">
        <button
          type="button"
          class="flex w-full items-baseline gap-2 py-1.5 text-left
                 disabled:cursor-not-allowed enabled:hover:text-primary"
          :disabled="!callable(method)"
          :title="callable(method) ? undefined : 'Relay cannot call a streaming method yet'"
          :style="{
            color: !callable(method)
              ? 'var(--text-muted)'
              : method.path === selected
                ? 'var(--accent-text)'
                : 'var(--text-secondary)',
          }"
          @click="emit('choose', method.path, method.example)"
        >
          <ArrowRightLeft :size="12" class="shrink-0 self-center" />
          <span class="min-w-0 flex-1 font-mono">
            <span class="text-muted">{{ method.service }}/</span>{{ method.name }}
          </span>
          <span class="shrink-0 text-xxs text-muted">{{ shape(method) }}</span>
        </button>
      </li>
    </ul>
  </div>
</template>
