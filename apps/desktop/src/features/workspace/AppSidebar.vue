<script setup lang="ts">
import { computed, ref } from 'vue'
import {
  ChevronDown,
  Clock,
  Copy,
  FilePlus,
  FolderPlus,
  Layers,
  Pencil,
  Plus,
  Search,
  Settings2,
  Terminal,
  Trash2,
  X,
} from '@lucide/vue'
import type { TreeNode } from '@relay/shared-types'
import { api } from '@/services/ipc'
import EmptyState from '@/components/ui/EmptyState.vue'
import MethodBadge from '@/components/ui/MethodBadge.vue'
import ContextMenu, { type MenuItem } from '@/components/ui/ContextMenu.vue'
import WorkspaceSettings from '@/features/workspace/WorkspaceSettings.vue'
import SidebarTree from '@/features/workspace/SidebarTree.vue'
import { useDragDrop } from '@/composables/useDragDrop'
import EnvironmentList from '@/features/environment/EnvironmentList.vue'
import HistoryList from '@/features/history/HistoryList.vue'
import type { SidebarView } from '@/types/ui'
import { useDialogStore } from '@/stores/dialogs'
import { useToastStore } from '@/stores/toast'
import { useTabsStore } from '@/stores/tabs'
import { useWorkspaceStore } from '@/stores/workspace'

const workspace = useWorkspaceStore()
const tabs = useTabsStore()
const dialogs = useDialogStore()
const toasts = useToastStore()

const drag = useDragDrop()
const view = ref<SidebarView>('collections')
const search = ref('')
const workspaceMenuOpen = ref(false)
const workspaceSettingsOpen = ref(false)

const selectedRequestId = computed(() => tabs.activeTab?.requestId ?? null)

/**
 * Search results, flattened.
 *
 * While searching the tree collapses to a flat list of matches: hierarchy is
 * not useful when you already know what you are looking for, and keeping it
 * would hide matches inside collapsed folders.
 */
const searchResults = computed<TreeNode[]>(() => {
  const term = search.value.trim().toLowerCase()
  if (!term) return []
  return workspace.tree
    .filter((node) => {
      if (node.kind !== 'request') return false
      return (
        node.name.toLowerCase().includes(term) || node.url.toLowerCase().includes(term)
      )
    })
    .slice(0, 100)
})

const views: { id: SidebarView; label: string; icon: typeof Layers }[] = [
  { id: 'collections', label: 'Collections', icon: Layers },
  { id: 'environments', label: 'Environments', icon: Settings2 },
  { id: 'history', label: 'History', icon: Clock },
]

async function newRequest () {
  tabs.openDraft()
}

async function newFolder (parentId: string | null = null) {
  const name = await dialogs.prompt({
    title: parentId ? 'New folder inside' : 'New folder',
    label: 'Name',
    placeholder: 'Users',
  })
  if (name) await workspace.createFolder(name, parentId)
}

async function renameWorkspace () {
  workspaceMenuOpen.value = false
  if (!workspace.current) return

  const name = await dialogs.prompt({
    title: 'Rename workspace',
    label: 'Name',
    value: workspace.current.name,
    confirmLabel: 'Rename',
  })
  if (name) await workspace.renameWorkspace(name)
}

async function deleteWorkspace () {
  workspaceMenuOpen.value = false
  const current = workspace.current
  if (!current) return

  const confirmed = await dialogs.confirm({
    title: `Delete "${current.name}"?`,
    description:
      'Every collection, request, environment and history entry in it goes too. Export it first if you might want it back.',
    confirmLabel: 'Delete workspace',
    destructive: true,
  })
  if (confirmed) await workspace.deleteWorkspace(current.id)
}

async function switchWorkspace (id: string) {
  workspaceMenuOpen.value = false
  await workspace.select(id)
}

async function newWorkspace () {
  workspaceMenuOpen.value = false
  const name = await dialogs.prompt({
    title: 'New workspace',
    label: 'Name',
    description: 'A workspace holds its own collections, environments and history.',
    placeholder: 'Work APIs',
  })
  if (name) await workspace.createWorkspace(name)
}

const menuPosition = ref<{ x: number; y: number } | null>(null)
const menuNode = ref<TreeNode | null>(null)

