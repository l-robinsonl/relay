import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { StreamMessage } from '@relay/shared-types'
import StreamLog from './StreamLog.vue'

function message (over: Partial<StreamMessage> = {}): StreamMessage {
  return {
    direction: 'inbound',
    name: 'text',
    data: 'hello',
    id: null,
    binaryBytes: null,
    ...over,
  }
}

function log (props: Partial<InstanceType<typeof StreamLog>['$props']> = {}) {
  return mount(StreamLog, {
    props: {
      lines: [{ at: Date.parse('2026-01-02T09:30:15Z'), message: message() }],
      live: true,
      ...props,
    },
  })
}

describe('StreamLog', () => {
  it('says which way each message went', () => {
    // The one question a conversation log has to answer. An icon on
    // its own was too quiet to do it, so this asserts on the text a
    // screen reader gets rather than on the arrow.
    const wrapper = log({
      lines: [
        { at: 1, message: message({ direction: 'outbound', data: 'ping' }) },
        { at: 2, message: message({ direction: 'inbound', data: 'pong' }) },
      ],
    })

    const rows = wrapper.findAll('li')
    expect(rows[0]!.text()).toContain('Sent')
    expect(rows[1]!.text()).toContain('Received')
  })

  it('does not label every line "text", only the unusual ones', () => {
    const plain = log({ lines: [{ at: 1, message: message() }] })
    expect(plain.find('li').text()).not.toContain('text')

    const binary = log({
      lines: [{ at: 1, message: message({ name: 'binary', binaryBytes: 4096, data: '00 ff' }) }],
    })
    expect(binary.find('li').text()).toContain('binary')
    expect(binary.find('li').text()).toContain('4096 bytes')
  })

  it('pretty-prints a JSON payload', () => {
    const wrapper = log({ lines: [{ at: 1, message: message({ data: '{"a":1}' }) }] })
    expect(wrapper.find('pre').text()).toBe('{\n  "a": 1\n}')
  })

  it('leaves a payload that is not JSON alone', () => {
    const wrapper = log({ lines: [{ at: 1, message: message({ data: '{not json' }) }] })
    expect(wrapper.find('pre').text()).toBe('{not json')
  })

  it('shows a clock time, not a relative one', () => {
    // Twenty messages in a second would all read "just now".
    const wrapper = log()
    expect(wrapper.text()).toMatch(/\d{2}:\d{2}:\d{2}/)
  })

  describe('the composer', () => {
    it('is absent on a one-way stream', () => {
      // Server-sent events cannot carry anything back, and a box that
      // silently does nothing is worse than no box.
      expect(log({ canSend: false }).find('textarea').exists()).toBe(false)
    })

    it('sends what was typed and then empties itself', async () => {
      const wrapper = log({ canSend: true })
      const box = wrapper.find('textarea')

      await box.setValue('{"op":"subscribe"}')
      await wrapper.find('form').trigger('submit')

      expect(wrapper.emitted('send')).toEqual([['{"op":"subscribe"}']])
      expect((box.element as HTMLTextAreaElement).value).toBe('')
    })

    it('sends on Enter, because that is what a chat box does', async () => {
      const wrapper = log({ canSend: true })
      await wrapper.find('textarea').setValue('hi')
      await wrapper.find('textarea').trigger('keydown.enter')

      expect(wrapper.emitted('send')).toEqual([['hi']])
    })

    it('sends nothing when the box is empty', async () => {
      const wrapper = log({ canSend: true })
      await wrapper.find('form').trigger('submit')
      expect(wrapper.emitted('send')).toBeUndefined()
    })

    it('sends whitespace, which is a payload somebody may be testing', async () => {
      const wrapper = log({ canSend: true })
      await wrapper.find('textarea').setValue('   ')
      await wrapper.find('form').trigger('submit')

      expect(wrapper.emitted('send')).toEqual([['   ']])
    })

    it('is disabled while nothing is connected', () => {
      const wrapper = log({ canSend: true, live: false })
      expect(wrapper.find('textarea').attributes('disabled')).toBeDefined()
      expect(wrapper.find('button[type="submit"]').attributes('disabled')).toBeDefined()
    })
  })
})
