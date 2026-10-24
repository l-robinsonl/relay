<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { CheckCircle2, Cloud, Loader2, Server, X } from '@lucide/vue'
import type { ServerHealth } from '@/services/ipc'
import { api } from '@/services/ipc'
import { formatRelativeTime } from '@/composables/useFormat'
import { useDialogStore } from '@/stores/dialogs'
import { useSyncStore } from '@/stores/sync'
import BillingPanel from '@/features/sync/BillingPanel.vue'
import MembersPanel from '@/features/sync/MembersPanel.vue'
import { useWorkspaceStore } from '@/stores/workspace'
import { HOSTED_SERVER } from '@/services/hosted'

/** Connecting a workspace to a server, or disconnecting it. */
const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const sync = useSyncStore()
const dialogs = useDialogStore()
const workspace = useWorkspaceStore()

const serverUrl = ref('')
/** Which server, before an address has been chosen. Null means unasked. */
const using = ref<'hosted' | 'own' | null>(null)

/** Picks Relay's own server, so there is nothing to type or look up. */
function useHosted () {
  using.value = 'hosted'
  serverUrl.value = HOSTED_SERVER
}

function useOwn () {
  using.value = 'own'
  serverUrl.value = ''
}
const email = ref('')
const password = ref('')
const join = ref(false)
const probing = ref(false)
const connecting = ref(false)
const health = ref<ServerHealth | null>(null)
const probeError = ref<string | null>(null)

/** Signing in, making an account, or arriving with an invitation. */
const mode = ref<'signIn' | 'register' | 'invited'>('signIn')

/** The link or token pasted out of an invitation e-mail. */
const invitation = ref('')
const offer = ref<import('@relay/shared-types').InvitationOffer | null>(null)
const offerError = ref<string | null>(null)
const checkingOffer = ref(false)
const displayName = ref('')
const busy = ref<'' | 'register' | 'verify' | 'reset' | 'join'>('')
const notice = ref<string | null>(null)
const authError = ref<string | null>(null)
/** Set when the server says the address is registered but unconfirmed. */
const unverified = ref(false)

function messageOf (error: unknown): string {
  if (error && typeof error === 'object' && 'message' in error) {
    const detail = 'detail' in error ? (error as { detail?: string | null }).detail : null
    const message = String((error as { message: unknown }).message)
    return detail ? `${message} — ${detail}` : message
  }
  return String(error)
}

/** Clears whatever the last attempt said, before the next one. */
function resetFeedback () {
  notice.value = null
  authError.value = null
}

const canConnect = computed(
  () => health.value !== null && email.value.trim() !== '' && password.value !== '',
)

/** Registration asks for a longer password, as the reset page does. */
const canRegister = computed(
  () => health.value !== null && email.value.trim() !== '' && password.value.length >= 12,
)

// The address comes from the invitation, not from the person accepting,
// so there is no e-mail field to fill in here.
const canJoin = computed(() => offer.value !== null && password.value !== '')

watch(
  () => props.open,
  (isOpen) => {
    if (!isOpen) return
    serverUrl.value = sync.status.serverUrl ?? ''
    email.value = sync.status.userEmail ?? ''
    password.value = ''
    displayName.value = ''
    health.value = null
    probeError.value = null
    mode.value = 'signIn'
    unverified.value = false
    invitation.value = ''
    offer.value = null
    offerError.value = null
    resetFeedback()
  },
)

// Typing an address is enough. The sign-in and create-account tabs
// only appear once the server has answered, and hiding them behind a
// separate "Check" click meant someone could type a perfectly good
// address, see nothing happen, and conclude there was no way to sign
// in at all.
let probeTimer: ReturnType<typeof setTimeout> | undefined
watch(serverUrl, (next) => {
  clearTimeout(probeTimer)
  health.value = null
  probeError.value = null
  if (!/^https?:\/\/[^\s]+\.[^\s]|^https?:\/\/localhost|^https?:\/\/127\./i.test(next.trim())) {
    return
  }
  probeTimer = setTimeout(() => void probe(), 700)
})

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
  resetFeedback()
  unverified.value = false
  try {
    const ok = await sync.connect(
      serverUrl.value.trim(),
      email.value.trim(),
      password.value,
      join.value,
    )
    // The password is not kept a moment longer than needed.
    password.value = ''
    if (ok) {
      emit('close')
    } else if ((sync.status.lastError ?? '').toLowerCase().includes('confirm your e-mail')) {
      // The password was right; only the address is unconfirmed. Offer
      // the way out rather than leaving them to re-type a good password.
      unverified.value = true
    }
  } finally {
    connecting.value = false
  }
}

async function register () {
  busy.value = 'register'
  resetFeedback()
  try {
    const profile = await api.syncRegister(
      serverUrl.value.trim(),
      email.value.trim(),
      password.value,
      displayName.value.trim(),
    )
    if (profile.emailVerifiedAt) {
      notice.value = 'Account created. Sign in below.'
    } else {
      notice.value =
        'Account created. Check your e-mail for a confirmation link, then sign in.'
      unverified.value = true
    }
    mode.value = 'signIn'
  } catch (error) {
    authError.value = messageOf(error)
  } finally {
    busy.value = ''
  }
}

