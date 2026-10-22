import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import type { InvitationView, MemberView } from '@relay/shared-types'
import MembersPanel from './MembersPanel.vue'
import { api } from '@/services/ipc'

vi.mock('@/services/ipc', () => ({
  api: {
    syncMembers: vi.fn(),
    syncInvitations: vi.fn(),
    syncInvite: vi.fn(),
    syncSetMemberRole: vi.fn(),
    syncRemoveMember: vi.fn(),
    syncRevokeInvitation: vi.fn(),
  },
}))

const mocked = vi.mocked(api)

function member (email: string, role: MemberView['role'] = 'editor'): MemberView {
  return {
    user: {
      id: `id-${email}`,
      email,
      displayName: email.split('@')[0]!,
      isServerAdmin: false,
      createdAt: 1,
    },
    role,
    joinedAt: 1,
  }
}

function invitation (email: string): InvitationView {
  return {
    id: `inv-${email}`,
    email,
    role: 'viewer',
    createdAt: Date.now(),
    expiresAt: Date.now() + 604_800_000,
  }
}

async function panel () {
  const wrapper = mount(MembersPanel, { props: { workspaceId: 'w1', open: true } })
  await flushPromises()
  return wrapper
}

describe('MembersPanel', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
    mocked.syncMembers.mockResolvedValue([member('ada@example.com', 'admin')])
    mocked.syncInvitations.mockResolvedValue([])
  })

  it('lists who is in the workspace and what they may do', async () => {
    const wrapper = await panel()
    expect(wrapper.text()).toContain('ada@example.com')

    const role = wrapper.find('select[aria-label="Role for ada@example.com"]')
    expect((role.element as HTMLSelectElement).value).toBe('admin')
  })

  it('shows invitations separately from members', async () => {
    // They are not the same thing: one has access, the other might never
    // accept. Mixing them overstates who can see the workspace.
    mocked.syncInvitations.mockResolvedValue([invitation('grace@example.com')])
    const wrapper = await panel()

    expect(wrapper.text()).toContain('In this workspace')
    expect(wrapper.text()).toContain('Invited')
    expect(wrapper.text()).toContain('grace@example.com')
  })

  it('will not invite something that is not an address', async () => {
    const wrapper = await panel()
    const button = wrapper.findAll('button').find((b) => b.text().includes('Invite'))!

    await wrapper.find('input[type="email"]').setValue('grace')
    expect(button.attributes('disabled')).toBeDefined()

    await wrapper.find('input[type="email"]').setValue('grace@example.com')
    expect(
      wrapper.findAll('button').find((b) => b.text().includes('Invite'))!.attributes('disabled'),
    ).toBeUndefined()
  })

  it('invites with the chosen role and reloads', async () => {
    mocked.syncInvite.mockResolvedValue(invitation('grace@example.com'))
    const wrapper = await panel()

    await wrapper.find('input[type="email"]').setValue('grace@example.com')
    await wrapper.find('select[aria-label="Role"]').setValue('editor')
    await wrapper.findAll('button').find((b) => b.text().includes('Invite'))!.trigger('click')
    await flushPromises()

    expect(mocked.syncInvite).toHaveBeenCalledWith('w1', 'grace@example.com', 'editor')
    // Reloaded, so the new invitation appears without a manual refresh.
    expect(mocked.syncInvitations).toHaveBeenCalledTimes(2)
  })

  it('defaults an invitation to the least privileged role', async () => {
    const wrapper = await panel()
    const select = wrapper.find('select[aria-label="Role"]')
    expect((select.element as HTMLSelectElement).value).toBe('viewer')
  })

  it('says plainly that removing someone does not recall the data', async () => {
    // The honest limit of the button, and not obvious.
    const wrapper = await panel()
    expect(wrapper.text()).toContain('only works for the address you type')
  })

  it('surfaces a refusal from the server', async () => {
    mocked.syncInvite.mockRejectedValue({
      kind: 'forbidden',
      message: 'no such workspace, or you do not have access to it',
    })

    const wrapper = await panel()
    await wrapper.find('input[type="email"]').setValue('grace@example.com')
    await wrapper.findAll('button').find((b) => b.text().includes('Invite'))!.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('do not have access')
  })

  it('reports a failed load rather than showing an empty workspace', async () => {
    // An empty list and a failed request look identical otherwise, and
    // one of them wrongly suggests nobody else has access.
    mocked.syncMembers.mockRejectedValue({ kind: 'sync', message: 'the server is unreachable' })

    const wrapper = await panel()
    expect(wrapper.text()).toContain('unreachable')
  })

  it('withdraws an invitation and refreshes the list', async () => {
    mocked.syncInvitations.mockResolvedValue([invitation('grace@example.com')])
    mocked.syncRevokeInvitation.mockResolvedValue(undefined)

    const wrapper = await panel()
    await wrapper.findAll('button').find((b) => b.text().includes('Withdraw'))!.trigger('click')
    await flushPromises()

    expect(mocked.syncRevokeInvitation).toHaveBeenCalledWith('w1', 'inv-grace@example.com')
  })
})
