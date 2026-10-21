<script setup lang="ts">
/**
 * The placeholder shown wherever there is nothing yet.
 *
 * Every empty state names the thing that is missing and offers the action
 * that fixes it, rather than just saying "No data".
 */
defineProps<{
  title: string
  description?: string
}>()
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center gap-3 px-8 py-10 text-center">
    <div class="text-muted opacity-60">
      <slot name="icon" />
    </div>
    <div class="space-y-1">
      <p class="text-sm font-medium text-secondary">{{ title }}</p>
      <p v-if="description" class="max-w-xs text-xs text-muted">{{ description }}</p>
    </div>
    <div v-if="$slots.action" class="mt-1">
      <slot name="action" />
    </div>
  </div>
</template>
