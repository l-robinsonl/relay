<script setup lang="ts">
import { computed } from 'vue'
import { AlertTriangle, Check, Terminal, X } from '@lucide/vue'
import type { ScriptOutcome } from '@/services/ipc'
import EmptyState from '@/components/ui/EmptyState.vue'
import { formatDuration } from '@/composables/useFormat'

/**
 * What the post-response script reported.
 *
 * Failures come first and are expanded; a run where everything passed
 * needs a count and nothing more.
 */
const props = defineProps<{ outcome: ScriptOutcome | null }>()

const summary = computed(() => {
  const outcome = props.outcome
  if (!outcome) return null

  const assertions = [
    ...outcome.tests.flatMap((t) => t.assertions),
    ...outcome.looseAssertions,
  ]
  const failedTests = outcome.tests.filter(
    (t) => t.error || t.assertions.some((a) => !a.passed),
  ).length

  return {
    passed: assertions.filter((a) => a.passed).length,
    failed: assertions.filter((a) => !a.passed).length,
    failedTests,
    total: assertions.length,
  }
})

/** Failing groups first — they are what the user needs to read. */
const ordered = computed(() => {
  const tests = props.outcome?.tests ?? []
  const failed = tests.filter((t) => t.error || t.assertions.some((a) => !a.passed))
  const passed = tests.filter((t) => !failed.includes(t))
  return [...failed, ...passed]
})

function testFailed (test: { error?: string | null; assertions: { passed: boolean }[] }) {
  return Boolean(test.error) || test.assertions.some((a) => !a.passed)
}

const consoleColour = {
  log: 'var(--text-secondary)',
  warn: 'var(--warning)',
  error: 'var(--danger)',
} as const
</script>

<template>
  <EmptyState
    v-if="!outcome"
    title="No tests ran"
    description="Write assertions under the Scripts tab and they run after each response."
  >
    <template #icon><Check :size="22" /></template>
  </EmptyState>

  <div v-else class="h-full overflow-y-auto">
    <!-- Summary -->
    <div
      v-if="summary"
      class="flex items-center gap-3 border-b border-subtle px-3 py-2 text-xs"
    >
      <span
        v-if="summary.passed"
        class="flex items-center gap-1"
        :style="{ color: 'var(--success)' }"
      >
        <Check :size="13" />
        {{ summary.passed }} passed
      </span>
      <span
        v-if="summary.failed"
        class="flex items-center gap-1"
        :style="{ color: 'var(--danger)' }"
      >
        <X :size="13" />
        {{ summary.failed }} failed
      </span>
      <span v-if="!summary.total && !outcome.error" class="text-muted">
        The script ran but made no assertions.
      </span>
      <span class="ml-auto tabular-nums text-muted">
        {{ formatDuration(outcome.durationMs) }}
      </span>
    </div>

    <!-- The script itself failed -->
    <div
      v-if="outcome.error"
      class="m-3 rounded-md border p-3"
      :style="{ borderColor: 'var(--danger)', backgroundColor: 'var(--danger-muted)' }"
    >
      <p
        class="flex items-center gap-1.5 text-xs font-medium"
        :style="{ color: 'var(--danger)' }"
      >
        <AlertTriangle :size="13" />
        The script did not finish
      </p>
      <p class="selectable mt-1 font-mono text-xs text-secondary">{{ outcome.error }}</p>
    </div>

    <!-- Named groups -->
    <ul v-if="ordered.length" class="divide-y divide-[var(--border-subtle)]">
      <li v-for="(test, index) in ordered" :key="index" class="px-3 py-2">
        <div class="flex items-start gap-2">
          <component
            :is="testFailed(test) ? X : Check"
            :size="13"
            class="mt-0.5 shrink-0"
            :style="{ color: testFailed(test) ? 'var(--danger)' : 'var(--success)' }"
          />
          <div class="min-w-0 flex-1">
            <p class="text-xs font-medium text-primary">{{ test.name }}</p>

            <p
              v-if="test.error"
              class="selectable mt-1 font-mono text-xs"
              :style="{ color: 'var(--danger)' }"
            >
              {{ test.error }}
            </p>

            <!-- Only failures are listed; a passing assertion needs no
                 explanation, and listing them buries the one that matters. -->
            <ul v-if="testFailed(test)" class="mt-1 space-y-0.5">
              <li
                v-for="(assertion, i) in test.assertions.filter((a) => !a.passed)"
                :key="i"
                class="selectable font-mono text-xs"
                :style="{ color: 'var(--danger)' }"
              >
                {{ assertion.message }}
              </li>
            </ul>

            <p v-else-if="test.assertions.length" class="mt-0.5 text-xxs text-muted">
              {{ test.assertions.length }}
              assertion{{ test.assertions.length === 1 ? '' : 's' }}
            </p>
          </div>
        </div>
      </li>
    </ul>

    <!-- Assertions made outside any group -->
    <ul
      v-if="outcome.looseAssertions.length"
      class="divide-y divide-[var(--border-subtle)] border-t border-subtle"
    >
      <li
        v-for="(assertion, index) in outcome.looseAssertions"
        :key="index"
        class="flex items-start gap-2 px-3 py-1.5"
      >
        <component
          :is="assertion.passed ? Check : X"
          :size="13"
          class="mt-0.5 shrink-0"
          :style="{ color: assertion.passed ? 'var(--success)' : 'var(--danger)' }"
        />
        <span
          class="selectable font-mono text-xs"
          :style="{ color: assertion.passed ? 'var(--text-secondary)' : 'var(--danger)' }"
        >
          {{ assertion.passed ? 'passed' : assertion.message }}
        </span>
      </li>
    </ul>

    <!-- Console -->
    <div v-if="outcome.console.length" class="border-t border-subtle">
      <p
        class="flex items-center gap-1.5 px-3 py-1.5 text-xxs font-medium uppercase
               tracking-wide text-muted"
      >
        <Terminal :size="12" />
        Console
      </p>
      <ul class="pb-2">
        <li
          v-for="(line, index) in outcome.console"
          :key="index"
          class="selectable whitespace-pre-wrap break-all px-3 py-0.5 font-mono text-xs"
          :style="{ color: consoleColour[line.level] }"
        >
          {{ line.text }}
        </li>
      </ul>
    </div>

    <!-- Variables a script captured -->
    <div
      v-if="Object.keys(outcome.variables).length"
      class="border-t border-subtle px-3 py-2"
    >
      <p class="mb-1 text-xxs font-medium uppercase tracking-wide text-muted">
        Variables set
      </p>
      <ul class="space-y-0.5">
        <li
          v-for="(value, key) in outcome.variables"
          :key="key"
          class="font-mono text-xs text-secondary"
        >
          <span class="text-primary">{{ key }}</span> = {{ value }}
        </li>
      </ul>
    </div>
  </div>
</template>
