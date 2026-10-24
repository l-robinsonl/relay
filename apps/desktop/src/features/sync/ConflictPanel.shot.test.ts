/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import type { ConflictView } from '@/services/ipc'
import ConflictPanel from './ConflictPanel.vue'
import { rendered } from '@/features/shots'
import { useSyncStore } from '@/stores/sync'

vi.mock('@/services/ipc', () => ({
  api: {
    syncConflicts: vi.fn(() => Promise.resolve([])),
    syncResolve: vi.fn(),
    syncStatus: vi.fn(),
  },
}))

const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

function conflict (over: Partial<ConflictView> = {}): ConflictView {
  return {
    id: 'cf1',
    entity: 'request',
    entityId: 'req_1',
    name: 'List invoices',
    local: { name: 'List invoices', method: 'GET', url: 'https://api.stripe.com/v1/invoices' },
    server: { name: 'List all invoices', method: 'GET', url: 'https://api.stripe.com/v1/invoices?limit=100' },
    serverDeleted: false,
    detectedAt: Date.now() - 2 * 3_600_000,
    ...over,
  }
}

const STATES: { name: string, conflicts: ConflictView[] }[] = [
  { name: '01-none', conflicts: [] },
  { name: '02-one-edit-clash', conflicts: [conflict()] },
  {
    name: '03-deleted-on-the-server',
    conflicts: [conflict({ id: 'cf2', serverDeleted: true, server: null, name: 'Void invoice' })],
  },
  {
    name: '04-several',
    conflicts: [
      conflict(),
      conflict({ id: 'cf2', name: 'Pay invoice', entityId: 'req_2' }),
      conflict({ id: 'cf3', entity: 'environment', name: 'Staging', entityId: 'env_1' }),
    ],
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('ConflictPanel — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })

    for (const state of STATES) {
      setActivePinia(createPinia())
      const sync = useSyncStore()
      sync.conflicts = state.conflicts as never

      const wrapper = mount(ConflictPanel, {
        props: { open: true },
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
