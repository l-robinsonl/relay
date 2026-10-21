<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { AlertTriangle } from '@lucide/vue'
import { useDialogStore } from '@/stores/dialogs'

/**
 * Renders whichever dialog is open.
 *
 * Mounted once at the root, so any part of the app can ask a question
 * without each one carrying its own modal.
 */
const dialogs = useDialogStore()

const draft = ref('')
const input = ref<HTMLInputElement | null>(null)

const isPrompt = computed(() => dialogs.pending?.kind === 'prompt')

const options = computed(() => dialogs.pending?.options ?? null)

const confirmLabel = computed(() => {
  const opts = options.value
  if (!opts) return 'OK'
  return opts.confirmLabel ?? (isPrompt.value ? 'Create' : 'Confirm')
})

const destructive = computed(
  () => dialogs.pending?.kind === 'confirm' && dialogs.pending.options.destructive === true,
)

// A prompt with an empty name would create an unnamed folder, which the
// backend refuses anyway; better not to offer the button.
const canSubmit = computed(() => !isPrompt.value || draft.value.trim().length > 0)

watch(
  () => dialogs.pending,
  async (current) => {
    if (!current) return
    draft.value = current.kind === 'prompt' ? (current.options.value ?? '') : ''
    await nextTick()
    input.value?.focus()
    input.value?.select()
  },
)

function submit () {
  if (!canSubmit.value) return
  dialogs.submit(isPrompt.value ? draft.value.trim() : '')
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="dialogs.pending && options"
      class="fixed inset-0 z-[70] flex items-start justify-center p-4 pt-[20vh]"
      role="dialog"
      aria-modal="true"
      aria-labelledby="dialog-title"
      @keydown.esc="dialogs.dismiss()"
    >
      <div class="absolute inset-0 bg-black/40 backdrop-blur-[2px]" @click="dialogs.dismiss()" />

      <div
        class="animate-slide-up relative w-full max-w-sm overflow-hidden rounded-lg border
               border-default bg-[var(--surface-overlay)]"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
      >
        <div class="space-y-3 p-4">
          <div class="flex items-start gap-2.5">
            <AlertTriangle
              v-if="destructive"
              :size="16"
              class="mt-0.5 shrink-0"
              :style="{ color: 'var(--danger)' }"
            />
            <div class="min-w-0 space-y-1">
              <h2 id="dialog-title" class="text-sm font-medium text-primary">
                {{ options.title }}
              </h2>
              <p
                v-if="'description' in options && options.description"
                class="text-xs text-muted"
              >
                {{ options.description }}
              </p>
            </div>
          </div>

          <div v-if="isPrompt && 'label' in options">
            <label
              v-if="options.label"
              for="dialog-input"
              class="mb-1 block text-xs font-medium text-secondary"
            >
              {{ options.label }}
            </label>
            <input
              id="dialog-input"
              ref="input"
              v-model="draft"
              class="input"
              :placeholder="'placeholder' in options ? options.placeholder : undefined"
              spellcheck="false"
              autocomplete="off"
              @keydown.enter.prevent="submit"
            />
          </div>
        </div>

        <div class="flex justify-end gap-2 border-t border-subtle px-4 py-3">
          <button type="button" class="btn text-xs" @click="dialogs.dismiss()">
            Cancel
          </button>
          <button
            type="button"
            class="btn text-xs"
            :class="destructive ? 'btn-danger' : 'btn-primary'"
            :disabled="!canSubmit"
            @click="submit"
          >
            {{ confirmLabel }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
