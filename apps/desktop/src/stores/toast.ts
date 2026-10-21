import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { Toast } from '@/types/ui'
import { isRelayError } from '@/services/ipc'

/** How long a toast stays on screen, by tone. */
const DISMISS_AFTER: Record<Toast['tone'], number> = {
  info: 3500,
  success: 2500,
  // Failures stay longer: they usually carry something worth reading.
  warning: 6000,
  danger: 8000,
}

/** Transient messages. */
export const useToastStore = defineStore('toast', () => {
  const items = ref<Toast[]>([])
  let nextId = 1

  function push (tone: Toast['tone'], message: string, detail?: string) {
    const id = nextId++
    const toast: Toast = detail ? { id, tone, message, detail } : { id, tone, message }
    items.value = [...items.value, toast]

    setTimeout(() => dismiss(id), DISMISS_AFTER[tone])
    return id
  }

  function dismiss (id: number) {
    items.value = items.value.filter((t) => t.id !== id)
  }

  const info = (message: string, detail?: string) => push('info', message, detail)
  const success = (message: string, detail?: string) => push('success', message, detail)
  const warn = (message: string, detail?: string) => push('warning', message, detail)

  /**
   * Reports a failure. Accepts a caught value of any shape so call sites do
   * not each have to work out what they caught.
   */
  function error (message: string, cause?: unknown) {
    if (cause === undefined) return push('danger', message)
    if (isRelayError(cause)) {
      return push('danger', message, cause.detail ?? cause.message)
    }
    return push('danger', message, cause instanceof Error ? cause.message : String(cause))
  }

  return { items, push, dismiss, info, success, warn, error }
})
