import { defineStore } from 'pinia'
import { ref } from 'vue'

/**
 * In-app dialogs.
 *
 * `window.prompt` and `window.confirm` work, but they render the
 * platform's own dialog — complete with a "JavaScript —
 * http://localhost:1420" title bar — which looks like a web page pretending
 * to be an application. They also block the whole webview.
 *
 * The API is promise-based so call sites read exactly as they did before:
 * `const name = await dialogs.prompt({ ... })`, null when cancelled.
 */

/** What to ask for in a text prompt. */
export interface PromptOptions {
  title: string
  /** Shown above the field. */
  label?: string
  /** Extra context below the field. */
  description?: string
  value?: string
  placeholder?: string
  confirmLabel?: string
}

/** What to confirm. */
export interface ConfirmOptions {
  title: string
  description?: string
  confirmLabel?: string
  /** Styles the confirm button as destructive. */
  destructive?: boolean
}

type Pending =
  | { kind: 'prompt'; options: PromptOptions; resolve: (value: string | null) => void }
  | { kind: 'confirm'; options: ConfirmOptions; resolve: (value: boolean) => void }

export const useDialogStore = defineStore('dialogs', () => {
  const pending = ref<Pending | null>(null)

  /** Asks for a line of text. Resolves to null if cancelled. */
  function prompt (options: PromptOptions): Promise<string | null> {
    return new Promise((resolve) => {
      // Only one dialog at a time; a second request cancels the first
      // rather than stacking modals on top of each other.
      dismiss()
      pending.value = { kind: 'prompt', options, resolve }
    })
  }

  /** Asks a yes/no question. */
  function confirm (options: ConfirmOptions): Promise<boolean> {
    return new Promise((resolve) => {
      dismiss()
      pending.value = { kind: 'confirm', options, resolve }
    })
  }

  /** Resolves the open dialog with the user's answer. */
  function submit (value: string | null) {
    const current = pending.value
    pending.value = null
    if (!current) return

    if (current.kind === 'prompt') current.resolve(value)
    else current.resolve(value !== null)
  }

  /** Cancels the open dialog, if any. */
  function dismiss () {
    const current = pending.value
    pending.value = null
    if (!current) return

    if (current.kind === 'prompt') current.resolve(null)
    else current.resolve(false)
  }

  return { pending, prompt, confirm, submit, dismiss }
})
