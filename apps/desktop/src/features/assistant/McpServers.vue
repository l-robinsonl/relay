<script setup lang="ts">
import { ref } from 'vue'
import { Check, Loader2, Plus, Server, Trash2, TriangleAlert } from '@lucide/vue'
import type { McpServer } from '@/services/ipc'
import { api } from '@/services/ipc'

/** Adding and checking the MCP servers the assistant may use. */
const servers = ref<McpServer[]>([])
const loaded = ref(false)

const name = ref('')
const command = ref('')
const args = ref('')
const adding = ref(false)
const error = ref<string | null>(null)
/** Tool names returned by the last successful check, by server name. */
const checked = ref<Record<string, string[]>>({})

function messageOf (value: unknown): string {
  if (value && typeof value === 'object' && 'message' in value) {
    return String((value as { message: unknown }).message)
  }
  return String(value)
}

async function load () {
  servers.value = await api.assistantMcpServers().catch(() => [])
  loaded.value = true
}
load()

/** Splits an argument string the way a shell would, roughly. */
function splitArgs (raw: string): string[] {
  return raw.match(/"[^"]*"|'[^']*'|\S+/g)?.map((a) => a.replace(/^["']|["']$/g, '')) ?? []
}

async function add () {
  const server: McpServer = {
    transport: 'stdio',
    name: name.value.trim(),
    command: command.value.trim(),
    args: splitArgs(args.value),
    env: {},
  }

  if (server.name === '' || server.command === '') return

  adding.value = true
  error.value = null
  try {
    // Checked before it is saved. A command that does not start is
    // worth knowing about now, not when the next question quietly has
    // fewer tools than expected.
    const tools = await api.assistantMcpProbe(server)
    checked.value[server.name] = tools

    servers.value = [...servers.value.filter((s) => s.name !== server.name), server]
    await api.assistantSetMcpServers(servers.value)

    name.value = ''
    command.value = ''
    args.value = ''
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    adding.value = false
  }
}

async function remove (target: McpServer) {
  servers.value = servers.value.filter((s) => s.name !== target.name)
  await api.assistantSetMcpServers(servers.value)
}
</script>

<template>
  <div class="space-y-2.5">
    <div>
      <h3 class="text-xs font-medium text-secondary">Your own tools</h3>
      <p class="mt-1 max-w-prose text-xs text-muted">
        Point the assistant at an MCP server you run and its tools appear beside
        Relay's own. Anything one offers is always confirmed before it runs — Relay
        cannot know what someone else's tool does.
      </p>
    </div>

    <div
      v-for="server in servers"
      :key="server.name"
      class="flex items-center gap-2 rounded border border-subtle px-2.5 py-1.5"
    >
      <Server :size="13" class="shrink-0 text-muted" />
      <div class="min-w-0 flex-1">
        <p class="truncate text-xs text-primary">{{ server.name }}</p>
        <p class="truncate font-mono text-xs text-muted">
          {{ server.transport === 'stdio' ? server.command : server.url }}
        </p>
      </div>
      <span
        v-if="checked[server.name]"
        class="shrink-0 text-xs"
        :style="{ color: 'var(--success)' }"
      >
        {{ checked[server.name]!.length }} tools
      </span>
      <button
        type="button"
        class="shrink-0 rounded p-1 text-muted hover:text-[var(--danger)]"
        :aria-label="`Remove ${server.name}`"
        @click="remove(server)"
      >
        <Trash2 :size="13" />
      </button>
    </div>

    <div class="space-y-2 rounded border border-dashed border-subtle p-2.5">
      <div class="grid grid-cols-2 gap-2">
        <input
          v-model="name"
          class="input text-xs"
          placeholder="A name, e.g. files"
          aria-label="Server name"
          spellcheck="false"
        />
        <input
          v-model="command"
          class="input input-mono text-xs"
          placeholder="The command to run"
          aria-label="Command"
          spellcheck="false"
        />
      </div>
      <input
        v-model="args"
        class="input input-mono text-xs"
        placeholder="Arguments, space separated (optional)"
        aria-label="Arguments"
        spellcheck="false"
      />
      <button
        type="button"
        class="btn w-full gap-1.5 text-xs"
        :disabled="adding || name.trim() === '' || command.trim() === ''"
        @click="add"
      >
        <Loader2 v-if="adding" :size="12" class="animate-spin" />
        <Plus v-else :size="12" />
        Add and check
      </button>
    </div>

    <p
      v-if="error"
      class="flex items-start gap-1.5 text-xs"
      :style="{ color: 'var(--danger)' }"
    >
      <TriangleAlert :size="12" class="mt-px shrink-0" />
      <span>{{ error }}</span>
    </p>
    <p
      v-else-if="Object.keys(checked).length"
      class="flex items-center gap-1.5 text-xs"
      :style="{ color: 'var(--success)' }"
    >
      <Check :size="12" />
      Connected and listed its tools.
    </p>

    <p class="text-xs text-muted">
      Local servers only for now — Relay speaks to them over their standard input and
      output. Remote servers over HTTP are not supported yet.
    </p>
  </div>
</template>
