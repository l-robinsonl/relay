/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import StreamLog from './StreamLog.vue'

const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'
const now = Date.now()

const LINES = [
  { at: now - 9000, event: { event: 'price', data: '{"symbol":"GBP","bid":1.27}', id: '1' } },
  { at: now - 4000, event: { event: 'price', data: '{"symbol":"EUR","bid":1.19}', id: '2' } },
  { at: now - 500, event: { event: 'heartbeat', data: 'ok' } },
]

const STATES = [
  { name: '01-waiting', props: { lines: [], live: true } },
  { name: '02-not-started', props: { lines: [], live: false } },
  { name: '03-receiving', props: { lines: LINES, live: true } },
  {
    name: '04-closed-badly',
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
