import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import type { HttpExchange, RequestDoc } from '@relay/shared-types'
import type { RequestSection, ResponseTab, Tab, TabResult } from '@/types/ui'
import type { ScriptOutcome } from '@/services/ipc'
import { api, onExecutionProgress } from '@/services/ipc'
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
    tabs.value = tabs.value.map((tab) =>
      tab.id === tabId ? { ...tab, doc: { ...tab.doc, ...patch } } : tab,
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
  async function send (tabId: string) {
    const tab = tabs.value.find((t) => t.id === tabId)
    if (!tab || tab.executionId) return

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
    cancel,
    watchProgress,
  }
})
