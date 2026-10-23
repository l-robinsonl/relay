<script setup lang="ts">
import { computed, ref } from 'vue'
import { Check, ChevronRight, Loader2, Send, TriangleAlert, X } from '@lucide/vue'
import type { ToolCall, ToolResult, ToolRisk } from '@/services/ipc'

/** One tool call in the transcript: running, finished, or awaiting an answer. */
const props = defineProps<{
  call: ToolCall
  result?: ToolResult | null
  /** Set when the person is being asked whether this may run. */
  asking?: ToolRisk | null
  busy?: boolean
}>()

const emit = defineEmits<{ answer: [allow: boolean] }>()

const open = ref(false)

/** A readable name. The model sees snake_case; a person should not. */
const label = computed(() =>
  props.call.name.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase()),
)

/** The single most useful argument, shown without expanding. */
const summary = computed(() => {
  const input = props.call.input
  if (!input || typeof input !== 'object') return ''
  const record = input as Record<string, unknown>
  for (const key of ['name', 'url', 'id', 'email']) {
    const value = record[key]
    if (typeof value === 'string' && value !== '') return value
  }
  return ''
})

const failed = computed(() => props.result?.outcome.isError === true)

const pretty = computed(() => JSON.stringify(props.call.input, null, 2))
</script>

<template>
  <div
    class="rounded border text-xs"
    :style="
      asking
        ? { borderColor: 'var(--accent)' }
        : { borderColor: 'var(--border-subtle)' }
    "
  >
    <button
      type="button"
      class="flex w-full items-center gap-2 px-2.5 py-1.5 text-left"
      :aria-expanded="open"
      @click="open = !open"
    >
      <ChevronRight
        :size="12"
        class="shrink-0 text-muted transition-transform"
        :class="open ? 'rotate-90' : ''"
      />

      <Loader2 v-if="busy && !result" :size="12" class="shrink-0 animate-spin text-muted" />
      <TriangleAlert
        v-else-if="failed"
        :size="12"
        class="shrink-0"
        :style="{ color: 'var(--danger)' }"
      />
      <Send
        v-else-if="asking"
        :size="12"
        class="shrink-0"
        :style="{ color: 'var(--accent)' }"
      />
      <Check
        v-else-if="result"
        :size="12"
        class="shrink-0"
        :style="{ color: 'var(--success)' }"
      />

      <span class="text-secondary">{{ label }}</span>
      <span v-if="summary" class="truncate font-mono text-muted">{{ summary }}</span>
    </button>

    <!-- Asking. In the transcript rather than a modal, so what it wants
         to do is read in the context of why it wants to. -->
    <div v-if="asking" class="border-t px-2.5 py-2" :style="{ borderColor: 'var(--border-subtle)' }">
      <p class="mb-2 text-xs text-secondary">
        <template v-if="asking === 'send'">
          This sends a real request with your credentials. The server cannot tell it
          apart from one you sent yourself.
        </template>
        <template v-else>
          This destroys something. It cannot be undone from here.
        </template>
      </p>
      <div class="flex gap-2">
        <!-- Deliberately not disabled by `busy`. The turn is always in
             flight while this is on screen — it is in flight *because*
             it is waiting for this answer — so disabling on `busy`
             would disable the buttons exactly when they are needed. -->
        <!-- Red when it destroys something, not the inviting blue.
             Both answers were styled identically, so the button with
             the most visual pull — the one people hit to make a
             prompt go away — was the one that deletes. The colour
             should say which of the two you cannot take back. -->
        <button
          type="button"
          class="btn text-xs"
          :class="asking === 'destructive' ? 'btn-danger' : 'btn-primary'"
          @click.stop="emit('answer', true)"
        >
          <Check :size="12" />
          Allow
        </button>
        <button type="button" class="btn text-xs" @click.stop="emit('answer', false)">
          <X :size="12" />
          Don't
        </button>
      </div>
    </div>

    <div v-if="open" class="border-t px-2.5 py-2" :style="{ borderColor: 'var(--border-subtle)' }">
      <p class="mb-1 text-xxs uppercase tracking-wide text-muted">Arguments</p>
      <pre class="overflow-x-auto whitespace-pre-wrap break-all font-mono text-xs text-secondary">{{ pretty }}</pre>

      <template v-if="result">
        <p class="mb-1 mt-2 text-xxs uppercase tracking-wide text-muted">Result</p>
        <pre
          class="max-h-48 overflow-auto whitespace-pre-wrap break-all font-mono text-xs"
          :style="failed ? { color: 'var(--danger)' } : { color: 'var(--text-secondary)' }"
        >{{ result.outcome.content }}</pre>
      </template>
    </div>
  </div>
</template>
