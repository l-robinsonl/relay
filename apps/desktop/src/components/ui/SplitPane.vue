<script setup lang="ts">
import { computed, ref } from 'vue'

/**
 * Two panes with a draggable divider.
 *
 * The size is reported as a percentage so the layout survives a window
 * resize, and dragging uses pointer capture so the divider keeps tracking
 * even when the cursor outruns it.
 */
const props = withDefaults(
  defineProps<{
    modelValue: number
    orientation?: 'horizontal' | 'vertical'
    min?: number
    max?: number
  }>(),
  { orientation: 'horizontal', min: 15, max: 85 },
)

const emit = defineEmits<{ 'update:modelValue': [number] }>()

const container = ref<HTMLDivElement | null>(null)
const dragging = ref(false)

const isHorizontal = computed(() => props.orientation === 'horizontal')

/** The second pane's size; the first takes the remainder. */
const secondSize = computed(() => `${100 - props.modelValue}%`)

function onPointerDown (event: PointerEvent) {
  dragging.value = true
  ;(event.target as HTMLElement).setPointerCapture(event.pointerId)
}

function onPointerMove (event: PointerEvent) {
  if (!dragging.value || !container.value) return
  const rect = container.value.getBoundingClientRect()

  const fraction = isHorizontal.value
    ? (event.clientX - rect.left) / rect.width
    : (event.clientY - rect.top) / rect.height

  const percent = Math.min(Math.max(fraction * 100, props.min), props.max)
  emit('update:modelValue', Math.round(percent * 10) / 10)
}

function onPointerUp (event: PointerEvent) {
  dragging.value = false
  ;(event.target as HTMLElement).releasePointerCapture(event.pointerId)
}

/** Keyboard resizing, so the divider is not mouse-only. */
function onKeydown (event: KeyboardEvent) {
  const decrease = isHorizontal.value ? 'ArrowLeft' : 'ArrowUp'
  const increase = isHorizontal.value ? 'ArrowRight' : 'ArrowDown'
  const step = event.shiftKey ? 10 : 2

  if (event.key === decrease) {
    event.preventDefault()
    emit('update:modelValue', Math.max(props.modelValue - step, props.min))
  } else if (event.key === increase) {
    event.preventDefault()
    emit('update:modelValue', Math.min(props.modelValue + step, props.max))
  }
}
</script>

<template>
  <div
    ref="container"
    class="flex min-h-0 min-w-0"
    :class="isHorizontal ? 'flex-row' : 'flex-col'"
  >
    <div class="min-h-0 min-w-0 flex-1">
      <slot name="first" />
    </div>

    <div
      role="separator"
      tabindex="0"
      :aria-orientation="isHorizontal ? 'vertical' : 'horizontal'"
      :aria-valuenow="Math.round(modelValue)"
      :aria-valuemin="min"
      :aria-valuemax="max"
      aria-label="Resize panes"
      class="group relative shrink-0 touch-none transition-colors"
      :class="[
        isHorizontal ? 'w-px cursor-col-resize' : 'h-px cursor-row-resize',
        dragging ? 'bg-[var(--accent)]' : 'bg-[var(--border-subtle)]',
      ]"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @keydown="onKeydown"
    >
      <!-- A 1px divider is almost impossible to grab, so the hit area is
           widened invisibly on both sides. -->
      <div
        class="absolute"
        :class="
          isHorizontal
            ? '-inset-x-1.5 inset-y-0'
            : '-inset-y-1.5 inset-x-0'
        "
      />
      <div
        class="absolute opacity-0 transition-opacity group-hover:opacity-100"
        :class="[
          isHorizontal ? '-inset-x-px inset-y-0' : '-inset-y-px inset-x-0',
        ]"
        :style="{ backgroundColor: 'var(--accent)' }"
      />
    </div>

    <div
      class="min-h-0 min-w-0"
      :style="isHorizontal ? { width: secondSize } : { height: secondSize }"
    >
      <slot name="second" />
    </div>
  </div>
</template>
