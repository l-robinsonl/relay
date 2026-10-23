import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import type { AssistantSettings } from '@/services/ipc'
import AssistantSettingsDialog from './AssistantSettings.vue'
import { api } from '@/services/ipc'

vi.mock('@/services/ipc', () => ({
  api: {
    assistantHasKey: vi.fn(),
    assistantSetKey: vi.fn(),
    assistantClearKey: vi.fn(),
  },
}))

const mocked = vi.mocked(api)

async function dialog (settings: Partial<AssistantSettings> = {}) {
  const wrapper = mount(AssistantSettingsDialog, {
    props: {
      open: true,
      modelValue: { provider: 'anthropic', model: '', baseUrl: '', ...settings },
    },
    global: { stubs: { Teleport: true } },
  })
  await flushPromises()
  return wrapper
}

describe('AssistantSettings', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocked.assistantHasKey.mockResolvedValue(false)
    mocked.assistantSetKey.mockResolvedValue(undefined)
    mocked.assistantClearKey.mockResolvedValue(undefined)
  })

  it('says where the key goes and that Relay keeps none', async () => {
    // The whole BYOK argument, said before they paste anything.
    const text = (await dialog()).text()
    expect(text).toContain('Relay ships no key')
    expect(text).toContain("credential store")
  })

  it('offers a local endpoint, and explains why you would want one', async () => {
    const wrapper = await dialog({ provider: 'compatible' })
    expect(wrapper.find('#assistant-url').exists()).toBe(true)
    expect(wrapper.text()).toContain('no prompt leaves it')
  })

  it('asks for an endpoint only for a compatible provider', async () => {
    expect((await dialog()).find('#assistant-url').exists()).toBe(false)
  })

  it('stores a typed key and does not keep it in the component', async () => {
    const wrapper = await dialog()
    await wrapper.find('#assistant-key').setValue('sk-ant-typed-in')
    await wrapper.findAll('button').find((b) => b.text().includes('Save'))!.trigger('click')
    await flushPromises()
    await wrapper.vm.$nextTick()

    expect(mocked.assistantSetKey).toHaveBeenCalledWith('anthropic', 'sk-ant-typed-in')
    // Cleared from the component; it is in the credential store now.
    const field = wrapper.find('#assistant-key').element as HTMLInputElement
    expect(field.value).toBe('')
  })

  it('does not overwrite a stored key when the box is left blank', async () => {
    // Saving a model change should not wipe the key.
    mocked.assistantHasKey.mockResolvedValue(true)
    const wrapper = await dialog()
    await wrapper.findAll('button').find((b) => b.text().includes('Save'))!.trigger('click')
    await flushPromises()

    expect(mocked.assistantSetKey).not.toHaveBeenCalled()
  })

  it('hides the key by default and can reveal it', async () => {
    const wrapper = await dialog()
    expect(wrapper.find('#assistant-key').attributes('type')).toBe('password')

    await wrapper.find('button[aria-label="Show the key"]').trigger('click')
    expect(wrapper.find('#assistant-key').attributes('type')).toBe('text')
  })

  it('says when a key is already stored, and can forget it', async () => {
    mocked.assistantHasKey.mockResolvedValue(true)
    const wrapper = await dialog()
    expect(wrapper.text()).toContain('A key is stored')

    await wrapper.findAll('button').find((b) => b.text().includes('Forget'))!.trigger('click')
    await flushPromises()

    expect(mocked.assistantClearKey).toHaveBeenCalledWith('anthropic')
    expect(wrapper.text()).not.toContain('A key is stored')
  })

  it('emits the provider change rather than mutating the prop', async () => {
    const wrapper = await dialog()
    await wrapper.find('#assistant-provider').setValue('openai')

    const [emitted] = wrapper.emitted('update:modelValue')!.at(-1) as [AssistantSettings]
    expect(emitted.provider).toBe('openai')
    expect(emitted.model).toBe('')
  })

  it('checks for a key per provider, not once', async () => {
    // A key stored for Claude says nothing about OpenAI.
    const wrapper = await dialog()
    await wrapper.setProps({ modelValue: { provider: 'openai', model: '', baseUrl: '' } })
    await flushPromises()

    expect(mocked.assistantHasKey).toHaveBeenCalledWith('openai')
  })
})
