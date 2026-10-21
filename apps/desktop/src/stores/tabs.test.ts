import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import type { HttpExchange, RequestDoc } from '@relay/shared-types'

// The IPC layer is mocked: these tests are about tab behaviour, and the
// commands behind it are covered by the Rust end-to-end tests.
const mocks = vi.hoisted(() => ({
  requestGet: vi.fn(),
  requestSave: vi.fn(),
  httpSend: vi.fn(),
  httpSendWithTests: vi.fn(),
  httpCancel: vi.fn(),
}))

vi.mock('@/services/ipc', () => ({
  api: mocks,
  onExecutionProgress: vi.fn().mockResolvedValue(() => {}),
  isRelayError: () => false,
  toRelayError: (e: unknown) => ({ kind: 'internal', message: String(e) }),
}))

vi.mock('@/stores/workspace', () => ({
  useWorkspaceStore: () => ({
    current: { id: 'w1', name: 'Test' },
    activeEnvironmentId: null,
    refreshTree: vi.fn().mockResolvedValue(undefined),
  }),
}))

import { useTabsStore } from './tabs'

function savedDoc (over: Partial<RequestDoc> = {}): RequestDoc {
  return {
    id: 'r1',
    workspaceId: 'w1',
    collectionId: null,
    name: 'Saved request',
    description: '',
    method: 'GET',
    url: 'https://example.com',
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
    updatedAt: 2,
    rev: 0,
    dirty: false,
    ...over,
  }
}

beforeEach(() => {
  setActivePinia(createPinia())
  vi.clearAllMocks()
  // jsdom does not provide randomUUID in every version.
  if (!globalThis.crypto?.randomUUID) {
    Object.defineProperty(globalThis, 'crypto', {
      value: { randomUUID: () => `id-${Math.random().toString(36).slice(2)}` },
      configurable: true,
    })
  }
})

describe('tab lifecycle', () => {
  it('opens a draft that counts as unsaved', () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()

    expect(tab).not.toBeNull()
    expect(tabs.tabs).toHaveLength(1)
    expect(tabs.activeTabId).toBe(tab!.id)
    // A draft has never been saved, so it is always dirty.
    expect(tabs.isDirty(tab!)).toBe(true)
  })

  it('opens a saved request as clean', async () => {
    mocks.requestGet.mockResolvedValue(savedDoc())
    const tabs = useTabsStore()

    const tab = await tabs.openRequest('r1')

    expect(tab).not.toBeNull()
    expect(tabs.isDirty(tab!)).toBe(false)
  })

  it('focuses an already-open request instead of opening it twice', async () => {
    mocks.requestGet.mockResolvedValue(savedDoc())
    const tabs = useTabsStore()

    const first = await tabs.openRequest('r1')
    const second = await tabs.openRequest('r1')

    expect(tabs.tabs).toHaveLength(1)
    expect(second!.id).toBe(first!.id)
    expect(mocks.requestGet).toHaveBeenCalledTimes(1)
  })

  it('marks a tab dirty once it is edited', async () => {
    mocks.requestGet.mockResolvedValue(savedDoc())
    const tabs = useTabsStore()
    const tab = await tabs.openRequest('r1')

    expect(tabs.isDirty(tabs.tabs[0]!)).toBe(false)
    tabs.update(tab!.id, { url: 'https://changed.example' })
    expect(tabs.isDirty(tabs.tabs[0]!)).toBe(true)
  })

  it('becomes clean again after saving', async () => {
    mocks.requestGet.mockResolvedValue(savedDoc())
    const tabs = useTabsStore()
    const tab = await tabs.openRequest('r1')
    tabs.update(tab!.id, { url: 'https://changed.example' })

    mocks.requestSave.mockImplementation(async (doc: RequestDoc) => ({
      ...doc,
      updatedAt: Date.now(),
    }))
    await tabs.save(tab!.id)

    expect(tabs.isDirty(tabs.tabs[0]!)).toBe(false)
  })

  it('does not treat a save-only timestamp change as an edit', async () => {
    // updatedAt and rev change on every save; if they counted, a tab would
    // look dirty the instant it was saved.
    mocks.requestGet.mockResolvedValue(savedDoc())
    const tabs = useTabsStore()
    const tab = await tabs.openRequest('r1')

    mocks.requestSave.mockResolvedValue(savedDoc({ updatedAt: 999, rev: 7, dirty: true }))
    await tabs.save(tab!.id)

    expect(tabs.isDirty(tabs.tabs[0]!)).toBe(false)
  })

  it('assigns the server id after the first save of a draft', async () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()
    tabs.update(tab!.id, { name: 'New one' })

    mocks.requestSave.mockResolvedValue(savedDoc({ id: 'assigned-id', name: 'New one' }))
    await tabs.save(tab!.id)

    expect(tabs.tabs[0]!.requestId).toBe('assigned-id')
    expect(tabs.isDirty(tabs.tabs[0]!)).toBe(false)
  })

  it('activates a neighbouring tab when the active one closes', () => {
    const tabs = useTabsStore()
    const first = tabs.openDraft()!
    const second = tabs.openDraft()!
    const third = tabs.openDraft()!

    tabs.activate(second.id)
    tabs.close(second.id)

    expect(tabs.tabs).toHaveLength(2)
    // The tab that took its position becomes active, not the first one.
    expect(tabs.activeTabId).toBe(third.id)
    expect(tabs.tabs.map((t) => t.id)).toEqual([first.id, third.id])
  })

  it('leaves no active tab once the last one closes', () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!
    tabs.close(tab.id)

    expect(tabs.tabs).toHaveLength(0)
    expect(tabs.activeTabId).toBeNull()
  })

  it('cancels an in-flight request when its tab is closed', () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!
    tabs.tabs[0]!.executionId = 'exec-1'

    tabs.close(tab.id)

    // Closing a tab must not leave a request running with nowhere to land.
    expect(mocks.httpCancel).toHaveBeenCalledWith('exec-1')
  })

  it('closes every other tab, cancelling their requests', () => {
    const tabs = useTabsStore()
    const keep = tabs.openDraft()!
    const other = tabs.openDraft()!
    tabs.tabs.find((t) => t.id === other.id)!.executionId = 'exec-2'

    tabs.closeOthers(keep.id)

    expect(tabs.tabs).toHaveLength(1)
    expect(tabs.activeTabId).toBe(keep.id)
    expect(mocks.httpCancel).toHaveBeenCalledWith('exec-2')
  })
})

