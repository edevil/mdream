import { describe, expect, it } from 'vitest'
import { htmlToMarkdown, streamHtmlToMarkdown } from '../../src'

const dangerous = [
  'javascript:alert(1)',
  'JaVaScRiPt:alert(1)',
  '\tdata:text/html,payload',
  '\u007Ffile:///etc/passwd',
  'vbscript:msgbox(1)',
]

describe('url policy', () => {
  it.each(dangerous)('rejects %j in strict mode', (url) => {
    expect(htmlToMarkdown(`<a href="${url}">safe text</a>`, { urlPolicy: 'strict' })).toBe('safe text')
    expect(htmlToMarkdown(`<img src="${url}" alt="image">`, { urlPolicy: 'strict' })).toBe('')
  })

  it('preserves existing behavior by default', () => {
    expect(htmlToMarkdown('<a href="javascript:alert(1)">text</a>'))
      .toBe('[text](<javascript:alert(1)>)')
  })

  it('rejects invalid policies before conversion', () => {
    expect(() => htmlToMarkdown('<p>text</p>', { urlPolicy: 'invalid' } as any))
      .toThrow('Invalid urlPolicy: invalid')
    expect(() => streamHtmlToMarkdown(null, { urlPolicy: 'invalid' } as any))
      .toThrow('Invalid urlPolicy: invalid')
  })
})
