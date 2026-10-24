import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import SyncSettings from './SyncSettings.vue'
import { api } from '@/services/ipc'
import { HOSTED_SERVER } from '@/services/hosted'

vi.mock('@/services/ipc', () => ({
  api: {
    syncProbe: vi.fn(),
    syncRegister: vi.fn(),
    syncRequestVerification: vi.fn(),
    syncRequestPasswordReset: vi.fn(),
    syncInvitationOffer: vi.fn(),
    syncJoinInvited: vi.fn(),
    syncStatus: vi.fn().mockResolvedValue({ connected: false }),
    onSyncChanged: vi.fn().mockResolvedValue(() => {}),
  },
}))

const mocked = vi.mocked(api)

/** Picks "my own server", which is what reveals the address field. */
async function chooseOwnServer (wrapper: { findAll: (s: string) => { text: () => string, trigger: (e: string) => Promise<unknown> }[] }) {
  const own = wrapper.findAll('button').find((b) => b.text().includes('my own server'))
  if (own) await own.trigger('click')
}

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

  await chooseOwnServer(wrapper)
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

  it('offers a server to someone who does not run one', async () => {
    // The dialog used to open on an empty address field with a
    // placeholder of https://relay.internal. Anyone without a server
    // of their own had nothing to type and no way to discover an
    // account was on offer at all.
    const wrapper = mount(SyncSettings, {
      props: { open: true },
      global: { stubs: { Teleport: true } },
    })

    expect(wrapper.text()).toContain("Use Relay's server")
    expect(wrapper.text()).toContain('Use my own server')
    // Neither is preselected: self-hosting is not the lesser path.
    expect(wrapper.find('#server-url').exists()).toBe(false)
  })

  it('fills in the address when Relay\'s own server is chosen', async () => {
    vi.useFakeTimers()
    mocked.syncProbe.mockResolvedValue({
      status: 'ok', backend: 'postgres', protocol: '1', version: '0.1.0',
    })

    const wrapper = mount(SyncSettings, {
      props: { open: true },
      global: { stubs: { Teleport: true } },
    })
    await wrapper.findAll('button').find((b) => b.text().includes("Relay's server"))!.trigger('click')

    const field = wrapper.find('#server-url').element as HTMLInputElement
    expect(field.value).toBe(HOSTED_SERVER)

    // And it goes looking without a further click, so the sign-in
    // options appear on their own.
    await vi.advanceTimersByTimeAsync(1000)
    vi.useRealTimers()
    await flushPromises()

    expect(mocked.syncProbe).toHaveBeenCalledWith(HOSTED_SERVER)
    expect(wrapper.text()).toContain('Create an account')
  })

  it('lets someone change their mind about which server', async () => {
    const wrapper = mount(SyncSettings, {
      props: { open: true },
      global: { stubs: { Teleport: true } },
    })
    await wrapper.findAll('button').find((b) => b.text().includes("Relay's server"))!.trigger('click')
    await wrapper.findAll('button').find((b) => b.text().includes('Change'))!.trigger('click')

    expect(wrapper.text()).toContain('Use my own server')
    expect(wrapper.find('#server-url').exists()).toBe(false)
  })

  it('checks the address on its own, without a second click', async () => {
    // The sign-in and create-account tabs only appear once the server
    // has answered. Hiding that behind a separate "Check" button meant
    // someone could type a perfectly good address, watch nothing
    // happen, and conclude there was no way to sign in at all.
    vi.useFakeTimers()
    mocked.syncProbe.mockResolvedValue({
      status: 'ok', backend: 'sqlite', protocol: '1', version: '0.1.0',
    })

    const wrapper = mount(SyncSettings, {
      props: { open: true },
      global: { stubs: { Teleport: true } },
    })
    await chooseOwnServer(wrapper)
    await wrapper.find('#server-url').setValue('https://relay.test')

    expect(mocked.syncProbe).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(1000)
    vi.useRealTimers()
    await flushPromises()

    expect(mocked.syncProbe).toHaveBeenCalledWith('https://relay.test')
    expect(wrapper.text()).toContain('Create an account')
  })

  it('does not go looking before the address is a plausible one', async () => {
    // Otherwise it fires a request at every keystroke of "h", "ht",
    // "htt"...
    vi.useFakeTimers()
    const wrapper = mount(SyncSettings, {
      props: { open: true },
      global: { stubs: { Teleport: true } },
    })
    await chooseOwnServer(wrapper)

    for (const partial of ['h', 'https:/', 'https://rel']) {
      await wrapper.find('#server-url').setValue(partial)
      await vi.advanceTimersByTimeAsync(1000)
    }
    vi.useRealTimers()

    expect(mocked.syncProbe).not.toHaveBeenCalled()
  })

  it('offers to create an account, not just sign in', async () => {
    // Before this there was no way to register from the application at
    // all: you needed the HTTP API or an administrator.
    const wrapper = await panel()
    expect(wrapper.text()).toContain('Create an account')
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

  it('offers a way in for someone who was invited', async () => {
    // Without this the invitation e-mail tells people to open Relay and
    // sign in, and on an invite-only server registration refuses them.
    const wrapper = await panel()
    expect(wrapper.text()).toContain('I was invited')
  })

  it('shows what the invitation grants before asking for a password', async () => {
    mocked.syncInvitationOffer.mockResolvedValue({
      workspaceId: 'w1',
      workspaceName: 'Marketing API',
      email: 'grace@example.com',
      role: 'editor',
    })

    const wrapper = await panel()
    await wrapper.findAll('[role="tab"]')[2]!.trigger('click')
    await wrapper.find('#sync-invite').setValue('https://relay.test/invite?token=abc')
    await wrapper.find('#sync-invite').trigger('blur')
    await flushPromises()

    expect(wrapper.text()).toContain('Marketing API')
    expect(wrapper.text()).toContain('grace@example.com')
    expect(wrapper.text()).toContain('editor')
  })

  it('does not ask an invitee for their address', async () => {
    // The invitation decides it, and a field they could change would
    // only produce a refusal they do not understand.
    const wrapper = await panel()
    await wrapper.findAll('[role="tab"]')[2]!.trigger('click')
    expect(wrapper.find('#sync-email').exists()).toBe(false)
  })

  it('will not join until the invitation has been read', async () => {
    const wrapper = await panel()
    await wrapper.findAll('[role="tab"]')[2]!.trigger('click')
    await wrapper.find('#sync-password').setValue('correct horse battery')

    const join = wrapper.findAll('button').find((b) => b.text().includes('Join'))!
    expect(join.attributes('disabled')).toBeDefined()
  })

  it('passes the whole pasted link through, not a token it parsed itself', async () => {
    // Extraction happens in Rust, where it is tested. Doing it twice
    // means two behaviours to keep in step.
    mocked.syncInvitationOffer.mockResolvedValue({
      workspaceId: 'w1',
      workspaceName: 'Marketing API',
      email: 'grace@example.com',
      role: 'editor',
    })
    mocked.syncJoinInvited.mockResolvedValue({
      id: 'w1',
      name: 'Marketing API',
      description: '',
      variables: [],
      sortOrder: 0,
      createdAt: 1,
      updatedAt: 1,
      rev: 0,
      dirty: false,
    })

    const wrapper = await panel()
    await wrapper.findAll('[role="tab"]')[2]!.trigger('click')
    await wrapper.find('#sync-invite').setValue('https://relay.test/invite?token=abc')
    await wrapper.find('#sync-invite').trigger('blur')
    await flushPromises()
    await wrapper.find('#sync-password').setValue('correct horse battery')
    await wrapper.findAll('button').find((b) => b.text().includes('Join'))!.trigger('click')
    await flushPromises()

    expect(mocked.syncJoinInvited).toHaveBeenCalledWith(
      'https://relay.test',
      'https://relay.test/invite?token=abc',
      'correct horse battery',
      '',
    )
  })
})
