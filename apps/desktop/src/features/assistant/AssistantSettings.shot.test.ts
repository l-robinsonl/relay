/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import type { AssistantSettings as Settings } from '@/services/ipc'
import AssistantSettings from './AssistantSettings.vue'
import { api } from '@/services/ipc'
import { rendered } from '@/features/shots'

vi.mock('@/services/ipc', () => ({
  api: {
    assistantHasKey: vi.fn(),
    assistantSetKey: vi.fn(),
    assistantClearKey: vi.fn(),
    // The panel embeds McpServers, which loads on mount.
    assistantMcpServers: vi.fn(() => Promise.resolve([])),
    assistantSetMcpServers: vi.fn(),
    assistantMcpProbe: vi.fn(),
  },
}))

const mocked = vi.mocked(api)
const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

const STATES: { name: string, settings: Settings, hasKey: boolean }[] = [
  {
    name: '01-anthropic-no-key',
    settings: { provider: 'anthropic', model: 'claude-opus-5', baseUrl: '' },
    hasKey: false,
  },
  {
    name: '02-anthropic-key-saved',
    settings: { provider: 'anthropic', model: 'claude-opus-5', baseUrl: '' },
    hasKey: true,
  },
  {
    name: '03-openai',
    settings: { provider: 'openai', model: 'gpt-5', baseUrl: '' },
    hasKey: true,
  },
  {
    name: '04-compatible-endpoint',
    settings: {
      provider: 'compatible',
      model: 'llama-3.3-70b',
      baseUrl: 'http://localhost:11434/v1',
    },
    hasKey: false,
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('AssistantSettings — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })

    for (const state of STATES) {
      vi.clearAllMocks()
      mocked.assistantHasKey.mockResolvedValue(state.hasKey)

      // Teleports to body, so it must be attached to be seen.
      const wrapper = mount(AssistantSettings, {
        props: { open: true, modelValue: state.settings },
        attachTo: document.body,
      })
      await flushPromises()
      writeFileSync(`${OUT}/${state.name}.html`, rendered(wrapper as never))
      wrapper.unmount()
      document.body.innerHTML = ''
    }

    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
