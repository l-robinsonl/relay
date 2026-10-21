<script setup lang="ts">
import { ref } from 'vue'
import type { RequestScripts } from '@relay/shared-types'
import CodeEditor from '@/components/ui/CodeEditor.vue'

const props = defineProps<{ modelValue: RequestScripts }>()
const emit = defineEmits<{ 'update:modelValue': [RequestScripts] }>()

type Phase = 'preRequest' | 'postResponse'

const PHASES: { id: Phase, label: string, caption: string }[] = [
  { id: 'preRequest', label: 'Pre-request', caption: 'Runs before the request is sent' },
  { id: 'postResponse', label: 'Tests', caption: 'Runs after the response arrives' },
]

// Opens on whichever already has a script, so a request with only a
// pre-request script does not look empty.
const phase = ref<Phase>(
  props.modelValue.preRequest.trim() !== '' && props.modelValue.postResponse.trim() === ''
    ? 'preRequest'
    : 'postResponse',
)

function update (which: Phase, source: string) {
  emit('update:modelValue', { ...props.modelValue, [which]: source })
}

function hasContent (which: Phase) {
  return props.modelValue[which].trim() !== ''
}
</script>

<template>
  <div class="flex h-full flex-col">
    <div
      class="flex shrink-0 items-center gap-1 border-b border-subtle px-2"
      role="tablist"
      aria-label="Script phase"
    >
      <button
        v-for="item in PHASES"
        :key="item.id"
        type="button"
        role="tab"
        class="relative px-2.5 py-1.5 text-xs transition-colors"
        :class="phase === item.id ? 'text-primary' : 'text-muted hover:text-secondary'"
        :aria-selected="phase === item.id"
        @click="phase = item.id"
      >
        {{ item.label }}
        <template v-if="hasContent(item.id)">
          <span
            class="ml-1 inline-block size-1.5 rounded-full align-middle"
            :style="{ backgroundColor: 'var(--accent)' }"
            aria-hidden="true"
          />
          <!-- The dot is the only sign a phase has a script, so it needs
               a spoken equivalent. Wrapped, because `sr-only` is
               absolutely positioned and would leave the flow otherwise. -->
          <span><span class="sr-only"> (has a script)</span></span>
        </template>
        <span
          v-if="phase === item.id"
          class="absolute inset-x-1 -bottom-px h-0.5 rounded-full"
          :style="{ backgroundColor: 'var(--accent)' }"
        />
      </button>
      <span class="ml-auto pr-1 text-xxs uppercase tracking-wide text-muted">
        {{ PHASES.find((p) => p.id === phase)?.caption }}
      </span>
    </div>

    <CodeEditor
      v-if="phase === 'preRequest'"
      class="min-h-0 flex-1"
      language="javascript"
      :model-value="modelValue.preRequest"
      placeholder="relay.setHeader('X-Request-Id', crypto.randomUUID())"
      @update:model-value="update('preRequest', $event)"
    />
    <CodeEditor
      v-else
      class="min-h-0 flex-1"
      language="javascript"
      :model-value="modelValue.postResponse"
      placeholder="expect(response.status).toBe(200)"
      @update:model-value="update('postResponse', $event)"
    />

    <p v-if="phase === 'preRequest'" class="shrink-0 border-t border-subtle px-3 py-2 text-xs text-muted">
      Runs before each send, in the same sandbox: no filesystem, network or process access.
      <code class="font-mono">request</code> holds the request with its variables already
      resolved. Change it with
      <code class="font-mono">relay.setHeader</code>,
      <code class="font-mono">setQueryParam</code>,
      <code class="font-mono">setUrl</code>,
      <code class="font-mono">setMethod</code> or
      <code class="font-mono">setBody</code>, or set a variable for the request to use with
      <code class="font-mono">relay.setVariable</code>. Secret values are not readable, and a
      variable set here lasts for this send only. If the script throws, the request is not sent.
    </p>
    <p v-else class="shrink-0 border-t border-subtle px-3 py-2 text-xs text-muted">
      Runs after each response, in a sandbox with no filesystem, network or process
      access. <code class="font-mono">response</code>,
      <code class="font-mono">request</code> and
      <code class="font-mono">relay.getVariable</code> are available; secret values
      are not. Results appear under Tests.
    </p>
  </div>
</template>
