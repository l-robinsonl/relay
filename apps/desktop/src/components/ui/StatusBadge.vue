<script setup lang="ts">
import { computed } from 'vue'
import { statusTone } from '@/composables/useFormat'

const props = defineProps<{
  status: number
  statusText?: string
}>()

const tone = computed(() => statusTone(props.status))

const colors: Record<ReturnType<typeof statusTone>, { fg: string; bg: string }> = {
  success: { fg: 'var(--success)', bg: 'var(--success-muted)' },
  info: { fg: 'var(--info)', bg: 'var(--accent-muted)' },
  warning: { fg: 'var(--warning)', bg: 'var(--warning-muted)' },
  danger: { fg: 'var(--danger)', bg: 'var(--danger-muted)' },
}
</script>

<template>
  <span
    class="badge gap-1.5 px-2 py-0.5 font-mono tabular-nums"
    :style="{ color: colors[tone].fg, backgroundColor: colors[tone].bg }"
  >
    <span>{{ status }}</span>
    <span v-if="statusText" class="font-sans font-medium opacity-80">{{ statusText }}</span>
  </span>
</template>
