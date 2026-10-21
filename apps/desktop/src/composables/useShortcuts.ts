import { onBeforeUnmount, onMounted } from 'vue'

/** A keyboard shortcut binding. */
export interface Shortcut {
  /** The key as reported by `KeyboardEvent.key`, compared case-insensitively. */
  key: string
  /** Requires Ctrl on Windows and Linux, Cmd on macOS. */
  mod?: boolean
  shift?: boolean
  alt?: boolean
  /** What it does. */
  handler: (event: KeyboardEvent) => void
  /** Fire even while a text field has focus. Off by default. */
  allowInInput?: boolean
  /** Shown in the shortcuts list and the command palette. */
  description: string
}

/** True when the platform uses Cmd rather than Ctrl. */
export const isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '')

/** Renders a shortcut the way the current platform writes it. */
export function describeShortcut (shortcut: Pick<Shortcut, 'key' | 'mod' | 'shift' | 'alt'>) {
  const parts: string[] = []
  if (shortcut.mod) parts.push(isMac ? '⌘' : 'Ctrl')
  if (shortcut.shift) parts.push(isMac ? '⇧' : 'Shift')
  if (shortcut.alt) parts.push(isMac ? '⌥' : 'Alt')
  parts.push(shortcut.key.length === 1 ? shortcut.key.toUpperCase() : shortcut.key)
  return parts.join(isMac ? '' : '+')
}

function isTextEntry (target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  if (target.isContentEditable) return true
  const tag = target.tagName
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT'
}

/** True when the event matches the binding. */
export function matches (event: KeyboardEvent, shortcut: Shortcut): boolean {
  if (event.key.toLowerCase() !== shortcut.key.toLowerCase()) return false
  const mod = isMac ? event.metaKey : event.ctrlKey
  if (Boolean(shortcut.mod) !== mod) return false
  if (Boolean(shortcut.shift) !== event.shiftKey) return false
  if (Boolean(shortcut.alt) !== event.altKey) return false
  // A shortcut without a modifier must never steal a keystroke from a field.
  if (!shortcut.allowInInput && isTextEntry(event.target)) return false
  return true
}

/** Registers shortcuts for the lifetime of the calling component. */
export function useShortcuts (shortcuts: () => Shortcut[]) {
  function onKeydown (event: KeyboardEvent) {
    for (const shortcut of shortcuts()) {
      if (matches(event, shortcut)) {
        event.preventDefault()
        shortcut.handler(event)
        return
      }
    }
  }

  onMounted(() => window.addEventListener('keydown', onKeydown))
  onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))
}
