import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import type { Collection, Environment, TreeNode, Workspace } from '@relay/shared-types'
import { api } from '@/services/ipc'
import { useToastStore } from '@/stores/toast'

/** The workspace currently open, its tree, and its environments. */
export const useWorkspaceStore = defineStore('workspace', () => {
  const toasts = useToastStore()

  const workspaces = ref<Workspace[]>([])
  const current = ref<Workspace | null>(null)
  const tree = ref<TreeNode[]>([])
  const collections = ref<Collection[]>([])
  const environments = ref<Environment[]>([])
  const activeEnvironmentId = ref<string | null>(null)
  const expandedFolders = ref<Set<string>>(new Set())
  const loading = ref(false)
  const loaded = ref(false)

  const activeEnvironment = computed(
    () => environments.value.find((e) => e.id === activeEnvironmentId.value) ?? null,
  )

  /**
   * Folders and requests grouped by parent, so the sidebar can render a tree
   * from the flat list the backend sends without repeatedly filtering it.
   */
  const nodesByParent = computed(() => {
    const groups = new Map<string, TreeNode[]>()
    for (const node of tree.value) {
      const key = node.parentId ?? ''
      const bucket = groups.get(key)
      if (bucket) bucket.push(node)
      else groups.set(key, [node])
    }
    for (const bucket of groups.values()) {
      bucket.sort((a, b) => {
        // Folders before requests, then by explicit order, then by name.
        if (a.kind !== b.kind) return a.kind === 'folder' ? -1 : 1
        if (a.sortOrder !== b.sortOrder) return a.sortOrder - b.sortOrder
        return a.name.localeCompare(b.name)
      })
    }
    return groups
  })

  /** The folder chain for a collection, outermost first. */
  function folderChain (collectionId: string | null | undefined): Collection[] {
    if (!collectionId) return []
    const byId = new Map(collections.value.map((c) => [c.id, c]))
    const chain: Collection[] = []
    let cursor: string | null | undefined = collectionId
    // Bounded so a corrupted parent link cannot spin the UI thread.
    for (let hops = 0; cursor && hops < 100; hops += 1) {
      const collection: Collection | undefined = byId.get(cursor)
      if (!collection) break
      chain.unshift(collection)
      cursor = collection.parentId ?? null
    }
    return chain
  }

  async function loadWorkspaces () {
    workspaces.value = await api.workspaceList()
  }

  /** Loads the active workspace and everything it contains. */
  async function initialise () {
    loading.value = true
    try {
      const workspace = await api.workspaceActive()
      await loadWorkspaces()
      await select(workspace.id)
      loaded.value = true
    } catch (error) {
      toasts.error('Could not open your workspace', error)
    } finally {
      loading.value = false
    }
  }

  /** Switches to a workspace and loads its contents. */
  async function select (workspaceId: string) {
    const workspace =
      workspaces.value.find((w) => w.id === workspaceId) ??
      (await api.workspaceList()).find((w) => w.id === workspaceId)
    if (!workspace) return

    current.value = workspace
    await Promise.all([refreshTree(), refreshEnvironments()])
    await api.stateSet('active_workspace', workspaceId)

    const remembered = await api.stateGet(`active_environment:${workspaceId}`)
    activeEnvironmentId.value =
      remembered && environments.value.some((e) => e.id === remembered) ? remembered : null
  }

  async function refreshTree () {
    if (!current.value) return
    const workspaceId = current.value.id
    const [nextTree, nextCollections] = await Promise.all([
      api.collectionTree(workspaceId),
      api.collectionList(workspaceId),
    ])
    tree.value = nextTree
    collections.value = nextCollections
  }

  async function refreshEnvironments () {
    if (!current.value) return
    environments.value = await api.environmentList(current.value.id)
  }

  async function setActiveEnvironment (environmentId: string | null) {
    activeEnvironmentId.value = environmentId
    if (current.value) {
      await api.stateSet(`active_environment:${current.value.id}`, environmentId ?? '')
    }
  }

  async function createWorkspace (name: string) {
    const workspace = await api.workspaceCreate(name)
    await loadWorkspaces()
    await select(workspace.id)
    return workspace
  }

  async function createFolder (name: string, parentId: string | null = null) {
    if (!current.value) return null
    const collection = await api.collectionCreate(current.value.id, name, parentId)
    await refreshTree()
    if (parentId) expandedFolders.value.add(parentId)
    return collection
  }

  async function deleteFolder (id: string) {
    await api.collectionDelete(id)
    await refreshTree()
  }

  async function renameFolder (id: string, name: string) {
    const collection = collections.value.find((c) => c.id === id)
    if (!collection) return
    await api.collectionSave({ ...collection, name })
    await refreshTree()
  }

  async function createEnvironment (name: string) {
    if (!current.value) return null
    const environment = await api.environmentCreate(current.value.id, name)
    await refreshEnvironments()
    return environment
  }

  async function saveEnvironment (environment: Environment) {
    await api.environmentSave(environment)
    await refreshEnvironments()
  }

  async function deleteEnvironment (id: string) {
    await api.environmentDelete(id)
    if (activeEnvironmentId.value === id) await setActiveEnvironment(null)
    await refreshEnvironments()
  }

  /** True when `ancestorId` is at or above `nodeId` in the tree. */
  function isAncestor (ancestorId: string, nodeId: string): boolean {
    const byId = new Map(collections.value.map((c) => [c.id, c]))
    let cursor: string | null | undefined = nodeId
    for (let hops = 0; cursor && hops < 100; hops += 1) {
      if (cursor === ancestorId) return true
      cursor = byId.get(cursor)?.parentId ?? null
    }
    return false
  }

  /** Moves a request or folder to a new parent and position. */
  async function moveNode (
    node: TreeNode,
    parentId: string | null,
    sortOrder: number,
  ): Promise<void> {
    try {
      if (node.kind === 'request') {
        await api.requestMove(node.id, parentId, sortOrder)
      } else {
        const collection = collections.value.find((c) => c.id === node.id)
        if (!collection) return
        await api.collectionSave({ ...collection, parentId, sortOrder })
      }
      await refreshTree()
      if (parentId) expandedFolders.value = new Set(expandedFolders.value).add(parentId)
    } catch (error) {
      // The most likely rejection is a move that would detach a subtree,
      // which the backend refuses outright.
      toasts.error('That move is not allowed', error)
      await refreshTree()
    }
  }

  function toggleFolder (id: string) {
    // A new Set so Vue's reactivity sees the change; mutating in place does
    // not trigger dependents for Set.
    const next = new Set(expandedFolders.value)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    expandedFolders.value = next
  }

  function isExpanded (id: string) {
    return expandedFolders.value.has(id)
  }

  return {
    workspaces,
    current,
    tree,
    collections,
    environments,
    activeEnvironmentId,
    activeEnvironment,
    expandedFolders,
    loading,
    loaded,
    nodesByParent,
    folderChain,
    initialise,
    loadWorkspaces,
    select,
    refreshTree,
    refreshEnvironments,
    setActiveEnvironment,
    createWorkspace,
    createFolder,
    deleteFolder,
    renameFolder,
    createEnvironment,
    saveEnvironment,
    deleteEnvironment,
    toggleFolder,
    isExpanded,
    isAncestor,
    moveNode,
  }
})
