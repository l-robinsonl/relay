<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { Loader2, Send, Sparkles, SquarePen, TriangleAlert } from '@lucide/vue'
import type { UnlistenFn } from '@tauri-apps/api/event'
import type {
  AssistantEvent,
  AssistantSettings,
  StoredMessage,
  ToolCall,
  ToolResult,
  ToolRisk,
} from '@/services/ipc'
import { api } from '@/services/ipc'
import { useWorkspaceStore } from '@/stores/workspace'
import ToolCallRow from './ToolCallRow.vue'

const workspace = useWorkspaceStore()

/** One thing in the transcript. */
type Entry =
  | { kind: 'said', role: 'user' | 'assistant', text: string }
  | { kind: 'tool', call: ToolCall, result: ToolResult | null, asking: ToolRisk | null }

const entries = ref<Entry[]>([])
const question = ref('')
const conversationId = ref<string | null>(null)
const thinking = ref(false)
const error = ref<string | null>(null)
const hasKey = ref<boolean | null>(null)
const transcript = ref<HTMLElement | null>(null)

/** Shown as an example; kept out of the template so the parser cannot
 *  mistake its braces for an interpolation. */
const VARIABLE_EXAMPLE = '{{baseUrl}}'

const settings = ref<AssistantSettings>({
  provider: 'anthropic',
  model: '',
  baseUrl: '',
})

let unlisten: UnlistenFn | null = null

const canSend = computed(() => question.value.trim() !== '' && !thinking.value)
/** True while the assistant is waiting on an answer from the person. */
const waiting = computed(() => entries.value.some((e) => e.kind === 'tool' && e.asking))

function messageOf (value: unknown): string {
  if (value && typeof value === 'object' && 'message' in value) {
    const detail = 'detail' in value ? (value as { detail?: string | null }).detail : null
    const message = String((value as { message: unknown }).message)
    return detail ? `${message} — ${detail}` : message
  }
  return String(value)
}

async function scrollDown () {
  await nextTick()
  const element = transcript.value
  if (element) element.scrollTop = element.scrollHeight
}

/** Applies one event from the running turn. */
function apply (event: AssistantEvent) {
  switch (event.type) {
    case 'said':
      entries.value.push({ kind: 'said', role: 'assistant', text: event.text })
      break
    case 'asking':
      entries.value.push({
        kind: 'tool',
        call: event.call,
        result: null,
        asking: event.risk,
      })
      break
    case 'running': {
      // An approved call was already added when it was asked about;
      // adding it again would show it twice.
      const known = entries.value.find(
        (e) => e.kind === 'tool' && e.call.id === event.call.id,
      )
      if (!known) {
        entries.value.push({ kind: 'tool', call: event.call, result: null, asking: null })
      }
      break
    }
    case 'ran': {
      const row = entries.value.find(
        (e) => e.kind === 'tool' && e.call.id === event.result.callId,
      )
      if (row && row.kind === 'tool') {
        row.result = event.result
        row.asking = null
      }
      break
    }
  }
  scrollDown()
}

async function listen () {
  if (unlisten) return
  unlisten = await api.onAssistantEvent(apply)
}

onBeforeUnmount(() => {
  unlisten?.()
  // A turn parked on a question the person can no longer answer would
  // otherwise wait forever.
  if (waiting.value) api.assistantDenyAll()
})

watch(
  () => settings.value.provider,
  async (provider) => {
    hasKey.value = await api.assistantHasKey(provider).catch(() => false)
  },
  { immediate: true },
)

async function answer (callId: string, allow: boolean) {
  const row = entries.value.find((e) => e.kind === 'tool' && e.call.id === callId)
  if (row && row.kind === 'tool') row.asking = null
  await api.assistantApprove(callId, allow)
}

async function ask () {
  const text = question.value.trim()
  if (text === '' || !workspace.current) return

  entries.value.push({ kind: 'said', role: 'user', text })
  question.value = ''
  error.value = null
  thinking.value = true
  await listen()
  await scrollDown()

  try {
    conversationId.value = await api.assistantAsk(
      workspace.current.id,
      conversationId.value,
      text,
      settings.value,
    )
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    thinking.value = false
    await scrollDown()
  }
}

