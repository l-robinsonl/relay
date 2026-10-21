<script setup lang="ts">
/**
 * Relay's mark, and optionally its wordmark.
 *
 * The geometry matches `scripts/render-icon.py`, so the application icon
 * and anything drawn in the interface are the same shape rather than two
 * drifting approximations.
 *
 * SVG rather than the PNG the installer uses: it stays crisp at any size
 * and on any display, and the mark can take its colour from the theme
 * instead of being baked blue.
 */
withDefaults(
  defineProps<{
    /** Height of the mark, in pixels. */
    size?: number
    /** Draws the name beside the mark. */
    wordmark?: boolean
    /** Draws the rounded square behind it, as the app icon does. */
    boxed?: boolean
  }>(),
  { size: 18, wordmark: false, boxed: false },
)
</script>

<template>
  <!-- The mark takes the accent; the name takes ordinary text colour.
       Tinting both would make a title bar shout. -->
  <span
    class="inline-flex select-none items-center gap-1.5"
    :style="{ color: 'var(--accent)' }"
  >
    <svg
      :width="size"
      :height="size"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
      class="shrink-0"
    >
      <rect
        v-if="boxed"
        x="1.1"
        y="1.1"
        width="21.8"
        height="21.8"
        rx="5.7"
        fill="var(--accent)"
      />
      <!-- The trailing chevron carries the weight; the leading one is
           smaller and thinner, so the pair reads as a hand-off rather
           than as a symmetric pair of arrows. -->
      <g
        :stroke="boxed ? '#fff' : 'currentColor'"
        stroke-linecap="round"
        stroke-linejoin="round"
        fill="none"
      >
        <polyline points="7.08,7.32 11.76,12 7.08,16.68" stroke-width="2.55" />
        <polyline points="13.37,8.57 16.80,12 13.37,15.43" stroke-width="2.02" />
      </g>
    </svg>

    <span
      v-if="wordmark"
      class="text-sm font-semibold"
      :style="{ color: 'var(--text-primary)', letterSpacing: '-0.01em' }"
    >Relay</span>
  </span>
</template>
