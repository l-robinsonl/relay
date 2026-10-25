<script setup lang="ts">
import { computed, ref } from 'vue'
import {
  AlertCircle,
  Check,
  Copy,
  Download,
  Ban,
  Loader2,
  Wand2,
  Zap,
} from '@lucide/vue'
import type { Tab, ResponseTab } from '@/types/ui'
import CodeEditor from '@/components/ui/CodeEditor.vue'
import EmptyState from '@/components/ui/EmptyState.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import TestResults from '@/features/response/TestResults.vue'
import { formatBody, formatBytes, formatDuration } from '@/composables/useFormat'
import { describeShortcut } from '@/composables/useShortcuts'
import { useTabsStore } from '@/stores/tabs'
import StreamLog from '@/features/response/StreamLog.vue'

const props = defineProps<{ tab: Tab }>()

const tabs = useTabsStore()
const copied = ref(false)
const prettified = ref(true)

const sending = computed(() => props.tab.executionId !== null)
const result = computed(() => props.tab.result)

const response = computed(() =>
  result.value?.kind === 'response' ? result.value.response : null,
)

const responseTabs = computed<{ id: ResponseTab; label: string; count?: number }[]>(() => [
  { id: 'body', label: 'Body' },
  { id: 'headers', label: 'Headers', count: response.value?.headers.length ?? 0 },
  { id: 'cookies', label: 'Cookies', count: response.value?.cookies.length ?? 0 },
  { id: 'tests', label: 'Tests', count: testCount.value },
])

/** Shown on the Tests tab so a failure is visible without opening it. */
const testCount = computed(() => {
  const tests = props.tab.tests
  if (!tests) return 0
  return (
    tests.tests.flatMap((t) => t.assertions).length + tests.looseAssertions.length
  )
})

const testsFailed = computed(() => {
  const tests = props.tab.tests
  if (!tests) return false
  if (tests.error) return true
  if (tests.looseAssertions.some((a) => !a.passed)) return true
  return tests.tests.some((t) => t.error || t.assertions.some((a) => !a.passed))
})

/** The body as text, pretty-printed when it is JSON and that is wanted. */
const bodyText = computed(() => {
  const body = response.value?.body
  if (!body) return ''
  switch (body.kind) {
    case 'text':
      return prettified.value
        ? formatBody(body.text, response.value?.contentType)
        : body.text
    case 'file':
      return body.preview
    case 'binary':
      return ''
    case 'truncated':
      return ''
    case 'empty':
      return ''
  }
})

const bodyLanguage = computed(() => {
  const type = response.value?.contentType ?? ''
  if (type.includes('json')) return 'json'
  if (type.includes('xml') || type.includes('html')) return 'xml'
  if (type.includes('javascript')) return 'javascript'
  return 'text'
})

const canPrettify = computed(() => (response.value?.contentType ?? '').includes('json'))

async function copyBody () {
  await navigator.clipboard.writeText(bodyText.value)
  copied.value = true
  setTimeout(() => (copied.value = false), 1500)
}

const sendShortcut = describeShortcut({ key: 'Enter', mod: true })
</script>

