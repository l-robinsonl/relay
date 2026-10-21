<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref } from 'vue'
import {
  Columns2,
  Loader2,
  Moon,
  PanelLeft,
  Rows2,
  Sun,
  Terminal,
} from '@lucide/vue'
import type { UnlistenFn } from '@tauri-apps/api/event'
import AppSidebar from '@/features/workspace/AppSidebar.vue'
import CommandPalette, { type Command } from '@/features/command-palette/CommandPalette.vue'
import EmptyState from '@/components/ui/EmptyState.vue'
import ConflictPanel from '@/features/sync/ConflictPanel.vue'
import DialogHost from '@/components/ui/DialogHost.vue'
import ImportDialog from '@/features/import-export/ImportDialog.vue'
import RelayLogo from '@/components/ui/RelayLogo.vue'
import SyncIndicator from '@/features/sync/SyncIndicator.vue'
import SyncSettings from '@/features/sync/SyncSettings.vue'
import RequestPanel from '@/features/request/RequestPanel.vue'
import ResponsePanel from '@/features/response/ResponsePanel.vue'
import SplitPane from '@/components/ui/SplitPane.vue'
import TabBar from '@/features/request/TabBar.vue'
import ToastHost from '@/components/ui/ToastHost.vue'
import { api } from '@/services/ipc'
import { describeShortcut, useShortcuts, type Shortcut } from '@/composables/useShortcuts'
import { useSettingsStore } from '@/stores/settings'
import { useSyncStore } from '@/stores/sync'
import { useTabsStore } from '@/stores/tabs'
import { useToastStore } from '@/stores/toast'
import { useWorkspaceStore } from '@/stores/workspace'

const workspace = useWorkspaceStore()
const tabs = useTabsStore()
const settings = useSettingsStore()
const sync = useSyncStore()
const toasts = useToastStore()

const paletteOpen = ref(false)
const importOpen = ref(false)
const syncSettingsOpen = ref(false)
const conflictsOpen = ref(false)
let unlistenProgress: UnlistenFn | undefined
let unlistenSync: UnlistenFn | undefined

/** Pastes a cURL command from the clipboard into a new tab. */
async function importCurlFromClipboard () {
  if (!workspace.current) return
  try {
    const text = await navigator.clipboard.readText()
    if (!text.trim()) {
      toasts.warn('The clipboard is empty')
      return
    }
    const doc = await api.importCurl(text, workspace.current.id)
    const tab = tabs.openDraft()
    if (tab) {
      tabs.update(tab.id, doc)
      toasts.success(`Imported ${doc.method} ${doc.name}`, doc.description || undefined)
    }
  } catch (error) {
    toasts.error('That does not look like a cURL command', error)
  }
}

/** Copies the active request to the clipboard as a cURL command. */
async function copyAsCurl () {
  const tab = tabs.activeTab
  if (!tab) return
  try {
    const command = await api.exportCurl(tab.doc, workspace.activeEnvironmentId)
    await navigator.clipboard.writeText(command)
    toasts.success('Copied as cURL')
  } catch (error) {
    toasts.error('Could not build the cURL command', error)
  }
}

/** Writes the whole workspace to a file the user chooses. */
async function exportWorkspace () {
  if (!workspace.current) return
  try {
    const { save } = await import('@tauri-apps/plugin-dialog')
    const path = await save({
      defaultPath: `${workspace.current.name.replace(/[^a-z0-9]+/gi, '-').toLowerCase()}.relay.json`,
      filters: [{ name: 'Relay workspace export', extensions: ['json'] }],
    })
    if (!path) return

    await api.exportWorkspaceToPath(workspace.current.id, path)
    toasts.success('Workspace exported', 'Secret values were not included.')
  } catch (error) {
    toasts.error('Could not export the workspace', error)
  }
}

