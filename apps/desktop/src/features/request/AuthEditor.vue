<script setup lang="ts">
import { computed } from 'vue'
import type { AuthConfig, OAuth2Config } from '@relay/shared-types'
import OAuth2Editor from './OAuth2Editor.vue'

const props = defineProps<{ modelValue: AuthConfig }>()
const emit = defineEmits<{ 'update:modelValue': [AuthConfig] }>()

const TYPES = [
  { id: 'inherit', label: 'Inherit from parent' },
  { id: 'none', label: 'No auth' },
  { id: 'basic', label: 'Basic' },
  { id: 'bearer', label: 'Bearer token' },
  { id: 'apiKey', label: 'API key' },
  { id: 'oauth2', label: 'OAuth 2.0' },
] as const

type AuthType = (typeof TYPES)[number]['id']

/** Shown as an example; kept out of the template so the parser cannot
 *  mistake its braces for an interpolation. */
const VARIABLE_EXAMPLE = '{{apiToken}}'

const type = computed(() => props.modelValue.type as AuthType)

function setType (next: AuthType) {
  switch (next) {
    case 'inherit':
      emit('update:modelValue', { type: 'inherit' })
      break
    case 'none':
      emit('update:modelValue', { type: 'none' })
      break
    case 'basic':
      emit('update:modelValue', { type: 'basic', username: '', password: '' })
      break
    case 'bearer':
      emit('update:modelValue', { type: 'bearer', token: '' })
      break
    case 'apiKey':
      emit('update:modelValue', { type: 'apiKey', key: '', value: '', in: 'header' })
      break
    case 'oauth2':
      emit('update:modelValue', { type: 'oauth2', ...BLANK_OAUTH })
      break
  }
}

/** A new OAuth block. PKCE starts on, as it should stay. */
const BLANK_OAUTH: OAuth2Config = {
  grant: 'authorizationCode',
  authUrl: '',
  tokenUrl: '',
  clientId: '',
  clientSecret: '',
  scope: '',
  audience: '',
  redirectUri: '',
  username: '',
  password: '',
  usePkce: true,
  clientAuth: 'header',
}

/** The OAuth block, flattened out of the tagged union for the child.
 *  The discriminator is stripped so what reaches the IPC layer is an
 *  `OAuth2Config` and nothing else. */
const oauth = computed<OAuth2Config>(() => {
  if (props.modelValue.type !== 'oauth2') return BLANK_OAUTH
  const { type: _tag, ...config } = props.modelValue
  return config
})

function setOAuth (next: OAuth2Config) {
  emit('update:modelValue', { type: 'oauth2', ...next })
}
</script>

<template>
  <div class="space-y-4 p-4">
    <div class="max-w-xs">
      <label for="auth-type" class="mb-1 block text-xs font-medium text-secondary">
        Type
      </label>
      <select
        id="auth-type"
        class="input py-1.5 text-xs"
        :value="type"
        @change="setType(($event.target as HTMLSelectElement).value as AuthType)"
      >
        <option v-for="item in TYPES" :key="item.id" :value="item.id">{{ item.label }}</option>
      </select>
    </div>

    <p v-if="type === 'inherit'" class="max-w-prose text-xs text-muted">
      This request uses whatever authentication its nearest parent folder or collection
      defines. If none do, nothing is sent.
    </p>

    <p v-else-if="type === 'none'" class="max-w-prose text-xs text-muted">
      No authentication header is added. You can still set one manually under Headers.
    </p>

    <div v-else-if="modelValue.type === 'basic'" class="grid max-w-md gap-3 sm:grid-cols-2">
      <div>
        <label for="basic-username" class="mb-1 block text-xs font-medium text-secondary">
          Username
        </label>
        <input
          id="basic-username"
          class="input input-mono"
          :value="modelValue.username"
          spellcheck="false"
          autocomplete="off"
          @input="
            emit('update:modelValue', {
              type: 'basic',
              username: ($event.target as HTMLInputElement).value,
              password: modelValue.type === 'basic' ? modelValue.password : '',
            })
          "
        />
      </div>
      <div>
        <label for="basic-password" class="mb-1 block text-xs font-medium text-secondary">
          Password
        </label>
        <input
          id="basic-password"
          type="password"
          class="input input-mono"
          :value="modelValue.password"
          autocomplete="off"
          @input="
            emit('update:modelValue', {
              type: 'basic',
              username: modelValue.type === 'basic' ? modelValue.username : '',
              password: ($event.target as HTMLInputElement).value,
            })
          "
        />
      </div>
    </div>

    <div v-else-if="modelValue.type === 'bearer'" class="max-w-md">
      <label for="bearer-token" class="mb-1 block text-xs font-medium text-secondary">
        Token
      </label>
      <input
        id="bearer-token"
        class="input input-mono"
        :value="modelValue.token"
        :placeholder="VARIABLE_EXAMPLE"
        spellcheck="false"
        autocomplete="off"
        @input="emit('update:modelValue', { type: 'bearer', token: ($event.target as HTMLInputElement).value })"
      />
      <p class="mt-1.5 text-xs text-muted">
        Reference a secret variable — <code class="font-mono">{{ VARIABLE_EXAMPLE }}</code> —
        to keep the value in your operating system's credential store instead of the
        database.
      </p>
    </div>

    <OAuth2Editor
      v-else-if="modelValue.type === 'oauth2'"
      :model-value="oauth"
      @update:model-value="setOAuth"
    />

    <div v-else-if="modelValue.type === 'apiKey'" class="grid max-w-xl gap-3 sm:grid-cols-3">
      <div>
        <label for="apikey-key" class="mb-1 block text-xs font-medium text-secondary">
          Key
        </label>
        <input
          id="apikey-key"
          class="input input-mono"
          :value="modelValue.key"
          placeholder="X-API-Key"
          spellcheck="false"
          @input="
            emit('update:modelValue', {
              type: 'apiKey',
              key: ($event.target as HTMLInputElement).value,
              value: modelValue.type === 'apiKey' ? modelValue.value : '',
              in: modelValue.type === 'apiKey' ? modelValue.in : 'header',
            })
          "
        />
      </div>
      <div>
        <label for="apikey-value" class="mb-1 block text-xs font-medium text-secondary">
          Value
        </label>
        <input
          id="apikey-value"
          class="input input-mono"
          :value="modelValue.value"
          spellcheck="false"
          autocomplete="off"
          @input="
            emit('update:modelValue', {
              type: 'apiKey',
              key: modelValue.type === 'apiKey' ? modelValue.key : '',
              value: ($event.target as HTMLInputElement).value,
              in: modelValue.type === 'apiKey' ? modelValue.in : 'header',
            })
          "
        />
      </div>
      <div>
        <label for="apikey-in" class="mb-1 block text-xs font-medium text-secondary">
          Send in
        </label>
        <select
          id="apikey-in"
          class="input py-1.5 text-xs"
          :value="modelValue.in"
          @change="
            emit('update:modelValue', {
              type: 'apiKey',
              key: modelValue.type === 'apiKey' ? modelValue.key : '',
              value: modelValue.type === 'apiKey' ? modelValue.value : '',
              in: ($event.target as HTMLSelectElement).value as 'header' | 'query',
            })
          "
        >
          <option value="header">Header</option>
          <option value="query">Query parameter</option>
        </select>
      </div>
    </div>
  </div>
</template>
