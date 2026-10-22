<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Loader2, Mail, Trash2, TriangleAlert, UserPlus } from '@lucide/vue'
import type { InvitationView, MemberView, Role } from '@relay/shared-types'
import { api } from '@/services/ipc'
import { formatRelativeTime } from '@/composables/useFormat'
import { useDialogStore } from '@/stores/dialogs'

const props = defineProps<{ workspaceId: string, open: boolean }>()

const dialogs = useDialogStore()

const members = ref<MemberView[]>([])
const invitations = ref<InvitationView[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const notice = ref<string | null>(null)

const inviteEmail = ref('')
const inviteRole = ref<Role>('viewer')
const inviting = ref(false)

const ROLES: { id: Role, label: string, hint: string }[] = [
  { id: 'viewer', label: 'Viewer', hint: 'Can read everything; every change they push is refused.' },
  { id: 'editor', label: 'Editor', hint: 'Can read and change the workspace.' },
  { id: 'admin', label: 'Admin', hint: 'Can also manage who else is here.' },
]

function messageOf (value: unknown): string {
  if (value && typeof value === 'object' && 'message' in value) {
    const detail = 'detail' in value ? (value as { detail?: string | null }).detail : null
    const message = String((value as { message: unknown }).message)
    return detail ? `${message} — ${detail}` : message
  }
  return String(value)
}

async function load () {
  loading.value = true
  error.value = null
  try {
    // Both or neither: a half-loaded panel invites the wrong decision.
    const [people, pending] = await Promise.all([
      api.syncMembers(props.workspaceId),
      api.syncInvitations(props.workspaceId),
    ])
    members.value = people
    invitations.value = pending
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    loading.value = false
  }
}

watch(() => [props.open, props.workspaceId], ([isOpen]) => {
  if (isOpen) load()
}, { immediate: true })

const canInvite = computed(() => inviteEmail.value.trim().includes('@'))

async function invite () {
  inviting.value = true
  error.value = null
  notice.value = null
  try {
    await api.syncInvite(props.workspaceId, inviteEmail.value.trim(), inviteRole.value)
    notice.value = `Invitation sent to ${inviteEmail.value.trim()}.`
    inviteEmail.value = ''
    await load()
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    inviting.value = false
  }
}

async function changeRole (member: MemberView, role: Role) {
  error.value = null
  try {
    await api.syncSetMemberRole(props.workspaceId, member.user.id, role)
    await load()
  } catch (e) {
    error.value = messageOf(e)
    await load()
  }
}

async function remove (member: MemberView) {
  const confirmed = await dialogs.confirm({
    title: `Remove ${member.user.email}?`,
    description:
      'They lose access to this workspace on the server. Whatever they already ' +
      'pulled stays on their own machine — removing someone is not the same as ' +
      'getting the data back.',
    confirmLabel: 'Remove',
    destructive: true,
  })
  if (!confirmed) return

  error.value = null
  try {
    await api.syncRemoveMember(props.workspaceId, member.user.id)
    await load()
  } catch (e) {
    error.value = messageOf(e)
  }
}

async function revoke (invitation: InvitationView) {
  error.value = null
  try {
    await api.syncRevokeInvitation(props.workspaceId, invitation.id)
    await load()
  } catch (e) {
    error.value = messageOf(e)
  }
}
</script>

<template>
  <div class="space-y-4">
    <div v-if="loading && members.length === 0" class="flex items-center gap-2 text-xs text-muted">
      <Loader2 :size="13" class="animate-spin" />
      Loading members…
    </div>

    <template v-else>
      <!-- Who is here -->
      <div v-if="members.length" class="space-y-1.5">
        <h3 class="text-xs font-medium text-secondary">In this workspace</h3>
        <div
          v-for="member in members"
          :key="member.user.id"
          class="flex items-center gap-2 rounded border border-subtle px-2.5 py-1.5"
        >
          <div class="min-w-0 flex-1">
            <p class="truncate text-xs text-primary">{{ member.user.displayName }}</p>
            <p class="truncate text-xs text-muted">{{ member.user.email }}</p>
          </div>

          <select
            class="input w-auto shrink-0 py-1 text-xs"
            :value="member.role"
            :aria-label="`Role for ${member.user.email}`"
            @change="changeRole(member, ($event.target as HTMLSelectElement).value as Role)"
          >
            <option v-for="r in ROLES" :key="r.id" :value="r.id">{{ r.label }}</option>
          </select>

          <button
            type="button"
            class="shrink-0 rounded p-1 text-muted hover:text-[var(--danger)]"
            :aria-label="`Remove ${member.user.email}`"
            @click="remove(member)"
          >
            <Trash2 :size="13" />
          </button>
        </div>
      </div>

      <!-- Sent, not yet accepted -->
      <div v-if="invitations.length" class="space-y-1.5">
        <h3 class="text-xs font-medium text-secondary">Invited</h3>
        <div
          v-for="invitation in invitations"
          :key="invitation.id"
          class="flex items-center gap-2 rounded border border-dashed border-subtle px-2.5 py-1.5"
        >
          <Mail :size="13" class="shrink-0 text-muted" />
          <div class="min-w-0 flex-1">
            <p class="truncate text-xs text-primary">{{ invitation.email }}</p>
            <p class="truncate text-xs text-muted">
              {{ invitation.role }} · expires {{ formatRelativeTime(invitation.expiresAt) }}
            </p>
          </div>
          <button
            type="button"
            class="btn shrink-0 text-xs"
            :aria-label="`Withdraw the invitation to ${invitation.email}`"
            @click="revoke(invitation)"
          >
            Withdraw
          </button>
        </div>
      </div>

      <!-- Invite someone -->
      <div class="space-y-2 border-t border-subtle pt-3">
        <h3 class="text-xs font-medium text-secondary">Invite someone</h3>
        <div class="flex gap-2">
          <input
            v-model="inviteEmail"
            type="email"
            class="input"
            placeholder="them@example.com"
            spellcheck="false"
            aria-label="Address to invite"
            @keydown.enter.prevent="canInvite && invite()"
          />
          <select v-model="inviteRole" class="input w-auto shrink-0 py-1.5 text-xs" aria-label="Role">
            <option v-for="r in ROLES" :key="r.id" :value="r.id">{{ r.label }}</option>
          </select>
          <button
            type="button"
            class="btn btn-primary shrink-0 gap-1.5 text-xs"
            :disabled="!canInvite || inviting"
            @click="invite"
          >
            <Loader2 v-if="inviting" :size="13" class="animate-spin" />
            <UserPlus v-else :size="13" />
            Invite
          </button>
        </div>
        <p class="text-xs text-muted">
          {{ ROLES.find((r) => r.id === inviteRole)?.hint }}
          The invitation only works for the address you type, and lapses after a week.
        </p>
      </div>

      <p v-if="notice" class="text-xs" :style="{ color: 'var(--success)' }">{{ notice }}</p>
      <div
        v-if="error"
        class="flex items-start gap-2 rounded border p-2.5 text-xs"
        :style="{
          borderColor: 'var(--danger)',
          backgroundColor: 'var(--danger-muted)',
          color: 'var(--danger)',
        }"
      >
        <TriangleAlert :size="13" class="mt-px shrink-0" />
        <span>{{ error }}</span>
      </div>
    </template>
  </div>
</template>
