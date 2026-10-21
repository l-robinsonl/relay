<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { Component } from 'vue'

/** One entry in a context menu. */
export interface MenuItem {
  id: string
  label: string
  icon?: Component
  shortcut?: string
  /** Styles the item as destructive and moves it below a divider. */
  destructive?: boolean
  disabled?: boolean
  run: () => void | Promise<void>
}

const props = defineProps<{
  /** Where it was opened, in viewport coordinates. */
  position: { x: number; y: number } | null
  items: MenuItem[]
}>()

const emit = defineEmits<{ close: [] }>()

const menu = ref<HTMLElement | null>(null)
const active = ref(0)
const placement = ref({ left: 0, top: 0 })

const ordinary = computed(() => props.items.filter((i) => !i.destructive))
const destructive = computed(() => props.items.filter((i) => i.destructive))
const ordered = computed(() => [...ordinary.value, ...destructive.value])

/**
 * Keeps the menu on screen.
 *
 * Opened near the right or bottom edge, a menu placed naively at the
 * pointer is clipped — and the items most likely to be clipped are the
 * destructive ones at the bottom.
 */
async function place () {
  if (!props.position) return
  await nextTick()

  const element = menu.value
  if (!element) return

  const { width, height } = element.getBoundingClientRect()
  const margin = 8

  placement.value = {
    left: Math.min(props.position.x, window.innerWidth - width - margin),
    top: Math.min(props.position.y, window.innerHeight - height - margin),
  }
}

watch(
  () => props.position,
  async (value) => {
    if (!value) return
    active.value = 0
    await place()
    menu.value?.focus()
  },
)

function onKeydown (event: KeyboardEvent) {
  if (!props.position) return

  const count = ordered.value.length
  if (event.key === 'Escape') {
    event.preventDefault()
    emit('close')
  } else if (event.key === 'ArrowDown') {
    event.preventDefault()
    active.value = (active.value + 1) % count
  } else if (event.key === 'ArrowUp') {
    event.preventDefault()
    active.value = (active.value - 1 + count) % count
  } else if (event.key === 'Enter') {
    event.preventDefault()
    void choose(ordered.value[active.value])
  }
}

async function choose (item: MenuItem | undefined) {
  if (!item || item.disabled) return
  emit('close')
  await item.run()
}

// A click anywhere else, or a scroll, dismisses it — the behaviour every
// context menu has, and its absence is immediately noticeable.
function onPointerDown (event: PointerEvent) {
  if (menu.value && !menu.value.contains(event.target as Node)) emit('close')
}

onMounted(() => {
  window.addEventListener('pointerdown', onPointerDown, true)
  window.addEventListener('resize', () => emit('close'))
})

onBeforeUnmount(() => {
  window.removeEventListener('pointerdown', onPointerDown, true)
})
</script>

<template>
  <Teleport to="body">
    <div
      v-if="position"
      ref="menu"
      role="menu"
      tabindex="-1"
      class="animate-in fixed z-[65] min-w-44 overflow-hidden rounded-md border
             border-default bg-[var(--surface-overlay)] py-1 outline-none"
      :style="{
        left: `${placement.left}px`,
        top: `${placement.top}px`,
        boxShadow: 'var(--shadow-overlay)',
      }"
      @keydown="onKeydown"
      @contextmenu.prevent
    >
      <button
        v-for="(item, index) in ordinary"
        :key="item.id"
        type="button"
        role="menuitem"
        class="row rounded-none px-3"
        :class="[
          ordered.indexOf(item) === active && 'is-selected',
          item.disabled && 'pointer-events-none opacity-40',
        ]"
        @click="choose(item)"
        @mousemove="active = index"
      >
        <component :is="item.icon" v-if="item.icon" :size="13" class="shrink-0" />
        <span class="text-sm">{{ item.label }}</span>
        <kbd v-if="item.shortcut" class="kbd ml-auto">{{ item.shortcut }}</kbd>
      </button>

      <div v-if="destructive.length && ordinary.length" class="my-1 border-t border-subtle" />

      <button
        v-for="item in destructive"
        :key="item.id"
        type="button"
        role="menuitem"
        class="row rounded-none px-3"
        :class="[
          ordered.indexOf(item) === active && 'is-selected',
          item.disabled && 'pointer-events-none opacity-40',
        ]"
        :style="{ color: 'var(--danger)' }"
        @click="choose(item)"
        @mousemove="active = ordered.indexOf(item)"
      >
        <component :is="item.icon" v-if="item.icon" :size="13" class="shrink-0" />
        <span class="text-sm">{{ item.label }}</span>
      </button>
    </div>
  </Teleport>
</template>
