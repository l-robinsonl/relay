<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { CheckCircle2, ExternalLink, Loader2, RefreshCw, Trash2, TriangleAlert } from '@lucide/vue'
import type { ClientAuthStyle, OAuth2Config, OAuth2Grant } from '@relay/shared-types'
import type { OAuthTokenStatus } from '@/services/ipc'
import { api } from '@/services/ipc'
import { formatRelativeTime } from '@/composables/useFormat'

const props = defineProps<{ modelValue: OAuth2Config }>()
const emit = defineEmits<{ 'update:modelValue': [OAuth2Config] }>()

const GRANTS: { id: OAuth2Grant, label: string, hint: string }[] = [
  {
    id: 'authorizationCode',
    label: 'Authorization code',
    hint: 'Signs in through your browser. The right choice for an API acting on your behalf.',
  },
  {
    id: 'clientCredentials',
    label: 'Client credentials',
    hint: 'No user involved: the application authenticates as itself.',
  },
  {
    id: 'password',
    label: 'Password',
    hint: 'Sends a username and password directly. Deprecated by the OAuth working group; use it only where nothing else is offered.',
  },
]

const status = ref<OAuthTokenStatus | null>(null)
const busy = ref<'' | 'fetch' | 'authorize'>('')
const error = ref<string | null>(null)

const grant = computed(() => props.modelValue.grant)
const usesBrowser = computed(() => grant.value === 'authorizationCode')
const needsAuthUrl = computed(() => usesBrowser.value)
const needsUserCredentials = computed(() => grant.value === 'password')

/** Shown as an example; kept out of the template so the parser cannot
 *  mistake its braces for an interpolation. */
const VARIABLE_EXAMPLE = '{{clientSecret}}'

function update (patch: Partial<OAuth2Config>) {
  emit('update:modelValue', { ...props.modelValue, ...patch })
}

/** Reads the status for the configuration as it now stands. */
async function refreshStatus () {
  try {
    status.value = await api.oauthTokenStatus(props.modelValue)
  } catch {
    // A status read failing is not worth a banner; the buttons still work.
    status.value = null
  }
}

// Changing any field may point at a different token, so the badge has to
// follow the form rather than only the last button pressed.
watch(() => props.modelValue, refreshStatus, { immediate: true, deep: true })

async function getToken () {
  busy.value = 'fetch'
  error.value = null
  try {
    status.value = await api.oauthFetchToken(props.modelValue)
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    busy.value = ''
  }
}

async function authorize () {
  busy.value = 'authorize'
  error.value = null
  try {
    status.value = await api.oauthAuthorize(props.modelValue)
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    busy.value = ''
  }
}

async function forget () {
  await api.oauthForgetToken(props.modelValue)
  error.value = null
  await refreshStatus()
}

function messageOf (value: unknown): string {
  if (value && typeof value === 'object' && 'message' in value) {
    const detail = 'detail' in value ? (value as { detail?: string | null }).detail : null
    const message = String((value as { message: unknown }).message)
    return detail ? `${message} — ${detail}` : message
  }
  return String(value)
}

const expiry = computed(() => {
  const at = status.value?.expiresAt
  if (!at) return null
  return formatRelativeTime(at)
})
</script>