/** The actions available for whatever was right-clicked. */
const menuItems = computed<MenuItem[]>(() => {
  const node = menuNode.value
  if (!node) return []

  if (node.kind === 'folder') {
    return [
      {
        id: 'new-request',
        label: 'New request here',
        icon: FilePlus,
        run: () => void tabs.openDraft(node.id),
      },
      {
        id: 'new-folder',
        label: 'New folder here',
        icon: FolderPlus,
        run: () => newFolder(node.id),
      },
      { id: 'rename', label: 'Rename…', icon: Pencil, run: () => renameNode(node) },
      {
        id: 'delete',
        label: 'Delete folder',
        icon: Trash2,
        destructive: true,
        run: () => deleteNode(node),
      },
    ]
  }

  return [
    { id: 'open', label: 'Open', icon: FilePlus, run: () => void tabs.openRequest(node.id) },
    { id: 'rename', label: 'Rename…', icon: Pencil, run: () => renameNode(node) },
    { id: 'duplicate', label: 'Duplicate', icon: Copy, run: () => duplicate(node) },
    { id: 'curl', label: 'Copy as cURL', icon: Terminal, run: () => copyAsCurl(node) },
    {
      id: 'delete',
      label: 'Delete request',
      icon: Trash2,
      destructive: true,
      run: () => deleteNode(node),
    },
  ]
})

function handleContextMenu (node: TreeNode, event: MouseEvent) {
  menuNode.value = node
  menuPosition.value = { x: event.clientX, y: event.clientY }
}

function closeMenu () {
  menuPosition.value = null
  menuNode.value = null
}

async function renameNode (node: TreeNode) {
  const name = await dialogs.prompt({
    title: node.kind === 'folder' ? 'Rename folder' : 'Rename request',
    label: 'Name',
    value: node.name,
    confirmLabel: 'Rename',
  })
  if (!name || name === node.name) return

  if (node.kind === 'folder') await workspace.renameFolder(node.id, name)
  else await workspace.renameRequest(node.id, name)
}

async function duplicate (node: TreeNode) {
  const copy = await workspace.duplicateRequest(node.id)
  if (copy) await tabs.openRequest(copy.id)
}

async function copyAsCurl (node: TreeNode) {
  try {
    const doc = await api.requestGet(node.id)
    const command = await api.exportCurl(doc, workspace.activeEnvironmentId)
    await navigator.clipboard.writeText(command)
    toasts.success('Copied as cURL')
  } catch (error) {
    toasts.error('Could not build the cURL command', error)
  }
}

async function deleteNode (node: TreeNode) {
  if (node.kind === 'folder') {
    const confirmed = await dialogs.confirm({
      title: `Delete "${node.name}"?`,
      description:
        'Everything inside it goes too. If this workspace syncs, it is removed for everyone.',
      confirmLabel: 'Delete folder',
      destructive: true,
    })
    if (confirmed) await workspace.deleteFolder(node.id)
    return
  }

  const confirmed = await dialogs.confirm({
    title: `Delete "${node.name}"?`,
    confirmLabel: 'Delete request',
    destructive: true,
  })
  if (confirmed) {
    tabs.closeByRequestId(node.id)
    await workspace.deleteRequest(node.id)
  }
}
</script>

