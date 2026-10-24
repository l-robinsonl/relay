import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { createPinia } from 'pinia'
import type { RequestDoc, TreeNode, Workspace } from '@relay/shared-types'

/**
 * A smoke test for the whole application tree.
 *
 * It mounts the real layout, sidebar, tab bar, request panel and response
 * panel over a mocked IPC layer. It cannot replace running the desktop
 * binary, but it does catch the failures that otherwise only appear at
 * runtime: a component that does not compose, a store read before it is
 * populated, a template referring to something that no longer exists.
 */

const workspace: Workspace = {
  id: 'w1',
  name: 'My Workspace',
  description: '',
  variables: [],
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 1,
  rev: 0,
  dirty: false,
}

const tree: TreeNode[] = [
  {
    kind: 'folder',
    id: 'c1',
    parentId: null,
    name: 'Users',
    sortOrder: 0,
    childCount: 1,
  },
  {
    kind: 'request',
    id: 'r1',
    parentId: 'c1',
    name: 'List users',
    method: 'GET',
    url: 'https://api.example.com/users',
    sortOrder: 0,
  },
]

const savedRequest: RequestDoc = {
  id: 'r1',
  workspaceId: 'w1',
  collectionId: 'c1',
  name: 'List users',
  description: '',
  kind: 'http',
  method: 'GET',
  url: 'https://api.example.com/users',
  headers: [],
  queryParams: [],
  pathParams: [],
  body: { mode: 'none' },
  auth: { type: 'inherit' },
  settings: {
    redirectPolicy: 'follow',
    maxRedirects: 10,
    verifyTls: true,
    storeCookies: true,
    sendCookies: true,
    decompress: true,
    encodeUrl: true,
  },
  scripts: { preRequest: '', postResponse: '' },
  tags: [],
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 1,
  rev: 0,
  dirty: false,
}

const mocks = vi.hoisted(() => ({
  workspaceActive: vi.fn(),
  workspaceList: vi.fn(),
  collectionTree: vi.fn(),
  collectionList: vi.fn(),
  environmentList: vi.fn(),
  historyList: vi.fn(),
  requestGet: vi.fn(),
  requestResolve: vi.fn(),
  stateGet: vi.fn(),
  stateSet: vi.fn(),
  syncStatus: vi.fn(),
  syncConflicts: vi.fn(),
  syncWatch: vi.fn(),
  syncUnwatch: vi.fn(),
}))

vi.mock('@/services/ipc', () => ({
  api: mocks,
  onExecutionProgress: vi.fn().mockResolvedValue(() => {}),
  onStreamEvent: vi.fn().mockResolvedValue(() => {}),
  onSyncEvent: vi.fn().mockResolvedValue(() => {}),
  isRelayError: () => false,
  toRelayError: (e: unknown) => ({ kind: 'internal', message: String(e) }),
}))

// CodeMirror reaches for layout APIs jsdom does not implement.
vi.mock('@/components/ui/CodeEditor.vue', () => ({
  default: { name: 'CodeEditor', template: '<div class="code-editor-stub" />' },
}))

import AppLayout from './AppLayout.vue'

beforeEach(() => {
  vi.clearAllMocks()
  mocks.workspaceActive.mockResolvedValue(workspace)
  mocks.workspaceList.mockResolvedValue([workspace])
  mocks.collectionTree.mockResolvedValue(tree)
  mocks.collectionList.mockResolvedValue([])
  mocks.environmentList.mockResolvedValue([])
  mocks.historyList.mockResolvedValue([])
  mocks.requestGet.mockResolvedValue(savedRequest)
  mocks.requestResolve.mockResolvedValue({ spec: {}, missingVariables: [] })
  mocks.stateGet.mockResolvedValue(null)
  mocks.stateSet.mockResolvedValue(undefined)
  // A local-only workspace: the ordinary case, and what the shell must
  // render without a server configured.
  mocks.syncStatus.mockResolvedValue({
    connected: false,
    serverUrl: null,
    userEmail: null,
    signedIn: false,
    lastPulledSeq: 0,
    lastSyncAt: null,
    lastError: null,
    pendingChanges: 0,
    openConflicts: 0,
  })
  mocks.syncConflicts.mockResolvedValue([])
  mocks.syncWatch.mockResolvedValue(false)
  mocks.syncUnwatch.mockResolvedValue(undefined)

  if (!globalThis.crypto?.randomUUID) {
    Object.defineProperty(globalThis, 'crypto', {
      value: { randomUUID: () => `id-${Math.random().toString(36).slice(2)}` },
      configurable: true,
    })
  }
})

