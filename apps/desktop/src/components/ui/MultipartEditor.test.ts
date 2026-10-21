import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { FormField } from '@relay/shared-types'
import MultipartEditor from './MultipartEditor.vue'

function editor (modelValue: FormField[]) {
  return mount(MultipartEditor, { props: { modelValue } })
}

const textField: FormField = { key: 'name', enabled: true, kind: 'text', value: 'Ada' }
const fileField: FormField = {
  key: 'avatar',
  enabled: true,
  kind: 'file',
  path: '/tmp/a.png',
}

describe('MultipartEditor', () => {
  it('renders a blank trailing row so adding is just typing', () => {
    const wrapper = editor([])
    expect(wrapper.findAll('input:not([type="checkbox"])').length).toBe(2)
  })

  it('renders text and file rows differently', () => {
    const wrapper = editor([textField, fileField])

    // A text row carries a value input; a file row shows the chosen file on
    // a button instead, so there is nothing to type a path into by hand.
    expect(wrapper.text()).toContain('a.png')
    expect(
      wrapper.findAll('input[placeholder="Value"]').length,
      'a file row should not offer a free-text value field',
    ).toBe(2) // the text row and the trailing blank row

    expect(
      wrapper.findAll('button').some((b) => b.text().includes('a.png')),
      'the chosen file should be shown on its button',
    ).toBe(true)
  })

  it('emits an update when a value changes', async () => {
    const wrapper = editor([textField])
    const valueInput = wrapper.findAll('input:not([type="checkbox"])')[1]!

    await valueInput.setValue('Grace')

    const emitted = wrapper.emitted('update:modelValue')!
    expect(emitted[0]![0]).toEqual([{ ...textField, value: 'Grace' }])
  })

  it('switches a row from text to file, discarding the old value', async () => {
    const wrapper = editor([textField])
    const toggle = wrapper
      .findAll('button')
      .find((b) => b.attributes('aria-label') === 'Change to a file')!

    await toggle.trigger('click')

    const emitted = wrapper.emitted('update:modelValue')!
    // The text value must not linger on a field that now sends a file.
    expect(emitted[0]![0]).toEqual([
      { key: 'name', enabled: true, kind: 'file', path: '' },
    ])
  })

  it('switches a row from file back to text', async () => {
    const wrapper = editor([fileField])
    const toggle = wrapper
      .findAll('button')
      .find((b) => b.attributes('aria-label') === 'Change to a text value')!

    await toggle.trigger('click')

    const emitted = wrapper.emitted('update:modelValue')!
    expect(emitted[0]![0]).toEqual([
      { key: 'avatar', enabled: true, kind: 'text', value: '' },
    ])
  })

  it('toggles a row without removing it', async () => {
    const wrapper = editor([textField])
    await wrapper.find('input[type="checkbox"]').setValue(false)

    const emitted = wrapper.emitted('update:modelValue')!
    expect(emitted[0]![0]).toEqual([{ ...textField, enabled: false }])
  })

  it('removes a row', async () => {
    const wrapper = editor([textField, fileField])
    const remove = wrapper
      .findAll('button')
      .find((b) => b.attributes('aria-label') === 'Remove name')!

    await remove.trigger('click')

    expect(wrapper.emitted('update:modelValue')![0]![0]).toEqual([fileField])
  })

  it('says that files are streamed rather than buffered', () => {
    expect(editor([]).text()).toContain('streamed from disk')
  })
})
