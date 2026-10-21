<script setup lang="ts">
import { computed } from 'vue'
import { FileUp, Trash2, Type, X } from '@lucide/vue'
import type { FormField } from '@relay/shared-types'

/**
 * The multipart form editor.
 *
 * Each row is either text or a file. The engine streams file parts from disk
 * rather than reading them into memory, so a large upload costs nothing here.
 */
const props = defineProps<{ modelValue: FormField[] }>()
const emit = defineEmits<{ 'update:modelValue': [FormField[]] }>()

/** The stored rows plus a trailing blank, so adding is just typing. */
const rows = computed<FormField[]>(() => [
  ...props.modelValue,
  { key: '', enabled: true, kind: 'text', value: '' } as FormField,
])

function commit (index: number, next: FormField) {
  const fields = [...props.modelValue]
  if (index < fields.length) fields[index] = next
  else fields.push(next)
  emit('update:modelValue', fields)
}

function patchKey (index: number, row: FormField, key: string) {
  commit(index, { ...row, key })
}

function patchEnabled (index: number, row: FormField, enabled: boolean) {
  commit(index, { ...row, enabled })
}

function setText (index: number, row: FormField, value: string) {
  commit(index, { key: row.key, enabled: row.enabled, kind: 'text', value })
}

/** Switches a row between a text value and a file, discarding the other. */
function toggleKind (index: number, row: FormField) {
  if (row.kind === 'text') {
    commit(index, { key: row.key, enabled: row.enabled, kind: 'file', path: '' })
  } else {
    commit(index, { key: row.key, enabled: row.enabled, kind: 'text', value: '' })
  }
}

async function pickFile (index: number, row: FormField) {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const selected = await open({ multiple: false })
  if (typeof selected !== 'string') return
  commit(index, { key: row.key, enabled: row.enabled, kind: 'file', path: selected })
}

function remove (index: number) {
  emit(
    'update:modelValue',
    props.modelValue.filter((_, i) => i !== index),
  )
}

function fileName (path: string) {
  return path.split(/[/\\]/).pop() ?? path
}
</script>

<template>
  <div class="flex flex-col">
    <div
      class="grid grid-cols-[2rem_minmax(0,1fr)_2rem_minmax(0,1.6fr)_2rem] items-center
             gap-x-2 border-b border-subtle px-3 py-1.5 text-xxs font-medium uppercase
             tracking-wide text-muted"
    >
      <span class="sr-only">Enabled</span>
      <span>Field</span>
      <span class="sr-only">Type</span>
      <span>Value</span>
      <span class="sr-only">Remove</span>
    </div>

    <div
      v-for="(row, index) in rows"
      :key="index"
      class="group grid grid-cols-[2rem_minmax(0,1fr)_2rem_minmax(0,1.6fr)_2rem] items-center
             gap-x-2 border-b border-subtle px-3 py-1 transition-colors
             hover:bg-[var(--surface-hover)]"
    >
      <input
        type="checkbox"
        class="h-3.5 w-3.5 cursor-pointer accent-[var(--accent)]"
        :checked="row.enabled"
        :disabled="index >= modelValue.length"
        :aria-label="row.key ? `Enable ${row.key}` : 'Enable this row'"
        @change="patchEnabled(index, row, ($event.target as HTMLInputElement).checked)"
      />

      <input
        class="input-mono w-full border-0 bg-transparent px-0 py-1 text-primary outline-none
               placeholder:text-muted"
        :value="row.key"
        placeholder="Field"
        spellcheck="false"
        autocomplete="off"
        @input="patchKey(index, row, ($event.target as HTMLInputElement).value)"
      />

      <button
        type="button"
        class="rounded p-1 text-muted transition-colors hover:text-primary"
        :title="row.kind === 'file' ? 'Change to a text value' : 'Change to a file'"
        :aria-label="row.kind === 'file' ? 'Change to a text value' : 'Change to a file'"
        @click="toggleKind(index, row)"
      >
        <component :is="row.kind === 'file' ? FileUp : Type" :size="13" />
      </button>

      <!-- Text value -->
      <input
        v-if="row.kind === 'text'"
        class="input-mono w-full border-0 bg-transparent px-0 py-1 text-secondary outline-none
               placeholder:text-muted"
        :value="row.value"
        placeholder="Value"
        spellcheck="false"
        autocomplete="off"
        @input="setText(index, row, ($event.target as HTMLInputElement).value)"
      />

      <!-- File value -->
      <div v-else class="flex min-w-0 items-center gap-1">
        <button
          type="button"
          class="btn btn-ghost min-w-0 px-1.5 py-0.5 text-xs"
          @click="pickFile(index, row)"
        >
          <FileUp :size="12" class="shrink-0" />
          <span class="truncate">
            {{ row.path ? fileName(row.path) : 'Choose a file' }}
          </span>
        </button>
        <button
          v-if="row.path"
          type="button"
          class="shrink-0 rounded p-0.5 text-muted hover:text-[var(--danger)]"
          aria-label="Clear the chosen file"
          @click="commit(index, { key: row.key, enabled: row.enabled, kind: 'file', path: '' })"
        >
          <X :size="11" />
        </button>
      </div>

      <button
        v-if="index < modelValue.length"
        type="button"
        class="btn-ghost rounded p-1 text-muted opacity-0 transition
               hover:text-[var(--danger)] group-hover:opacity-100 focus-visible:opacity-100"
        :aria-label="`Remove ${row.key || 'row'}`"
        @click="remove(index)"
      >
        <Trash2 :size="13" />
      </button>
    </div>

    <p class="px-3 py-2 text-xs text-muted">
      Files are streamed from disk when the request is sent, never read into memory.
    </p>
  </div>
</template>
