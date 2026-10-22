import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import SyncSettings from './SyncSettings.vue'
import { api } from '@/services/ipc'

vi.mock('@/services/ipc', () => ({
  api: {
    syncProbe: vi.fn(),
    syncRegister: vi.fn(),
    syncRequestVerification: vi.fn(),
    syncRequestPasswordReset: vi.fn(),
    syncStatus: vi.fn().mockResolvedValue({ connected: false }),
    onSyncChanged: vi.fn().mockResolvedValue(() => {}),
  },
}))

const mocked = vi.mocked(api)

/** Mounts the panel already past the server check. */
async function panel () {
  mocked.syncProbe.mockResolvedValue({
    status: 'ok',
    backend: 'sqlite',
    protocol: '1',
    version: '0.1.0',
  })

  const wrapper = mount(SyncSettings, {
    props: { open: true },
    global: { stubs: { Teleport: true } },
  })

  await wrapper.find('#server-url').setValue('https://relay.test')
  await wrapper.findAll('button').find((b) => b.text().includes('Check'))!.trigger('click')
  await flushPromises()
  return wrapper
}

describe('SyncSettings', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
  })

  it('offers to create an account, not just sign in', async () => {
    // Before this there was no way to register from the application at
    // all: you needed the HTTP API or an administrator.
    const wrapper = await panel()
    expect(wrapper.text()).toContain('Create one')
  })

  it('asks for a display name only when registering', async () => {
    const wrapper = await panel()
    expect(wrapper.find('#sync-name').exists()).toBe(false)

    await wrapper.findAll('[role="tab"]')[1]!.trigger('click')
    expect(wrapper.find('#sync-name').exists()).toBe(true)
  })

  it('will not register a password shorter than the reset page accepts', async () => {
    const wrapper = await panel()
    await wrapper.findAll('[role="tab"]')[1]!.trigger('click')
    await wrapper.find('#sync-email').setValue('ada@example.com')
    await wrapper.find('#sync-password').setValue('short')

    const create = wrapper.findAll('button').find((b) => b.text().includes('Create the account'))!
    expect(create.attributes('disabled')).toBeDefined()

    await wrapper.find('#sync-password').setValue('a long enough passphrase')
    expect(
      wrapper
        .findAll('button')
        .find((b) => b.text().includes('Create the account'))!
        .attributes('disabled'),
    ).toBeUndefined()
  })

  it('tells the user to check their inbox after registering', async () => {
    mocked.syncRegister.mockResolvedValue({
      id: 'u1',
      email: 'ada@example.com',
      displayName: 'Ada',
      isServerAdmin: false,
      createdAt: 1,
    })

    const wrapper = await panel()
    await wrapper.findAll('[role="tab"]')[1]!.trigger('click')
    await wrapper.find('#sync-email').setValue('ada@example.com')
    await wrapper.find('#sync-password').setValue('a long enough passphrase')
    await wrapper.findAll('button').find((b) => b.text().includes('Create the account'))!.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('Check your e-mail')
    // And it drops back to sign-in, which is what they do next.
    expect(wrapper.find('#sync-name').exists()).toBe(false)
  })

  it('says the same thing whether or not a reset address is known', async () => {
    // The wording has to match the server, which answers identically
    // for a registered and an unregistered address.
    mocked.syncRequestPasswordReset.mockResolvedValue(undefined)

    const wrapper = await panel()
    await wrapper.find('#sync-email').setValue('nobody@example.com')
    await wrapper.findAll('button').find((b) => b.text().includes('Forgotten'))!.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('If that address has an account')
    expect(wrapper.text()).not.toMatch(/no such|not found|unknown/i)
  })

  it('surfaces a registration failure instead of swallowing it', async () => {
    mocked.syncRegister.mockRejectedValue({
      kind: 'conflict',
      message: 'that e-mail address already has an account on this server',
      detail: 'Sign in instead, or reset the password.',
    })

    const wrapper = await panel()
    await wrapper.findAll('[role="tab"]')[1]!.trigger('click')
    await wrapper.find('#sync-email').setValue('ada@example.com')
    await wrapper.find('#sync-password').setValue('a long enough passphrase')
    await wrapper.findAll('button').find((b) => b.text().includes('Create the account'))!.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('already has an account')
    expect(wrapper.text()).toContain('Sign in instead')
  })

  it('does not show the join checkbox while registering', async () => {
    // "This workspace already exists on the server" is meaningless
    // before you have an account.
    const wrapper = await panel()
    expect(wrapper.text()).toContain('already exists on the server')

    await wrapper.findAll('[role="tab"]')[1]!.trigger('click')
    expect(wrapper.text()).not.toContain('already exists on the server')
  })
})
