import { describe, expect, it } from 'vitest'
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'

/**
 * A spinning icon must have an even `size`.
 *
 * A rotating SVG turns about the centre of its box, so a 13px icon
 * turns about 6.5px — half a pixel. The browser redraws the shape at
 * a slightly different subpixel offset every frame and the spinner
 * visibly orbits its own centre rather than turning on the spot. It
 * is obvious once seen and invisible in a screenshot, which is why
 * this is a test and not a note.
 *
 * The sizes beside it are usually odd, so the next person to add a
 * spinner will copy one. This is here to stop them.
 */
function vueFiles (dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry)
    if (statSync(path).isDirectory()) return vueFiles(path)
    return path.endsWith('.vue') ? [path] : []
  })
}

describe('spinners', () => {
  it('are all an even number of pixels across', () => {
    const offenders: string[] = []

    for (const path of vueFiles('src')) {
      const lines = readFileSync(path, 'utf8').split('\n')
      lines.forEach((line, index) => {
        if (!line.includes('animate-spin')) return

        // The size may be on this line or on one just above it, in a
        // multi-line element.
        const nearby = lines.slice(Math.max(0, index - 5), index + 2).join('\n')
        const size = nearby.match(/:size="(\d+)"/)
        if (size && Number(size[1]) % 2 !== 0) {
          offenders.push(`${path}:${index + 1} — size ${size[1]}`)
        }
      })
    }

    expect(offenders).toEqual([])
  })
})
