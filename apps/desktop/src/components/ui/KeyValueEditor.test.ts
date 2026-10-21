import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { KeyValue } from '@relay/shared-types'
import KeyValueEditor from './KeyValueEditor.vue'

function rows (modelValue: KeyValue[]) {
  return mount(KeyValueEditor, { props: { modelValue } })
}

describe('KeyValueEditor', () => {
  it('always renders one extra blank row so adding is just typing', () => {
    const wrapper = rows([])
    // One blank row and no stored entries.
    expect(wrapper.findAll('input[type="text"]').length).toBe(0)
    expect(wrapper.findAll('input:not([type="checkbox"])').length).toBe(2)
  })

  it('renders a row per entry, plus the blank', () => {
    const wrapper = rows([
      { key: 'Accept', value: 'application/json', enabled: true },
      { key: 'X-Trace', value: '1', enabled: true },
    ])
    // Two entries plus one blank, two fields each.
    expect(wrapper.findAll('input:not([type="checkbox"])').length).toBe(6)
  })

  it('emits an updated list when a key changes', async () => {
    const wrapper = rows([{ key: 'Accept', value: 'application/json', enabled: true }])
    const keyInput = wrapper.findAll('input:not([type="checkbox"])')[0]!

    await keyInput.setValue('Accept-Language')

    const emitted = wrapper.emitted('update:modelValue')
    expect(emitted).toBeTruthy()
    expect(emitted![0]![0]).toEqual([
      { key: 'Accept-Language', value: 'application/json', enabled: true },
    ])
  })

  it('promotes the blank row to a real entry when typed into', async () => {
    const wrapper = rows([])
    const blankKey = wrapper.findAll('input:not([type="checkbox"])')[0]!

    await blankKey.setValue('X-New')

    const emitted = wrapper.emitted('update:modelValue')!
    expect(emitted[0]![0]).toEqual([{ key: 'X-New', value: '', enabled: true }])
  })

  it('toggles a row without deleting it', async () => {
    const wrapper = rows([{ key: 'Accept', value: 'json', enabled: true }])

    await wrapper.find('input[type="checkbox"]').setValue(false)

    const emitted = wrapper.emitted('update:modelValue')!
    expect(emitted[0]![0]).toEqual([{ key: 'Accept', value: 'json', enabled: false }])
  })

  it('removes a row', async () => {
    const wrapper = rows([
      { key: 'A', value: '1', enabled: true },
      { key: 'B', value: '2', enabled: true },
    ])

    await wrapper.findAll('button')[0]!.trigger('click')

    const emitted = wrapper.emitted('update:modelValue')!
    expect(emitted[0]![0]).toEqual([{ key: 'B', value: '2', enabled: true }])
  })

  it('offers no remove button on the blank row', () => {
    const wrapper = rows([{ key: 'A', value: '1', enabled: true }])
    // One real row has a remove button; the blank one does not.
    expect(wrapper.findAll('button')).toHaveLength(1)
  })

  it('labels its controls for screen readers', () => {
    const wrapper = rows([{ key: 'Accept', value: 'json', enabled: true }])
    expect(wrapper.find('input[type="checkbox"]').attributes('aria-label')).toBe(
      'Enable Accept',
    )
    expect(wrapper.findAll('button')[0]!.attributes('aria-label')).toBe('Remove Accept')
  })
})
