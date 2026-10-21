<script setup lang="ts">
import { computed } from 'vue'
import { FileUp, Wand2 } from '@lucide/vue'
import type { KeyValue, RequestBody } from '@relay/shared-types'
import CodeEditor from '@/components/ui/CodeEditor.vue'
import KeyValueEditor from '@/components/ui/KeyValueEditor.vue'
import { tryFormatJson } from '@/composables/useFormat'

const props = defineProps<{ modelValue: RequestBody }>()
const emit = defineEmits<{ 'update:modelValue': [RequestBody] }>()

const MODES = [
  { id: 'none', label: 'None' },
  { id: 'json', label: 'JSON' },
  { id: 'text', label: 'Text' },
  { id: 'xml', label: 'XML' },
  { id: 'urlencoded', label: 'Form URL-encoded' },
  { id: 'multipart', label: 'Multipart' },
  { id: 'binary', label: 'Binary' },
  { id: 'graphql', label: 'GraphQL' },
] as const

type Mode = (typeof MODES)[number]['id']

const mode = computed(() => props.modelValue.mode as Mode)

const text = computed(() => {
  const body = props.modelValue
  if (body.mode === 'json' || body.mode === 'text' || body.mode === 'xml') return body.text
  if (body.mode === 'raw') return body.text
  return ''
})

const language = computed(() => {
  switch (mode.value) {
    case 'json':
      return 'json'
    case 'xml':
      return 'xml'
    case 'graphql':
      return 'graphql'
    default:
      return 'text'
  }
})

const canPrettyPrint = computed(
  () => mode.value === 'json' && tryFormatJson(text.value) !== null,
)

// Narrowed accessors. Vue's template compiler cannot narrow a discriminated
// union across the `v-else-if` chain, so the narrowing happens here where
// TypeScript can check it.
const urlEncodedFields = computed<KeyValue[]>(() =>
  props.modelValue.mode === 'urlencoded' ? props.modelValue.fields : [],
)

const graphQlQuery = computed(() =>
  props.modelValue.mode === 'graphql' ? props.modelValue.query : '',
)

const graphQlVariables = computed(() =>
  props.modelValue.mode === 'graphql' ? props.modelValue.variables : '',
)

const binaryPath = computed(() =>
  props.modelValue.mode === 'binary' ? props.modelValue.path : '',
)

/** Switching mode builds a fresh body, so no stale content can be sent. */
function setMode (next: Mode) {
  if (next === mode.value) return
  const carried = text.value

  switch (next) {
    case 'none':
      emit('update:modelValue', { mode: 'none' })
      break
    case 'json':
    case 'text':
    case 'xml':
      emit('update:modelValue', { mode: next, text: carried })
      break
    case 'urlencoded':
      emit('update:modelValue', { mode: 'urlencoded', fields: [] })
      break
    case 'multipart':
      emit('update:modelValue', { mode: 'multipart', fields: [] })
      break
    case 'binary':
      emit('update:modelValue', { mode: 'binary', path: '', contentType: null })
      break
    case 'graphql':
      emit('update:modelValue', { mode: 'graphql', query: carried, variables: '' })
      break
  }
}

function setText (value: string) {
  if (mode.value === 'json' || mode.value === 'text' || mode.value === 'xml') {
    emit('update:modelValue', { mode: mode.value, text: value })
  }
}

function setFields (fields: KeyValue[]) {
  emit('update:modelValue', { mode: 'urlencoded', fields })
}

function prettyPrint () {
  const formatted = tryFormatJson(text.value)
  if (formatted !== null) setText(formatted)
}

async function pickFile () {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const selected = await open({ multiple: false })
  if (typeof selected === 'string') {
    emit('update:modelValue', { mode: 'binary', path: selected, contentType: null })
  }
}
</script>

<template>
  <div class="flex h-full flex-col">
    <div class="flex shrink-0 items-center gap-2 border-b border-subtle px-3 py-1.5">
      <label class="sr-only" for="body-mode">Body type</label>
      <select
        id="body-mode"
        class="input w-auto py-1 text-xs"
        :value="mode"
        @change="setMode(($event.target as HTMLSelectElement).value as Mode)"
      >
        <option v-for="item in MODES" :key="item.id" :value="item.id">{{ item.label }}</option>
      </select>

      <button
        v-if="canPrettyPrint"
        type="button"
        class="btn btn-ghost py-1 text-xs"
        @click="prettyPrint"
      >
        <Wand2 :size="12" />
        Format
      </button>
    </div>

    <div class="min-h-0 flex-1">
      <div
        v-if="mode === 'none'"
        class="flex h-full items-center justify-center px-6 text-center text-xs text-muted"
      >
        This request has no body.
      </div>

      <CodeEditor
        v-else-if="mode === 'json' || mode === 'text' || mode === 'xml'"
        :model-value="text"
        :language="language"
        placeholder="Request body"
        @update:model-value="setText"
      />

      <KeyValueEditor
        v-else-if="mode === 'urlencoded'"
        :model-value="urlEncodedFields"
        key-placeholder="Field"
        @update:model-value="setFields"
      />

      <div
        v-else-if="mode === 'multipart'"
        class="flex h-full items-center justify-center px-6 text-center text-xs text-muted"
      >
        Multipart fields are editable from the Rust API and will get a dedicated editor in
        the next slice. The engine already streams file parts without buffering them.
      </div>

      <div v-else-if="mode === 'binary'" class="space-y-3 p-4">
        <button type="button" class="btn text-xs" @click="pickFile">
          <FileUp :size="13" />
          Choose a file
        </button>
        <p v-if="binaryPath" class="text-xs text-secondary">
          <span class="font-mono selectable">{{ binaryPath }}</span>
        </p>
        <p class="text-xs text-muted">
          The file is streamed from disk when the request is sent, never read into memory.
        </p>
      </div>

      <div v-else-if="mode === 'graphql'" class="flex h-full flex-col">
        <CodeEditor
          :model-value="graphQlQuery"
          language="graphql"
          placeholder="query { ... }"
          class="min-h-0 flex-1"
          @update:model-value="
            emit('update:modelValue', {
              mode: 'graphql',
              query: $event,
              variables: graphQlVariables,
            })
          "
        />
        <div class="shrink-0 border-t border-subtle">
          <p class="px-3 py-1 text-xxs font-medium uppercase tracking-wide text-muted">
            Variables
          </p>
          <CodeEditor
            :model-value="graphQlVariables"
            language="json"
            placeholder="{}"
            class="h-32"
            @update:model-value="
              emit('update:modelValue', {
                mode: 'graphql',
                query: graphQlQuery,
                variables: $event,
              })
            "
          />
        </div>
      </div>
    </div>
  </div>
</template>
