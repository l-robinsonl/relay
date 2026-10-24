<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { ArrowDown, Radio, Send, Trash2 } from '@lucide/vue'
import type { StreamMessage } from '@relay/shared-types'
import { formatClockTime } from '@/composables/useFormat'

/** One line in the log. */
export interface LogLine {
  at: number
  message: StreamMessage
}

const props = defineProps<{
  lines: LogLine[]
  /** Whether the connection is currently open. */
  live: boolean
  /** Why it closed, when it closed badly. */
  error?: string | null
  /**
   * Whether this protocol can carry a message out.
   *
   * A websocket can; server-sent events cannot, and showing a box
   * that silently does nothing would be worse than showing none.
   */
  canSend?: boolean
}>()

const emit = defineEmits<{ clear: []; send: [string] }>()

const scroller = ref<HTMLElement | null>(null)
const draft = ref('')
/**
 * Whether new lines scroll into view.
 *
 * Turned off the moment somebody scrolls up, and back on when they
 * reach the bottom again. A log that yanks you back to the end while
 * you are reading something earlier is unusable on a busy stream,
 * which is exactly when you most want to read it.
 */
const following = ref(true)

function onScroll () {
  const el = scroller.value
  if (!el) return
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24
  following.value = atBottom
}

function toBottom () {
  const el = scroller.value
  if (!el) return
  el.scrollTop = el.scrollHeight
  following.value = true
}

watch(
  () => props.lines.length,
  async () => {
    if (!following.value) return
    await nextTick()
    toBottom()
  },
)

function send () {
  const text = draft.value
  // Deliberately not trimmed: an empty frame and a frame of spaces are
  // both things somebody may be testing for. Only a wholly empty box
  // is treated as nothing to send.
  if (!text) return
  emit('send', text)
  draft.value = ''
}

/** Messages whose data parses as JSON are worth pretty-printing. */
function pretty (data: string): string {
  const trimmed = data.trim()
  if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) return data
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2)
  } catch {
    return data
  }
}

const count = computed(() => props.lines.length)

function outbound (line: LogLine) {
  return line.message.direction === 'outbound'
}

/**
 * The frame type, when it is worth saying.
 *
 * On a websocket almost every line is a text frame, so printing
 * `text` against each one buries the two that are not. A named
 * server-sent event is always worth showing, which is why the
 * exception is by value and not by protocol.
 */
function label (message: StreamMessage): string {
  return message.name === 'text' ? '' : message.name
}
</script>

<template>
  <div class="flex h-full flex-col">
    <div class="flex shrink-0 items-center gap-2 border-b border-subtle px-3 py-1.5 text-xs">
      <span
        class="flex items-center gap-1.5"
        :style="{ color: live ? 'var(--success)' : 'var(--text-muted)' }"
      >
        <Radio :size="13" :class="live && 'animate-pulse'" />
        {{ live ? 'Listening' : 'Closed' }}
      </span>
      <span class="text-muted">
        {{ count }} {{ count === 1 ? 'message' : 'messages' }}
      </span>

      <div class="ml-auto flex items-center gap-1">
        <button
          v-if="!following"
          type="button"
          class="btn gap-1 py-0.5 text-xs"
          @click="toBottom"
        >
          <ArrowDown :size="12" />
          Follow
        </button>
        <button
          type="button"
          class="btn gap-1 py-0.5 text-xs"
          :disabled="!count"
          aria-label="Clear the log"
          @click="emit('clear')"
        >
          <Trash2 :size="12" />
          Clear
        </button>
      </div>
    </div>

    <div
      v-if="error"
      class="shrink-0 border-b px-3 py-2 text-xs"
      :style="{
        borderColor: 'var(--border-subtle)',
        backgroundColor: 'var(--danger-muted)',
        color: 'var(--danger)',
      }"
    >
      {{ error }}
    </div>

    <div ref="scroller" class="min-h-0 flex-1 overflow-y-auto" @scroll="onScroll">
      <p v-if="!count && live" class="px-3 py-3 text-xs text-muted">
        Connected. Nothing has arrived yet.
      </p>
      <p v-else-if="!count" class="px-3 py-3 text-xs text-muted">
        Nothing yet. Press connect to open the connection.
      </p>

      <ul v-else class="divide-y divide-[var(--border-subtle)]">
        <!-- A sent line is tinted and marked, because the one thing
             a conversation log has to answer at a glance is who
             spoke. An icon alone was too quiet to do it. -->
        <li
          v-for="(line, index) in lines"
          :key="index"
          class="border-l-2 py-1.5 pl-2.5 pr-3"
          :style="{
            borderLeftColor: outbound(line) ? 'var(--accent)' : 'transparent',
            backgroundColor: outbound(line) ? 'var(--accent-muted)' : 'transparent',
          }"
        >
          <div class="flex items-baseline gap-2">
            <span
              class="shrink-0 font-mono text-xs font-semibold"
              :style="{ color: outbound(line) ? 'var(--accent-text)' : 'var(--success)' }"
              aria-hidden="true"
            >{{ outbound(line) ? '→' : '←' }}</span>
            <span><span class="sr-only">{{ outbound(line) ? 'Sent' : 'Received' }}</span></span>
            <span
              v-if="label(line.message)"
              class="shrink-0 font-mono text-xxs"
              :style="{ color: 'var(--text-secondary)' }"
            >{{ label(line.message) }}</span>
            <span v-if="line.message.id" class="shrink-0 font-mono text-xxs text-muted">
              #{{ line.message.id }}
            </span>
            <span v-if="line.message.binaryBytes" class="shrink-0 text-xxs text-muted">
              {{ line.message.binaryBytes }} bytes
            </span>
            <span class="ml-auto shrink-0 text-xxs text-muted">
              {{ formatClockTime(line.at) }}
            </span>
          </div>
          <pre
            v-if="line.message.data"
            class="selectable mt-0.5 whitespace-pre-wrap break-all font-mono text-xs
                   text-secondary"
          >{{ pretty(line.message.data) }}</pre>
        </li>
      </ul>
    </div>

    <!-- The composer. A websocket is a conversation; without this the
         panel can only listen to one. -->
    <form
      v-if="canSend"
      class="flex shrink-0 items-end gap-2 border-t border-subtle p-2"
      @submit.prevent="send"
    >
      <textarea
        v-model="draft"
        rows="2"
        class="input input-mono min-h-0 flex-1 resize-y py-1.5 text-xs"
        :placeholder="live ? 'Message to send' : 'Connect first'"
        :disabled="!live"
        spellcheck="false"
        aria-label="Message to send"
        @keydown.enter.exact.prevent="send"
      />
      <button
        type="submit"
        class="btn btn-primary shrink-0 px-3 py-1.5 text-xs"
        :disabled="!live || !draft"
      >
        <Send :size="12" />
        Send
      </button>
    </form>
  </div>
</template>
