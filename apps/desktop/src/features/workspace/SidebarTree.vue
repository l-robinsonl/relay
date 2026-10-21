<script setup lang="ts">
import { computed } from 'vue'
import { ChevronRight, FileText, Folder, FolderOpen } from '@lucide/vue'
import type { TreeNode } from '@relay/shared-types'
import MethodBadge from '@/components/ui/MethodBadge.vue'
import {
  dropPositionFor,
  isInvalidDrop,
  orderBetween,
  type DragState,
} from '@/composables/useDragDrop'
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
    drag: DragState
  }>(),
  { depth: 0, selectedRequestId: null },
)

const emit = defineEmits<{
  openRequest: [id: string]
  contextMenu: [node: TreeNode, event: MouseEvent]
  dragStart: [node: TreeNode]
  dragOver: [nodeId: string, position: 'before' | 'after' | 'inside']
  dragEnd: []
  move: [node: TreeNode, parentId: string | null, sortOrder: number]
}>()

const workspace = useWorkspaceStore()

const nodes = computed<TreeNode[]>(() => workspace.nodesByParent.get(props.parentId ?? '') ?? [])

// Indentation is applied as padding on the row rather than a nested wrapper,
// so the hover and selection background still spans the full sidebar width.
const indent = computed(() => `${0.5 + props.depth * 0.75}rem`)

function onDragOver (node: TreeNode, event: DragEvent) {
  const dragging = props.drag.dragging
  if (!dragging) return
  if (isInvalidDrop(dragging, node, workspace.isAncestor)) return

  event.preventDefault()
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  emit('dragOver', node.id, dropPositionFor(node, event.clientY - rect.top, rect.height))
}

/** Resolves a drop into a concrete parent and sort order. */
function onDrop (target: TreeNode) {
  const dragging = props.drag.dragging
  const position = props.drag.position
  if (!dragging || !position) return
  if (isInvalidDrop(dragging, target, workspace.isAncestor)) {
    emit('dragEnd')
    return
  }

  if (position === 'inside' && target.kind === 'folder') {
    // Append inside the folder, after whatever is already there.
    const children = workspace.nodesByParent.get(target.id) ?? []
    const last = children.at(-1)?.sortOrder ?? null
    emit('move', dragging, target.id, orderBetween(last, null))
    emit('dragEnd')
    return
  }

  // Place next to the target, among its siblings.
  const siblings = nodes.value
  const index = siblings.findIndex((n) => n.id === target.id)
  const previous = position === 'before' ? siblings[index - 1] : siblings[index]
  const next = position === 'before' ? siblings[index] : siblings[index + 1]

  emit(
    'move',
    dragging,
    props.parentId,
    orderBetween(previous?.sortOrder ?? null, next?.sortOrder ?? null),
  )
  emit('dragEnd')
}

/** The drop indicator to draw on a row, if any. */
function indicator (nodeId: string) {
  if (props.drag.overId !== nodeId) return null
  return props.drag.position
}
</script>

<template>
  <ul role="group" class="contents">
    <li v-for="node in nodes" :key="node.id">
      <template v-if="node.kind === 'folder'">
        <button
          type="button"
          class="row group relative"
          :class="indicator(node.id) === 'inside' && 'ring-1 ring-[var(--accent)]'"
          :style="{ paddingInlineStart: indent }"
          :aria-expanded="workspace.isExpanded(node.id)"
          draggable="true"
          @click="workspace.toggleFolder(node.id)"
          @contextmenu.prevent="emit('contextMenu', node, $event)"
          @dragstart="emit('dragStart', node)"
          @dragover="onDragOver(node, $event)"
          @drop.prevent="onDrop(node)"
          @dragend="emit('dragEnd')"
        >
          <span
            v-if="indicator(node.id) === 'before' || indicator(node.id) === 'after'"
            class="pointer-events-none absolute inset-x-0 h-0.5"
            :class="indicator(node.id) === 'before' ? 'top-0' : 'bottom-0'"
            :style="{ backgroundColor: 'var(--accent)' }"
          />
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
          :drag="drag"
          @open-request="emit('openRequest', $event)"
          @context-menu="(n, e) => emit('contextMenu', n, e)"
          @drag-start="emit('dragStart', $event)"
          @drag-over="(id, pos) => emit('dragOver', id, pos)"
          @drag-end="emit('dragEnd')"
          @move="(n, p, s) => emit('move', n, p, s)"
        />
      </template>

      <button
        v-else
        type="button"
        class="row group relative"
        :class="selectedRequestId === node.id && 'is-selected'"
        :style="{ paddingInlineStart: `calc(${indent} + 0.8125rem)` }"
        :aria-selected="selectedRequestId === node.id"
        draggable="true"
        @click="emit('openRequest', node.id)"
        @contextmenu.prevent="emit('contextMenu', node, $event)"
        @dragstart="emit('dragStart', node)"
        @dragover="onDragOver(node, $event)"
        @drop.prevent="onDrop(node)"
        @dragend="emit('dragEnd')"
      >
        <span
          v-if="indicator(node.id)"
          class="pointer-events-none absolute inset-x-0 h-0.5"
          :class="indicator(node.id) === 'before' ? 'top-0' : 'bottom-0'"
          :style="{ backgroundColor: 'var(--accent)' }"
        />
        <FileText :size="13" class="shrink-0 text-muted opacity-0 group-hover:opacity-100" />
        <MethodBadge :method="node.method" class="w-10 shrink-0 text-right" />
        <span class="truncate text-sm">{{ node.name }}</span>
      </button>
    </li>
  </ul>
</template>
