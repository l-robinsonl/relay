/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import type { ScriptOutcome } from '@/services/ipc'
import TestResults from './TestResults.vue'

const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

function outcome (over: Partial<ScriptOutcome> = {}): ScriptOutcome {
  return {
    tests: [],
    looseAssertions: [],
    console: [],
    variables: {},
    durationMs: 12,
    ...over,
  }
}

const STATES: { name: string, outcome: ScriptOutcome | null }[] = [
  { name: '01-nothing-ran', outcome: null },
  {
    name: '02-all-passing',
    outcome: outcome({
      tests: [
        {
          name: 'the invoice is created',
          assertions: [
            { message: 'status is 201', passed: true },
            { message: 'body has an id', passed: true },
          ],
        },
      ],
    }),
  },
  {
    name: '03-a-failure',
    outcome: outcome({
      tests: [
        {
          name: 'the invoice is created',
          assertions: [
            { message: 'status is 201', passed: true },
            { message: 'expected 201, got 402 Payment Required', passed: false },
          ],
        },
      ],
      durationMs: 148,
    }),
  },
  {
    name: '04-script-threw',
    outcome: outcome({
      error: "TypeError: cannot read property 'id' of undefined at line 4",
    }),
  },
  {
    name: '05-console-and-variables',
    outcome: outcome({
      tests: [{ name: 'signs in', assertions: [{ message: 'got a token', passed: true }] }],
      console: [
        { level: 'log', text: 'token length 64' },
        { level: 'warn', text: 'retrying once' },
        { level: 'error', text: 'the refresh endpoint 500ed' },
      ],
      variables: { authToken: 'set', requestId: 'set' },
    }),
  },
  {
    name: '06-loose-assertions',
    outcome: outcome({
      looseAssertions: [
        { message: 'response under 500ms', passed: true },
        { message: 'content-type is json', passed: false },
      ],
    }),
  },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('TestResults — markup for screenshots', () => {
  it('writes every state to disk', () => {
    mkdirSync(OUT, { recursive: true })
    for (const state of STATES) {
      const wrapper = mount(TestResults, { props: { outcome: state.outcome } })
      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }
    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