async function resendVerification () {
  busy.value = 'verify'
  resetFeedback()
  try {
    await api.syncRequestVerification(serverUrl.value.trim(), email.value.trim())
    notice.value = 'If that address has an account, another link is on its way.'
  } catch (error) {
    authError.value = messageOf(error)
  } finally {
    busy.value = ''
  }
}

/** Reads what the pasted invitation offers, so it can be shown first. */
async function checkInvitation () {
  const pasted = invitation.value.trim()
  offer.value = null
  offerError.value = null
  if (pasted === '') return

  checkingOffer.value = true
  try {
    offer.value = await api.syncInvitationOffer(serverUrl.value.trim(), pasted)
  } catch (error) {
    offerError.value = messageOf(error)
  } finally {
    checkingOffer.value = false
  }
}

async function joinInvited () {
  busy.value = 'join'
  resetFeedback()
  try {
    const joined = await api.syncJoinInvited(
      serverUrl.value.trim(),
      invitation.value.trim(),
      password.value,
      displayName.value.trim(),
    )
    password.value = ''
    await workspace.loadWorkspaces()
    await workspace.select(joined.id)
    emit('close')
  } catch (error) {
    authError.value = messageOf(error)
  } finally {
    busy.value = ''
  }
}

async function forgotPassword () {
  busy.value = 'reset'
  resetFeedback()
  try {
    await api.syncRequestPasswordReset(serverUrl.value.trim(), email.value.trim())
    // Worded so it says the same thing whether or not the address is
    // registered, which is what the server does too.
    notice.value = 'If that address has an account, a reset link is on its way.'
  } catch (error) {
    authError.value = messageOf(error)
  } finally {
    busy.value = ''
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

            <!-- Membership. Only meaningful once there is a server to
                 ask, which is why it lives inside the connected branch. -->
            <div v-if="workspace.current" class="border-t border-subtle pt-3">
              <MembersPanel :workspace-id="workspace.current.id" :open="open" />
            </div>

            <!-- Seats. Renders nothing at all unless the server
                 charges, so self-hosting shows no trace of billing. -->
            <div v-if="workspace.current">
              <BillingPanel :workspace-id="workspace.current.id" :open="open" />
            </div>
          </div>

          <!-- Not connected -->
          <div v-else class="space-y-3">
            <p class="text-xs text-muted">
              Put this workspace on a server to use it from more than one machine, or
              to share it with people you work with.
            </p>
            <p class="text-xs text-muted">
              Nothing here is required — Relay is complete without a server, and
              everything you have made stays on this machine either way.
            </p>

            <!-- Someone without a server of their own had nothing to
                 type here, and no way to discover that an account was
                 even on offer. The two sit side by side deliberately:
                 self-hosting is not the lesser path. -->
            <div v-if="using === null" class="grid gap-2 sm:grid-cols-2">
              <button
                type="button"
                class="rounded-md border border-subtle p-3 text-left transition-colors
                       hover:border-[var(--border-strong)] hover:bg-[var(--surface-raised)]"
                @click="useHosted"
              >
                <span class="flex items-center gap-1.5 text-xs font-medium text-primary">
                  <Cloud :size="13" />
                  Use Relay's server
                </span>
                <span class="mt-1 block text-xs text-muted">
                  Make an account in a moment. Nothing to set up.
                </span>
              </button>
              <button
                type="button"
                class="rounded-md border border-subtle p-3 text-left transition-colors
                       hover:border-[var(--border-strong)] hover:bg-[var(--surface-raised)]"
                @click="useOwn"
              >
                <span class="flex items-center gap-1.5 text-xs font-medium text-primary">
                  <Server :size="13" />
                  Use my own server
                </span>
                <span class="mt-1 block text-xs text-muted">
                  Any Relay server you or your team run.
                </span>
              </button>
            </div>

            <div v-else>
              <div class="mb-1 flex items-center justify-between">
                <label for="server-url" class="block text-xs font-medium text-secondary">
                  {{ using === 'hosted' ? "Relay's server" : 'Server address' }}
                </label>
                <button
                  type="button"
                  class="text-xs text-muted underline underline-offset-2 hover:text-secondary"
                  @click="using = null; serverUrl = ''; health = null; probeError = null"
                >
                  Change
                </button>
              </div>
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
                <!-- Still here for a retry; the address is checked on its
                     own a moment after you stop typing. -->
                <button
                  type="button"
                  class="btn shrink-0 text-xs"
                  :disabled="probing || !serverUrl.trim()"
                  @click="probe"
                >
                  <Loader2 v-if="probing" :size="13" class="animate-spin" />
                  {{ probing ? 'Checking' : 'Check' }}
                </button>
              </div>

              <p v-if="probing" class="mt-1.5 flex items-center gap-1.5 text-xs text-muted">
                <Loader2 :size="12" class="animate-spin" />
                Looking for a Relay server there…
              </p>
              <p
                v-else-if="health"
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
              <div class="flex gap-1 rounded-md border border-subtle p-0.5" role="tablist">
                <button
                  v-for="item in [
                    { id: 'signIn', label: 'Sign in' },
                    { id: 'register', label: 'Create an account' },
                    { id: 'invited', label: 'I was invited' },
                  ]"
                  :key="item.id"
                  type="button"
                  role="tab"
                  class="flex-1 rounded px-2 py-1 text-xs transition-colors"
                  :class="
                    mode === item.id
                      ? 'bg-[var(--surface-raised)] text-primary'
                      : 'text-muted hover:text-secondary'
                  "
                  :aria-selected="mode === item.id"
                  @click="mode = item.id as typeof mode; resetFeedback()"
                >
                  {{ item.label }}
                </button>
              </div>

              <template v-if="mode === 'invited'">
                <div>
                  <label for="sync-invite" class="mb-1 block text-xs font-medium text-secondary">
                    Invitation link
                  </label>
                  <input
                    id="sync-invite"
                    v-model="invitation"
                    class="input input-mono"
                    placeholder="Paste the whole link from the e-mail"
                    spellcheck="false"
                    autocomplete="off"
                    @blur="checkInvitation"
                    @keydown.enter.prevent="checkInvitation"
                  />
                  <p v-if="checkingOffer" class="mt-1.5 text-xs text-muted">Checking…</p>
                  <p
                    v-else-if="offerError"
                    class="mt-1.5 text-xs"
                    :style="{ color: 'var(--danger)' }"
                  >
                    {{ offerError }}
                  </p>
                </div>

                <!-- What is actually on offer, shown before they commit
                     to it rather than after. -->
                <div
                  v-if="offer"
                  class="rounded-md border p-2.5 text-xs"
                  :style="{ borderColor: 'var(--accent)', color: 'var(--text-secondary)' }"
                >
                  You have been invited to <strong>{{ offer.workspaceName }}</strong>
                  as {{ offer.role }}, at <strong>{{ offer.email }}</strong>.
                  <span class="mt-1 block text-muted">
                    That address is the one the invitation admits. Set a password below
                    — if you already have an account, use the one you have.
                  </span>
                </div>
              </template>

              <div v-else>
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

              <div v-if="mode === 'register' || mode === 'invited'">
                <label for="sync-name" class="mb-1 block text-xs font-medium text-secondary">
                  Display name <span class="font-normal text-muted">(optional)</span>
                </label>
                <input
                  id="sync-name"
                  v-model="displayName"
                  class="input"
                  placeholder="What teammates see"
                  autocomplete="name"
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
                  :autocomplete="mode === 'register' ? 'new-password' : 'current-password'"
                  @keydown.enter.prevent="
                    mode === 'register' ? canRegister && register() : canConnect && connect()
                  "
                />
                <p v-if="mode === 'register'" class="mt-1 text-xs text-muted">
                  Twelve characters or more. A phrase you can remember beats a short
                  tangle you cannot.
                </p>
                <button
                  v-else
                  type="button"
                  class="mt-1 text-xs text-muted underline-offset-2 hover:underline"
                  :disabled="busy !== '' || !email.trim()"
                  @click="forgotPassword"
                >
                  Forgotten your password?
                </button>
              </div>

              <!-- Shown when the password was right but the address is
                   unconfirmed: re-typing a correct password will not help. -->
              <div
                v-if="unverified"
                class="rounded-md border p-2.5 text-xs"
                :style="{ borderColor: 'var(--accent)', color: 'var(--text-secondary)' }"
              >
                <p class="mb-1.5">
                  This address has not been confirmed yet. Open the link Relay e-mailed
                  you, then sign in.
                </p>
                <button
                  type="button"
                  class="btn text-xs"
                  :disabled="busy !== '' || !email.trim()"
                  @click="resendVerification"
                >
                  <Loader2 v-if="busy === 'verify'" :size="13" class="animate-spin" />
                  Send another link
                </button>
              </div>

              <p
                v-if="notice"
                class="text-xs"
                :style="{ color: 'var(--success)' }"
              >
                {{ notice }}
              </p>
              <p
                v-if="authError"
                class="text-xs"
                :style="{ color: 'var(--danger)' }"
              >
                {{ authError }}
              </p>

              <label
                v-if="mode === 'signIn'"
                class="flex cursor-pointer items-start gap-2 text-xs text-secondary"
              >
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
                v-if="mode === 'invited'"
                type="button"
                class="btn btn-primary w-full text-xs"
                :disabled="!canJoin || busy !== ''"
                @click="joinInvited"
              >
                <Loader2 v-if="busy === 'join'" :size="13" class="animate-spin" />
                Join {{ offer ? offer.workspaceName : 'the workspace' }}
              </button>
              <button
                v-else-if="mode === 'register'"
                type="button"
                class="btn btn-primary w-full text-xs"
                :disabled="!canRegister || busy !== ''"
                @click="register"
              >
                <Loader2 v-if="busy === 'register'" :size="13" class="animate-spin" />
                Create the account
              </button>
              <button
                v-else
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
