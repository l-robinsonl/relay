import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { RequestSettings } from '@relay/shared-types'
import SettingsEditor from './SettingsEditor.vue'

function settings (overrides: Partial<RequestSettings> = {}): RequestSettings {
  return {
    timeoutMs: null,
    redirectPolicy: 'follow',
    maxRedirects: 10,
    verifyTls: true,
    storeCookies: true,
    sendCookies: true,
    decompress: true,
    maxResponseBytes: null,
    encodeUrl: true,
    ...overrides,
  }
}

function editor (overrides: Partial<RequestSettings> = {}) {
  return mount(SettingsEditor, { props: { modelValue: settings(overrides) } })
}

/** The settings object of the most recent emit. */
function emitted (wrapper: ReturnType<typeof editor>): RequestSettings {
  const [value] = wrapper.emitted('update:modelValue')!.at(-1) as [RequestSettings]
  return value
}

describe('SettingsEditor', () => {
  it('warns prominently when TLS verification is off', () => {
    expect(editor().text()).not.toContain('Anyone on the network path')

    const off = editor({ verifyTls: false })
    expect(off.text()).toContain('Anyone on the network path')
    expect(off.text()).toContain('never for a request carrying real credentials')
  })

  it('hides the certificate fields until one is asked for', async () => {
    const wrapper = editor()
    expect(wrapper.find('#cert-path').exists()).toBe(false)

    expect(editor({ clientCertificate: { hosts: [], pemPath: '' } })
      .find('#cert-path').exists()).toBe(true)
  })

  it('turns the certificate on and off without losing the rest', async () => {
    const wrapper = editor()
    const toggle = wrapper.findAll('input[type="checkbox"]').at(-2)!
    await toggle.setValue(true)

    const after = emitted(wrapper)
    expect(after.clientCertificate).toEqual({ hosts: [], pemPath: '' })
    expect(after.verifyTls).toBe(true)
  })

  it('stores the host list as a list, not a string', async () => {
    const wrapper = editor({
      clientCertificate: { hosts: [], pemPath: '/tmp/c.pem' },
    })
    await wrapper.find('#cert-hosts').setValue('internal.example.com,  api.corp ,')

    expect(emitted(wrapper).clientCertificate!.hosts).toEqual([
      'internal.example.com',
      'api.corp',
    ])
  })

  it('says the certificate file itself is not copied anywhere', () => {
    // The promise that makes it safe to sync a request that uses one.
    const wrapper = editor({
      clientCertificate: { hosts: [], pemPath: '' },
    })
    expect(wrapper.text()).toContain('only this path travels')
  })

  it('edits a proxy, keeping an empty optional field null rather than empty', async () => {
    const wrapper = editor({
      proxy: { url: 'http://proxy.corp:8080', username: 'ada', password: null, bypass: [] },
    })
    await wrapper.find('#proxy-user').setValue('')

    expect(emitted(wrapper).proxy!.username).toBeNull()
  })

  it('keeps the proxy URL when the bypass list changes', async () => {
    const wrapper = editor({
      proxy: { url: 'http://proxy.corp:8080', username: null, password: null, bypass: [] },
    })
    await wrapper.find('#proxy-bypass').setValue('localhost, .internal')

    const proxy = emitted(wrapper).proxy!
    expect(proxy.url).toBe('http://proxy.corp:8080')
    expect(proxy.bypass).toEqual(['localhost', '.internal'])
  })
})
