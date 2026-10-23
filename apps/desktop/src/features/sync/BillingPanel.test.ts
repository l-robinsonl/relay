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
    expect(mocked.syncBillingCheckout).toHaveBeenCalledWith('w1', null)
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
})
