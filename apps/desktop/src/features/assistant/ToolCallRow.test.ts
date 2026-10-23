import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import ToolCallRow from './ToolCallRow.vue'
import type { ToolCall, ToolRisk } from '@/services/ipc'

const CALL: ToolCall = { id: 'call-1', name: 'delete_request', input: { id: 'req_1' } }

function row (asking: ToolRisk | null, busy = true) {
  return mount(ToolCallRow, { props: { call: CALL, asking, busy } })
}

function allowButton (wrapper: ReturnType<typeof row>) {
  return wrapper.findAll('button').find((b) => b.text().includes('Allow'))!
}

describe('ToolCallRow', () => {
  it('colours the destructive answer as destructive', () => {
    // Both answers were styled the same, so the button with the most
    // visual pull — the one people hit to make a prompt go away — was
    // the one that deletes. Colour is the only thing distinguishing
    // "cannot be undone" from "can".
    const destructive = allowButton(row('destructive'))
    expect(destructive.classes()).toContain('btn-danger')
    expect(destructive.classes()).not.toContain('btn-primary')

    const send = allowButton(row('send'))
    expect(send.classes()).toContain('btn-primary')
    expect(send.classes()).not.toContain('btn-danger')
  })

  it('says which kind of harm it is asking about', () => {
    expect(row('send').text()).toContain('cannot tell it apart from one you sent yourself')
    expect(row('destructive').text()).toContain('cannot be undone')
  })

  it('leaves both answers clickable while the turn is in flight', () => {
    // The turn is always in flight while this is on screen — it is in
    // flight *because* it is waiting for this answer. Disabling on
    // `busy` killed the buttons exactly when they were needed.
    const wrapper = row('destructive', true)
    for (const button of wrapper.findAll('button')) {
      expect(button.attributes('disabled')).toBeUndefined()
    }
  })

  it('reports the answer the person gave', async () => {
    const wrapper = row('send')

    await allowButton(wrapper).trigger('click')
    expect(wrapper.emitted('answer')?.[0]).toEqual([true])

    const refuse = wrapper.findAll('button').find((b) => b.text().includes("Don't"))!
    await refuse.trigger('click')
    expect(wrapper.emitted('answer')?.[1]).toEqual([false])
  })

  it('asks nothing when it is not asking', () => {
    const wrapper = row(null, false)
    expect(wrapper.text()).not.toContain('Allow')
    expect(wrapper.findAll('button').some((b) => b.text().includes("Don't"))).toBe(false)
  })
})
