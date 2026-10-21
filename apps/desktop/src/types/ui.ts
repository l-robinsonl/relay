import type { HttpResponseData, RequestDoc, TransportError } from '@relay/shared-types'

/** An editor tab. */
export interface Tab {
  id: string
  /** The saved request this tab edits, or null for an unsaved draft. */
  requestId: string | null
  /** The working copy, which may differ from what is saved. */
  doc: RequestDoc
  /** The last saved state, used to detect unsaved changes. */
  savedSnapshot: string | null
  /** The result of the most recent send, if any. */
  result: TabResult | null
  /** The id of an in-flight execution, or null. */
  executionId: string | null
  /** Bytes received so far by the in-flight request. */
  bytesRead: number
  /** Which request-configuration tab is showing. */
  activeSection: RequestSection
  /** Which response tab is showing. */
  activeResponseTab: ResponseTab
}

/** The outcome shown in the response panel. */
export type TabResult =
  | { kind: 'response'; response: HttpResponseData }
  | { kind: 'error'; error: TransportError; elapsedMs: number }
  | { kind: 'cancelled'; elapsedMs: number }

/** Sections of the request editor. */
export type RequestSection = 'params' | 'headers' | 'body' | 'auth' | 'scripts' | 'settings'

/** Sections of the response panel. */
export type ResponseTab = 'body' | 'headers' | 'cookies' | 'tests'

/** The sidebar's view. */
export type SidebarView = 'collections' | 'environments' | 'history'

/** A transient message shown to the user. */
export interface Toast {
  id: number
  tone: 'info' | 'success' | 'warning' | 'danger'
  message: string
  detail?: string
}
