/**
 * A camera, not a test. See `BillingPanel.shot.test.ts`.
 *
 * This row is the most consequential thing in the interface: it is
 * where someone decides whether the assistant may send a request or
 * delete something. If it is unclear, or the buttons are easy to miss,
 * people will click the blue one to make it go away.
 *
 * Skipped unless RELAY_SHOTS=1.
 */
import { describe, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import ToolCallRow from './ToolCallRow.vue'
import type { ToolCall, ToolResult, ToolRisk } from '@/services/ipc'

const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

function call (name: string, input: unknown): ToolCall {
  return { id: 'call-1', name, input }
}

function result (content: string, isError = false): ToolResult {
  return { callId: 'call-1', outcome: { content, isError } }
}

const STATES: {
  name: string
  props: { call: ToolCall, result?: ToolResult | null, asking?: ToolRisk | null, busy?: boolean }
}[] = [
  {
    // `busy` is true for the whole turn, so a call with no result yet
    // is a spinner.
    name: '01-running',
    props: { call: call('list_requests', { collection: 'Billing API' }), busy: true },
  },
  {
    name: '02-finished',
    props: {
      call: call('list_requests', { collection: 'Billing API' }),
      result: result('4 requests: List invoices, Get invoice, Void invoice, Pay invoice'),
    },
  },
  {
    name: '03-failed',
    props: {
      call: call('get_request', { id: 'req_missing' }),
      result: result('no request with that id', true),
    },
  },
  {
    name: '04-asking-to-send',
    props: {
      call: call('send_request', { id: 'req_1', name: 'POST /v1/invoices/:id/void' }),
      asking: 'send',
      busy: true,
    },
  },
  {
    name: '05-asking-to-destroy',
    props: {
      call: call('delete_request', { id: 'req_1', name: 'Void invoice' }),
      asking: 'destructive',
      busy: true,
    },
  },
  {
    name: '06-long-input',
    props: {
      call: call('create_request', {
        name: 'Create subscription',
        method: 'POST',
        url: 'https://api.stripe.com/v1/subscriptions?expand[]=latest_invoice.payment_intent',
        body: 'customer={{customerId}}&items[0][price]={{priceId}}&items[0][quantity]=5',
      }),
      result: result('created req_9f2a'),
    },
  },
  {
    // A call with no result after the turn ended — abandoned window,
    // or a turn that died. It carries no status icon at all, which is
    // recorded here so a future look at this decides whether it
    // should.
    name: '07-never-finished',
    props: { call: call('send_request', { id: 'req_1' }), busy: false },
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('ToolCallRow — markup for screenshots', () => {
  it('writes every state to disk', () => {
    mkdirSync(OUT, { recursive: true })

    for (const state of STATES) {
      const wrapper = mount(ToolCallRow, { props: state.props })
      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }

    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
