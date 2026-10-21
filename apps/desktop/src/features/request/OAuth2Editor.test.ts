import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import type { OAuth2Config } from '@relay/shared-types'
import OAuth2Editor from './OAuth2Editor.vue'
import { api } from '@/services/ipc'

vi.mock('@/services/ipc', () => ({
  api: {
    oauthTokenStatus: vi.fn(),
    oauthFetchToken: vi.fn(),
    oauthAuthorize: vi.fn(),
    oauthForgetToken: vi.fn(),
  },
}))

const mocked = vi.mocked(api)

function config (overrides: Partial<OAuth2Config> = {}): OAuth2Config {
  return {
    grant: 'authorizationCode',
    authUrl: 'https://id.example.com/authorize',
    tokenUrl: 'https://id.example.com/token',
    clientId: 'abc',
    clientSecret: '',
    scope: '',
    audience: '',
    redirectUri: '',
    username: '',
    password: '',
    usePkce: true,
    clientAuth: 'header',
    ...overrides,
  }
}

async function editor (overrides: Partial<OAuth2Config> = {}) {
  const wrapper = mount(OAuth2Editor, { props: { modelValue: config(overrides) } })
  await flushPromises()
  return wrapper
}

describe('OAuth2Editor', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocked.oauthTokenStatus.mockResolvedValue({ hasToken: false, canRefresh: false })
  })

  it('offers the grants Relay can actually run', async () => {
    const options = (await editor()).findAll('#oauth-grant option').map((o) => o.text())

    expect(options).toEqual(['Authorization code', 'Client credentials', 'Password'])
    // The implicit grant is deliberately absent: it leaks tokens through
    // URL fragments and issues no refresh token.
    expect(options.join()).not.toMatch(/implicit/i)
  })

  it('asks for an authorization URL only where a browser is involved', async () => {
    expect((await editor()).find('#oauth-auth-url').exists()).toBe(true)

    const clientCredentials = await editor({ grant: 'clientCredentials' })
    expect(clientCredentials.find('#oauth-auth-url').exists()).toBe(false)
    expect(clientCredentials.find('#oauth-token-url').exists()).toBe(true)
  })

  it('asks for a username and password only for the password grant', async () => {
    expect((await editor()).find('#oauth-username').exists()).toBe(false)
    expect((await editor({ grant: 'password' })).find('#oauth-username').exists()).toBe(true)
  })

  it('starts with PKCE on', async () => {
    const checkbox = (await editor()).find('input[type="checkbox"]')
    expect((checkbox.element as HTMLInputElement).checked).toBe(true)
  })

  it('hides the PKCE switch where it does not apply', async () => {
    const wrapper = await editor({ grant: 'clientCredentials' })
    expect(wrapper.find('input[type="checkbox"]').exists()).toBe(false)
  })

  it('offers Authorize for the browser grant and Get token otherwise', async () => {
    expect((await editor()).text()).toContain('Authorize')

    const clientCredentials = await editor({ grant: 'clientCredentials' })
    expect(clientCredentials.text()).toContain('Get token')
    expect(clientCredentials.text()).not.toContain('Authorize')
  })

  it('reports a held token with its expiry', async () => {
    mocked.oauthTokenStatus.mockResolvedValue({
      hasToken: true,
      canRefresh: true,
      expiresAt: Date.now() + 3_600_000,
      scope: 'read:user',
    })

    const wrapper = await editor()

    expect(wrapper.text()).toContain('Token held')
    expect(wrapper.text()).toContain('renews itself')
    expect(wrapper.text()).toContain('Forget token')
  })

  it('shows the failure from the provider rather than swallowing it', async () => {
    mocked.oauthFetchToken.mockRejectedValue({
      kind: 'transport',
      message: 'the authorization server refused: invalid_client',
      detail: 'Check the client secret.',
    })

    const wrapper = await editor({ grant: 'clientCredentials' })
    await wrapper.find('button').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('invalid_client')
    expect(wrapper.text()).toContain('Check the client secret.')
  })

  it('emits a changed grant without losing the other fields', async () => {
    const wrapper = await editor({ clientId: 'keep-me' })
    await wrapper.find('#oauth-grant').setValue('clientCredentials')

    const [emitted] = wrapper.emitted('update:modelValue')![0] as [OAuth2Config]
    expect(emitted.grant).toBe('clientCredentials')
    expect(emitted.clientId).toBe('keep-me')
  })

  it('says plainly that tokens are not stored', async () => {
    // The promise the whole design rests on; if the wording goes, the
    // user has no way to know it is still true.
    const text = (await editor()).text()
    expect(text).toContain('never written to the database')
  })
})