async function mountApp () {
  const wrapper = mount(AppLayout, {
    global: { plugins: [createPinia()] },
    attachTo: document.body,
  })
  await flushPromises()
  await flushPromises()
  return wrapper
}

describe('the application shell', () => {
  it('mounts and loads the active workspace', async () => {
    const wrapper = await mountApp()

    expect(mocks.workspaceActive).toHaveBeenCalled()
    expect(wrapper.text()).toContain('Relay')
    expect(wrapper.text()).toContain('My Workspace')
  })

  it('renders the collection tree', async () => {
    const wrapper = await mountApp()
    expect(wrapper.text()).toContain('Users')
  })

  it('opens a draft request so the editor is never empty on first run', async () => {
    const wrapper = await mountApp()

    // A request panel and a response panel are both present.
    expect(wrapper.find('section[aria-label="Request"]').exists()).toBe(true)
    expect(wrapper.find('section[aria-label="Response"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('Send a request to see the response')
  })

  it('shows the request configuration sections', async () => {
    const wrapper = await mountApp()
    const labels = wrapper
      .findAll('[role="tab"]')
      .map((tab) => tab.text())
      .join(' ')

    for (const section of ['Params', 'Headers', 'Body', 'Auth', 'Scripts', 'Settings']) {
      expect(labels).toContain(section)
    }
  })

  it('opens a saved request from the sidebar', async () => {
    const wrapper = await mountApp()

    // Expand the folder, then click the request inside it.
    const folder = wrapper.findAll('button').find((b) => b.text().includes('Users'))
    await folder!.trigger('click')
    await flushPromises()

    const request = wrapper.findAll('button').find((b) => b.text().includes('List users'))
    expect(request).toBeDefined()
    await request!.trigger('click')
    await flushPromises()

    expect(mocks.requestGet).toHaveBeenCalledWith('r1')
  })

  it('opens the command palette on Ctrl+K', async () => {
    const wrapper = await mountApp()

    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true }))
    await flushPromises()

    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull()
    wrapper.unmount()
  })

  it('toggles the sidebar', async () => {
    const wrapper = await mountApp()
    expect(wrapper.text()).toContain('Collections')

    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', ctrlKey: true }))
    await flushPromises()

    expect(wrapper.text()).not.toContain('Collections')
    wrapper.unmount()
  })

  it('shows a local-only workspace as local, not as an error', async () => {
    const wrapper = await mountApp()
    // Running without a server is the normal case, not a failure state.
    expect(wrapper.text()).toContain('Local only')
  })

  it('still opens an editor when a listener cannot be registered', async () => {
    // These used to be a chain of awaits, so one failing registration
    // skipped the line that opens a tab: a blank window, no message.
    const ipc = await import('@/services/ipc')
    vi.mocked(ipc.onExecutionProgress).mockRejectedValueOnce(new Error('no such event'))
    vi.spyOn(console, 'error').mockImplementation(() => {})

    const wrapper = await mountApp()

    expect(wrapper.find('section[aria-label="Request"]').exists()).toBe(true)
  })

  it('renders the split panes with a keyboard-resizable divider', async () => {
    const wrapper = await mountApp()
    const separator = wrapper.find('[role="separator"]')

    expect(separator.exists()).toBe(true)
    expect(separator.attributes('tabindex')).toBe('0')
    expect(separator.attributes('aria-label')).toBe('Resize panes')
  })
})
