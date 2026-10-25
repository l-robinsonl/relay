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
    assistantSetKey: vi.fn(),
    assistantClearKey: vi.fn(),
    onAssistantEvent: vi.fn(),
    stateGet: vi.fn(),
    stateSet: vi.fn(),
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
    mocked.stateGet.mockResolvedValue(null)
    mocked.stateSet.mockResolvedValue(undefined)
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

  describe('a streamed answer', () => {
    it('grows as the fragments arrive', async () => {
      const wrapper = await panel()
      await askSomething(wrapper)

      emit({ type: 'delta', text: 'Check' })
      await flushPromises()
      expect(wrapper.text()).toContain('Check')

      emit({ type: 'delta', text: 'ing the' })
      emit({ type: 'delta', text: ' endpoint.' })
      await flushPromises()
      expect(wrapper.text()).toContain('Checking the endpoint.')
    })

    it('shows one message, not one per fragment', async () => {
      const wrapper = await panel()
      await askSomething(wrapper)

      emit({ type: 'delta', text: 'a' })
      emit({ type: 'delta', text: 'b' })
      emit({ type: 'delta', text: 'c' })
      await flushPromises()

      // Three paragraphs reading "a", "b", "c" would also contain
      // "abc" in `text()`, so count the elements.
      const said = wrapper.findAll('p.whitespace-pre-wrap')
      expect(said).toHaveLength(1)
      expect(said[0]!.text()).toBe('abc')
    })

    it('the complete message replaces the fragments rather than repeating them', async () => {
      // `said` is what gets persisted. If it appended instead, the
      // panel would show the answer twice and disagree with history.
      const wrapper = await panel()
      await askSomething(wrapper)

      emit({ type: 'delta', text: 'Half' })
      emit({ type: 'said', text: 'Half a sentence.' })
      await flushPromises()

      const said = wrapper.findAll('p.whitespace-pre-wrap')
      expect(said).toHaveLength(1)
      expect(said[0]!.text()).toBe('Half a sentence.')
    })

    it('a message with no fragments before it still appears', async () => {
      // A provider with no streaming path sends `said` alone.
      const wrapper = await panel()
      await askSomething(wrapper)

      emit({ type: 'said', text: 'All at once.' })
      await flushPromises()

      expect(wrapper.text()).toContain('All at once.')
    })

    it('a second message starts a new paragraph', async () => {
      // Two model turns around a tool call. Appending the second to
      // the first would run them together.
      const wrapper = await panel()
      await askSomething(wrapper)

      emit({ type: 'delta', text: 'Looking.' })
      emit({ type: 'said', text: 'Looking.' })
      emit({ type: 'delta', text: 'Found it.' })
      await flushPromises()

      const said = wrapper.findAll('p.whitespace-pre-wrap')
      expect(said.map((p) => p.text())).toEqual(['Looking.', 'Found it.'])
    })

    it('drops the spinner while the text is doing the talking', async () => {
      const wrapper = await panel()
      await askSomething(wrapper)
      expect(wrapper.text()).toContain('Working…')

      emit({ type: 'delta', text: 'Here goes' })
      await flushPromises()
      expect(wrapper.text()).not.toContain('Working…')

      // And it comes back between messages, when there is nothing to read.
      emit({ type: 'said', text: 'Here goes' })
      await flushPromises()
      expect(wrapper.text()).toContain('Working…')
    })

    it('shows a cursor only on the message still being written', async () => {
      const wrapper = await panel()
      await askSomething(wrapper)

      emit({ type: 'delta', text: 'typing' })
      await flushPromises()
      expect(wrapper.findAll('.caret')).toHaveLength(1)

      emit({ type: 'said', text: 'typing' })
      await flushPromises()
      expect(wrapper.findAll('.caret')).toHaveLength(0)
    })
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
