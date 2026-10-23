/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
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
const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

function member (email: string, role: MemberView['role'] = 'editor', name?: string): MemberView {
  return {
    user: {
      id: `id-${email}`,
      email,
      displayName: name ?? email.split('@')[0]!,
      isServerAdmin: false,
      createdAt: 1,
    },
    role,
    joinedAt: Date.now() - 86_400_000,
  }
}

function invitation (email: string, role: InvitationView['role'] = 'viewer'): InvitationView {
  return {
    id: `inv-${email}`,
    email,
    role,
    createdAt: Date.now(),
    expiresAt: Date.now() + 604_800_000,
  }
}

const STATES: { name: string, members: MemberView[], invitations: InvitationView[] }[] = [
  { name: '01-just-you', members: [member('ada@example.com', 'admin')], invitations: [] },
  {
    name: '02-a-small-team',
    members: [
      member('ada@example.com', 'admin', 'Ada Lovelace'),
      member('grace@example.com', 'editor', 'Grace Hopper'),
      member('alan@example.com', 'viewer', 'Alan Turing'),
    ],
    invitations: [invitation('katherine@example.com', 'editor')],
  },
  {
    name: '03-long-names-and-addresses',
    members: [
      member(
        'a.very.long.address.indeed@some-rather-long-company-domain.example.com',
        'admin',
        'Someone With A Quite Long Display Name',
      ),
    ],
    invitations: [invitation('another.long.address@some-long-domain.example.com')],
  },
  {
    name: '04-several-invitations',
    members: [member('ada@example.com', 'admin')],
    invitations: [
      invitation('one@example.com', 'viewer'),
      invitation('two@example.com', 'editor'),
      invitation('three@example.com', 'admin'),
    ],
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('MembersPanel — markup for screenshots', () => {
  it('writes every state to disk', async () => {
    mkdirSync(OUT, { recursive: true })
    setActivePinia(createPinia())

    for (const state of STATES) {
      vi.clearAllMocks()
      mocked.syncMembers.mockResolvedValue(state.members)
      mocked.syncInvitations.mockResolvedValue(state.invitations)

      const wrapper = mount(MembersPanel, { props: { workspaceId: 'w1', open: true } })
      await flushPromises()
      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }

    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