<template>
  <!-- A connection has no single response, so it gets the log
       instead of the status bar, tabs and body viewer below. -->
  <section
    v-if="tab.doc.kind === 'sse' || tab.doc.kind === 'websocket'"
    class="flex h-full min-h-0 flex-col surface-base"
    aria-label="Stream"
  >
    <StreamLog
      :lines="tab.stream"
      :live="tab.streaming"
      :error="tab.streamError"
      :can-send="tab.doc.kind === 'websocket'"
      @clear="tabs.clearStream(tab.id)"
      @send="tabs.streamSend(tab.id, $event)"
    />
  </section>

  <section v-else class="flex h-full min-h-0 flex-col surface-base" aria-label="Response">
    <!-- Status bar -->
    <div
      class="flex h-9 shrink-0 items-center gap-3 border-b border-subtle px-3"
      aria-live="polite"
    >
      <template v-if="sending">
        <Loader2 :size="14" class="animate-spin" :style="{ color: 'var(--accent)' }" />
        <span class="text-xs text-secondary">Sending…</span>
        <span v-if="tab.bytesRead" class="text-xs tabular-nums text-muted">
          {{ formatBytes(tab.bytesRead) }} received
        </span>
      </template>

      <template v-else-if="response">
        <StatusBadge :status="response.status" :status-text="response.statusText" />
        <span class="text-xs tabular-nums text-muted">
          {{ formatDuration(response.timings.totalMs) }}
        </span>
        <span class="text-xs tabular-nums text-muted">
          {{ formatBytes(response.sizeBytes) }}
        </span>
        <span v-if="response.httpVersion" class="text-xs text-muted">
          {{ response.httpVersion }}
        </span>

        <div class="ml-auto flex items-center gap-1">
          <button
            v-if="canPrettify"
            type="button"
            class="btn btn-ghost px-2 py-1 text-xs"
            :aria-pressed="prettified"
            @click="prettified = !prettified"
          >
            <Wand2 :size="12" />
            {{ prettified ? 'Raw' : 'Pretty' }}
          </button>
          <button
            type="button"
            class="btn btn-ghost px-2 py-1 text-xs"
            :disabled="!bodyText"
            @click="copyBody"
          >
            <component :is="copied ? Check : Copy" :size="12" />
            {{ copied ? 'Copied' : 'Copy' }}
          </button>
        </div>
      </template>

      <template v-else-if="result?.kind === 'error'">
        <AlertCircle :size="13" :style="{ color: 'var(--danger)' }" />
        <span class="text-xs font-medium" :style="{ color: 'var(--danger)' }">
          Request failed
        </span>
        <span class="text-xs tabular-nums text-muted">
          {{ formatDuration(result.elapsedMs) }}
        </span>
      </template>

      <template v-else-if="result?.kind === 'cancelled'">
        <Ban :size="13" class="text-muted" />
        <span class="text-xs text-secondary">Cancelled</span>
        <span class="text-xs tabular-nums text-muted">
          {{ formatDuration(result.elapsedMs) }}
        </span>
      </template>

      <span v-else class="text-xs text-muted">No response yet</span>
    </div>

    <!-- Indeterminate progress -->
    <div v-if="sending" class="h-0.5 shrink-0 overflow-hidden bg-[var(--surface-sunken)]">
      <div
        class="animate-indeterminate h-full w-1/4 rounded-full"
        :style="{ backgroundColor: 'var(--accent)' }"
      />
    </div>

    <!-- Error detail -->
    <div
      v-if="result?.kind === 'error'"
      class="min-h-0 flex-1 overflow-y-auto p-4"
    >
      <div
        class="max-w-prose space-y-2 rounded-md border p-3"
        :style="{ borderColor: 'var(--danger)', backgroundColor: 'var(--danger-muted)' }"
      >
        <p class="text-xs font-semibold" :style="{ color: 'var(--danger)' }">
          {{ result.error.kind }}
        </p>
        <p class="text-sm text-primary">{{ result.error.message }}</p>
        <p v-if="result.error.detail" class="selectable font-mono text-xs text-muted">
          {{ result.error.detail }}
        </p>
      </div>
    </div>

    <!-- Response content -->
    <template v-else-if="response">
      <nav
        class="flex shrink-0 items-center gap-0.5 border-b border-subtle px-2"
        role="tablist"
        aria-label="Response sections"
      >
        <button
          v-for="item in responseTabs"
          :key="item.id"
          type="button"
          role="tab"
          class="tab"
          :aria-selected="tab.activeResponseTab === item.id"
          @click="tabs.setResponseTab(tab.id, item.id)"
        >
          {{ item.label }}
          <span
            v-if="item.count"
            class="ml-1 rounded-full px-1.5 text-xxs tabular-nums"
            :style="
              item.id === 'tests' && testsFailed
                ? { backgroundColor: 'var(--danger-muted)', color: 'var(--danger)' }
                : { backgroundColor: 'var(--surface-active)', color: 'var(--text-muted)' }
            "
          >{{ item.count }}</span>
        </button>
      </nav>

      <div class="min-h-0 flex-1 overflow-hidden">
        <!-- Body -->
        <template v-if="tab.activeResponseTab === 'body'">
          <div
            v-if="response.body.kind === 'empty'"
            class="flex h-full items-center justify-center text-xs text-muted"
          >
            The response has no body.
          </div>

          <div
            v-else-if="response.body.kind === 'binary'"
            class="flex h-full flex-col items-center justify-center gap-2 text-center"
          >
            <Download :size="22" class="text-muted" />
            <p class="text-sm text-secondary">Binary response</p>
            <p class="text-xs text-muted">
              {{ formatBytes(response.sizeBytes) }} of
              {{ response.contentType ?? 'unknown type' }}
            </p>
          </div>

          <div
            v-else-if="response.body.kind === 'truncated'"
            class="flex h-full flex-col items-center justify-center gap-2 px-6 text-center"
          >
            <AlertCircle :size="22" :style="{ color: 'var(--warning)' }" />
            <p class="text-sm text-secondary">Response too large</p>
            <p class="max-w-sm text-xs text-muted">{{ response.body.reason }}</p>
            <p class="text-xs text-muted">
              Raise the limit under Settings if you need the whole body.
            </p>
          </div>

          <div v-else class="flex h-full flex-col">
            <p
              v-if="response.body.kind === 'file'"
              class="shrink-0 border-b border-subtle px-3 py-1.5 text-xs text-muted"
            >
              Showing the first part of a {{ formatBytes(response.body.size) }} response,
              which was written to disk rather than held in memory.
            </p>
            <CodeEditor
              class="min-h-0 flex-1"
              readonly
              :language="bodyLanguage"
              :model-value="bodyText"
            />
          </div>
        </template>

        <!-- Headers -->
        <div
          v-else-if="tab.activeResponseTab === 'headers'"
          class="h-full overflow-y-auto"
        >
          <dl class="divide-y divide-[var(--border-subtle)]">
            <div
              v-for="(header, index) in response.headers"
              :key="`${header.key}-${index}`"
              class="grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)] gap-3 px-3 py-1.5
                     hover:bg-[var(--surface-hover)]"
            >
              <dt class="selectable truncate font-mono text-xs text-secondary">
                {{ header.key }}
              </dt>
              <dd class="selectable break-all font-mono text-xs text-primary">
                {{ header.value }}
              </dd>
            </div>
          </dl>
        </div>

        <!-- Cookies -->
        <div
          v-else-if="tab.activeResponseTab === 'cookies'"
          class="h-full overflow-y-auto"
        >
          <EmptyState
            v-if="!response.cookies.length"
            title="No cookies"
            description="This response did not set any cookies."
          />
          <table v-else class="w-full text-xs">
            <thead>
              <tr class="border-b border-subtle text-left text-xxs uppercase
                         tracking-wide text-muted">
                <th class="px-3 py-1.5 font-medium">Name</th>
                <th class="px-3 py-1.5 font-medium">Value</th>
                <th class="px-3 py-1.5 font-medium">Path</th>
                <th class="px-3 py-1.5 font-medium">Flags</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="cookie in response.cookies"
                :key="cookie.name"
                class="border-b border-subtle hover:bg-[var(--surface-hover)]"
              >
                <td class="selectable px-3 py-1.5 font-mono text-secondary">
                  {{ cookie.name }}
                </td>
                <td class="selectable max-w-xs truncate px-3 py-1.5 font-mono text-primary">
                  {{ cookie.value }}
                </td>
                <td class="px-3 py-1.5 font-mono text-muted">{{ cookie.path ?? '—' }}</td>
                <td class="px-3 py-1.5">
                  <span v-if="cookie.httpOnly" class="badge mr-1 bg-[var(--surface-active)]">
                    HttpOnly
                  </span>
                  <span v-if="cookie.secure" class="badge mr-1 bg-[var(--surface-active)]">
                    Secure
                  </span>
                  <span v-if="cookie.sameSite" class="badge bg-[var(--surface-active)]">
                    {{ cookie.sameSite }}
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- Tests -->
        <TestResults v-else :outcome="tab.tests" />
      </div>
    </template>

    <!-- Nothing sent yet -->
    <EmptyState
      v-else-if="!sending"
      class="min-h-0 flex-1"
      title="Send a request to see the response"
      :description="`Press ${sendShortcut} or click Send.`"
    >
      <template #icon><Zap :size="22" /></template>
    </EmptyState>
  </section>
</template>
