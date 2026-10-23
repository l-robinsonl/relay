/**
 * Not a test — a camera.
 *
 * Much of this interface has been written without anyone looking at
 * it, and the bugs that get found are layout ones no assertion
 * catches: a column shifted left, a button wrapping, text running
 * under an icon. This mounts the panel in each of its states and
 * writes the markup to disk so it can be rendered against the real
 * stylesheet and photographed.
 *
 * Skipped unless RELAY_SHOTS=1, so it costs a normal run nothing.
 */
import { describe, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import type { BillingView } from '@relay/shared-types'
import BillingPanel from './BillingPanel.vue'
import { api } from '@/services/ipc'

vi.mock('@/services/ipc', () => ({
  api: {
    syncBilling: vi.fn(),
    syncBillingCheckout: vi.fn(),
    syncBillingPortal: vi.fn(),
  },
}))

const mocked = vi.mocked(api)
const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

function view (over: Partial<BillingView> = {}): BillingView {
  return {
    metered: true,
    seats: 5,
    members: 2,
    active: true,
    hasSubscription: true,
    canManage: true,
    ...over,
  }
}

/** Every state the panel can be in, including the awkward ones. */
const STATES: { name: string, billing: BillingView }[] = [
  { name: '01-no-subscription', billing: view({ hasSubscription: false, seats: null, members: 3 }) },
  { name: '02-paid-and-healthy', billing: view() },
  { name: '03-over-the-limit', billing: view({ members: 7, seats: 5 }) },
  {
    name: '04-past-due',
    billing: view({
      members: 7,
      notice: 'A payment failed. This workspace still works; Stripe will try the card again.',
    }),
  },
  {
    name: '05-cancelled',
    billing: view({
      active: false,
      notice:
        "This workspace's subscription has ended. Your data is untouched and still syncs; "
        + 'you cannot add anybody new until it is renewed.',
    }),
  },
  { name: '06-not-an-admin', billing: view({ canManage: false, members: 7, seats: 5 }) },
  { name: '07-one-person', billing: view({ hasSubscription: false, seats: null, members: 1 }) },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('BillingPanel — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })

    for (const state of STATES) {
      vi.clearAllMocks()
      mocked.syncBilling.mockResolvedValue(state.billing)
      mocked.syncBillingPortal.mockResolvedValue('https://billing.stripe.com/p/session/y')

      const wrapper = mount(BillingPanel, { props: { workspaceId: 'w1', open: true } })
      await flushPromises()

      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }

    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
