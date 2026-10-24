import type { AssistantSettings } from '@/services/ipc'

/**
 * The model providers, and what to call them in front of a person.
 *
 * One list, because there were two: the settings dialog said "Claude"
 * while the panel printed the raw id and told people "No API key is
 * set for anthropic" — or, worse, "for compatible".
 */
export const PROVIDERS = [
  {
    id: 'anthropic' as const,
    label: 'Claude',
    placeholder: 'claude-opus-5-5',
    help: 'Keys from console.anthropic.com.',
  },
  {
    id: 'openai' as const,
    label: 'OpenAI',
    placeholder: 'gpt-5',
    help: 'Keys from platform.openai.com.',
  },
  {
    id: 'compatible' as const,
    label: 'Something else',
    placeholder: 'the model name that endpoint expects',
    help: 'Anything speaking the OpenAI chat-completions shape — Ollama, LM Studio, '
      + 'OpenRouter, Groq. A local endpoint usually needs no key at all.',
  },
]

/** What to call a provider in a sentence. */
export function providerLabel (id: AssistantSettings['provider']): string {
  return PROVIDERS.find((p) => p.id === id)?.label ?? id
}
