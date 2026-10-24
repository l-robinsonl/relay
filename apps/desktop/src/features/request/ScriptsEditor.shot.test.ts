/** A camera, not a test. See `scripts/shoot-ui.sh`. */
import { describe, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { mkdirSync, writeFileSync } from 'node:fs'
import type { RequestScripts } from '@relay/shared-types'
import ScriptsEditor from './ScriptsEditor.vue'

const OUT = process.env.RELAY_SHOTS_DIR ?? '/tmp/relay-shots'

const TESTS = `test("the invoice is created", () => {
  expect(response.status).toBe(201)
  expect(response.json().id).toBeTruthy()
})

// Carry the id into the next request.
relay.setVariable("invoiceId", response.json().id)`

const PRE = `// Stamp every request with a fresh idempotency key.
relay.setVariable("idemKey", crypto.randomUUID())
relay.request.headers["Idempotency-Key"] = "{{idemKey}}"`

const STATES: { name: string, value: RequestScripts }[] = [
  { name: '01-both-empty', value: { preRequest: '', postResponse: '' } },
  { name: '02-tests-written', value: { preRequest: '', postResponse: TESTS } },
  { name: '03-pre-request-written', value: { preRequest: PRE, postResponse: '' } },
  { name: '04-both-written', value: { preRequest: PRE, postResponse: TESTS } },
]

describe.skipIf(process.env.RELAY_SHOTS !== '1')('ScriptsEditor — markup for screenshots', () => {
  it('writes every state to disk', () => {
    mkdirSync(OUT, { recursive: true })
    for (const state of STATES) {
      const wrapper = mount(ScriptsEditor, { props: { modelValue: state.value } })
      writeFileSync(`${OUT}/${state.name}.html`, wrapper.html())
      wrapper.unmount()
    }
    writeFileSync(`${OUT}/states.json`, JSON.stringify(STATES.map((s) => s.name), null, 2))
  })
})
