<script setup lang="ts">
import { computed } from 'vue'
import { methodColor, methodLabel } from '@/composables/useFormat'

const props = withDefaults(
  defineProps<{
    method: string
    size?: 'sm' | 'md'
  }>(),
  { size: 'sm' },
)

const color = computed(() => methodColor(props.method))
const label = computed(() => methodLabel(props.method))
</script>

<template>
  <span
    class="font-mono font-semibold tracking-tight tabular-nums"
    :class="size === 'sm' ? 'text-[0.625rem]' : 'text-xs'"
    :style="{ color }"
    :title="method"
  >{{ label }}</span>
</template>
