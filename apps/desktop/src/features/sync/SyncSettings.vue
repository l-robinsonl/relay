<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { CheckCircle2, Cloud, Loader2, X } from '@lucide/vue'
import type { ServerHealth } from '@/services/ipc'
import { api } from '@/services/ipc'
import { formatRelativeTime } from '@/composables/useFormat'
import { useDialogStore } from '@/stores/dialogs'
import { useSyncStore } from '@/stores/sync'

/** Connecting a workspace to a server, or disconnecting it. */
const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const sync = useSyncStore()
const dialogs = useDialogStore()

const serverUrl = ref('')
const email = ref('')
const password = ref('')
const join = ref(false)
const probing = ref(false)
const connecting = ref(false)
const health = ref<ServerHealth | null>(null)
const probeError = ref<string | null>(null)

const canConnect = computed(
  () => health.value !== null && email.value.trim() !== '' && password.value !== '',
)

watch(
  () => props.open,
  (isOpen) => {
    if (!isOpen) return
    serverUrl.value = sync.status.serverUrl ?? ''
    email.value = sync.status.userEmail ?? ''
    password.value = ''
    health.value = null
    probeError.value = null
  },
)

async function probe () {
  const url = serverUrl.value.trim()
  if (!url) return

  probing.value = true
  probeError.value = null
  health.value = null

  try {
    health.value = await api.syncProbe(url)
  } catch (error) {
    probeError.value =
      error && typeof error === 'object' && 'message' in error
        ? String((error as { message: unknown }).message)
        : String(error)
  } finally {
    probing.value = false
  }
}

async function connect () {
  connecting.value = true
  try {
    const ok = await sync.connect(
      serverUrl.value.trim(),
      email.value.trim(),
      password.value,
      join.value,
    )
    // The password is not kept a moment longer than needed.
    password.value = ''
    if (ok) emit('close')
  } finally {
    connecting.value = false
  }
}

async function disconnect () {
  const confirmed = await dialogs.confirm({
    title: 'Stop syncing this workspace?',
    description:
      'Every request, collection and environment stays on this machine. Only the link to the server is removed.',
    confirmLabel: 'Disconnect',
  })
  if (!confirmed) return

  await sync.disconnect()
  emit('close')
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-center justify-center p-4"
      role="dialog"
      aria-modal="true"
      aria-labelledby="sync-title"
    >
      <div class="absolute inset-0 bg-black/40 backdrop-blur-[2px]" @click="emit('close')" />

      <div
        class="animate-slide-up relative flex max-h-[80vh] w-full max-w-lg flex-col
               overflow-hidden rounded-lg border border-default
               bg-[var(--surface-overlay)]"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
      >
        <header class="flex shrink-0 items-center gap-2 border-b border-subtle px-4 py-3">
          <Cloud :size="16" class="text-muted" />
          <h2 id="sync-title" class="text-sm font-medium text-primary">Synchronisation</h2>
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
          <!-- Already connected -->
          <div v-if="sync.status.connected" class="space-y-3">
            <div class="rounded-md border border-subtle p-3 text-xs">
              <p class="text-secondary">
                Connected to
                <span class="font-mono text-primary">{{ sync.status.serverUrl }}</span>
              </p>
              <p v-if="sync.status.userEmail" class="mt-1 text-muted">
                Signed in as {{ sync.status.userEmail }}
              </p>
              <p v-if="sync.status.lastSyncAt" class="mt-1 text-muted">
                Last synced {{ formatRelativeTime(sync.status.lastSyncAt) }}
              </p>
              <p
                v-if="sync.status.lastError"
                class="mt-1"
                :style="{ color: 'var(--danger)' }"
              >
                {{ sync.status.lastError }}
              </p>
            </div>

            <div class="flex gap-2">
              <button
                type="button"
                class="btn btn-primary flex-1 text-xs"
                :disabled="sync.syncing"
                @click="sync.syncNow()"
              >
                <Loader2 v-if="sync.syncing" :size="13" class="animate-spin" />
                Sync now
              </button>
              <button type="button" class="btn text-xs" @click="disconnect">
                Disconnect
              </button>
            </div>

            <p class="text-xs text-muted">
              Disconnecting leaves every request, collection and environment on this
              machine. Only the link to the server is removed.
            </p>
          </div>

          <!-- Not connected -->
          <div v-else class="space-y-3">
            <p class="text-xs text-muted">
              Relay works perfectly well without a server. Connect one only if you want
              this workspace on more than one machine, or shared with teammates.
            </p>

            <div>
              <label for="server-url" class="mb-1 block text-xs font-medium text-secondary">
                Server address
              </label>
              <div class="flex gap-2">
                <input
                  id="server-url"
                  v-model="serverUrl"
                  class="input input-mono"
                  placeholder="https://relay.internal"
                  spellcheck="false"
                  autocomplete="off"
                  @keydown.enter.prevent="probe"
                />
                <button
                  type="button"
                  class="btn shrink-0 text-xs"
                  :disabled="probing || !serverUrl.trim()"
                  @click="probe"
                >
                  <Loader2 v-if="probing" :size="13" class="animate-spin" />
                  Check
                </button>
              </div>

              <p
                v-if="health"
                class="mt-1.5 flex items-center gap-1.5 text-xs"
                :style="{ color: 'var(--success)' }"
              >
                <CheckCircle2 :size="12" />
                Relay {{ health.version }} · protocol {{ health.protocol }} ·
                {{ health.backend }}
              </p>
              <p
                v-else-if="probeError"
                class="mt-1.5 text-xs"
                :style="{ color: 'var(--danger)' }"
              >
                {{ probeError }}
              </p>
            </div>

            <template v-if="health">
              <div>
                <label for="sync-email" class="mb-1 block text-xs font-medium text-secondary">
                  E-mail
                </label>
                <input
                  id="sync-email"
                  v-model="email"
                  type="email"
                  class="input"
                  autocomplete="username"
                  spellcheck="false"
                />
              </div>

              <div>
                <label
                  for="sync-password"
                  class="mb-1 block text-xs font-medium text-secondary"
                >
                  Password
                </label>
                <input
                  id="sync-password"
                  v-model="password"
                  type="password"
                  class="input"
                  autocomplete="current-password"
                  @keydown.enter.prevent="canConnect && connect()"
                />
              </div>

              <label class="flex cursor-pointer items-start gap-2 text-xs text-secondary">
                <input
                  v-model="join"
                  type="checkbox"
                  class="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
                />
                <span>
                  This workspace already exists on the server
                  <span class="block text-muted">
                    Tick this on a second machine, or when a teammate shared it with you.
                    The server's copy becomes the starting point.
                  </span>
                </span>
              </label>

              <div
                class="rounded-md border p-2.5 text-xs"
                :style="{
                  borderColor: 'var(--border-default)',
                  color: 'var(--text-muted)',
                }"
              >
                Secret environment values are never uploaded. Teammates will see the
                variable names and supply their own values.
              </div>

              <button
                type="button"
                class="btn btn-primary w-full text-xs"
                :disabled="!canConnect || connecting"
                @click="connect"
              >
                <Loader2 v-if="connecting" :size="13" class="animate-spin" />
                Connect and sync
              </button>
            </template>
          </div>
        </div>
      </div>
    </div>
  </Teleport>
</template>
