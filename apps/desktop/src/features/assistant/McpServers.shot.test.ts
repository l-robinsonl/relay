/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import type { McpServer } from '@/services/ipc'
import McpServers from './McpServers.vue'
import { api } from '@/services/ipc'

vi.mock('@/services/ipc', () => ({
  api: {
    assistantMcpServers: vi.fn(),
    assistantSetMcpServers: vi.fn(),
    assistantMcpProbe: vi.fn(),
  },
}))

const mocked = vi.mocked(api)
const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

function stdio (name: string, command: string, args: string[] = []): McpServer {
  return { transport: 'stdio', name, command, args, env: {} }
}

const STATES: { name: string, servers: McpServer[] }[] = [
  { name: '01-none-configured', servers: [] },
  { name: '02-one-server', servers: [stdio('filesystem', 'npx', ['-y', '@modelcontextprotocol/server-filesystem', '/home/me/notes'])] },
  {
    name: '03-several',
    servers: [
      stdio('filesystem', 'npx', ['-y', '@modelcontextprotocol/server-filesystem', '/home/me']),
      stdio('github', 'docker', ['run', '-i', '--rm', 'ghcr.io/github/github-mcp-server']),
      stdio('sqlite', '/usr/local/bin/mcp-sqlite', ['--db', '/var/data/app.db']),
    ],
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('McpServers — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })

    for (const state of STATES) {
      vi.clearAllMocks()
      mocked.assistantMcpServers.mockResolvedValue(state.servers)

      const wrapper = mount(McpServers)
      await flushPromises()
      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }

    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
