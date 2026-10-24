<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { ArrowDown, Radio, Trash2 } from '@lucide/vue'
import type { ServerEvent } from '@relay/shared-types'
import { formatRelativeTime } from '@/composables/useFormat'

/** One line in the log. */
export interface LogLine {
  at: number
  event: ServerEvent
}

const props = defineProps<{
  lines: LogLine[]
  /** Whether the connection is currently open. */
  live: boolean
  /** Why it closed, when it closed badly. */
  error?: string | null
}>()

const emit = defineEmits<{ clear: [] }>()

const scroller = ref<HTMLElement | null>(null)
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

/** Events whose data parses as JSON are worth pretty-printing. */
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
        {{ count }} {{ count === 1 ? 'event' : 'events' }}
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
        Nothing yet. Press send to open the connection.
      </p>

      <ul v-else class="divide-y divide-[var(--border-subtle)]">
        <li v-for="(line, index) in lines" :key="index" class="px-3 py-1.5">
          <div class="flex items-baseline gap-2">
            <span
              class="shrink-0 font-mono text-xxs"
              :style="{ color: 'var(--accent-text)' }"
            >{{ line.event.event }}</span>
            <span v-if="line.event.id" class="shrink-0 font-mono text-xxs text-muted">
              #{{ line.event.id }}
            </span>
            <span class="ml-auto shrink-0 text-xxs text-muted">
              {{ formatRelativeTime(line.at) }}
            </span>
          </div>
          <pre
            class="selectable mt-0.5 whitespace-pre-wrap break-all font-mono text-xs
                   text-secondary"
          >{{ pretty(line.event.data) }}</pre>
        </li>
      </ul>
    </div>
  </div>
</template>
