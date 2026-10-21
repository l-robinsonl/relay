import { describe, expect, it } from 'vitest'
import { describeShortcut, matches, type Shortcut } from './useShortcuts'

function shortcut (over: Partial<Shortcut> = {}): Shortcut {
  return {
    key: 's',
    mod: true,
    description: 'Save',
    handler: () => {},
    ...over,
  }
}

function event (init: Partial<KeyboardEventInit> & { key: string }, target?: EventTarget) {
  const keyboardEvent = new KeyboardEvent('keydown', init)
  if (target) Object.defineProperty(keyboardEvent, 'target', { value: target })
  return keyboardEvent
}

describe('matches', () => {
  it('matches a modifier combination', () => {
    expect(matches(event({ key: 's', ctrlKey: true }), shortcut())).toBe(true)
  })

  it('is case-insensitive about the key', () => {
    expect(matches(event({ key: 'S', ctrlKey: true }), shortcut())).toBe(true)
  })

  it('rejects the key without its modifier', () => {
    expect(matches(event({ key: 's' }), shortcut())).toBe(false)
  })

  it('rejects an extra modifier that was not asked for', () => {
    expect(matches(event({ key: 's', ctrlKey: true, shiftKey: true }), shortcut())).toBe(false)
  })

  it('matches when shift is part of the binding', () => {
    expect(
      matches(
        event({ key: 'v', ctrlKey: true, shiftKey: true }),
        shortcut({ key: 'v', shift: true }),
      ),
    ).toBe(true)
  })

  it('does not steal an unmodified keystroke from a text field', () => {
    const input = document.createElement('input')
    const binding = shortcut({ key: 'n', mod: false })
    expect(matches(event({ key: 'n' }, input), binding)).toBe(false)
    expect(matches(event({ key: 'n' }), binding)).toBe(true)
  })

  it('still fires inside a field when the binding opts in', () => {
    const input = document.createElement('input')
    const binding = shortcut({ key: 's', allowInInput: true })
    expect(matches(event({ key: 's', ctrlKey: true }, input), binding)).toBe(true)
  })

  it('treats a contenteditable element as a text field', () => {
    const div = document.createElement('div')
    div.contentEditable = 'true'
    Object.defineProperty(div, 'isContentEditable', { value: true })
    expect(matches(event({ key: 'n' }, div), shortcut({ key: 'n', mod: false }))).toBe(false)
  })
})

describe('describeShortcut', () => {
  it('renders a modifier combination', () => {
    // The test runs under jsdom, which reports a non-Mac platform.
    expect(describeShortcut({ key: 's', mod: true })).toBe('Ctrl+S')
    expect(describeShortcut({ key: 'v', mod: true, shift: true })).toBe('Ctrl+Shift+V')
  })

  it('leaves named keys unabbreviated', () => {
    expect(describeShortcut({ key: 'Enter', mod: true })).toBe('Ctrl+Enter')
  })
})
