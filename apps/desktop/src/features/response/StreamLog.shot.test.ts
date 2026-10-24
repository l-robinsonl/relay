/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import StreamLog from './StreamLog.vue'

const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'
const now = Date.now()

function inbound (name: string, data: string, id?: string) {
  return { direction: 'inbound', name, data, id: id ?? null, binaryBytes: null }
}

const LINES = [
  { at: now - 9000, message: inbound('price', '{"symbol":"GBP","bid":1.27}', '1') },
  { at: now - 4000, message: inbound('price', '{"symbol":"EUR","bid":1.19}', '2') },
  { at: now - 500, message: inbound('heartbeat', 'ok') },
]

/** A websocket reads as a conversation, so the shot has to be one. */
const CONVERSATION = [
  {
    at: now - 12000,
    message: {
      direction: 'outbound',
      name: 'text',
      data: '{"op":"subscribe","channel":"trades"}',
      id: null,
      binaryBytes: null,
    },
  },
  { at: now - 11000, message: inbound('text', '{"ok":true,"channel":"trades"}') },
  { at: now - 6000, message: inbound('text', '{"price":1.2713,"size":500}') },
  {
    at: now - 3000,
    message: {
      direction: 'inbound',
      name: 'binary',
      data: '00 1f 8b 08 00 00 00 00 00',
      id: null,
      binaryBytes: 4096,
    },
  },
  { at: now - 1000, message: inbound('ping', '') },
]

const STATES = [
  {
    name: '01-websocket-conversation',
    props: { lines: CONVERSATION, live: true, canSend: true },
  },
  {
    name: '02-websocket-disconnected',
    props: { lines: CONVERSATION.slice(0, 2), live: false, canSend: true },
  },
  { name: '03-waiting', props: { lines: [], live: true } },
  { name: '04-not-started', props: { lines: [], live: false } },
  { name: '05-receiving', props: { lines: LINES, live: true } },
  {
    name: '06-closed-badly',
    props: {
      lines: LINES.slice(0, 1),
      live: false,
      error: 'connection reset by peer',
    },
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('StreamLog — markup for screenshots', () => {
  it('writes every state to disk', () => {
    mkdirSync(OUT, { recursive: true })
    for (const state of STATES) {
      const wrapper = mount(StreamLog, { props: state.props as never })
      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }
    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
