<script setup lang="ts">
import { computed } from 'vue'
import { Trash2 } from '@lucide/vue'
import type { KeyValue } from '@relay/shared-types'

/**
 * The editable key/value grid used for headers, query parameters and form
 * fields.
 *
 * It always renders one blank row at the end, so adding an entry is just
 * typing rather than hunting for an "Add" button. The blank row is not part
 * of the model until something is typed into it.
 */
const props = withDefaults(
  defineProps<{
    modelValue: KeyValue[]
    keyPlaceholder?: string
    valuePlaceholder?: string
    disabled?: boolean
  }>(),
  {
    keyPlaceholder: 'Name',
    valuePlaceholder: 'Value',
    disabled: false,
  },
)

const emit = defineEmits<{ 'update:modelValue': [KeyValue[]] }>()

/** The stored rows plus a trailing blank for new input. */
const rows = computed<KeyValue[]>(() => [
  ...props.modelValue,
  { key: '', value: '', enabled: true },
])

function commit (index: number, patch: Partial<KeyValue>) {
  const next = [...props.modelValue]
  const existing = next[index]

  if (existing) {
    next[index] = { ...existing, ...patch }
  } else {
    // Typing in the trailing blank row promotes it to a real entry.
    next.push({ key: '', value: '', enabled: true, ...patch })
  }

  emit('update:modelValue', next)
}

function remove (index: number) {
  emit(
    'update:modelValue',
    props.modelValue.filter((_, i) => i !== index),
  )
}
</script>

<template>
  <div class="flex flex-col">
    <div
      class="grid grid-cols-[2rem_minmax(0,1fr)_minmax(0,1.6fr)_2rem] items-center gap-x-2
             border-b border-subtle px-3 py-1.5 text-xxs font-medium uppercase
             tracking-wide text-muted"
    >
      <span class="sr-only">Enabled</span>
      <span>{{ keyPlaceholder }}</span>
      <span>{{ valuePlaceholder }}</span>
      <span class="sr-only">Remove</span>
    </div>

    <div
      v-for="(row, index) in rows"
      :key="index"
      class="group grid grid-cols-[2rem_minmax(0,1fr)_minmax(0,1.6fr)_2rem] items-center
             gap-x-2 border-b border-subtle px-3 py-1 transition-colors
             hover:bg-[var(--surface-hover)]"
    >
      <input
        type="checkbox"
        class="h-3.5 w-3.5 cursor-pointer accent-[var(--accent)]"
        :checked="row.enabled"
        :disabled="disabled || index >= modelValue.length"
        :aria-label="row.key ? `Enable ${row.key}` : 'Enable this row'"
        @change="commit(index, { enabled: ($event.target as HTMLInputElement).checked })"
      />

      <input
        class="input-mono w-full border-0 bg-transparent px-0 py-1 text-primary
               outline-none placeholder:text-muted focus:ring-0"
        :value="row.key"
        :placeholder="keyPlaceholder"
        :disabled="disabled"
        spellcheck="false"
        autocomplete="off"
        @input="commit(index, { key: ($event.target as HTMLInputElement).value })"
      />

      <input
        class="input-mono w-full border-0 bg-transparent px-0 py-1 text-secondary
               outline-none placeholder:text-muted focus:ring-0"
        :value="row.value"
        :placeholder="valuePlaceholder"
        :disabled="disabled"
        spellcheck="false"
        autocomplete="off"
        @input="commit(index, { value: ($event.target as HTMLInputElement).value })"
      />

      <button
        v-if="index < modelValue.length"
        type="button"
        class="btn-ghost rounded p-1 text-muted opacity-0 transition
               hover:text-[var(--danger)] group-hover:opacity-100 focus-visible:opacity-100"
        :disabled="disabled"
        :aria-label="`Remove ${row.key || 'row'}`"
        @click="remove(index)"
      >
        <Trash2 :size="13" />
      </button>
    </div>
  </div>
</template>
