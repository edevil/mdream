import { describe, expect, it } from 'vitest'
import { resolveUrl } from '../../src/tags'

const cases = [
  ['//cdn.example/path', 'https://example.test/base', 'https://cdn.example/path'],
  ['//cdn.example/path', 'http://example.test/base', 'http://cdn.example/path'],
  ['/root', 'https://user:pass@example.test:8443/docs/page?old=1#old', 'https://user:pass@example.test:8443/root'],
  ['../asset', 'https://example.test/docs/guide/', 'https://example.test/docs/asset'],
  ['./asset', 'https://example.test/docs/page', 'https://example.test/docs/asset'],
  ['page', 'https://example.test/docs/', 'https://example.test/docs/page'],
  ['page', 'https://example.test/docs', 'https://example.test/page'],
  ['?new=1', 'https://example.test/docs/page?old=1#old', 'https://example.test/docs/page?new=1'],
  ['#new', 'https://example.test/docs/page?old=1#old', '#new'],
  ['https://other.test/a/../b', 'https://example.test/', 'https://other.test/b'],
  ['mailto:a@b.com', 'https://example.test/', 'mailto:a@b.com'],
] as const

describe('url resolution', () => {
  it.each(cases)('resolves %s against %s', (reference, origin, expected) => {
    expect(resolveUrl(reference, origin)).toBe(expected)
  })

  it('preserves protocol-relative references without a base scheme', () => {
    expect(resolveUrl('//cdn.example/path')).toBe('//cdn.example/path')
  })

  it('preserves references when the base is invalid', () => {
    expect(resolveUrl('../asset', 'not a URL')).toBe('../asset')
  })
})
