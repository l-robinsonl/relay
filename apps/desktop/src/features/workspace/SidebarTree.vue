<script setup lang="ts">
import { computed } from 'vue'
import { ChevronRight, FileText, Folder, FolderOpen } from '@lucide/vue'
import type { TreeNode } from '@relay/shared-types'
import MethodBadge from '@/components/ui/MethodBadge.vue'
import { useWorkspaceStore } from '@/stores/workspace'

/**
 * One level of the sidebar tree, rendered recursively.
 *
 * The tree arrives from Rust as a flat list grouped by parent, so each level
 * is a cheap lookup rather than a filter over everything.
 */
const props = withDefaults(
  defineProps<{
    parentId: string | null
    depth?: number
    selectedRequestId?: string | null
  }>(),
  { depth: 0, selectedRequestId: null },
)

const emit = defineEmits<{
  openRequest: [id: string]
  contextMenu: [node: TreeNode, event: MouseEvent]
}>()

const workspace = useWorkspaceStore()

const nodes = computed<TreeNode[]>(() => workspace.nodesByParent.get(props.parentId ?? '') ?? [])

// Indentation is applied as padding on the row rather than a nested wrapper,
// so the hover and selection background still spans the full sidebar width.
const indent = computed(() => `${0.5 + props.depth * 0.75}rem`)
</script>

<template>
  <ul role="group" class="contents">
    <li v-for="node in nodes" :key="node.id">
      <template v-if="node.kind === 'folder'">
        <button
          type="button"
          class="row group"
          :style="{ paddingInlineStart: indent }"
          :aria-expanded="workspace.isExpanded(node.id)"
          @click="workspace.toggleFolder(node.id)"
          @contextmenu.prevent="emit('contextMenu', node, $event)"
        >
          <ChevronRight
            :size="13"
            class="shrink-0 text-muted transition-transform duration-150"
            :class="workspace.isExpanded(node.id) && 'rotate-90'"
          />
          <component
            :is="workspace.isExpanded(node.id) ? FolderOpen : Folder"
            :size="14"
            class="shrink-0 text-muted"
          />
          <span class="truncate text-sm">{{ node.name }}</span>
          <span v-if="node.childCount" class="ml-auto shrink-0 text-xxs text-muted tabular-nums">
            {{ node.childCount }}
          </span>
        </button>

        <SidebarTree
          v-if="workspace.isExpanded(node.id)"
          :parent-id="node.id"
          :depth="depth + 1"
          :selected-request-id="selectedRequestId"
          @open-request="emit('openRequest', $event)"
          @context-menu="(n, e) => emit('contextMenu', n, e)"
        />
      </template>

      <button
        v-else
        type="button"
        class="row group"
        :class="selectedRequestId === node.id && 'is-selected'"
        :style="{ paddingInlineStart: `calc(${indent} + 0.8125rem)` }"
        :aria-selected="selectedRequestId === node.id"
        @click="emit('openRequest', node.id)"
        @contextmenu.prevent="emit('contextMenu', node, $event)"
      >
        <FileText :size="13" class="shrink-0 text-muted opacity-0 group-hover:opacity-100" />
        <MethodBadge :method="node.method" class="w-10 shrink-0 text-right" />
        <span class="truncate text-sm">{{ node.name }}</span>
      </button>
    </li>
  </ul>
</template>