const shortcuts = computed<Shortcut[]>(() => [
  {
    key: 'k',
    mod: true,
    allowInInput: true,
    description: 'Open the command palette',
    handler: () => (paletteOpen.value = !paletteOpen.value),
  },
  {
    key: 'Enter',
    mod: true,
    allowInInput: true,
    description: 'Send the request',
    handler: () => tabs.activeTab && tabs.send(tabs.activeTab.id),
  },
  {
    key: 's',
    mod: true,
    allowInInput: true,
    description: 'Save the request',
    handler: () => tabs.activeTab && tabs.save(tabs.activeTab.id),
  },
  {
    key: 'n',
    mod: true,
    allowInInput: true,
    description: 'New request',
    handler: () => tabs.openDraft(),
  },
  {
    key: 'w',
    mod: true,
    allowInInput: true,
    description: 'Close the tab',
    handler: () => tabs.activeTab && tabs.close(tabs.activeTab.id),
  },
  {
    key: 'b',
    mod: true,
    allowInInput: true,
    description: 'Toggle the sidebar',
    handler: () => settings.toggleSidebar(),
  },
  {
    key: 'v',
    mod: true,
    shift: true,
    allowInInput: true,
    description: 'Import a cURL command from the clipboard',
    handler: importCurlFromClipboard,
  },
  {
    key: 'c',
    mod: true,
    shift: true,
    allowInInput: true,
    description: 'Copy the request as cURL',
    handler: copyAsCurl,
  },
  {
    key: 'i',
    mod: true,
    shift: true,
    allowInInput: true,
    description: 'Import a collection or workspace',
    handler: () => (importOpen.value = true),
  },
  {
    key: 'e',
    mod: true,
    shift: true,
    allowInInput: true,
    description: 'Export this workspace',
    handler: exportWorkspace,
  },
  {
    key: 'r',
    mod: true,
    shift: true,
    allowInInput: true,
    description: 'Sync this workspace now',
    handler: () => void sync.syncNow(),
  },
  {
    key: ',',
    mod: true,
    allowInInput: true,
    description: 'Synchronisation settings',
    handler: () => (syncSettingsOpen.value = true),
  },
])

useShortcuts(() => shortcuts.value)

/** Palette entries: every open request, plus the commands. */
const commands = computed<Command[]>(() => {
  const requests: Command[] = workspace.tree
    .filter((node) => node.kind === 'request')
    .map((node) => ({
      id: `request:${node.id}`,
      label: node.name,
      hint: node.url,
      group: 'Requests',
      method: node.method,
      run: async () => {
        await tabs.openRequest(node.id)
      },
    }))

  const actions: Command[] = shortcuts.value
    .filter((shortcut) => shortcut.key !== 'k')
    .map((shortcut) => ({
      id: `command:${shortcut.description}`,
      label: shortcut.description,
      group: 'Commands',
      shortcut: describeShortcut(shortcut),
      run: () => shortcut.handler(new KeyboardEvent('keydown')),
    }))

  const themeCommands: Command[] = [
    {
      id: 'theme:toggle',
      label: `Switch to the ${settings.theme === 'dark' ? 'light' : 'dark'} theme`,
      group: 'Appearance',
      run: () => settings.toggleTheme(),
    },
    {
      id: 'layout:toggle',
      label: `Arrange panes ${settings.orientation === 'horizontal' ? 'vertically' : 'horizontally'}`,
      group: 'Appearance',
      run: () => settings.toggleOrientation(),
    },
  ]

  return [...requests, ...actions, ...themeCommands]
})

onMounted(async () => {
  await settings.load()
  await workspace.initialise()
  unlistenProgress = await tabs.watchProgress()

  await sync.refresh()
  unlistenSync = await sync.listenForChanges()
  await sync.watch()

  // Open something rather than greeting the user with a blank editor.
  if (!tabs.tabs.length) tabs.openDraft()
})

onBeforeUnmount(() => {
  unlistenProgress?.()
  unlistenSync?.()
  void sync.unwatch()
})
</script>

