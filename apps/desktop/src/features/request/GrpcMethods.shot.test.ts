/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'

const { schema } = vi.hoisted(() => ({ schema: {
  services: [
    {
      name: 'stripe.v1.Invoices',
      methods: [
        {
          name: 'Create',
          inputType: 'stripe.v1.CreateInvoiceRequest',
          outputType: 'stripe.v1.Invoice',
          clientStreaming: false,
          serverStreaming: false,
          example: '{\n  "customer": "",\n  "currency": ""\n}',
        },
        {
          name: 'List',
          inputType: 'stripe.v1.ListInvoicesRequest',
          outputType: 'stripe.v1.Invoice',
          clientStreaming: false,
          serverStreaming: true,
          example: '{}',
        },
      ],
    },
    {
      name: 'stripe.v1.Customers',
      methods: [
        {
          name: 'Upload',
          inputType: 'stripe.v1.Chunk',
          outputType: 'stripe.v1.Ack',
          clientStreaming: true,
          serverStreaming: false,
          example: '{}',
        },
      ],
    },
  ],
} }))

vi.mock('@/services/ipc', () => ({
  api: {
    grpcSchema: vi.fn().mockResolvedValue(schema),
  },
}))

import GrpcMethods from './GrpcMethods.vue'

const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

const doc = { id: 'r1', kind: 'grpc', url: 'localhost:50051' } as never

describe.skipIf(process.env.RELAY_SHOTS !== '1')('GrpcMethods — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })
    const names: string[] = []

    // Before anything has been read.
    const fresh = mount(GrpcMethods, {
      props: { doc, environment: null, selected: '' },
    })
    writeFileSync(`${OUT}/01-nothing-read-yet.html`, fresh.html())
    names.push('01-nothing-read-yet')
    fresh.unmount()

    // After reflection, with one method chosen.
    const loaded = mount(GrpcMethods, {
      props: { doc, environment: null, selected: 'stripe.v1.Invoices/Create' },
    })
    await loaded.find('button').trigger('click')
    await flushPromises()
    writeFileSync(`${OUT}/02-methods.html`, loaded.html())
    names.push('02-methods')
    loaded.unmount()

    writeFileSync(`${OUT}/states.json`, JSON.stringify(names, null, 2))
  })
})
