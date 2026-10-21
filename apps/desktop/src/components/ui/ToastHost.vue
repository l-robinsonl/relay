<script setup lang="ts">
import { AlertTriangle, CheckCircle2, Info, XCircle, X } from '@lucide/vue'
import type { Toast } from '@/types/ui'
import { useToastStore } from '@/stores/toast'

const toasts = useToastStore()

const icons = {
  info: Info,
  success: CheckCircle2,
  warning: AlertTriangle,
  danger: XCircle,
} as const

const colors: Record<Toast['tone'], string> = {
  info: 'var(--info)',
  success: 'var(--success)',
  warning: 'var(--warning)',
  danger: 'var(--danger)',
}
</script>

<template>
  <div
    class="pointer-events-none fixed bottom-4 right-4 z-[60] flex w-80 flex-col gap-2"
    role="status"
    aria-live="polite"
  >
    <div
      v-for="toast in toasts.items"
      :key="toast.id"
      class="animate-slide-up pointer-events-auto flex items-start gap-2.5 rounded-md border
             border-default bg-[var(--surface-overlay)] p-3"
      :style="{ boxShadow: 'var(--shadow-overlay)' }"
    >
      <component
        :is="icons[toast.tone]"
        :size="15"
        class="mt-px shrink-0"
        :style="{ color: colors[toast.tone] }"
      />
      <div class="min-w-0 flex-1 space-y-0.5">
        <p class="text-xs font-medium text-primary">{{ toast.message }}</p>
        <p v-if="toast.detail" class="selectable break-words text-xs text-muted">
          {{ toast.detail }}
        </p>
      </div>
      <button
        type="button"
        class="shrink-0 rounded p-0.5 text-muted hover:text-primary"
        aria-label="Dismiss"
        @click="toasts.dismiss(toast.id)"
      >
        <X :size="12" />
      </button>
    </div>
  </div>
</template>
