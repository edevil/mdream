import { describe, expect, it } from 'vitest'
import { engines, htmlToMarkdown, resolveEngine } from '../utils/engines'

const cases = [
  ['//cdn.example/path', 'http://example.test/base', 'http://cdn.example/path'],
  ['/root', 'https://user:pass@example.test:8443/docs/page?old=1#old', 'https://user:pass@example.test:8443/root'],
  ['../asset', 'https://example.test/docs/guide/', 'https://example.test/docs/asset'],
  ['./asset', 'https://example.test/docs/page', 'https://example.test/docs/asset'],
  ['?new=1', 'https://example.test/docs/page?old=1#old', 'https://example.test/docs/page?new=1'],
  ['#new', 'https://example.test/docs/page?old=1#old', '#new'],
] as const

describe.each(engines)('url resolution parity: $name', (engineConfig) => {
  it.each(cases)('resolves %s against %s', async (reference, origin, expected) => {
    const engine = await resolveEngine(engineConfig.engine)
    const markdown = htmlToMarkdown(`<a href="${reference}">link</a>`, { engine, origin })
    expect(markdown).toBe(`[link](${expected})`)
  })
})