<template>
  <div class="flex h-full flex-col surface-base">
    <!-- Title bar -->
    <header
      class="flex h-9 shrink-0 items-center gap-2 border-b border-subtle px-2 surface-sunken"
      data-tauri-drag-region
    >
      <button
        type="button"
        class="btn btn-ghost p-1.5"
        :aria-pressed="settings.sidebarVisible"
        :title="`Toggle sidebar (${describeShortcut({ key: 'b', mod: true })})`"
        @click="settings.toggleSidebar()"
      >
        <PanelLeft :size="14" />
      </button>

      <RelayLogo :size="15" wordmark title="Relay" />

      <button
        type="button"
        class="btn btn-ghost ml-2 gap-2 px-2 py-1 text-xs text-muted"
        @click="paletteOpen = true"
      >
        <Terminal :size="12" />
        <span>Search</span>
        <kbd class="kbd">{{ describeShortcut({ key: 'K', mod: true }) }}</kbd>
      </button>

      <div class="ml-auto flex items-center gap-1">
        <SyncIndicator
          @open-settings="syncSettingsOpen = true"
          @open-conflicts="conflictsOpen = true"
        />
        <button
          type="button"
          class="btn btn-ghost p-1.5"
          :title="`Arrange panes ${settings.orientation === 'horizontal' ? 'vertically' : 'horizontally'}`"
          @click="settings.toggleOrientation()"
        >
          <component :is="settings.orientation === 'horizontal' ? Columns2 : Rows2" :size="14" />
        </button>
        <button
          type="button"
          class="btn btn-ghost p-1.5"
          :title="`Switch to the ${settings.theme === 'dark' ? 'light' : 'dark'} theme`"
          @click="settings.toggleTheme()"
        >
          <component :is="settings.theme === 'dark' ? Sun : Moon" :size="14" />
        </button>
      </div>
    </header>

    <!-- Body -->
    <div class="flex min-h-0 flex-1">
      <!-- overflow-hidden matters: the sidebar has a fixed pixel width,
           and anything inside it that refuses to shrink would otherwise
           spill into the editor, which paints over it. -->
      <div
        v-if="settings.sidebarVisible"
        class="min-w-0 shrink-0 overflow-hidden border-r border-subtle"
        :style="{ width: `${settings.sidebarWidth}px` }"
      >
        <AppSidebar />
      </div>

      <main class="flex min-w-0 flex-1 flex-col">
        <div
          v-if="workspace.loading"
          class="flex flex-1 items-center justify-center gap-2 text-sm text-muted"
        >
          <Loader2 :size="16" class="animate-spin" />
          Opening your workspace…
        </div>

        <template v-else-if="tabs.tabs.length">
          <TabBar />
          <SplitPane
            v-if="tabs.activeTab"
            v-model="settings.responseSize"
            class="min-h-0 flex-1"
            :orientation="settings.orientation"
          >
            <template #first>
              <RequestPanel :key="tabs.activeTab.id" :tab="tabs.activeTab" />
            </template>
            <template #second>
              <ResponsePanel :key="tabs.activeTab.id" :tab="tabs.activeTab" />
            </template>
          </SplitPane>
        </template>

        <EmptyState
          v-else
          class="flex-1"
          title="No request open"
          description="Create a request, or press Ctrl+K to search."
        >
          <template #icon>
            <RelayLogo :size="34" boxed class="opacity-90" />
          </template>
          <template #action>
            <button type="button" class="btn btn-primary text-xs" @click="tabs.openDraft()">
              New request
            </button>
          </template>
        </EmptyState>
      </main>
    </div>

    <CommandPalette
      :open="paletteOpen"
      :commands="commands"
      @close="paletteOpen = false"
    />
    <ImportDialog :open="importOpen" @close="importOpen = false" />
    <SyncSettings :open="syncSettingsOpen" @close="syncSettingsOpen = false" />
    <ConflictPanel :open="conflictsOpen" @close="conflictsOpen = false" />
    <DialogHost />
    <ToastHost />
  </div>
</template>
