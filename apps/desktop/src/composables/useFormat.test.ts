import { describe, expect, it } from 'vitest'
import {
  formatBytes,
  formatDuration,
  formatRelativeTime,
  methodColor,
  methodLabel,
  statusTone,
  tryFormatJson,
} from './useFormat'

describe('formatBytes', () => {
  it('scales to a readable unit', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(1024)).toBe('1.0 KB')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(1024 * 1024)).toBe('1.0 MB')
    expect(formatBytes(1024 * 1024 * 1024)).toBe('1.0 GB')
  })

  it('drops the decimal once the number is large enough not to need it', () => {
    expect(formatBytes(1024 * 20)).toBe('20 KB')
  })

  it('does not produce nonsense for invalid input', () => {
    expect(formatBytes(-1)).toBe('—')
    expect(formatBytes(Number.NaN)).toBe('—')
  })
})

describe('formatDuration', () => {
  it('scales from milliseconds to minutes', () => {
    expect(formatDuration(0.4)).toBe('<1 ms')
    expect(formatDuration(42)).toBe('42 ms')
    expect(formatDuration(1500)).toBe('1.50 s')
    expect(formatDuration(65_000)).toBe('1m 5s')
  })

  it('handles invalid input', () => {
    expect(formatDuration(-5)).toBe('—')
    expect(formatDuration(Number.NaN)).toBe('—')
  })
})

describe('formatRelativeTime', () => {
  it('describes recent times relatively', () => {
    const now = Date.now()
    expect(formatRelativeTime(now)).toBe('just now')
    expect(formatRelativeTime(now - 5 * 60_000)).toBe('5 min ago')
    expect(formatRelativeTime(now - 2 * 3_600_000)).toBe('2 hours ago')
    expect(formatRelativeTime(now - 3_600_000)).toBe('1 hour ago')
    expect(formatRelativeTime(now - 2 * 86_400_000)).toBe('2 days ago')
  })

  it('falls back to a date for anything older than a week', () => {
    const old = Date.now() - 30 * 86_400_000
    expect(formatRelativeTime(old)).not.toContain('ago')
  })

  it('treats a slightly future timestamp as clock skew', () => {
    expect(formatRelativeTime(Date.now() + 10_000)).toBe('just now')
  })

  it('describes a real future time as being ahead', () => {
    // An invitation lapses after a week, and the members panel
    // showed every one of them as "expires just now" — which reads
    // as already gone.
    const now = Date.now()
    expect(formatRelativeTime(now + 30 * 60_000)).toBe('in 30 min')
    expect(formatRelativeTime(now + 2 * 3_600_000)).toBe('in 2 hours')
    expect(formatRelativeTime(now + 3_600_000)).toBe('in 1 hour')
    expect(formatRelativeTime(now + 6 * 86_400_000)).toBe('in 6 days')
    expect(formatRelativeTime(now + 86_400_000)).toBe('in 1 day')
    expect(formatRelativeTime(now + 60 * 86_400_000)).toContain('on ')
  })
})

describe('statusTone', () => {
  it('maps status families to tones', () => {
    expect(statusTone(200)).toBe('success')
    expect(statusTone(204)).toBe('success')
    expect(statusTone(301)).toBe('info')
    expect(statusTone(404)).toBe('warning')
    expect(statusTone(500)).toBe('danger')
    expect(statusTone(503)).toBe('danger')
  })
})

describe('methodColor', () => {
  it('gives each known method its own token', () => {
    expect(methodColor('GET')).toBe('var(--method-get)')
    expect(methodColor('post')).toBe('var(--method-post)')
    expect(methodColor('DELETE')).toBe('var(--method-delete)')
  })

  it('falls back for custom methods rather than producing an invalid token', () => {
    expect(methodColor('PURGE')).toBe('var(--method-other)')
  })
})

describe('methodLabel', () => {
  it('leaves normal methods alone', () => {
    expect(methodLabel('GET')).toBe('GET')
    expect(methodLabel('OPTIONS')).toBe('OPTIONS')
  })

  it('truncates a long custom method so the layout holds', () => {
    expect(methodLabel('VERYLONGMETHOD')).toBe('VERYLO…')
  })
})

describe('tryFormatJson', () => {
  it('pretty-prints valid JSON', () => {
    expect(tryFormatJson('{"a":1}')).toBe('{\n  "a": 1\n}')
  })

  it('returns null rather than mangling text that is not JSON', () => {
    expect(tryFormatJson('hello')).toBeNull()
    expect(tryFormatJson('{not json')).toBeNull()
    expect(tryFormatJson('')).toBeNull()
  })

  it('handles arrays', () => {
    expect(tryFormatJson('[1,2]')).toBe('[\n  1,\n  2\n]')
  })
})
