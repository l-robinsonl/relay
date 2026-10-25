import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'

const { schema, grpcSchema } = vi.hoisted(() => ({
  grpcSchema: vi.fn(),
  schema: {
    services: [
      {
        name: 'relay.test.Echo',
        methods: [
          {
            name: 'Say',
            inputType: 'relay.test.Ping',
            outputType: 'relay.test.Pong',
            clientStreaming: false,
            serverStreaming: false,
            example: '{\n  "text": ""\n}',
          },
          {
            name: 'Watch',
            inputType: 'relay.test.Ping',
            outputType: 'relay.test.Pong',
            clientStreaming: false,
            serverStreaming: true,
            example: '{}',
          },
        ],
      },
    ],
  },
}))

vi.mock('@/services/ipc', () => ({ api: { grpcSchema } }))

import GrpcMethods from './GrpcMethods.vue'

const doc = { id: 'r1', kind: 'grpc', url: 'localhost:50051' } as never

function panel (selected = '') {
  return mount(GrpcMethods, { props: { doc, environment: null, selected } })
}

async function loaded (selected = '') {
  const wrapper = panel(selected)
  await wrapper.find('button').trigger('click')
  await flushPromises()
  return wrapper
}

beforeEach(() => {
  vi.clearAllMocks()
  grpcSchema.mockResolvedValue(schema)
})

describe('GrpcMethods', () => {
  it('reads nothing until asked', () => {
    panel()
    // A reflection call opens a connection; doing it on mount would
    // dial every server whose tab happens to be open.
    expect(grpcSchema).not.toHaveBeenCalled()
  })

  it('lists what the server serves', async () => {
    const wrapper = await loaded()
    expect(wrapper.text()).toContain('relay.test.Echo/')
    expect(wrapper.text()).toContain('Say')
    expect(wrapper.text()).toContain('unary')
  })

  it('choosing a method hands back its example body', async () => {
    // So the message editor is not an empty box against a type
    // nobody can see.
    const wrapper = await loaded()
    const say = wrapper.findAll('li button').find((b) => b.text().includes('Say'))
    await say!.trigger('click')

    expect(wrapper.emitted('choose')).toEqual([
      ['relay.test.Echo/Say', '{\n  "text": ""\n}'],
    ])
  })

  it('a method that cannot be called yet cannot be picked', async () => {
    // Listing it and failing on send would deliver the bad news
    // after the click instead of before it.
    const wrapper = await loaded()
    const watch = wrapper.findAll('li button').find((b) => b.text().includes('Watch'))

    expect(watch!.attributes('disabled')).toBeDefined()
    await watch!.trigger('click')
    expect(wrapper.emitted('choose')).toBeUndefined()
  })

  it('says how many it cannot call, once', async () => {
    const wrapper = await loaded()
    expect(wrapper.text()).toContain('1 streaming method is listed but cannot be called')
  })

  it('filters by the whole path, not just the method name', async () => {
    const wrapper = await loaded()
    await wrapper.find('input').setValue('echo/say')
    expect(wrapper.findAll('li')).toHaveLength(1)
  })

  it('a server with reflection switched off says so', async () => {
    grpcSchema.mockRejectedValue({ kind: 'invalid', message: 'unimplemented' })
    const wrapper = await loaded()

    expect(wrapper.text()).toContain('unimplemented')
    expect(wrapper.findAll('li')).toHaveLength(0)
  })
})
