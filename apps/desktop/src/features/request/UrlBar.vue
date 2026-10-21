<script setup lang="ts">
import { computed, ref } from 'vue'
import { Loader2, Send, Square } from '@lucide/vue'
import { methodColor } from '@/composables/useFormat'

/**
 * The method selector, URL field and send button.
 *
 * The URL field highlights `{{variables}}` by rendering a styled mirror
 * behind a transparent input. A contenteditable would give richer
 * highlighting but breaks text selection, undo and IME input — all of which
 * a URL field must get right.
 */
const props = defineProps<{
  method: string
  url: string
  sending: boolean
  unresolved: string[]
}>()

const emit = defineEmits<{
  'update:method': [string]
  'update:url': [string]
  send: []
  cancel: []
}>()

const METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS']

const customMethod = ref(false)
const mirror = ref<HTMLDivElement | null>(null)

const color = computed(() => methodColor(props.method))

const isKnownMethod = computed(() => METHODS.includes(props.method.toUpperCase()))

/** The URL split into literal text and variable references. */
const segments = computed(() => {
  const parts: { text: string; variable: boolean; known: boolean }[] = []
  const pattern = /\{\{([^{}]*)\}\}/g
  let index = 0
  let match: RegExpExecArray | null

  while ((match = pattern.exec(props.url)) !== null) {
    if (match.index > index) {
      parts.push({ text: props.url.slice(index, match.index), variable: false, known: true })
    }
    const name = (match[1] ?? '').trim()
    parts.push({
      text: match[0],
      variable: true,
      known: !props.unresolved.includes(name),
    })
    index = match.index + match[0].length
  }

  if (index < props.url.length) {
    parts.push({ text: props.url.slice(index), variable: false, known: true })
  }
  return parts
})

/** Keeps the highlight layer aligned when the input scrolls horizontally. */
function syncScroll (event: Event) {
  if (mirror.value) mirror.value.scrollLeft = (event.target as HTMLInputElement).scrollLeft
}
</script>

<template>
  <div class="flex items-stretch gap-2">
    <!-- Method -->
    <div class="shrink-0">
      <input
        v-if="customMethod || !isKnownMethod"
        class="input input-mono w-28 font-semibold uppercase"
        :style="{ color }"
        :value="method"
        aria-label="HTTP method"
        spellcheck="false"
        @input="emit('update:method', ($event.target as HTMLInputElement).value.toUpperCase())"
        @blur="customMethod = false"
      />
      <select
        v-else
        class="input w-28 cursor-pointer font-mono font-semibold"
        :style="{ color }"
        :value="method"
        aria-label="HTTP method"
        @change="
          ($event.target as HTMLSelectElement).value === '__custom__'
            ? (customMethod = true)
            : emit('update:method', ($event.target as HTMLSelectElement).value)
        "
      >
        <option v-for="item in METHODS" :key="item" :value="item">{{ item }}</option>
        <option value="__custom__">Custom…</option>
      </select>
    </div>

    <!-- URL with variable highlighting -->
    <div class="relative min-w-0 flex-1">
      <div
        ref="mirror"
        aria-hidden="true"
        class="input-mono pointer-events-none absolute inset-0 flex items-center
               overflow-hidden whitespace-pre px-2.5 py-1.5"
      >
        <span v-if="!url" class="text-muted">Enter a URL, or paste a cURL command</span>
        <template v-for="(segment, index) in segments" v-else :key="index">
          <span
            v-if="segment.variable"
            class="rounded-sm px-0.5"
            :style="{
              color: segment.known ? 'var(--accent-text)' : 'var(--danger)',
              backgroundColor: segment.known ? 'var(--accent-muted)' : 'var(--danger-muted)',
            }"
          >{{ segment.text }}</span>
          <span v-else>{{ segment.text }}</span>
        </template>
      </div>

      <input
        class="input input-mono relative w-full bg-transparent px-2.5 py-1.5
               text-transparent caret-[var(--text-primary)]"
        :value="url"
        type="text"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        autocorrect="off"
        aria-label="Request URL"
        @input="emit('update:url', ($event.target as HTMLInputElement).value)"
        @scroll="syncScroll"
        @keydown.enter.prevent="emit('send')"
      />
    </div>

    <!-- Send / cancel -->
    <button
      v-if="sending"
      type="button"
      class="btn btn-danger shrink-0 px-4"
      @click="emit('cancel')"
    >
      <Square :size="13" fill="currentColor" />
      Cancel
    </button>
    <button
      v-else
      type="button"
      class="btn btn-primary shrink-0 px-4"
      :disabled="!url.trim()"
      @click="emit('send')"
    >
      <Loader2 v-if="sending" :size="14" class="animate-spin" />
      <Send v-else :size="13" />
      Send
    </button>
  </div>
</template>
