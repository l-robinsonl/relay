import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type {
  Collection,
  Environment,
  ExecutionProgress,
  HistoryEntry,
  HttpExchange,
  ImportReport,
  OAuth2Config,
  InvitationOffer,
  InvitationView,
  MemberView,
  RelayError,
  RequestDoc,
  Role,
  UserProfile,
  TreeNode,
  Workspace,
} from '@relay/shared-types'

/**
 * The typed boundary between the interface and the Rust core.
 *
 * Every call goes through `call`, so there is exactly one place that knows
 * how a Rust `Result` arrives and how to turn a rejection back into a
 * `RelayError` the UI can branch on.
 */

/** What a sync server reports about itself. */
export interface ServerHealth {
  status: string
  backend: string
  protocol: string
  version: string
}

/** How a workspace's synchronisation stands. */
export interface SyncStatus {
  connected: boolean
  serverUrl: string | null
  userEmail: string | null
  signedIn: boolean
  lastPulledSeq: number
  lastSyncAt: number | null
  lastError: string | null
  pendingChanges: number
  openConflicts: number
}

/** What one synchronisation pass did. */
export interface SyncOutcome {
  pulled: number
  pushed: number
  conflicts: number
  rejected: number
  head: number
}

/** A conflict awaiting the user's decision. */
export interface ConflictView {
  id: string
  entity: string
  entityId: string
  name: string | null
  local: Record<string, unknown>
  server: Record<string, unknown> | null
  serverDeleted: boolean
  detectedAt: number
}

/** One assertion's verdict. */
export interface Assertion {
  passed: boolean
  message: string
}

/** A named group of assertions. */
export interface TestCase {
  name: string
  assertions: Assertion[]
  error?: string | null
}

/** A line a script printed. */
export interface ConsoleLine {
  level: 'log' | 'warn' | 'error'
  text: string
}

/** Everything a script run produced. */
export interface ScriptOutcome {
  tests: TestCase[]
  looseAssertions: Assertion[]
  console: ConsoleLine[]
  variables: Record<string, string>
  error?: string | null
  durationMs: number
}

/**
 * What the Auth tab knows about a held OAuth token.
 *
 * Carries no token value: the interface needs to say whether one is held
 * and when it lapses, and has no reason to hold the credential itself.
 */
export interface OAuthTokenStatus {
  hasToken: boolean
  expiresAt?: number | null
  scope?: string | null
  canRefresh: boolean
}

/** An exchange together with whatever its tests reported. */
export interface SendOutcome {
  exchange: HttpExchange
  tests?: ScriptOutcome | null
}

/** A resolved request, as returned by `request_resolve`. */
export interface ResolvedPreview {
  spec: import('@relay/shared-types').HttpRequestSpec
  missingVariables: string[]
}

/** True when a thrown value is a structured error from the core. */
export function isRelayError (value: unknown): value is RelayError {
  return (
    typeof value === 'object' &&
    value !== null &&
    'kind' in value &&
    'message' in value &&
    typeof (value as RelayError).message === 'string'
  )
}

/** Turns anything thrown by `invoke` into a `RelayError`. */
export function toRelayError (value: unknown): RelayError {
  if (isRelayError(value)) return value
  return {
    kind: 'internal',
    message: value instanceof Error ? value.message : String(value),
  }
}

/**
 * Invokes a Tauri command.
 *
 * Rejections are normalised so callers never have to guess whether they
 * caught a `RelayError`, an `Error`, or a bare string from the IPC layer.
 */
async function call<T> (command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw toRelayError(error)
  }
}