describe('sending', () => {
  it('records a successful response', async () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!
    tabs.update(tab.id, { url: 'https://example.com' })

    const exchange: HttpExchange = {
      outcome: 'success',
      executionId: 'e1',
      request: { method: 'GET', url: 'https://example.com', headers: [], queryParams: [], body: { mode: 'none' }, auth: { type: 'none' }, settings: tab.doc.settings },
      response: {
        status: 200,
        statusText: 'OK',
        httpVersion: 'HTTP/1.1',
        headers: [],
        cookies: [],
        body: { kind: 'text', text: 'ok' },
        sizeBytes: 2,
        timings: { totalMs: 10, timeToFirstByteMs: 8, downloadMs: 2 },
        redirects: [],
        finalUrl: 'https://example.com',
        contentType: 'text/plain',
        isText: true,
      },
    }
    mocks.httpSendWithTests.mockResolvedValue({ exchange, tests: null })

    await tabs.send(tab.id)

    const result = tabs.tabs[0]!.result
    expect(result?.kind).toBe('response')
    expect(result?.kind === 'response' && result.response.status).toBe(200)
    expect(tabs.tabs[0]!.executionId).toBeNull()
  })

  it('records a transport failure without throwing', async () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!
    tabs.update(tab.id, { url: 'https://unreachable.example' })

    mocks.httpSendWithTests.mockResolvedValue({
      tests: null,
      exchange: {
      outcome: 'failure',
      executionId: 'e1',
      request: { method: 'GET', url: 'x', headers: [], queryParams: [], body: { mode: 'none' }, auth: { type: 'none' }, settings: tab.doc.settings },
      error: { kind: 'dnsFailure', message: 'Could not resolve the hostname.' },
      elapsedMs: 30,
      } satisfies HttpExchange,
    })

    await tabs.send(tab.id)

    const result = tabs.tabs[0]!.result
    expect(result?.kind).toBe('error')
    expect(result?.kind === 'error' && result.error.kind).toBe('dnsFailure')
  })

  it('records a cancellation', async () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!
    tabs.update(tab.id, { url: 'https://example.com' })

    mocks.httpSendWithTests.mockResolvedValue({
      exchange: {
        outcome: 'cancelled',
        executionId: 'e1',
        elapsedMs: 12,
      } satisfies HttpExchange,
      tests: null,
    })

    await tabs.send(tab.id)
    expect(tabs.tabs[0]!.result?.kind).toBe('cancelled')
  })

  it('refuses to send with no URL', async () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!

    await tabs.send(tab.id)

    expect(mocks.httpSendWithTests).not.toHaveBeenCalled()
  })

  it('does not start a second request while one is in flight', async () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!
    tabs.update(tab.id, { url: 'https://example.com' })
    tabs.tabs[0]!.executionId = 'already-running'

    await tabs.send(tab.id)

    expect(mocks.httpSendWithTests).not.toHaveBeenCalled()
  })

  it('clears the in-flight marker when sending throws', async () => {
    const tabs = useTabsStore()
    const tab = tabs.openDraft()!
    tabs.update(tab.id, { url: 'https://example.com' })

    mocks.httpSendWithTests.mockRejectedValue({ kind: 'interpolation', message: 'unresolved: baseUrl' })
    await tabs.send(tab.id)

    // A failure must not leave the Send button stuck showing Cancel.
    expect(tabs.tabs[0]!.executionId).toBeNull()
  })
})
