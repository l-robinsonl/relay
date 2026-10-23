import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
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

async function panel (existing: McpServer[] = []) {
  mocked.assistantMcpServers.mockResolvedValue(existing)
  const wrapper = mount(McpServers)
  await flushPromises()
  return wrapper
}

async function fill (wrapper: Awaited<ReturnType<typeof panel>>, args = '') {
  await wrapper.find('input[aria-label="Server name"]').setValue('files')
  await wrapper.find('input[aria-label="Command"]').setValue('mcp-filesystem')
  if (args) await wrapper.find('input[aria-label="Arguments"]').setValue(args)
  await wrapper.findAll('button').find((b) => b.text().includes('Add and check'))!.trigger('click')
  await flushPromises()
}

describe('McpServers', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocked.assistantSetMcpServers.mockResolvedValue(undefined)
    mocked.assistantMcpProbe.mockResolvedValue(['read_file', 'write_file'])
  })

  it('says someone elses tool is always confirmed', async () => {
    // The rule, stated where a person is about to add one.
    const text = (await panel()).text()
    expect(text).toContain('always confirmed')
    expect(text).toContain('cannot know what someone else')
  })

  it('checks the server before saving it', async () => {
    // A command that does not start is worth knowing now, not when the
    // next question quietly has fewer tools than expected.
    const wrapper = await panel()
    await fill(wrapper)

    expect(mocked.assistantMcpProbe).toHaveBeenCalled()
    expect(mocked.assistantSetMcpServers).toHaveBeenCalled()
    // Probed before saved.
    expect(mocked.assistantMcpProbe.mock.invocationCallOrder[0])
      .toBeLessThan(mocked.assistantSetMcpServers.mock.invocationCallOrder[0]!)
  })

  it('does not save a server that would not start', async () => {
    mocked.assistantMcpProbe.mockRejectedValue({
      kind: 'internal',
      message: "could not start the MCP server 'files'",
    })

    const wrapper = await panel()
    await fill(wrapper)

    expect(mocked.assistantSetMcpServers).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('could not start')
  })

  it('splits arguments the way a shell would, keeping quoted paths whole', async () => {
    const wrapper = await panel()
    await fill(wrapper, '--root "/home/me/my notes" --readonly')

    const server = mocked.assistantMcpProbe.mock.calls[0]![0] as McpServer & { args: string[] }
    expect(server.args).toEqual(['--root', '/home/me/my notes', '--readonly'])
  })

  it('reports how many tools the server offered', async () => {
    const wrapper = await panel()
    await fill(wrapper)
    expect(wrapper.text()).toContain('2 tools')
  })

  it('lists servers already configured', async () => {
    const wrapper = await panel([
      { transport: 'stdio', name: 'files', command: 'mcp-filesystem', args: [], env: {} },
    ])
    expect(wrapper.text()).toContain('files')
    expect(wrapper.text()).toContain('mcp-filesystem')
  })

  it('removing one persists the shorter list', async () => {
    const wrapper = await panel([
      { transport: 'stdio', name: 'files', command: 'a', args: [], env: {} },
      { transport: 'stdio', name: 'other', command: 'b', args: [], env: {} },
    ])

    await wrapper.find('button[aria-label="Remove files"]').trigger('click')
    await flushPromises()

    const saved = mocked.assistantSetMcpServers.mock.calls[0]![0] as McpServer[]
    expect(saved).toHaveLength(1)
    expect(saved[0]!.name).toBe('other')
  })

  it('says plainly that remote servers are not supported yet', async () => {
    expect((await panel()).text()).toContain('not supported yet')
  })
})
