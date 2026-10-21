<script setup lang="ts">
import { AlertTriangle, FileKey, FolderOpen } from '@lucide/vue'
import type { ClientCertificate, ProxyConfig, RequestSettings } from '@relay/shared-types'

const props = defineProps<{ modelValue: RequestSettings }>()
const emit = defineEmits<{ 'update:modelValue': [RequestSettings] }>()

function patch (changes: Partial<RequestSettings>) {
  emit('update:modelValue', { ...props.modelValue, ...changes })
}

// --- client certificate ---------------------------------------------------

const BLANK_CERTIFICATE: ClientCertificate = { hosts: [], pemPath: '' }

function patchCertificate (changes: Partial<ClientCertificate>) {
  patch({
    clientCertificate: {
      ...(props.modelValue.clientCertificate ?? BLANK_CERTIFICATE),
      ...changes,
    },
  })
}

async function pickCertificate () {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const selected = await open({
    multiple: false,
    filters: [{ name: 'PEM certificate', extensions: ['pem', 'crt', 'cer', 'key'] }],
  })
  if (typeof selected === 'string') patchCertificate({ pemPath: selected })
}

/** Host lists are edited as text and stored as a list. */
function parseHosts (text: string): string[] {
  return text
    .split(',')
    .map((host) => host.trim())
    .filter((host) => host !== '')
}

// --- proxy ----------------------------------------------------------------

const BLANK_PROXY: ProxyConfig = { url: '', username: null, password: null, bypass: [] }

function patchProxy (changes: Partial<ProxyConfig>) {
  patch({ proxy: { ...(props.modelValue.proxy ?? BLANK_PROXY), ...changes } })
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

    <!-- Client certificate. Mutual TLS: the server asks the client to
         prove who it is, which internal services often do. -->
    <fieldset class="space-y-2.5">
      <legend class="mb-1 text-xs font-medium text-secondary">Client certificate</legend>

      <label class="flex cursor-pointer items-center gap-2 text-xs text-secondary">
        <input
          type="checkbox"
          class="h-3.5 w-3.5 accent-[var(--accent)]"
          :checked="modelValue.clientCertificate !== null && modelValue.clientCertificate !== undefined"
          @change="
            patch({
              clientCertificate: ($event.target as HTMLInputElement).checked
                ? BLANK_CERTIFICATE
                : null,
            })
          "
        />
        Present a certificate during the TLS handshake
      </label>

      <div v-if="modelValue.clientCertificate" class="space-y-2.5 pl-5">
        <div>
          <label for="cert-path" class="mb-1 block text-xs font-medium text-secondary">
            PEM file
          </label>
          <div class="flex gap-2">
            <input
              id="cert-path"
              class="input input-mono"
              :value="modelValue.clientCertificate.pemPath"
              placeholder="/etc/ssl/private/client.pem"
              spellcheck="false"
              @input="patchCertificate({ pemPath: ($event.target as HTMLInputElement).value })"
            />
            <button type="button" class="btn shrink-0 gap-1.5 text-xs" @click="pickCertificate">
              <FolderOpen class="size-3.5" />
              Choose
            </button>
          </div>
          <p class="mt-1.5 flex items-start gap-1.5 text-xs text-muted">
            <FileKey class="mt-px size-3.5 shrink-0" />
            <span>
              One file holding the certificate chain and its private key. Relay reads it
              at send time and never copies it into the database, an export or a sync
              payload — only this path travels.
            </span>
          </p>
        </div>

        <div>
          <label for="cert-hosts" class="mb-1 block text-xs font-medium text-secondary">
            Only for these hosts <span class="font-normal text-muted">(optional)</span>
          </label>
          <input
            id="cert-hosts"
            class="input input-mono"
            :value="modelValue.clientCertificate.hosts.join(', ')"
            placeholder="internal.example.com, api.corp"
            spellcheck="false"
            @input="patchCertificate({ hosts: parseHosts(($event.target as HTMLInputElement).value) })"
          />
          <p class="mt-1.5 max-w-prose text-xs text-muted">
            Comma separated. A name also covers its subdomains. Leave blank to present
            the certificate to whatever host this request goes to.
          </p>
        </div>
      </div>
    </fieldset>

    <!-- Proxy. -->
    <fieldset class="space-y-2.5">
      <legend class="mb-1 text-xs font-medium text-secondary">Proxy</legend>

      <label class="flex cursor-pointer items-center gap-2 text-xs text-secondary">
        <input
          type="checkbox"
          class="h-3.5 w-3.5 accent-[var(--accent)]"
          :checked="modelValue.proxy !== null && modelValue.proxy !== undefined"
          @change="
            patch({ proxy: ($event.target as HTMLInputElement).checked ? BLANK_PROXY : null })
          "
        />
        Send this request through a proxy
      </label>

      <div v-if="modelValue.proxy" class="space-y-2.5 pl-5">
        <div>
          <label for="proxy-url" class="mb-1 block text-xs font-medium text-secondary">
            Proxy URL
          </label>
          <input
            id="proxy-url"
            class="input input-mono"
            :value="modelValue.proxy.url"
            placeholder="http://proxy.corp:8080"
            spellcheck="false"
            @input="patchProxy({ url: ($event.target as HTMLInputElement).value })"
          />
        </div>

        <div class="grid gap-3 sm:grid-cols-2">
          <div>
            <label for="proxy-user" class="mb-1 block text-xs font-medium text-secondary">
              Username <span class="font-normal text-muted">(optional)</span>
            </label>
            <input
              id="proxy-user"
              class="input input-mono"
              :value="modelValue.proxy.username ?? ''"
              autocomplete="off"
              @input="
                patchProxy({ username: ($event.target as HTMLInputElement).value || null })
              "
            />
          </div>
          <div>
            <label for="proxy-password" class="mb-1 block text-xs font-medium text-secondary">
              Password <span class="font-normal text-muted">(optional)</span>
            </label>
            <input
              id="proxy-password"
              type="password"
              class="input input-mono"
              :value="modelValue.proxy.password ?? ''"
              autocomplete="off"
              @input="
                patchProxy({ password: ($event.target as HTMLInputElement).value || null })
              "
            />
          </div>
        </div>

        <div>
          <label for="proxy-bypass" class="mb-1 block text-xs font-medium text-secondary">
            Bypass for <span class="font-normal text-muted">(optional)</span>
          </label>
          <input
            id="proxy-bypass"
            class="input input-mono"
            :value="modelValue.proxy.bypass.join(', ')"
            placeholder="localhost, 127.0.0.1, .internal"
            spellcheck="false"
            @input="patchProxy({ bypass: parseHosts(($event.target as HTMLInputElement).value) })"
          />
        </div>
      </div>
    </fieldset>
  </div>
</template>
