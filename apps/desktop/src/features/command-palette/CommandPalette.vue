<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { ArrowRight, Search } from '@lucide/vue'
import MethodBadge from '@/components/ui/MethodBadge.vue'

/** One entry in the palette. */
export interface Command {
  id: string
  label: string
  hint?: string
  group: string
  /** Shown as a method chip when the entry is a request. */
  method?: string
  shortcut?: string
  run: () => void | Promise<void>
}

const props = defineProps<{
  open: boolean
  commands: Command[]
}>()

const emit = defineEmits<{ close: [] }>()

const query = ref('')
const selected = ref(0)
const input = ref<HTMLInputElement | null>(null)

/**
 * Matches on a subsequence rather than a substring, so "nr" finds
 * "New request" — the way every command palette a developer already uses
 * behaves.
 */
function score (text: string, term: string): number {
  if (!term) return 1
  const haystack = text.toLowerCase()
  const needle = term.toLowerCase()

  const exact = haystack.indexOf(needle)
  if (exact === 0) return 1000
  if (exact > 0) return 500 - exact

  let index = 0
  let hits = 0
  for (const char of needle) {
    const found = haystack.indexOf(char, index)
    if (found === -1) return 0
    hits += found === index ? 2 : 1
    index = found + 1
  }
  return hits
}

const results = computed(() => {
  const term = query.value.trim()
  return props.commands
    .map((command) => ({
      command,
      score: Math.max(score(command.label, term), score(command.hint ?? '', term) * 0.6),
    }))
    .filter((entry) => entry.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, 50)
    .map((entry) => entry.command)
})

/** Results grouped for display, preserving relevance order. */
const grouped = computed(() => {
  const groups = new Map<string, Command[]>()
  for (const command of results.value) {
    const bucket = groups.get(command.group)
    if (bucket) bucket.push(command)
    else groups.set(command.group, [command])
  }
  return groups
})

/** The flat order, so arrow keys move through groups seamlessly. */
const flat = computed(() => [...grouped.value.values()].flat())

watch(
  () => props.open,
  async (isOpen) => {
    if (!isOpen) return
    query.value = ''
    selected.value = 0
    await nextTick()
    input.value?.focus()
  },
)

watch(query, () => (selected.value = 0))

async function run (command: Command | undefined) {
  if (!command) return
  emit('close')
  await command.run()
}

function onKeydown (event: KeyboardEvent) {
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    selected.value = (selected.value + 1) % Math.max(flat.value.length, 1)
  } else if (event.key === 'ArrowUp') {
    event.preventDefault()
    selected.value =
      (selected.value - 1 + flat.value.length) % Math.max(flat.value.length, 1)
  } else if (event.key === 'Enter') {
    event.preventDefault()
    void run(flat.value[selected.value])
  } else if (event.key === 'Escape') {
    event.preventDefault()
    emit('close')
  }
}

function indexOf (command: Command) {
  return flat.value.indexOf(command)
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-start justify-center p-4 pt-[12vh]"
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
    >
      <div
        class="absolute inset-0 bg-black/40 backdrop-blur-[2px]"
        @click="emit('close')"
      />

      <div
        class="animate-slide-up relative flex max-h-[60vh] w-full max-w-xl flex-col
               overflow-hidden rounded-lg border border-default
               bg-[var(--surface-overlay)]"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
      >
        <div class="flex shrink-0 items-center gap-2 border-b border-subtle px-3">
          <Search :size="15" class="shrink-0 text-muted" />
          <input
            ref="input"
            v-model="query"
            class="w-full border-0 bg-transparent py-3 text-sm text-primary outline-none
                   placeholder:text-muted"
            placeholder="Search requests and commands…"
            aria-label="Search requests and commands"
            spellcheck="false"
            autocomplete="off"
            @keydown="onKeydown"
          />
          <kbd class="kbd shrink-0">Esc</kbd>
        </div>

        <div class="min-h-0 flex-1 overflow-y-auto p-1.5">
          <p v-if="!flat.length" class="px-3 py-6 text-center text-xs text-muted">
            Nothing matches “{{ query }}”.
          </p>

          <template v-for="[group, items] in grouped" v-else :key="group">
            <p
              class="px-2 pb-1 pt-2 text-xxs font-medium uppercase tracking-wide text-muted"
            >
              {{ group }}
            </p>
            <button
              v-for="command in items"
              :key="command.id"
              type="button"
              class="row"
              :class="indexOf(command) === selected && 'is-selected'"
              @click="run(command)"
              @mousemove="selected = indexOf(command)"
            >
              <MethodBadge
                v-if="command.method"
                :method="command.method"
                class="w-10 shrink-0 text-right"
              />
              <ArrowRight v-else :size="13" class="shrink-0 text-muted" />
              <span class="truncate text-sm">{{ command.label }}</span>
              <span v-if="command.hint" class="truncate text-xs text-muted">
                {{ command.hint }}
              </span>
              <kbd v-if="command.shortcut" class="kbd ml-auto shrink-0">
                {{ command.shortcut }}
              </kbd>
            </button>
          </template>
        </div>
      </div>
    </div>
  </Teleport>
</template>
