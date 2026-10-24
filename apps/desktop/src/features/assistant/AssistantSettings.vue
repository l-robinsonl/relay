<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Check, Eye, EyeOff, Loader2, Sparkles, X } from '@lucide/vue'
import type { AssistantSettings } from '@/services/ipc'
import { api } from '@/services/ipc'
import { PROVIDERS, needsKey } from './providers'
import McpServers from './McpServers.vue'

const props = defineProps<{ open: boolean, modelValue: AssistantSettings }>()
const emit = defineEmits<{
  close: []
  'update:modelValue': [AssistantSettings]
}>()


const key = ref('')
const showKey = ref(false)
const hasKey = ref(false)
const saving = ref(false)
const saved = ref(false)

const current = computed(() => PROVIDERS.find((p) => p.id === props.modelValue.provider) ?? PROVIDERS[0]!)
const isLocal = computed(() => props.modelValue.provider === 'compatible')

function patch (changes: Partial<AssistantSettings>) {
  emit('update:modelValue', { ...props.modelValue, ...changes })
}

watch(
  [() => props.open, () => props.modelValue.provider],
  async ([isOpen, provider]) => {
    if (!isOpen) return
    key.value = ''
    saved.value = false
    hasKey.value = await api.assistantHasKey(provider).catch(() => false)
  },
  { immediate: true },
)

async function save () {
  saving.value = true
  try {
    const typed = key.value.trim()
    if (typed !== '') {
      await api.assistantSetKey(props.modelValue.provider, typed)
      // Not kept in the component a moment longer than needed; it is
      // in the OS credential store now.
      key.value = ''
      hasKey.value = true
    }
    saved.value = true
  } finally {
    saving.value = false
  }
}

async function forget () {
  await api.assistantClearKey(props.modelValue.provider)
  hasKey.value = false
  key.value = ''
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-center justify-center p-4"
      role="dialog"
      aria-modal="true"
      aria-labelledby="assistant-settings-title"
    >
      <div class="absolute inset-0 bg-black/40 backdrop-blur-[2px]" @click="emit('close')" />

      <div
        class="animate-slide-up relative flex max-h-[80vh] w-full max-w-md flex-col
               overflow-hidden rounded-lg border border-default bg-[var(--surface-overlay)]"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
      >
        <header class="flex shrink-0 items-center gap-2 border-b border-subtle px-4 py-3">
          <Sparkles :size="14" class="text-muted" />
          <h2 id="assistant-settings-title" class="text-sm font-medium text-primary">
            Assistant
          </h2>
          <button
            type="button"
            class="ml-auto rounded p-1 text-muted hover:text-primary"
            aria-label="Close"
            @click="emit('close')"
          >
            <X :size="14" />
          </button>
        </header>

        <div class="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
          <p class="text-xs text-muted">
            Relay ships no key and runs no server of its own for this. Your requests go
            from this machine straight to whichever provider you choose, and the key is
            kept in your operating system's credential store.
          </p>

          <div>
            <label for="assistant-provider" class="mb-1 block text-xs font-medium text-secondary">
              Provider
            </label>
            <select
              id="assistant-provider"
              class="input py-1.5 text-xs"
              :value="modelValue.provider"
              @change="patch({ provider: ($event.target as HTMLSelectElement).value as AssistantSettings['provider'] })"
            >
              <option v-for="p in PROVIDERS" :key="p.id" :value="p.id">{{ p.label }}</option>
            </select>
            <p class="mt-1.5 max-w-prose text-xs text-muted">{{ current.help }}</p>
          </div>

          <div v-if="isLocal">
            <label for="assistant-url" class="mb-1 block text-xs font-medium text-secondary">
              Endpoint
            </label>
            <input
              id="assistant-url"
              class="input input-mono"
              :value="modelValue.baseUrl"
              placeholder="http://localhost:11434/v1"
              spellcheck="false"
              @input="patch({ baseUrl: ($event.target as HTMLInputElement).value })"
            />
            <p class="mt-1.5 max-w-prose text-xs text-muted">
              Point this at a model running on your own machine and no prompt leaves it.
            </p>
          </div>

          <div>
            <label for="assistant-model" class="mb-1 block text-xs font-medium text-secondary">
              Model <span class="font-normal text-muted">(optional)</span>
            </label>
            <input
              id="assistant-model"
              class="input input-mono"
              :value="modelValue.model"
              :placeholder="current.placeholder"
              spellcheck="false"
              @input="patch({ model: ($event.target as HTMLInputElement).value })"
            />
            <p class="mt-1.5 text-xs text-muted">Leave blank for the default.</p>
          </div>

          <!-- Relay's own server holds the key, so there is nothing
               to paste and a blank field would only confuse. -->
          <div v-if="!needsKey(modelValue.provider)" class="rounded-md border border-subtle p-3">
            <p class="text-xs text-secondary">
              The assistant runs on your subscription. Nothing to set up here.
            </p>
            <p class="mt-1 text-xs text-muted">
              Requests go to the server this workspace syncs with, which runs the model
              on its own key. If you run out of included requests, add your own key under
              any other provider — that works without limit.
            </p>
          </div>

          <div v-else>
            <label for="assistant-key" class="mb-1 block text-xs font-medium text-secondary">
              API key
              <span v-if="isLocal" class="font-normal text-muted">(usually not needed)</span>
            </label>
            <div class="flex gap-2">
              <input
                id="assistant-key"
                v-model="key"
                :type="showKey ? 'text' : 'password'"
                class="input input-mono"
                :placeholder="hasKey ? 'A key is already stored' : 'Paste your key'"
                autocomplete="off"
                spellcheck="false"
              />
              <button
                type="button"
                class="btn shrink-0 px-2"
                :aria-label="showKey ? 'Hide the key' : 'Show the key'"
                @click="showKey = !showKey"
              >
                <EyeOff v-if="showKey" :size="13" />
                <Eye v-else :size="13" />
              </button>
            </div>

            <p
              v-if="hasKey"
              class="mt-1.5 flex items-center gap-1.5 text-xs"
              :style="{ color: 'var(--success)' }"
            >
              <Check :size="12" />
              A key is stored for {{ current.label }}.
              <button
                type="button"
                class="underline-offset-2 hover:underline"
                :style="{ color: 'var(--text-muted)' }"
                @click="forget"
              >
                Forget it
              </button>
            </p>
          </div>

          <div class="border-t border-subtle pt-4">
            <McpServers />
          </div>
        </div>

        <footer class="flex shrink-0 items-center gap-2 border-t border-subtle px-4 py-3">
          <p v-if="saved" class="text-xs" :style="{ color: 'var(--success)' }">Saved.</p>
          <button
            type="button"
            class="btn btn-primary ml-auto text-xs"
            :disabled="saving"
            @click="save"
          >
            <Loader2 v-if="saving" :size="13" class="animate-spin" />
            Save
          </button>
        </footer>
      </div>
    </div>
  </Teleport>
</template>
