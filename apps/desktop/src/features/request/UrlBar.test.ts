import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import UrlBar from './UrlBar.vue'

function bar (props: Partial<InstanceType<typeof UrlBar>['$props']> = {}) {
  return mount(UrlBar, {
    props: {
      method: 'GET',
      kind: 'http' as const,
      url: '',
      sending: false,
      unresolved: [],
      ...props,
    },
  })
}

describe('UrlBar', () => {
  it('offers the standard methods and a custom option', () => {
    const options = bar().findAll('option').map((o) => o.text())
    expect(options).toContain('GET')
    expect(options).toContain('DELETE')
    expect(options).toContain('Custom…')
  })

  it('shows a text field instead of the dropdown for a custom method', () => {
    const wrapper = bar({ method: 'PURGE' })
    expect(wrapper.find('select').exists()).toBe(false)
    expect(wrapper.find('input[aria-label="HTTP method"]').exists()).toBe(true)
  })

  it('emits the chosen method', async () => {
    const wrapper = bar()
    await wrapper.find('select').setValue('POST')
    expect(wrapper.emitted('update:method')![0]).toEqual(['POST'])
  })

  it('emits URL changes', async () => {
    const wrapper = bar()
    await wrapper.find('input[aria-label="Request URL"]').setValue('https://example.com')
    expect(wrapper.emitted('update:url')![0]).toEqual(['https://example.com'])
  })

  it('sends on Enter', async () => {
    const wrapper = bar({ url: 'https://example.com' })
    await wrapper.find('input[aria-label="Request URL"]').trigger('keydown.enter')
    expect(wrapper.emitted('send')).toHaveLength(1)
  })

  it('disables Send with an empty URL', () => {
    expect(bar().find('button').attributes('disabled')).toBeDefined()
    expect(bar({ url: 'https://x' }).find('button').attributes('disabled')).toBeUndefined()
  })

  it('offers Cancel instead of Send while a request is running', () => {
    const wrapper = bar({ url: 'https://x', sending: true })
    expect(wrapper.text()).toContain('Cancel')
    expect(wrapper.text()).not.toContain('Send')
  })

  it('emits cancel', async () => {
    const wrapper = bar({ url: 'https://x', sending: true })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('cancel')).toHaveLength(1)
  })

  it('highlights a resolved variable differently from an unresolved one', () => {
    const resolved = bar({ url: '{{baseUrl}}/users', unresolved: [] })
    expect(resolved.html()).toContain('var(--accent-text)')

    const missing = bar({ url: '{{baseUrl}}/users', unresolved: ['baseUrl'] })
    expect(missing.html()).toContain('var(--danger)')
  })

  it('shows a placeholder only while the URL is empty', () => {
    expect(bar().text()).toContain('Enter a URL')
    expect(bar({ url: 'https://x' }).text()).not.toContain('Enter a URL')
  })

  describe('protocols', () => {
    // This control is the only way to reach SSE or websockets at all.
    // Everything behind it was built and tested before anything could
    // select it, which is a mistake this project has made before.
    it('offers them alongside the methods', () => {
      const options = bar().findAll('option').map((o) => o.text())
      expect(options).toContain('SSE')
      expect(options).toContain('WS')
    })

    it('picking one changes the kind, not the method', async () => {
      const wrapper = bar()
      await wrapper.find('select').setValue('sse')

      expect(wrapper.emitted('update:kind')).toEqual([['sse']])
      expect(wrapper.emitted('update:method')).toBeUndefined()
    })

    it('shows the protocol rather than the method once it is set', () => {
      // Otherwise the control reads GET while the request is a stream.
      const wrapper = bar({ kind: 'sse', method: 'GET' })
      expect((wrapper.find('select').element as HTMLSelectElement).value).toBe('sse')
    })

    it('picking a verb goes back to an ordinary request', async () => {
      const wrapper = bar({ kind: 'websocket' })
      await wrapper.find('select').setValue('POST')

      expect(wrapper.emitted('update:kind')).toEqual([['http']])
      expect(wrapper.emitted('update:method')).toEqual([['POST']])
    })

    it('says connect and disconnect, because that is what it does', () => {
      expect(bar({ kind: 'sse', url: 'https://x' }).text()).toContain('Connect')
      expect(bar({ kind: 'sse', url: 'https://x', sending: true }).text())
        .toContain('Disconnect')
    })
  })
})