<template>
  <aside class="flex h-full min-w-0 flex-col surface-sunken">
    <!-- Workspace switcher -->
    <div class="relative shrink-0 border-b border-subtle px-2 py-2">
      <button
        type="button"
        class="row justify-between"
        :aria-expanded="workspaceMenuOpen"
        @click="workspaceMenuOpen = !workspaceMenuOpen"
      >
        <span class="truncate text-sm font-medium text-primary">
          {{ workspace.current?.name ?? 'Loading…' }}
        </span>
        <ChevronDown :size="14" class="shrink-0 text-muted" />
      </button>

      <div
        v-if="workspaceMenuOpen"
        class="animate-slide-up absolute inset-x-2 top-full z-30 mt-1 overflow-hidden
               rounded-md border border-default bg-[var(--surface-overlay)] py-1"
        :style="{ boxShadow: 'var(--shadow-overlay)' }"
      >
        <button
          v-for="item in workspace.workspaces"
          :key="item.id"
          type="button"
          class="row rounded-none"
          :class="item.id === workspace.current?.id && 'is-selected'"
          @click="switchWorkspace(item.id)"
        >
          <span class="truncate text-sm">{{ item.name }}</span>
        </button>
        <div class="my-1 border-t border-subtle" />
        <button type="button" class="row rounded-none" @click="newWorkspace">
          <Plus :size="13" />
          <span class="text-sm">New workspace</span>
        </button>
        <button
          type="button"
          class="row rounded-none"
          @click="workspaceMenuOpen = false; workspaceSettingsOpen = true"
        >
          <Settings2 :size="13" />
          <span class="text-sm">Workspace settings</span>
        </button>
        <button type="button" class="row rounded-none" @click="renameWorkspace">
          <Pencil :size="13" />
          <span class="text-sm">Rename this workspace</span>
        </button>
        <button
          type="button"
          class="row rounded-none"
          :style="{ color: 'var(--danger)' }"
          @click="deleteWorkspace"
        >
          <Trash2 :size="13" />
          <span class="text-sm">Delete this workspace</span>
        </button>
      </div>
    </div>

    <!-- View switcher -->
    <nav
      class="flex shrink-0 gap-0.5 border-b border-subtle px-2 py-1"
      aria-label="Sidebar sections"
    >
      <button
        v-for="item in views"
        :key="item.id"
        type="button"
        class="flex min-w-0 flex-1 items-center justify-center gap-1.5 rounded px-2
               py-1.5 text-xs font-medium transition-colors"
        :class="
          view === item.id
            ? 'bg-[var(--accent-muted)] text-primary'
            : 'text-muted hover:bg-[var(--surface-hover)] hover:text-secondary'
        "
        :aria-current="view === item.id ? 'page' : undefined"
        @click="view = item.id"
      >
        <component :is="item.icon" :size="13" class="shrink-0" />
        <span class="truncate">{{ item.label }}</span>
      </button>
    </nav>

    <!-- Collections -->
    <template v-if="view === 'collections'">
      <div class="shrink-0 space-y-2 px-2 py-2">
        <div class="relative">
          <Search
            :size="13"
            class="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-muted"
          />
          <input
            v-model="search"
            type="search"
            class="input py-1 pl-7 pr-7 text-xs"
            placeholder="Search requests"
            aria-label="Search requests"
          />
          <button
            v-if="search"
            type="button"
            class="absolute right-1.5 top-1/2 -translate-y-1/2 rounded p-0.5 text-muted
                   hover:text-primary"
            aria-label="Clear search"
            @click="search = ''"
          >
            <X :size="12" />
          </button>
        </div>

        <div class="flex gap-1">
          <button type="button" class="btn btn-ghost flex-1 py-1 text-xs" @click="newRequest">
            <Plus :size="13" />
            Request
          </button>
          <button type="button" class="btn btn-ghost flex-1 py-1 text-xs" @click="newFolder()">
            <FolderPlus :size="13" />
            Folder
          </button>
        </div>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        <!-- Search results -->
        <template v-if="search.trim()">
          <ul v-if="searchResults.length" role="list">
            <li v-for="node in searchResults" :key="node.id">
              <button
                type="button"
                class="row"
                :class="selectedRequestId === node.id && 'is-selected'"
                @click="tabs.openRequest(node.id)"
              >
                <MethodBadge
                  v-if="node.kind === 'request'"
                  :method="node.method"
                  class="w-10 shrink-0 text-right"
                />
                <span class="truncate text-sm">{{ node.name }}</span>
              </button>
            </li>
          </ul>
          <EmptyState
            v-else
            title="No matches"
            :description="`Nothing matches “${search}”.`"
          >
            <template #icon><Search :size="22" /></template>
          </EmptyState>
        </template>

        <!-- Tree -->
        <template v-else>
          <SidebarTree
            v-if="workspace.tree.length"
            :parent-id="null"
            :selected-request-id="selectedRequestId"
            :drag="drag.state.value"
            @open-request="tabs.openRequest"
            @context-menu="handleContextMenu"
            @drag-start="drag.start"
            @drag-over="drag.over"
            @drag-end="drag.clear"
            @move="workspace.moveNode"
          />
          <EmptyState
            v-else
            title="No requests yet"
            description="Create your first request, or paste a cURL command with Ctrl+Shift+V."
          >
            <template #icon><Layers :size="22" /></template>
            <template #action>
              <button type="button" class="btn btn-primary text-xs" @click="newRequest">
                <Plus :size="13" />
                New request
              </button>
            </template>
          </EmptyState>
        </template>
      </div>
    </template>

    <EnvironmentList v-else-if="view === 'environments'" class="min-h-0 flex-1" />
    <HistoryList v-else class="min-h-0 flex-1" />

    <ContextMenu :position="menuPosition" :items="menuItems" @close="closeMenu" />
    <WorkspaceSettings
      :open="workspaceSettingsOpen"
      @close="workspaceSettingsOpen = false"
    />

    <!-- Active environment -->
    <div class="shrink-0 border-t border-subtle px-2 py-2">
      <label class="sr-only" for="active-environment">Active environment</label>
      <select
        id="active-environment"
        class="input py-1 text-xs"
        :value="workspace.activeEnvironmentId ?? ''"
        @change="
          workspace.setActiveEnvironment(
            ($event.target as HTMLSelectElement).value || null,
          )
        "
      >
        <option value="">No environment</option>
        <option v-for="env in workspace.environments" :key="env.id" :value="env.id">
          {{ env.name }}
        </option>
      </select>
    </div>
  </aside>
</template>