export const api = {
  // --- workspaces ---------------------------------------------------------
  workspaceList: () => call<Workspace[]>('workspace_list'),
  workspaceActive: () => call<Workspace>('workspace_active'),
  workspaceCreate: (name: string) => call<Workspace>('workspace_create', { name }),
  workspaceSave: (workspace: Workspace) => call<Workspace>('workspace_save', { workspace }),
  workspaceDelete: (id: string) => call<void>('workspace_delete', { id }),

  // --- tree and collections -----------------------------------------------
  collectionTree: (workspaceId: string) => call<TreeNode[]>('collection_tree', { workspaceId }),
  collectionList: (workspaceId: string) => call<Collection[]>('collection_list', { workspaceId }),
  collectionCreate: (workspaceId: string, name: string, parentId?: string | null) =>
    call<Collection>('collection_create', { workspaceId, name, parentId: parentId ?? null }),
  collectionSave: (collection: Collection) =>
    call<Collection>('collection_save', { collection }),
  collectionDelete: (id: string) => call<void>('collection_delete', { id }),

  // --- requests ------------------------------------------------------------
  requestGet: (id: string) => call<RequestDoc>('request_get', { id }),
  requestCreate: (workspaceId: string, name: string, collectionId?: string | null) =>
    call<RequestDoc>('request_create', { workspaceId, name, collectionId: collectionId ?? null }),
  requestSave: (doc: RequestDoc) => call<RequestDoc>('request_save', { doc }),
  requestDelete: (id: string) => call<void>('request_delete', { id }),
  requestDuplicate: (id: string) => call<RequestDoc>('request_duplicate', { id }),
  requestMove: (id: string, collectionId: string | null, sortOrder: number) =>
    call<void>('request_move', { id, collectionId, sortOrder }),
  requestResolve: (doc: RequestDoc, environmentId?: string | null) =>
    call<ResolvedPreview>('request_resolve', { doc, environmentId: environmentId ?? null }),

  // --- execution -----------------------------------------------------------
  httpSend: (executionId: string, doc: RequestDoc, environmentId?: string | null) =>
    call<HttpExchange>('http_send', { executionId, doc, environmentId: environmentId ?? null }),
  httpSendWithTests: (
    executionId: string,
    doc: RequestDoc,
    environmentId?: string | null,
  ) =>
    call<SendOutcome>('http_send_with_tests', {
      executionId,
      doc,
      environmentId: environmentId ?? null,
    }),
  httpCancel: (executionId: string) => call<boolean>('http_cancel', { executionId }),
  cookiesClear: () => call<void>('cookies_clear'),

  // --- OAuth 2.0 -----------------------------------------------------------
  oauthTokenStatus: (config: OAuth2Config) =>
    call<OAuthTokenStatus>('oauth_token_status', { config }),
  oauthFetchToken: (config: OAuth2Config) =>
    call<OAuthTokenStatus>('oauth_fetch_token', { config }),
  oauthAuthorize: (config: OAuth2Config) =>
    call<OAuthTokenStatus>('oauth_authorize', { config }),
  oauthForgetToken: (config: OAuth2Config) =>
    call<void>('oauth_forget_token', { config }),
  oauthForgetAllTokens: () => call<void>('oauth_forget_all_tokens'),

  // --- environments and secrets --------------------------------------------
  environmentList: (workspaceId: string) =>
    call<Environment[]>('environment_list', { workspaceId }),
  environmentCreate: (workspaceId: string, name: string) =>
    call<Environment>('environment_create', { workspaceId, name }),
  environmentSave: (environment: Environment) =>
    call<Environment>('environment_save', { environment }),
  environmentDelete: (id: string) => call<void>('environment_delete', { id }),
  secretSet: (environmentId: string, key: string, value: string) =>
    call<void>('secret_set', { environmentId, key, value }),
  secretDelete: (environmentId: string, key: string) =>
    call<void>('secret_delete', { environmentId, key }),
  secretIsSet: (environmentId: string, key: string) =>
    call<boolean>('secret_is_set', { environmentId, key }),
  secretBackend: () => call<string>('secret_backend'),

  // --- history --------------------------------------------------------------
  historyList: (workspaceId: string, limit = 100, offset = 0) =>
    call<HistoryEntry[]>('history_list', { workspaceId, limit, offset }),
  historyClear: (workspaceId: string) => call<number>('history_clear', { workspaceId }),

  // --- import and export ------------------------------------------------------
  importCurl: (command: string, workspaceId: string) =>
    call<RequestDoc>('import_curl', { command, workspaceId }),
  importPreview: (content: string, workspaceId: string) =>
    call<ImportReport>('import_preview', { content, workspaceId }),
  importPreviewPath: (path: string, workspaceId: string) =>
    call<ImportReport>('import_preview_path', { path, workspaceId }),
  importApply: (report: ImportReport) => call<number>('import_apply', { report }),
  exportWorkspaceToPath: (workspaceId: string, path: string) =>
    call<void>('export_workspace_to_path', { workspaceId, path }),
  exportWorkspace: (workspaceId: string) =>
    call<string>('export_workspace', { workspaceId }),
  exportCurl: (doc: RequestDoc, environmentId?: string | null) =>
    call<string>('export_curl', { doc, environmentId: environmentId ?? null }),

  // --- application state --------------------------------------------------------
  // --- synchronisation ----------------------------------------------------
  syncProbe: (serverUrl: string) => call<ServerHealth>('sync_probe', { serverUrl }),
  // --- membership ----------------------------------------------------------
  syncMembers: (workspaceId: string) =>
    call<MemberView[]>('sync_members', { workspaceId }),
  syncSetMemberRole: (workspaceId: string, userId: string, role: Role) =>
    call<void>('sync_set_member_role', { workspaceId, userId, role }),
  syncRemoveMember: (workspaceId: string, userId: string) =>
    call<void>('sync_remove_member', { workspaceId, userId }),
  syncInvite: (workspaceId: string, email: string, role: Role) =>
    call<InvitationView>('sync_invite', { workspaceId, email, role }),
  syncInvitations: (workspaceId: string) =>
    call<InvitationView[]>('sync_invitations', { workspaceId }),
  syncRevokeInvitation: (workspaceId: string, invitationId: string) =>
    call<void>('sync_revoke_invitation', { workspaceId, invitationId }),

  syncRegister: (
    serverUrl: string,
    email: string,
    password: string,
    displayName: string,
    invitation?: string | null,
  ) =>
    call<UserProfile>('sync_register', {
      serverUrl,
      email,
      password,
      displayName,
      invitation: invitation ?? null,
    }),
  syncInvitationOffer: (serverUrl: string, token: string) =>
    call<InvitationOffer>('sync_invitation_offer', { serverUrl, token }),
  syncJoinInvited: (
    serverUrl: string,
    invitation: string,
    password: string,
    displayName: string,
  ) =>
    call<Workspace>('sync_join_invited', { serverUrl, invitation, password, displayName }),
  syncRequestVerification: (serverUrl: string, email: string) =>
    call<void>('sync_request_verification', { serverUrl, email }),
  syncRequestPasswordReset: (serverUrl: string, email: string) =>
    call<void>('sync_request_password_reset', { serverUrl, email }),
  syncConnect: (
    workspaceId: string,
    serverUrl: string,
    email: string,
    password: string,
    join: boolean,
  ) => call<SyncStatus>('sync_connect', { workspaceId, serverUrl, email, password, join }),
  syncNow: (workspaceId: string) => call<SyncOutcome>('sync_now', { workspaceId }),
  syncStatus: (workspaceId: string) => call<SyncStatus>('sync_status', { workspaceId }),
  syncDisconnect: (workspaceId: string) => call<void>('sync_disconnect', { workspaceId }),
  syncConflicts: (workspaceId: string) => call<ConflictView[]>('sync_conflicts', { workspaceId }),
  syncResolve: (conflictId: string, keepLocal: boolean) =>
    call<void>('sync_resolve', { conflictId, keepLocal }),
  syncWatch: (workspaceId: string) => call<boolean>('sync_watch', { workspaceId }),
  syncUnwatch: (workspaceId: string) => call<void>('sync_unwatch', { workspaceId }),

  // --- application state --------------------------------------------------
  stateGet: (key: string) => call<string | null>('state_get', { key }),
  stateSet: (key: string, value: string) => call<void>('state_set', { key, value }),
  databaseBackup: (destination: string) => call<void>('database_backup', { destination }),
}

/** Subscribes to progress events for in-flight requests. */
export function onExecutionProgress (
  handler: (progress: ExecutionProgress) => void,
): Promise<UnlistenFn> {
  return listen<ExecutionProgress>('relay://exec-progress', (event) => handler(event.payload))
}

/** What the server-change listener reports. */
export interface SyncEvent {
  workspaceId: string
  event: 'connected' | 'changed' | 'disconnected'
  seq?: number
}

/** Subscribes to live change notifications from a sync server. */
export function onSyncEvent (handler: (event: SyncEvent) => void): Promise<UnlistenFn> {
  return listen<SyncEvent>('relay://sync-changed', (event) => handler(event.payload))
}
