<script setup lang="ts">
import { Plus, X } from '@lucide/vue'
import MethodBadge from '@/components/ui/MethodBadge.vue'
import { useTabsStore } from '@/stores/tabs'

const tabs = useTabsStore()
</script>

<template>
  <div class="flex h-9 shrink-0 items-stretch border-b border-subtle surface-sunken">
    <div class="flex min-w-0 flex-1 items-stretch overflow-x-auto" role="tablist">
      <div
        v-for="tab in tabs.tabs"
        :key="tab.id"
        role="tab"
        :aria-selected="tab.id === tabs.activeTabId"
        class="group relative flex max-w-52 shrink-0 items-center gap-1.5 border-r
               border-subtle px-3 text-xs transition-colors"
        :class="
          tab.id === tabs.activeTabId
            ? 'surface-base text-primary'
            : 'text-muted hover:bg-[var(--surface-hover)]'
        "
      >
        <!-- The active tab is marked with a top rule rather than a border, so
             switching tabs never shifts the row by a pixel. -->
        <span
          v-if="tab.id === tabs.activeTabId"
          class="absolute inset-x-0 top-0 h-0.5"
          :style="{ backgroundColor: 'var(--accent)' }"
        />

        <button
          type="button"
          class="flex min-w-0 items-center gap-1.5 py-2"
          @click="tabs.activate(tab.id)"
        >
          <MethodBadge :method="tab.doc.method" />
          <span class="truncate">{{ tab.doc.name || 'Untitled' }}</span>
        </button>

        <button
          type="button"
          class="shrink-0 rounded p-0.5 transition-opacity hover:bg-[var(--surface-active)]"
          :class="tabs.isDirty(tab) ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'"
          :aria-label="`Close ${tab.doc.name || 'tab'}`"
          @click.stop="tabs.close(tab.id)"
        >
          <!-- An unsaved tab shows a dot that becomes a close button on hover,
               so the state is visible without a second affordance. -->
          <span
            v-if="tabs.isDirty(tab)"
            class="block h-1.5 w-1.5 rounded-full group-hover:hidden"
            :style="{ backgroundColor: 'var(--accent)' }"
            title="Unsaved changes"
          />
          <X
            :size="12"
            :class="tabs.isDirty(tab) ? 'hidden group-hover:block' : 'block'"
          />
        </button>
      </div>
    </div>

    <button
      type="button"
      class="flex shrink-0 items-center px-3 text-muted transition-colors
             hover:bg-[var(--surface-hover)] hover:text-primary"
      aria-label="New request"
      @click="tabs.openDraft()"
    >
      <Plus :size="14" />
    </button>
  </div>
</template>
