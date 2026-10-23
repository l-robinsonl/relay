import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import type { AssistantEvent } from '@/services/ipc'
import AssistantPanel from './AssistantPanel.vue'
import { api } from '@/services/ipc'
import { useWorkspaceStore } from '@/stores/workspace'

let emit: (event: AssistantEvent) => void = () => {}

vi.mock('@/services/ipc', () => ({
  api: {
    assistantAsk: vi.fn(),
    assistantApprove: vi.fn(),
    assistantDenyAll: vi.fn(),
    assistantHasKey: vi.fn(),
    onAssistantEvent: vi.fn(),
  },
}))

const mocked = vi.mocked(api)

async function panel () {
  const wrapper = mount(AssistantPanel)
  const store = useWorkspaceStore()
  store.current = { id: 'w1', name: 'Test' } as never
  await flushPromises()
  return wrapper
}

async function askSomething (wrapper: Awaited<ReturnType<typeof panel>>) {
  await wrapper.find('textarea').setValue('send the health check')
  await wrapper.find('button[aria-label="Send"]').trigger('click')
  await flushPromises()
}

describe('AssistantPanel', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
    mocked.assistantHasKey.mockResolvedValue(true)
    mocked.onAssistantEvent.mockImplementation(async (handler) => {
      emit = handler
      return () => {}
    })
    // Leaves the turn running so events can be delivered mid-flight.
    mocked.assistantAsk.mockImplementation(() => new Promise(() => {}))
  })

  it('says what it can do rather than showing a blank box', async () => {
    const wrapper = await panel()
    expect(wrapper.text()).toContain('Ask for anything you could do here yourself')
    // And is honest about the limits up front.
    expect(wrapper.text()).toContain('cannot read your secret values')
  })

  it('shows the question immediately rather than after the answer', async () => {
    // A turn can take minutes; the person needs to see it was received.
    const wrapper = await panel()
    await askSomething(wrapper)
    expect(wrapper.text()).toContain('send the health check')
  })

  it('asks inline, in the transcript, before sending a request', async () => {
    const wrapper = await panel()
    await askSomething(wrapper)

    emit({
      type: 'asking',
      call: { id: 'c1', name: 'send_request', input: { id: 'r1' } },
      risk: 'send',
    })
    await flushPromises()

    expect(wrapper.text()).toContain('sends a real request with your credentials')
    expect(wrapper.text()).toContain('cannot tell it apart from one you sent')
    expect(wrapper.find('button').exists()).toBe(true)
  })

  it('answers the waiting call when allowed', async () => {
    mocked.assistantApprove.mockResolvedValue(true)
    const wrapper = await panel()
    await askSomething(wrapper)

    emit({
      type: 'asking',
      call: { id: 'c1', name: 'send_request', input: {} },
      risk: 'send',
    })
    await flushPromises()

    const allow = wrapper.findAll('button').find((b) => b.text().includes('Allow'))!
    await allow.trigger('click')
    await flushPromises()

    expect(mocked.assistantApprove).toHaveBeenCalledWith('c1', true)
  })

  it('answers no when declined', async () => {
    mocked.assistantApprove.mockResolvedValue(true)
    const wrapper = await panel()
    await askSomething(wrapper)

    emit({
      type: 'asking',
      call: { id: 'c1', name: 'delete_request', input: {} },
      risk: 'destructive',
    })
    await flushPromises()
    expect(wrapper.text()).toContain('This destroys something')

    await wrapper.findAll('button').find((b) => b.text().includes("Don't"))!.trigger('click')
    await flushPromises()

    expect(mocked.assistantApprove).toHaveBeenCalledWith('c1', false)
  })

  it('does not show an approved call twice', async () => {
    // It arrives as `asking` and then again as `running`; the row has
    // to be matched by call id rather than appended.
    const wrapper = await panel()
    await askSomething(wrapper)

    const call = { id: 'c1', name: 'send_request', input: {} }
    emit({ type: 'asking', call, risk: 'send' })
    emit({ type: 'running', call })
    await flushPromises()

    const rows = wrapper.findAll('[aria-expanded]')
    expect(rows).toHaveLength(1)
  })

  it('attaches a result to the call it belongs to', async () => {
    const wrapper = await panel()
    await askSomething(wrapper)

    emit({ type: 'running', call: { id: 'c1', name: 'list_requests', input: {} } })
    emit({
      type: 'ran',
      result: { callId: 'c1', outcome: { content: '{"count":3}', isError: false } },
    })
    await flushPromises()

    await wrapper.find('[aria-expanded]').trigger('click')
    expect(wrapper.text()).toContain('{"count":3}')
  })

  it('surfaces a failed turn instead of leaving a spinner', async () => {
    mocked.assistantAsk.mockRejectedValue({
      kind: 'internal',
      message: 'no API key is set for Anthropic',
      detail: 'Add one under Settings → Assistant.',
    })

    const wrapper = await panel()
    await askSomething(wrapper)

    expect(wrapper.text()).toContain('no API key is set')
    expect(wrapper.text()).toContain('Add one under Settings')
  })

  it('says when no key is configured', async () => {
    mocked.assistantHasKey.mockResolvedValue(false)
    const wrapper = await panel()
    await flushPromises()

    expect(wrapper.text()).toContain('No API key is set')
    expect(wrapper.text()).toContain('Relay ships no key')
  })

  it('starting over clears the transcript', async () => {
    const wrapper = await panel()
    await askSomething(wrapper)
    expect(wrapper.text()).toContain('send the health check')

    await wrapper.find('button[aria-label="Start a new conversation"]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).not.toContain('send the health check')
  })
})