function startOver () {
  conversationId.value = null
  entries.value = []
  error.value = null
}

/** Loads an earlier conversation into the transcript. */
function hydrate (messages: StoredMessage[]) {
  const out: Entry[] = []
  for (const message of messages) {
    if (message.body.trim() !== '') {
      out.push({ kind: 'said', role: message.role, text: message.body })
    }
    for (const call of message.toolCalls) {
      out.push({ kind: 'tool', call, result: null, asking: null })
    }
    for (const result of message.toolResults) {
      const row = out.find((e) => e.kind === 'tool' && e.call.id === result.callId)
      if (row && row.kind === 'tool') row.result = result
    }
  }
  entries.value = out
}

defineExpose({ hydrate, startOver })
</script>

<template>
  <section class="flex h-full min-h-0 flex-col" aria-label="Assistant">
    <header class="flex shrink-0 items-center gap-2 border-b border-subtle px-3 py-2">
      <Sparkles :size="14" class="text-muted" />
      <h2 class="text-xs font-medium text-secondary">Assistant</h2>
      <button
        type="button"
        class="ml-auto rounded p-1 text-muted hover:text-primary"
        aria-label="Start a new conversation"
        @click="startOver"
      >
        <SquarePen :size="13" />
      </button>
    </header>

    <div ref="transcript" class="min-h-0 flex-1 space-y-2.5 overflow-y-auto p-3">
      <!-- Nothing yet. Say what it can do rather than showing a blank box. -->
      <div v-if="entries.length === 0" class="space-y-2 text-xs text-muted">
        <p class="text-secondary">Ask for anything you could do here yourself.</p>
        <ul class="space-y-1">
          <li>"Add a request that posts a new user to the staging API"</li>
          <li>"Why did the last health check fail?"</li>
          <li>"Point every request in this collection at {{ VARIABLE_EXAMPLE }}"</li>
        </ul>
        <p>
          It cannot read your secret values, and it asks before sending a request or
          deleting anything.
        </p>
      </div>

      <template v-for="(entry, index) in entries" :key="index">
        <div v-if="entry.kind === 'said'" class="text-xs">
          <p
            v-if="entry.role === 'user'"
            class="ml-auto max-w-[85%] rounded-lg px-2.5 py-1.5 text-primary"
            :style="{ backgroundColor: 'var(--surface-raised)' }"
          >
            {{ entry.text }}
          </p>
          <p v-else class="whitespace-pre-wrap text-secondary">{{ entry.text }}</p>
        </div>

        <ToolCallRow
          v-else
          :call="entry.call"
          :result="entry.result"
          :asking="entry.asking"
          :busy="thinking"
          @answer="(allow) => answer(entry.call.id, allow)"
        />
      </template>

      <p v-if="thinking && !waiting" class="flex items-center gap-1.5 text-xs text-muted">
        <Loader2 :size="12" class="animate-spin" />
        Working…
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
        <TriangleAlert :size="13" class="mt-px shrink-0" />
        <span>{{ error }}</span>
      </div>
    </div>

    <div class="shrink-0 border-t border-subtle p-2">
      <p
        v-if="hasKey === false"
        class="mb-2 rounded border p-2 text-xs text-muted"
        :style="{ borderColor: 'var(--border-default)' }"
      >
        No API key is set for {{ settings.provider }}. Add one under Settings — Relay
        ships no key and sends your requests straight to the provider.
      </p>

      <div class="flex gap-2">
        <textarea
          v-model="question"
          rows="2"
          class="input resize-none text-xs"
          placeholder="Ask the assistant…"
          :disabled="thinking"
          aria-label="Ask the assistant"
          @keydown.enter.exact.prevent="canSend && ask()"
        />
        <button
          type="button"
          class="btn btn-primary shrink-0 self-end text-xs"
          :disabled="!canSend"
          aria-label="Send"
          @click="ask"
        >
          <Loader2 v-if="thinking" :size="13" class="animate-spin" />
          <Send v-else :size="13" />
        </button>
      </div>
      <p class="mt-1 text-xxs text-muted">Enter to send, Shift+Enter for a new line.</p>
    </div>
  </section>
</template>
