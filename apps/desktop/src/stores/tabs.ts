import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import type { HttpExchange, HttpResponseData, RequestDoc } from '@relay/shared-types'
import type { RequestSection, ResponseTab, Tab, TabResult } from '@/types/ui'
import type { ScriptOutcome } from '@/services/ipc'
import type { StreamEvent } from '@relay/shared-types'
import { api, onExecutionProgress, onStreamEvent, toRelayError } from '@/services/ipc'
import { useWorkspaceStore } from '@/stores/workspace'
import { useToastStore } from '@/stores/toast'

/** A blank request, used for new tabs. */
function draftDoc (workspaceId: string, collectionId: string | null = null): RequestDoc {
  const now = Date.now()
  return {
    id: crypto.randomUUID(),
    workspaceId,
    collectionId,
    name: 'Untitled request',
    description: '',
    kind: 'http',
    method: 'GET',
    url: '',
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
    createdAt: now,
    updatedAt: now,
    rev: 0,
    dirty: false,
  }
}

/** A stable fingerprint of a document, for detecting unsaved changes. */
function fingerprint (doc: RequestDoc): string {
  // updatedAt and the revision fields change on save without the user having
  // edited anything, so they are excluded or the tab would always look dirty.
  const { updatedAt: _u, rev: _r, dirty: _d, ...rest } = doc
  return JSON.stringify(rest)
}

function newTab (doc: RequestDoc, requestId: string | null): Tab {
  return {
    id: crypto.randomUUID(),
    requestId,
    doc,
    savedSnapshot: requestId ? fingerprint(doc) : null,
    result: null,
    executionId: null,
    bytesRead: 0,
    activeSection: 'params',
    activeResponseTab: 'body',
    stream: [],
    streaming: false,
    streamError: null,
    tests: null,
  }
}