<template>
  <div class="space-y-4">
    <div class="grid max-w-3xl gap-3 sm:grid-cols-2">
      <div>
        <label for="oauth-grant" class="mb-1 block text-xs font-medium text-secondary">
          Grant type
        </label>
        <select
          id="oauth-grant"
          class="input py-1.5 text-xs"
          :value="grant"
          @change="update({ grant: ($event.target as HTMLSelectElement).value as OAuth2Grant })"
        >
          <option v-for="item in GRANTS" :key="item.id" :value="item.id">{{ item.label }}</option>
        </select>
        <p class="mt-1.5 max-w-prose text-xs text-muted">
          {{ GRANTS.find((g) => g.id === grant)?.hint }}
        </p>
      </div>

      <div>
        <label for="oauth-client-auth" class="mb-1 block text-xs font-medium text-secondary">
          Send client credentials
        </label>
        <select
          id="oauth-client-auth"
          class="input py-1.5 text-xs"
          :value="modelValue.clientAuth"
          @change="
            update({ clientAuth: ($event.target as HTMLSelectElement).value as ClientAuthStyle })
          "
        >
          <option value="header">As a Basic auth header</option>
          <option value="body">In the request body</option>
        </select>
        <p class="mt-1.5 max-w-prose text-xs text-muted">
          The header is what the specification prefers. Switch to the body if the
          provider answers <code class="font-mono">invalid_client</code>.
        </p>
      </div>
    </div>

    <div class="grid max-w-3xl gap-3 sm:grid-cols-2">
      <div v-if="needsAuthUrl">
        <label for="oauth-auth-url" class="mb-1 block text-xs font-medium text-secondary">
          Authorization URL
        </label>
        <input
          id="oauth-auth-url"
          class="input input-mono"
          :value="modelValue.authUrl"
          placeholder="https://id.example.com/authorize"
          spellcheck="false"
          @input="update({ authUrl: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label for="oauth-token-url" class="mb-1 block text-xs font-medium text-secondary">
          Token URL
        </label>
        <input
          id="oauth-token-url"
          class="input input-mono"
          :value="modelValue.tokenUrl"
          placeholder="https://id.example.com/oauth/token"
          spellcheck="false"
          @input="update({ tokenUrl: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label for="oauth-client-id" class="mb-1 block text-xs font-medium text-secondary">
          Client ID
        </label>
        <input
          id="oauth-client-id"
          class="input input-mono"
          :value="modelValue.clientId"
          spellcheck="false"
          autocomplete="off"
          @input="update({ clientId: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label for="oauth-client-secret" class="mb-1 block text-xs font-medium text-secondary">
          Client secret
        </label>
        <input
          id="oauth-client-secret"
          class="input input-mono"
          type="password"
          :value="modelValue.clientSecret"
          :placeholder="VARIABLE_EXAMPLE"
          autocomplete="off"
          @input="update({ clientSecret: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label for="oauth-scope" class="mb-1 block text-xs font-medium text-secondary">
          Scope
        </label>
        <input
          id="oauth-scope"
          class="input input-mono"
          :value="modelValue.scope"
          placeholder="read:user write:repo"
          spellcheck="false"
          @input="update({ scope: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label for="oauth-audience" class="mb-1 block text-xs font-medium text-secondary">
          Audience <span class="font-normal text-muted">(optional)</span>
        </label>
        <input
          id="oauth-audience"
          class="input input-mono"
          :value="modelValue.audience"
          placeholder="https://api.example.com"
          spellcheck="false"
          @input="update({ audience: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <template v-if="needsUserCredentials">
        <div>
          <label for="oauth-username" class="mb-1 block text-xs font-medium text-secondary">
            Username
          </label>
          <input
            id="oauth-username"
            class="input input-mono"
            :value="modelValue.username"
            spellcheck="false"
            autocomplete="off"
            @input="update({ username: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div>
          <label for="oauth-password" class="mb-1 block text-xs font-medium text-secondary">
            Password
          </label>
          <input
            id="oauth-password"
            class="input input-mono"
            type="password"
            :value="modelValue.password"
            autocomplete="off"
            @input="update({ password: ($event.target as HTMLInputElement).value })"
          />
        </div>
      </template>

      <div v-if="usesBrowser" class="sm:col-span-2">
        <label for="oauth-redirect" class="mb-1 block text-xs font-medium text-secondary">
          Redirect URI <span class="font-normal text-muted">(optional)</span>
        </label>
        <input
          id="oauth-redirect"
          class="input input-mono"
          :value="modelValue.redirectUri"
          placeholder="Leave blank to use a free loopback port"
          spellcheck="false"
          @input="update({ redirectUri: ($event.target as HTMLInputElement).value })"
        />
        <p class="mt-1.5 max-w-prose text-xs text-muted">
          Relay listens on <code class="font-mono">127.0.0.1</code> for the redirect. Set
          this only where the provider requires a registered URI — then register exactly
          what you type here, including the port.
        </p>
      </div>
    </div>

    <label v-if="usesBrowser" class="flex max-w-prose items-start gap-2 text-xs">
      <input
        type="checkbox"
        class="mt-0.5"
        :checked="modelValue.usePkce"
        @change="update({ usePkce: ($event.target as HTMLInputElement).checked })"
      />
      <span>
        <span class="font-medium text-secondary">Use PKCE</span>
        <span class="block text-muted">
          Proves the application that redeems the code is the one that asked for it.
          Leave this on; turn it off only for a provider that rejects it outright.
        </span>
      </span>
    </label>

    <div class="flex flex-wrap items-center gap-2 border-t border-subtle pt-3">
      <button
        v-if="usesBrowser"
        type="button"
        class="btn btn-primary gap-1.5 text-xs"
        :disabled="busy !== ''"
        @click="authorize"
      >
        <Loader2 v-if="busy === 'authorize'" class="size-3.5 animate-spin" />
        <ExternalLink v-else class="size-3.5" />
        {{ status?.hasToken ? 'Authorize again' : 'Authorize' }}
      </button>

      <button
        v-else
        type="button"
        class="btn btn-primary gap-1.5 text-xs"
        :disabled="busy !== ''"
        @click="getToken"
      >
        <Loader2 v-if="busy === 'fetch'" class="size-3.5 animate-spin" />
        <RefreshCw v-else class="size-3.5" />
        {{ status?.hasToken ? 'Get a new token' : 'Get token' }}
      </button>

      <button
        v-if="status?.hasToken"
        type="button"
        class="btn gap-1.5 text-xs"
        :disabled="busy !== ''"
        @click="forget"
      >
        <Trash2 class="size-3.5" />
        Forget token
      </button>

      <p
        v-if="status?.hasToken"
        class="flex items-center gap-1.5 text-xs text-muted"
      >
        <CheckCircle2 class="size-3.5" :style="{ color: 'var(--success)' }" />
        <span>
          Token held<template v-if="expiry">, expires {{ expiry }}</template>
          <template v-if="status.canRefresh"> · renews itself</template>
        </span>
      </p>
      <p v-else class="text-xs text-muted">
        No token yet.
        <template v-if="usesBrowser">Authorizing opens your browser once.</template>
      </p>
    </div>

    <p
      v-if="busy === 'authorize'"
      class="max-w-prose text-xs text-muted"
    >
      Waiting for your browser. Relay is listening on a loopback port and will pick the
      sign-in up automatically; this gives up after five minutes.
    </p>

    <div
      v-if="error"
      class="flex max-w-prose items-start gap-2 rounded border p-2.5 text-xs"
      :style="{
        borderColor: 'var(--danger)',
        backgroundColor: 'var(--danger-muted)',
        color: 'var(--danger)',
      }"
    >
      <TriangleAlert class="mt-px size-3.5 shrink-0" />
      <span>{{ error }}</span>
    </div>

    <p class="max-w-prose text-xs text-muted">
      Tokens are held in memory for this session only. They are never written to the
      database, included in an export, or sent to a sync server — so this request can be
      shared without sharing your access.
    </p>
  </div>
</template>
