import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
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

const PLANS = [
  {
    id: 'sync-annual',
    label: 'Sync, billed yearly',
    includesAssistant: false,
    interval: 'year',
    amount: 2400,
    currency: 'gbp',
  },
  {
    id: 'sync-monthly',
    label: 'Sync, billed monthly',
    includesAssistant: false,
    interval: 'month',
    amount: 300,
    currency: 'gbp',
  },
  {
    id: 'assistant-annual',
    label: 'Sync and the assistant, billed yearly',
    includesAssistant: true,
    interval: 'year',
    amount: 6000,
    currency: 'gbp',
  },
]

function view (over: Partial<BillingView> = {}): BillingView {
  return {
    metered: true,
    seats: 5,
    members: 2,
    active: true,
    hasSubscription: true,
    canManage: true,
    plans: PLANS,
    ...over,
  }
}

async function panel () {
  const wrapper = mount(BillingPanel, { props: { workspaceId: 'w1', open: true } })
  await flushPromises()
  return wrapper
}

describe('BillingPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocked.syncBilling.mockResolvedValue(view())
    mocked.syncBillingCheckout.mockResolvedValue('https://checkout.stripe.com/c/pay/x')
    mocked.syncBillingPortal.mockResolvedValue('https://billing.stripe.com/p/session/y')
  })

  it('shows nothing at all on a server that does not charge', async () => {
    // Self-hosting is the product. A greyed-out billing box would
    // make it look like a trial of something.
    mocked.syncBilling.mockResolvedValue(view({ metered: false, seats: null }))
    const wrapper = await panel()

    expect(wrapper.text()).toBe('')
  })

  it('says how many seats are taken of how many are paid for', async () => {
    const wrapper = await panel()
    expect(wrapper.text()).toContain('2 of 5 seats')
  })

  it('warns when there are more people than seats, without threatening anyone', async () => {
    mocked.syncBilling.mockResolvedValue(view({ members: 7, seats: 5 }))
    const wrapper = await panel()

    expect(wrapper.text()).toContain('more people here than seats')
    expect(wrapper.text()).toContain('Nobody is locked out')
  })

  it('prefers the server\'s own notice to its guess at one', async () => {
    mocked.syncBilling.mockResolvedValue(
      view({ members: 7, notice: 'Payment failed; this still works until 1 March.' }),
    )
    const wrapper = await panel()

    expect(wrapper.text()).toContain('until 1 March')
    expect(wrapper.text()).not.toContain('more people here than seats')
  })

  it('offers a checkout when there is no subscription yet', async () => {
    mocked.syncBilling.mockResolvedValue(view({ hasSubscription: false, seats: null, members: 3 }))
    const wrapper = await panel()

    const button = wrapper.findAll('button').find((b) => b.text().includes('Buy'))
    expect(button?.text()).toContain('Buy 3 seats')

    await button!.trigger('click')
    await flushPromises()

    // No seat count is sent: the server buys one for every member,
    // which is the only number that lets everyone keep working.
    expect(mocked.syncBillingCheckout).toHaveBeenCalledWith('w1', null, 'sync-annual')
    expect(mocked.syncBillingPortal).not.toHaveBeenCalled()
  })

  it('opens the portal instead once there is a subscription', async () => {
    const wrapper = await panel()

    const button = wrapper.findAll('button').find((b) => b.text().includes('Manage billing'))
    await button!.trigger('click')
    await flushPromises()

    expect(mocked.syncBillingPortal).toHaveBeenCalledWith('w1')
    expect(mocked.syncBillingCheckout).not.toHaveBeenCalled()
  })

  it('offers the link again in case no browser appeared', async () => {
    // On a bare Linux session there may be no handler registered, and
    // the window just never opens.
    const wrapper = await panel()
    await wrapper.findAll('button').find((b) => b.text().includes('Manage'))!.trigger('click')
    await flushPromises()

    const link = wrapper.find('a')
    expect(link.attributes('href')).toBe('https://billing.stripe.com/p/session/y')
  })

  it('offers a fresh checkout once a subscription has been cancelled', async () => {
    // The customer portal manages a live subscription. A cancelled
    // one has nothing there to manage, so sending someone to it to
    // resubscribe is sending them to an empty page.
    mocked.syncBilling.mockResolvedValue(
      view({ active: false, hasSubscription: true, members: 2, notice: 'It ended.' }),
    )
    const wrapper = await panel()

    const button = wrapper.findAll('button').find((b) => b.text().includes('Buy'))
    expect(button?.text()).toContain('Buy 2 seats')

    await button!.trigger('click')
    await flushPromises()
    expect(mocked.syncBillingCheckout).toHaveBeenCalled()
    expect(mocked.syncBillingPortal).not.toHaveBeenCalled()
  })

  it('names the action the warning just asked for', async () => {
    // Being told that buying the difference clears it, under a button
    // marked "Manage billing", leaves you guessing which screen does
    // that.
    mocked.syncBilling.mockResolvedValue(view({ members: 7, seats: 5 }))
    const wrapper = await panel()

    expect(wrapper.text()).toContain('Add seats')
    expect(wrapper.text()).not.toContain('Manage billing')
  })

  it('explains how payment works only before the first payment', async () => {
    // That is when someone wonders whether to trust a desktop app
    // with a card. Afterwards they know, and in a 320px panel it is
    // two lines of permanent boilerplate.
    mocked.syncBilling.mockResolvedValue(view({ hasSubscription: false, seats: null }))
    const before = await panel()
    expect(before.text()).toContain('Relay never sees a card number')

    mocked.syncBilling.mockResolvedValue(view({ hasSubscription: true }))
    const after = await panel()
    expect(after.text()).not.toContain('Relay never sees a card number')
  })

  it('does not explain payment to someone who cannot pay', async () => {
    mocked.syncBilling.mockResolvedValue(view({ canManage: false, hasSubscription: false }))
    const wrapper = await panel()

    expect(wrapper.text()).not.toContain('Relay never sees a card number')
  })

  it('does not offer the buttons to someone who cannot use them', async () => {
    mocked.syncBilling.mockResolvedValue(view({ canManage: false }))
    const wrapper = await panel()

    expect(wrapper.text()).toContain('Only an administrator')
    expect(wrapper.findAll('button')).toHaveLength(0)
  })

  it('hides itself when the server cannot be asked', async () => {
    // An older or unreachable server must not produce a red box about
    // money the user may not owe.
    mocked.syncBilling.mockRejectedValue({ message: 'not found' })
    const wrapper = await panel()

    expect(wrapper.text()).toBe('')
  })

  it('asks nothing while the dialog is shut', async () => {
    mount(BillingPanel, { props: { workspaceId: 'w1', open: false } })
    await flushPromises()

    expect(mocked.syncBilling).not.toHaveBeenCalled()
  })

  describe('choosing a plan', () => {
    const unsubscribed = { hasSubscription: false, seats: null, members: 1 }

    it('is offered before there is a subscription', async () => {
      mocked.syncBilling.mockResolvedValue(view(unsubscribed))
      const wrapper = await panel()

      expect(wrapper.findAll('input[type="radio"]')).toHaveLength(3)
      expect(wrapper.text()).toContain('Sync, billed yearly')
    })

    it('is not offered once one is live', async () => {
      // Changing plan on a live subscription is the portal's job,
      // where proration is worked out. Offering it here would start
      // a second subscription beside the first.
      mocked.syncBilling.mockResolvedValue(view({ hasSubscription: true }))
      const wrapper = await panel()

      expect(wrapper.findAll('input[type="radio"]')).toHaveLength(0)
    })

    it('is offered again when renewing a cancelled subscription', async () => {
      // A cancelled subscription still counts as having one, but the
      // button below it buys rather than manages — so without this
      // a renewal silently takes whatever the default is.
      mocked.syncBilling.mockResolvedValue(
        view({ hasSubscription: true, active: false }),
      )
      const wrapper = await panel()

      expect(wrapper.findAll('input[type="radio"]')).toHaveLength(3)
    })

    it("shows Stripe's amounts, not a figure of our own", async () => {
      mocked.syncBilling.mockResolvedValue(view(unsubscribed))
      const wrapper = await panel()

      // A yearly plan shows both: what leaves the account, and the
      // monthly figure it is being compared against.
      expect(wrapper.text()).toContain('£24.00 per person a year')
      expect(wrapper.text()).toContain('£2.00 a month')
      expect(wrapper.text()).toContain('£3.00 per person a month')
    })

    it('defaults to the first, which the server orders cheapest', async () => {
      mocked.syncBilling.mockResolvedValue(view(unsubscribed))
      const wrapper = await panel()

      await wrapper.find('button.btn-primary').trigger('click')

      expect(mocked.syncBillingCheckout).toHaveBeenCalledWith('w1', null, 'sync-annual')
    })

    it('buys the one that was picked', async () => {
      mocked.syncBilling.mockResolvedValue(view(unsubscribed))
      const wrapper = await panel()

      const assistant = wrapper
        .findAll('input[type="radio"]')
        .find((input) => (input.element as HTMLInputElement).value === 'assistant-annual')
      await assistant!.setValue()
      await wrapper.find('button.btn-primary').trigger('click')

      expect(mocked.syncBillingCheckout).toHaveBeenCalledWith('w1', null, 'assistant-annual')
    })

    it('a server selling one thing does not ask which', async () => {
      // A radio group of one is a question with no answer to give.
      mocked.syncBilling.mockResolvedValue(
        view({ ...unsubscribed, plans: [PLANS[0]!] }),
      )
      const wrapper = await panel()

      expect(wrapper.findAll('input[type="radio"]')).toHaveLength(0)
      await wrapper.find('button.btn-primary').trigger('click')
      expect(mocked.syncBillingCheckout).toHaveBeenCalledWith('w1', null, 'sync-annual')
    })
  })
})