/** Open editor tabs and request execution. */
export const useTabsStore = defineStore('tabs', () => {
  const workspace = useWorkspaceStore()
  const toasts = useToastStore()

  const tabs = ref<Tab[]>([])
  const activeTabId = ref<string | null>(null)

  const activeTab = computed(() => tabs.value.find((t) => t.id === activeTabId.value) ?? null)
  const hasUnsavedChanges = computed(() => tabs.value.some(isDirty))

  function isDirty (tab: Tab): boolean {
    if (!tab.requestId) return true
    return tab.savedSnapshot !== fingerprint(tab.doc)
  }

  function openDraft (collectionId: string | null = null) {
    if (!workspace.current) return null
    const tab = newTab(draftDoc(workspace.current.id, collectionId), null)
    tabs.value = [...tabs.value, tab]
    activeTabId.value = tab.id
    return tab
  }

  /** Opens a saved request, focusing it if it is already open. */
  async function openRequest (requestId: string) {
    const existing = tabs.value.find((t) => t.requestId === requestId)
    if (existing) {
      activeTabId.value = existing.id
      return existing
    }

    try {
      const doc = await api.requestGet(requestId)
      const tab = newTab(doc, requestId)
      tabs.value = [...tabs.value, tab]
      activeTabId.value = tab.id
      return tab
    } catch (error) {
      toasts.error('Could not open that request', error)
      return null
    }
  }

  function close (tabId: string) {
    const index = tabs.value.findIndex((t) => t.id === tabId)
    if (index === -1) return

    const tab = tabs.value[index]
    // Closing a tab must not leave a request running with nothing to show it.
    if (tab?.executionId) void api.httpCancel(tab.executionId)

    tabs.value = tabs.value.filter((t) => t.id !== tabId)

    if (activeTabId.value === tabId) {
      const next = tabs.value[index] ?? tabs.value[index - 1] ?? null
      activeTabId.value = next?.id ?? null
    }
  }

  function closeOthers (tabId: string) {
    for (const tab of tabs.value) {
      if (tab.id !== tabId && tab.executionId) void api.httpCancel(tab.executionId)
    }
    tabs.value = tabs.value.filter((t) => t.id === tabId)
    activeTabId.value = tabId
  }

  function activate (tabId: string) {
    activeTabId.value = tabId
  }

  function update (tabId: string, patch: Partial<RequestDoc>) {
    const tab = tabs.value.find((t) => t.id === tabId)
    // Changing the protocol while something is connected would strand
    // it: the stop button reads the kind to decide how to stop, so a
    // switch to GET leaves a live stream nothing can reach.
    if (
      tab &&
      patch.kind &&
      patch.kind !== tab.doc.kind &&
      tab.executionId &&
      tab.doc.kind !== 'grpc'
    ) {
      void cancel(tabId)
    }

    tabs.value = tabs.value.map((t) =>
      t.id === tabId ? { ...t, doc: { ...t.doc, ...patch } } : t,
    )
  }

  function setSection (tabId: string, section: RequestSection) {
    tabs.value = tabs.value.map((t) => (t.id === tabId ? { ...t, activeSection: section } : t))
  }

  function setResponseTab (tabId: string, responseTab: ResponseTab) {
    tabs.value = tabs.value.map((t) =>
      t.id === tabId ? { ...t, activeResponseTab: responseTab } : t,
    )
  }

  /** Saves a tab's request, creating it on first save. */
  async function save (tabId: string) {
    const tab = tabs.value.find((t) => t.id === tabId)
    if (!tab) return null

    try {
      const saved = await api.requestSave(tab.doc)
      tabs.value = tabs.value.map((t) =>
        t.id === tabId
          ? { ...t, doc: saved, requestId: saved.id, savedSnapshot: fingerprint(saved) }
          : t,
      )
      await workspace.refreshTree()
      return saved
    } catch (error) {
      toasts.error('Could not save the request', error)
      return null
    }
  }

  /** Sends the tab's request. */
  /**
   * Opens a stream, or closes one that is already open.
   *
   * Send and stop are the same button for a connection: there is no
   * other moment at which somebody wants to end it, and a separate
   * stop button would sit disabled almost all the time.
   */
  async function toggleStream (tabId: string) {
    const tab = tabs.value.find((t) => t.id === tabId)
    if (!tab) return

    if (tab.executionId) {
      await api.streamClose(tab.executionId)
      return
    }

    if (!tab.doc.url.trim()) {
      toasts.warn('Enter a URL before connecting')
      return
    }

    const executionId = crypto.randomUUID()
    patchTab(tabId, { executionId, streaming: true, streamError: null })

    try {
      await api.streamOpen(executionId, tab.doc, workspace.activeEnvironmentId)
    } catch (error) {
      patchTab(tabId, { executionId: null, streaming: false })
      toasts.error('The connection could not be opened', error)
    }
  }

  /** Applies a stream event to whichever tab is running it. */
  function applyStreamEvent (event: StreamEvent) {
    const tab = tabs.value.find((t) => t.executionId === event.executionId)
    if (!tab) return

    switch (event.type) {
      case 'open':
        patchTab(tab.id, { streaming: true, streamError: null })
        break
      case 'message':
        // A long-running stream would otherwise grow without bound
        // and take the window down with it. The newest are the ones
        // being read.
        patchTab(tab.id, {
          stream: [...tab.stream, { at: event.at, message: event.message }].slice(-2000),
        })
        break
      case 'closed':
        patchTab(tab.id, {
          executionId: null,
          streaming: false,
          streamError: event.error ?? null,
        })
        break
    }
  }

  /**
   * Calls a gRPC method and shows the answer where a response goes.
   *
   * Presented as a response rather than as a stream log: a unary call
   * has one answer, and putting it anywhere else would mean two
   * places to look for the same thing.
   */
  async function sendGrpc (tabId: string) {
    const tab = tabs.value.find((t) => t.id === tabId)
    if (!tab) return

    const executionId = crypto.randomUUID()
    patchTab(tabId, { executionId, result: null, tests: null, bytesRead: 0 })

    const started = performance.now()
    try {
      const json = await api.grpcSend(tab.doc, workspace.activeEnvironmentId)
      patchTab(tabId, {
        result: {
          kind: 'response',
          response: grpcResponse(json, performance.now() - started),
        },
      })
    } catch (error) {
      // gRPC's own status is the useful part — `NotFound: no such
      // customer`, not "the call failed" — and it arrives as the
      // error's message.
      const relay = toRelayError(error)
      patchTab(tabId, {
        result: {
          kind: 'error',
          error: { kind: 'other', message: relay.message, detail: relay.detail ?? null },
          elapsedMs: performance.now() - started,
        },
      })
    } finally {
      patchTab(tabId, { executionId: null })
    }
  }

  /**
   * Dresses a gRPC answer as a response so one pane shows both.
   *
   * There is no status line on a successful gRPC call — `OK` is the
   * absence of a `grpc-status` — so 200 stands in for it, and the
   * body viewer does the rest.
   */
  function grpcResponse (json: string, elapsedMs: number): HttpResponseData {
    const bytes = new TextEncoder().encode(json).length
    return {
      status: 200,
      statusText: 'OK',
      httpVersion: 'HTTP/2',
      headers: [],
      cookies: [],
      body: { kind: 'text', text: json },
      sizeBytes: bytes,
      decodedSizeBytes: null,
      timings: { totalMs: Math.round(elapsedMs) } as never,
      redirects: [],
      finalUrl: '',
      contentType: 'application/json',
      isText: true,
    }
  }

  /**
   * Sends a message on the tab's open websocket.
   *
   * Nothing is added to the log here. The frame is logged by Rust
   * once it has actually gone out, so the transcript records what
   * happened rather than what was attempted.
   */
  async function streamSend (tabId: string, text: string) {
    const tab = tabs.value.find((t) => t.id === tabId)
    if (!tab?.executionId) {
      toasts.warn('Nothing is connected')
      return
    }
    try {
      await api.streamSend(tab.executionId, text)
    } catch (error) {
      toasts.error('The message could not be sent', error)
    }
  }

  function clearStream (tabId: string) {
    patchTab(tabId, { stream: [], streamError: null })
  }

  async function send (tabId: string) {
    const tab = tabs.value.find((t) => t.id === tabId)
    if (!tab || tab.executionId) return

    // A connection is not a request: it has no single response, and
    // the same button opens and closes it.
    if (tab.doc.kind === 'sse' || tab.doc.kind === 'websocket') {
      await toggleStream(tabId)
      return
    }

    // gRPC answers once, like a request, but over a protocol the HTTP
    // engine knows nothing about.
    if (tab.doc.kind === 'grpc') {
      await sendGrpc(tabId)
      return
    }

    if (!tab.doc.url.trim()) {
      toasts.warn('Enter a URL before sending')
      return
    }

    const executionId = crypto.randomUUID()
    patchTab(tabId, { executionId, bytesRead: 0, result: null, tests: null })

    try {
      const outcome = await api.httpSendWithTests(
        executionId,
        tab.doc,
        workspace.activeEnvironmentId,
      )
      patchTab(tabId, {
        result: toResult(outcome.exchange),
        tests: outcome.tests ?? null,
        executionId: null,
      })

      // Jump to the results when a test failed: a red count in a tab the
      // user is not looking at is easy to miss.
      if (outcome.tests && !scriptPassed(outcome.tests)) {
        patchTab(tabId, { activeResponseTab: 'tests' })
      }

      // A script may have captured a token into a workspace variable.
      if (outcome.tests && Object.keys(outcome.tests.variables).length > 0) {
        await workspace.loadWorkspaces()
      }
    } catch (error) {
      patchTab(tabId, { executionId: null })
      toasts.error('The request could not be sent', error)
    }
  }

  /** True when a script reported nothing wrong. */
  function scriptPassed (outcome: ScriptOutcome): boolean {
    if (outcome.error) return false
    if (outcome.looseAssertions.some((a) => !a.passed)) return false
    return outcome.tests.every((t) => !t.error && t.assertions.every((a) => a.passed))
  }

  /** Cancels a tab's in-flight request. */
  async function cancel (tabId: string) {
    const tab = tabs.value.find((t) => t.id === tabId)
    if (!tab?.executionId) return
    // A stream is not cancelled, it is closed — and `http_cancel` has
    // never heard of it, so stopping one would silently do nothing.
    if (tab.doc.kind === 'sse' || tab.doc.kind === 'websocket') {
      await api.streamClose(tab.executionId)
      return
    }
    await api.httpCancel(tab.executionId)
  }

  function patchTab (tabId: string, patch: Partial<Tab>) {
    tabs.value = tabs.value.map((t) => (t.id === tabId ? { ...t, ...patch } : t))
  }

  function toResult (exchange: HttpExchange): TabResult {
    switch (exchange.outcome) {
      case 'success':
        return { kind: 'response', response: exchange.response }
      case 'failure':
        return { kind: 'error', error: exchange.error, elapsedMs: exchange.elapsedMs }
      case 'cancelled':
        return { kind: 'cancelled', elapsedMs: exchange.elapsedMs }
    }
  }

  /** Starts forwarding engine progress into the matching tab. */
  async function watchProgress () {
    return onExecutionProgress((progress) => {
      tabs.value = tabs.value.map((tab) =>
        tab.executionId === progress.executionId
          ? { ...tab, bytesRead: progress.bytesRead }
          : tab,
      )
    })
  }

  /** Starts forwarding stream events into the matching tab. */
  async function watchStreams () {
    return onStreamEvent(applyStreamEvent)
  }

  /** Forgets a closed request's tab, after it is deleted elsewhere. */
  function closeByRequestId (requestId: string) {
    const tab = tabs.value.find((t) => t.requestId === requestId)
    if (tab) close(tab.id)
  }

  return {
    tabs,
    activeTabId,
    activeTab,
    hasUnsavedChanges,
    isDirty,
    openDraft,
    openRequest,
    close,
    closeOthers,
    closeByRequestId,
    activate,
    update,
    setSection,
    setResponseTab,
    scriptPassed,
    save,
    send,
    toggleStream,
    watchStreams,
    applyStreamEvent,
    sendGrpc,
    streamSend,
    clearStream,
    cancel,
    watchProgress,
  }
})
