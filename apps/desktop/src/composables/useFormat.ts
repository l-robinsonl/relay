/** Formatting helpers shared across the interface. */

/** A byte count as a short human-readable string. */
export function formatBytes (bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—'
  if (bytes === 0) return '0 B'
  if (bytes < 1024) return `${bytes} B`

  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes / 1024
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  // One decimal below 10 keeps "1.4 MB" informative without "1.43 MB" noise.
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`
}

/** A duration in milliseconds, scaled to a readable unit. */
export function formatDuration (ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '—'
  if (ms < 1) return '<1 ms'
  if (ms < 1000) return `${Math.round(ms)} ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)} s`
  const minutes = Math.floor(ms / 60_000)
  const seconds = Math.round((ms % 60_000) / 1000)
  return `${minutes}m ${seconds}s`
}

/** A timestamp as a relative description, falling back to a date. */
export function formatRelativeTime (millis: number): string {
  const delta = Date.now() - millis

  // Ahead of us. Everything here used to collapse to "just now",
  // which was right for clock skew and wrong for anything genuinely
  // in the future: a pending invitation with a week left read as
  // "expires just now".
  if (delta < 0) {
    const ahead = -delta
    if (ahead < 60_000) return 'just now'
    if (ahead < 3_600_000) return `in ${Math.floor(ahead / 60_000)} min`
    if (ahead < 86_400_000) {
      const hours = Math.floor(ahead / 3_600_000)
      return `in ${hours} hour${hours === 1 ? '' : 's'}`
    }
    if (ahead < 604_800_000) {
      const days = Math.floor(ahead / 86_400_000)
      return `in ${days} day${days === 1 ? '' : 's'}`
    }
    return `on ${new Date(millis).toLocaleDateString()}`
  }

  if (delta < 60_000) return 'just now'
  if (delta < 3_600_000) {
    const minutes = Math.floor(delta / 60_000)
    return `${minutes} min ago`
  }
  if (delta < 86_400_000) {
    const hours = Math.floor(delta / 3_600_000)
    return `${hours} hour${hours === 1 ? '' : 's'} ago`
  }
  if (delta < 604_800_000) {
    const days = Math.floor(delta / 86_400_000)
    return `${days} day${days === 1 ? '' : 's'} ago`
  }
  return new Date(millis).toLocaleDateString()
}

/** The CSS variable holding a method's colour. */
export function methodColor (method: string): string {
  const known = ['get', 'post', 'put', 'patch', 'delete', 'head', 'options']
  const lowered = method.toLowerCase()
  return known.includes(lowered) ? `var(--method-${lowered})` : 'var(--method-other)'
}

/** A short label for a method, so long custom verbs do not break the layout. */
export function methodLabel (method: string): string {
  if (method.length <= 7) return method
  return `${method.slice(0, 6)}…`
}

/** The tone to colour an HTTP status with. */
export function statusTone (status: number): 'success' | 'info' | 'warning' | 'danger' {
  if (status >= 500) return 'danger'
  if (status >= 400) return 'warning'
  if (status >= 300) return 'info'
  if (status >= 200) return 'success'
  return 'info'
}

/** Pretty-prints JSON, returning null when the text is not valid JSON. */
export function tryFormatJson (text: string, indent = 2): string | null {
  const trimmed = text.trim()
  if (!trimmed) return null
  const first = trimmed[0]
  if (first !== '{' && first !== '[') return null
  try {
    return JSON.stringify(JSON.parse(trimmed), null, indent)
  } catch {
    return null
  }
}

/** Formats a body for display when its content type is known. */
export function formatBody (text: string, contentType: string | null | undefined): string {
  if (!contentType) return tryFormatJson(text) ?? text
  if (contentType.includes('json')) return tryFormatJson(text) ?? text
  return text
}
