import { beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useDialogStore } from './dialogs'

beforeEach(() => setActivePinia(createPinia()))

describe('the dialog store', () => {
  it('resolves a prompt with what the user typed', async () => {
    const dialogs = useDialogStore()
    const answer = dialogs.prompt({ title: 'New folder' })

    expect(dialogs.pending?.kind).toBe('prompt')
    dialogs.submit('Users')

    expect(await answer).toBe('Users')
    expect(dialogs.pending).toBeNull()
  })

  it('resolves a cancelled prompt with null, not an empty string', async () => {
    // An empty string would create an unnamed folder; null means "no".
    const dialogs = useDialogStore()
    const answer = dialogs.prompt({ title: 'New folder' })
    dialogs.dismiss()

    expect(await answer).toBeNull()
  })

  it('resolves a confirm with true or false', async () => {
    const dialogs = useDialogStore()

    const yes = dialogs.confirm({ title: 'Delete?' })
    dialogs.submit('')
    expect(await yes).toBe(true)

    const no = dialogs.confirm({ title: 'Delete?' })
    dialogs.dismiss()
    expect(await no).toBe(false)
  })

  it('never leaves a caller waiting when a second dialog opens', async () => {
    // Two modals stacked on each other would be a mess, and the first
    // caller would hang forever if its promise were dropped.
    const dialogs = useDialogStore()

    const first = dialogs.prompt({ title: 'First' })
    const second = dialogs.prompt({ title: 'Second' })

    expect(await first).toBeNull()

    dialogs.submit('answered')
    expect(await second).toBe('answered')
  })

  it('carries its options through to whatever renders it', () => {
    const dialogs = useDialogStore()
    void dialogs.confirm({
      title: 'Delete "Users"?',
      description: 'Everything inside goes too.',
      destructive: true,
    })

    expect(dialogs.pending?.options.title).toBe('Delete "Users"?')
    expect(dialogs.pending?.kind).toBe('confirm')
  })

  it('dismissing with nothing open is harmless', () => {
    const dialogs = useDialogStore()
    expect(() => dialogs.dismiss()).not.toThrow()
    expect(() => dialogs.submit('x')).not.toThrow()
  })
})
