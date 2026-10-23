#!/usr/bin/env node
/**
 * Writes packages/shared-types/src/index.ts from whatever ts-rs put in
 * generated/.
 *
 * It used to be maintained by hand, under a header saying not to. The
 * cost of that showed up twice: nineteen generated types were exported
 * from nowhere and so could not be imported at all, and somebody
 * reasonably responded by hand-writing copies of four of them in
 * ipc.ts — which then drifted, losing `requestPatch`. CI could not
 * have caught either: its drift check compares the generated files
 * against Rust, and both problems live on the other side of that.
 *
 * Run as part of `npm run bindings`.
 */
import { readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const GENERATED = 'packages/shared-types/src/generated'
const INDEX = 'packages/shared-types/src/index.ts'

/** Every generated module, as { name, path } relative to src/. */
function collect (dir, prefix = 'generated') {
  const out = []
  for (const entry of readdirSync(join('.', dir), { withFileTypes: true })) {
    if (entry.isDirectory()) {
      out.push(...collect(join(dir, entry.name), `${prefix}/${entry.name}`))
    } else if (entry.name.endsWith('.ts')) {
      const name = entry.name.replace(/\.ts$/, '')
      out.push({ name, path: `./${prefix}/${name}` })
    }
  }
  return out
}

const modules = collect(GENERATED).sort((a, b) => a.name.localeCompare(b.name))

const body = [
  '// Written by scripts/write-bindings-index.mjs from the Rust types that',
  '// derive `TS`. Run `npm run bindings` to refresh it.',
  '//',
  '// Do not edit by hand, and do not re-declare any of these elsewhere:',
  '// a second copy of a generated type drifts silently, because the',
  '// drift check in CI compares these files against Rust and cannot see',
  '// a copy someone typed out.',
  '',
  ...modules.map((m) => `export type { ${m.name} } from '${m.path}'`),
  '',
].join('\n')

const before = (() => {
  try {
    return readFileSync(INDEX, 'utf8')
  } catch {
    return ''
  }
})()

writeFileSync(INDEX, body)

if (before !== body) {
  console.log(`shared-types index: ${modules.length} types`)
} else {
  console.log(`shared-types index: unchanged (${modules.length} types)`)
}
