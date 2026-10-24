/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
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
    assistantMcpServers: vi.fn(() => Promise.resolve([])),
    assistantSetMcpServers: vi.fn(),
    assistantMcpProbe: vi.fn(),
    onAssistantEvent: vi.fn(),
    stateGet: vi.fn(),
    stateSet: vi.fn(),
  },
}))

const mocked = vi.mocked(api)
const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

/** Every state worth looking at, as a script of events. */
const STATES: {
  name: string
  hasKey?: boolean
  ask?: string
  script?: AssistantEvent[]
}[] = [
  { name: '01-empty' },
  { name: '02-no-key-yet', hasKey: false },
  {
    name: '03-thinking',
    ask: 'which of my requests touch the billing API?',
  },
  {
    name: '04-answered',
    ask: 'which of my requests touch the billing API?',
    script: [
      { type: 'running', call: { id: 'c1', name: 'list_requests', input: {} } },
      {
        type: 'ran',
        result: { callId: 'c1', outcome: { content: '12 requests', isError: false } },
      },
      {
        type: 'said',
        text: 'Four of them do: List invoices, Get invoice, Void invoice and Pay invoice. '
          + 'They all point at api.stripe.com and share the {{stripeKey}} variable.',
      },
    ],
  },
  {
    name: '05-asking-permission',
    ask: 'void the test invoice',
    script: [
      { type: 'said', text: 'I found "Void invoice". I need your say-so before sending it.' },
      {
        type: 'asking',
        call: { id: 'c2', name: 'send_request', input: { name: 'Void invoice' } },
        risk: 'send',
      },
    ],
  },
  {
    name: '06-a-tool-failed',
    ask: 'open request req_missing',
    script: [
      { type: 'running', call: { id: 'c3', name: 'get_request', input: { id: 'req_missing' } } },
      {
        type: 'ran',
        result: { callId: 'c3', outcome: { content: 'no request with that id', isError: true } },
      },
      { type: 'said', text: "There is no request with that id in this workspace." },
    ],
  },
  {
    name: '07-a-long-answer',
    ask: 'explain how variables resolve',
    script: [
      {
        type: 'said',
        text: 'Relay looks in four places, nearest first: the request itself, then the '
          + 'environment you have selected, then the collection the request sits in, and '
          + 'finally the workspace. The first one holding the name wins, so a request-level '
          + 'value quietly overrides an environment one. Secrets live in your operating '
          + "system's credential store rather than the database, which is why I cannot read "
          + 'them and neither can a script.',
      },
    ],
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('AssistantPanel — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })

    for (const state of STATES) {
      setActivePinia(createPinia())
      vi.clearAllMocks()
      mocked.assistantHasKey.mockResolvedValue(state.hasKey ?? true)
      mocked.stateGet.mockResolvedValue(null)
      mocked.stateSet.mockResolvedValue(undefined)
      mocked.onAssistantEvent.mockImplementation(async (handler) => {
        emit = handler
        return () => {}
      })
      // Never resolves: the turn stays in flight so mid-turn states hold.
      mocked.assistantAsk.mockImplementation(() => new Promise(() => {}))

      const wrapper = mount(AssistantPanel)
      useWorkspaceStore().current = { id: 'w1', name: 'Test' } as never
      await flushPromises()

      if (state.ask) {
        await wrapper.find('textarea').setValue(state.ask)
        await wrapper.find('button[aria-label="Send"]').trigger('click')
        await flushPromises()
      }
      for (const event of state.script ?? []) {
        emit(event)
        await flushPromises()
      }

      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }

    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
