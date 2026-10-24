<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { AlertTriangle, Save } from '@lucide/vue'
import type { Tab, RequestSection } from '@/types/ui'
import AuthEditor from '@/features/request/AuthEditor.vue'
import ScriptsEditor from '@/features/request/ScriptsEditor.vue'
import BodyEditor from '@/features/request/BodyEditor.vue'
import SettingsEditor from '@/features/request/SettingsEditor.vue'
import UrlBar from '@/features/request/UrlBar.vue'
import KeyValueEditor from '@/components/ui/KeyValueEditor.vue'
import { api } from '@/services/ipc'
import { useTabsStore } from '@/stores/tabs'
import { useWorkspaceStore } from '@/stores/workspace'

const props = defineProps<{ tab: Tab }>()

const tabs = useTabsStore()
const workspace = useWorkspaceStore()

const unresolved = ref<string[]>([])

const sections: { id: RequestSection; label: string; count?: () => number }[] = [
  { id: 'params', label: 'Params', count: () => props.tab.doc.queryParams.length },
  { id: 'headers', label: 'Headers', count: () => props.tab.doc.headers.length },
  { id: 'body', label: 'Body' },
  { id: 'auth', label: 'Auth' },
  { id: 'scripts', label: 'Scripts' },
  { id: 'settings', label: 'Settings' },
]

const dirty = computed(() => tabs.isDirty(props.tab))
const sending = computed(() => props.tab.executionId !== null)

const bodyIndicator = computed(() =>
  props.tab.doc.body.mode === 'none' ? '' : props.tab.doc.body.mode,
)

/**
 * Asks Rust which variables cannot be resolved, so the URL bar can mark them.
 *
 * Debounced, because it runs on every keystroke in the URL and resolution
 * touches the database to load the scope chain.
 */
let resolveTimer: ReturnType<typeof setTimeout> | undefined
watch(
  () => [props.tab.doc.url, props.tab.doc.collectionId, workspace.activeEnvironmentId],
  () => {
    clearTimeout(resolveTimer)
    resolveTimer = setTimeout(async () => {
      if (!props.tab.doc.url.includes('{{')) {
        unresolved.value = []
        return
      }
      try {
        const preview = await api.requestResolve(props.tab.doc, workspace.activeEnvironmentId)
        unresolved.value = preview.missingVariables
      } catch {
        // A preview failure must never block editing.
        unresolved.value = []
      }
    }, 250)
  },
  { immediate: true },
)
</script>

<template>
  <section class="flex h-full min-h-0 flex-col" aria-label="Request">
    <!-- Name and URL -->
    <div class="shrink-0 space-y-2 border-b border-subtle px-3 py-2.5">
      <div class="flex items-center gap-2">
        <input
          class="min-w-0 flex-1 border-0 bg-transparent px-0 py-0.5 text-sm font-medium
                 text-primary outline-none placeholder:text-muted"
          :value="tab.doc.name"
          placeholder="Request name"
          aria-label="Request name"
          @input="tabs.update(tab.id, { name: ($event.target as HTMLInputElement).value })"
        />
        <button
          type="button"
          class="btn btn-ghost shrink-0 py-1 text-xs"
          :disabled="!dirty"
          @click="tabs.save(tab.id)"
        >
          <Save :size="13" />
          {{ dirty ? 'Save' : 'Saved' }}
        </button>
      </div>

      <UrlBar
        :method="tab.doc.method"
        :url="tab.doc.url"
        :sending="sending"
        :unresolved="unresolved"
        @update:method="tabs.update(tab.id, { method: $event })"
        @update:url="tabs.update(tab.id, { url: $event })"
        @send="tabs.send(tab.id)"
        @cancel="tabs.cancel(tab.id)"
      />

      <p
        v-if="unresolved.length"
        class="flex items-center gap-1.5 text-xs"
        :style="{ color: 'var(--danger)' }"
      >
        <AlertTriangle :size="12" class="shrink-0" />
        <span>
          Unresolved:
          <code class="font-mono">{{ unresolved.join(', ') }}</code>
          — define {{ unresolved.length === 1 ? 'it' : 'them' }} in an environment,
          collection or workspace.
        </span>
      </p>
    </div>

    <!-- Section tabs -->
    <nav
      class="flex shrink-0 items-center gap-0.5 border-b border-subtle px-2"
      aria-label="Request sections"
      role="tablist"
    >
      <button
        v-for="section in sections"
        :key="section.id"
        type="button"
        role="tab"
        class="tab"
        :aria-selected="tab.activeSection === section.id"
        @click="tabs.setSection(tab.id, section.id)"
      >
        {{ section.label }}
        <span
          v-if="section.count?.()"
          class="ml-1 rounded-full px-1.5 text-xxs tabular-nums"
          :style="{ backgroundColor: 'var(--surface-active)', color: 'var(--text-muted)' }"
        >{{ section.count() }}</span>
        <span
          v-else-if="section.id === 'body' && bodyIndicator"
          class="ml-1 h-1.5 w-1.5 rounded-full align-middle"
          :style="{ backgroundColor: 'var(--accent)' }"
          :title="`Body: ${bodyIndicator}`"
        />
      </button>
    </nav>

    <!-- Section content -->
    <div class="min-h-0 flex-1 overflow-y-auto">
      <KeyValueEditor
        v-if="tab.activeSection === 'params'"
        :model-value="tab.doc.queryParams"
        key-placeholder="Parameter"
        @update:model-value="tabs.update(tab.id, { queryParams: $event })"
      />

      <KeyValueEditor
        v-else-if="tab.activeSection === 'headers'"
        :model-value="tab.doc.headers"
        key-placeholder="Header"
        @update:model-value="tabs.update(tab.id, { headers: $event })"
      />

      <BodyEditor
        v-else-if="tab.activeSection === 'body'"
        class="h-full"
        :model-value="tab.doc.body"
        :doc="tab.doc"
        :environment="workspace.activeEnvironmentId"
        @update:model-value="tabs.update(tab.id, { body: $event })"
      />

      <AuthEditor
        v-else-if="tab.activeSection === 'auth'"
        :model-value="tab.doc.auth"
        @update:model-value="tabs.update(tab.id, { auth: $event })"
      />

      <ScriptsEditor
        v-else-if="tab.activeSection === 'scripts'"
        :model-value="tab.doc.scripts"
        @update:model-value="tabs.update(tab.id, { scripts: $event })"
      />

      <SettingsEditor
        v-else
        :model-value="tab.doc.settings"
        @update:model-value="tabs.update(tab.id, { settings: $event })"
      />
    </div>
  </section>
</template>
