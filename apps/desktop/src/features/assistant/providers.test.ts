import { describe, expect, it } from 'vitest'
import { PROVIDERS, providerLabel } from './providers'

describe('providerLabel', () => {
  it('names a provider the way a person would', () => {
    // The panel used to print the raw id: "No API key is set for
    // anthropic", or "for compatible", which means nothing to anyone.
    expect(providerLabel('anthropic')).toBe('Claude')
    expect(providerLabel('openai')).toBe('OpenAI')
    expect(providerLabel('compatible')).toBe('Something else')
  })

  it('covers every provider the settings dialog offers', () => {
    // One list feeding both, so a fourth provider cannot be added to
    // the dialog and leave the panel printing an id.
    for (const provider of PROVIDERS) {
      expect(providerLabel(provider.id)).toBe(provider.label)
      expect(provider.label).not.toBe(provider.id)
    }
  })
})
