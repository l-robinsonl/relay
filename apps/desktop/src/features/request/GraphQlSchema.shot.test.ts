/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import type { Schema } from '@relay/shared-types'
import GraphQlSchema from './GraphQlSchema.vue'
import { api } from '@/services/ipc'

vi.mock('@/services/ipc', () => ({ api: { graphqlSchema: vi.fn() } }))

const mocked = vi.mocked(api)
const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

const SCHEMA: Schema = {
  queries: [
    {
      name: 'invoices',
      description: 'Every invoice, newest first.',
      typeName: '[Invoice!]!',
      args: [
        { name: 'limit', typeName: 'Int', description: 'How many to return.' },
        { name: 'status', typeName: 'InvoiceStatus', description: null },
      ],
    },
    { name: 'customer', description: null, typeName: 'Customer', args: [] },
  ],
  mutations: [
    { name: 'voidInvoice', description: 'Cancels an unpaid invoice.', typeName: 'Invoice!', args: [] },
  ],
  subscriptions: [],
  types: [],
}

const DOC = { id: 'r1', url: 'https://api.example.com/graphql' } as never

async function shoot (name: string, prepare: () => void, after?: (w: never) => Promise<void>) {
  vi.clearAllMocks()
  prepare()
  const wrapper = mount(GraphQlSchema, { props: { doc: DOC, environment: null } })
  await flushPromises()
  if (after) await after(wrapper as never)
  writeFileSync(`${OUT}/${name}.html`, wrapper.html())
  wrapper.unmount()
}

async function press (wrapper: never, label: string) {
  const w = wrapper as unknown as { findAll: (s: string) => { text: () => string, trigger: (e: string) => Promise<unknown> }[] }
  const button = w.findAll('button').find((b) => b.text().includes(label))
  if (button) await button.trigger('click')
  await flushPromises()
}

describe.skipIf(process.env.RELAY_SHOTS !== '1')('GraphQlSchema — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })
    const states: string[] = []

    await shoot('01-unasked', () => {
      mocked.graphqlSchema.mockResolvedValue(SCHEMA)
    })
    states.push('01-unasked')

    await shoot('02-loaded', () => {
      mocked.graphqlSchema.mockResolvedValue(SCHEMA)
    }, async (w) => { await press(w, 'Read schema') })
    states.push('02-loaded')

    await shoot('03-expanded', () => {
      mocked.graphqlSchema.mockResolvedValue(SCHEMA)
    }, async (w) => {
      await press(w, 'Read schema')
      await press(w, 'invoices')
    })
    states.push('03-expanded')

    await shoot('04-introspection-off', () => {
      mocked.graphqlSchema.mockRejectedValue({
        message: 'GraphQL introspection is not allowed',
      })
    }, async (w) => { await press(w, 'Read schema') })
    states.push('04-introspection-off')

    writeFileSync(`${OUT}/states.json`, JSON.stringify(states, null, 2))
  })
})
