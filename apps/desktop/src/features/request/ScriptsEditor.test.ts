import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { RequestScripts } from '@relay/shared-types'
import ScriptsEditor from './ScriptsEditor.vue'

// The real editor pulls in CodeMirror, which needs a layout engine jsdom
// does not have. The component under test is the chrome around it.
const CodeEditorStub = {
  props: ['modelValue', 'placeholder', 'language'],
  emits: ['update:modelValue'],
  template:
    '<textarea :placeholder="placeholder" :value="modelValue" ' +
    '@input="$emit(\'update:modelValue\', $event.target.value)" />',
}

function editor (scripts: Partial<RequestScripts> = {}) {
  return mount(ScriptsEditor, {
    props: { modelValue: { preRequest: '', postResponse: '', ...scripts } },
    global: { stubs: { CodeEditor: CodeEditorStub } },
  })
}

describe('ScriptsEditor', () => {
  it('offers both phases', () => {
    const labels = editor().findAll('[role="tab"]').map((t) => t.text())
    expect(labels[0]).toContain('Pre-request')
    expect(labels[1]).toContain('Tests')
  })

  it('opens on tests by default', () => {
    expect(editor().find('textarea').attributes('placeholder')).toContain('expect(')
  })

  it('opens on the pre-request script when that is the only one written', () => {
    // Otherwise a request whose only script is a pre-request one looks
    // like it has no scripts at all.
    const wrapper = editor({ preRequest: 'relay.setHeader("X", "1")' })
    expect(wrapper.find('textarea').attributes('placeholder')).toContain('relay.setHeader')
  })

  it('keeps the other phase intact when one is edited', async () => {
    const wrapper = editor({ postResponse: 'expect(response.status).toBe(200)' })
    await wrapper.findAll('[role="tab"]')[0]!.trigger('click')
    await wrapper.find('textarea').setValue('relay.setVariable("n", "1")')

    const [emitted] = wrapper.emitted('update:modelValue')!.at(-1) as [RequestScripts]
    expect(emitted.preRequest).toBe('relay.setVariable("n", "1")')
    expect(emitted.postResponse).toBe('expect(response.status).toBe(200)')
  })

  it('marks which phases have a script, audibly as well as visually', () => {
    const tabsOf = (w: ReturnType<typeof editor>) =>
      w.findAll('[role="tab"]').map((t) => t.text())

    expect(tabsOf(editor()).join()).not.toContain('has a script')

    const preOnly = tabsOf(editor({ preRequest: 'a' }))
    expect(preOnly[0]).toContain('has a script')
    expect(preOnly[1]).not.toContain('has a script')
  })

  it('says what a pre-request script may and may not do', () => {
    const wrapper = editor({ preRequest: 'x' })
    const text = wrapper.text()

    expect(text).toContain('Secret values are not readable')
    expect(text).toContain('lasts for this send only')
    expect(text).toContain('the request is not sent')
  })
})
