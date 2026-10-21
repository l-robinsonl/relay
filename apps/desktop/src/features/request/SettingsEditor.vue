<script setup lang="ts">
import { AlertTriangle } from '@lucide/vue'
import type { RequestSettings } from '@relay/shared-types'

const props = defineProps<{ modelValue: RequestSettings }>()
const emit = defineEmits<{ 'update:modelValue': [RequestSettings] }>()

function patch (changes: Partial<RequestSettings>) {
  emit('update:modelValue', { ...props.modelValue, ...changes })
}
</script>

<template>
  <div class="max-w-2xl space-y-5 p-4">
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label for="timeout" class="mb-1 block text-xs font-medium text-secondary">
          Timeout (ms)
        </label>
        <input
          id="timeout"
          type="number"
          min="0"
          step="1000"
          class="input input-mono"
          placeholder="120000 (default)"
          :value="modelValue.timeoutMs ?? ''"
          @input="
            patch({
              timeoutMs: ($event.target as HTMLInputElement).value
                ? Number(($event.target as HTMLInputElement).value)
                : null,
            })
          "
        />
      </div>

      <div>
        <label for="redirects" class="mb-1 block text-xs font-medium text-secondary">
          Redirects
        </label>
        <select
          id="redirects"
          class="input py-1.5 text-xs"
          :value="modelValue.redirectPolicy"
          @change="
            patch({
              redirectPolicy: ($event.target as HTMLSelectElement).value as 'follow' | 'never',
            })
          "
        >
          <option value="follow">Follow</option>
          <option value="never">Do not follow</option>
        </select>
      </div>

      <div v-if="modelValue.redirectPolicy === 'follow'">
        <label for="max-redirects" class="mb-1 block text-xs font-medium text-secondary">
          Maximum redirects
        </label>
        <input
          id="max-redirects"
          type="number"
          min="0"
          max="100"
          class="input input-mono"
          :value="modelValue.maxRedirects"
          @input="patch({ maxRedirects: Number(($event.target as HTMLInputElement).value) })"
        />
      </div>

      <div>
        <label for="max-bytes" class="mb-1 block text-xs font-medium text-secondary">
          Maximum response size (bytes)
        </label>
        <input
          id="max-bytes"
          type="number"
          min="0"
          class="input input-mono"
          placeholder="536870912 (default)"
          :value="modelValue.maxResponseBytes ?? ''"
          @input="
            patch({
              maxResponseBytes: ($event.target as HTMLInputElement).value
                ? Number(($event.target as HTMLInputElement).value)
                : null,
            })
          "
        />
      </div>
    </div>

    <fieldset class="space-y-2.5">
      <legend class="mb-1 text-xs font-medium text-secondary">Behaviour</legend>

      <label class="flex cursor-pointer items-center gap-2 text-xs text-secondary">
        <input
          type="checkbox"
          class="h-3.5 w-3.5 accent-[var(--accent)]"
          :checked="modelValue.decompress"
          @change="patch({ decompress: ($event.target as HTMLInputElement).checked })"
        />
        Decompress gzip, brotli, deflate and zstd responses
      </label>

      <label class="flex cursor-pointer items-center gap-2 text-xs text-secondary">
        <input
          type="checkbox"
          class="h-3.5 w-3.5 accent-[var(--accent)]"
          :checked="modelValue.sendCookies"
          @change="patch({ sendCookies: ($event.target as HTMLInputElement).checked })"
        />
        Send stored cookies
      </label>

      <label class="flex cursor-pointer items-center gap-2 text-xs text-secondary">
        <input
          type="checkbox"
          class="h-3.5 w-3.5 accent-[var(--accent)]"
          :checked="modelValue.storeCookies"
          @change="patch({ storeCookies: ($event.target as HTMLInputElement).checked })"
        />
        Store cookies from responses
      </label>
    </fieldset>

    <!-- TLS verification. Prominent, because turning it off is a real
         downgrade and the user should be able to see at a glance that it is. -->
    <div
      class="rounded-md border p-3"
      :style="
        modelValue.verifyTls
          ? { borderColor: 'var(--border-default)' }
          : { borderColor: 'var(--danger)', backgroundColor: 'var(--danger-muted)' }
      "
    >
      <label class="flex cursor-pointer items-start gap-2.5">
        <input
          type="checkbox"
          class="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
          :checked="modelValue.verifyTls"
          @change="patch({ verifyTls: ($event.target as HTMLInputElement).checked })"
        />
        <span class="space-y-1">
          <span class="block text-xs font-medium text-primary">
            Verify TLS certificates
          </span>
          <span v-if="modelValue.verifyTls" class="block text-xs text-muted">
            Certificates are checked against your operating system's trust store.
          </span>
          <span
            v-else
            class="flex items-start gap-1.5 text-xs"
            :style="{ color: 'var(--danger)' }"
          >
            <AlertTriangle :size="13" class="mt-px shrink-0" />
            <span>
              Verification is off for this request. Anyone on the network path can read and
              alter it. Only do this for an internal service whose certificate you already
              trust — never for a request carrying real credentials.
            </span>
          </span>
        </span>
      </label>
    </div>
  </div>
</template>
