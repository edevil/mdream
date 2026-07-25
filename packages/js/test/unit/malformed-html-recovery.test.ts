import type { MdreamOptions } from '../../src/types'
import { describe, expect, it } from 'vitest'
import { htmlToMarkdown, streamHtmlToMarkdown } from '../../src/index'
import { parseAttributes } from '../../src/parse'

async function streamConvert(html: string, split: number, options: Partial<MdreamOptions> = {}): Promise<string> {
  const stream = new ReadableStream<string>({
    start(controller) {
      controller.enqueue(html.slice(0, split))
      controller.enqueue(html.slice(split))
      controller.close()
    },
  })
  let output = ''
  for await (const chunk of streamHtmlToMarkdown(stream, options))
    output += chunk
  return output
}

async function expectAtEverySplit(html: string, expected: string, options: Partial<MdreamOptions> = {}): Promise<void> {
  expect(htmlToMarkdown(html, options), html).toBe(expected)
  for (let split = 0; split <= html.length; split++)
    expect(await streamConvert(html, split, options), `${html} split at ${split}`).toBe(expected)
}

describe('malformed HTML recovery', () => {
  it('reconsumes invalid tag openers as visible text', async () => {
    for (const [html, expected] of [
      ['<p>I <3 Rust</p>', 'I <3 Rust'],
      ['<p>I < 3 Rust</p>', 'I < 3 Rust'],
      ['<p>I <> Rust</p>', 'I <> Rust'],
      ['<p>I <<em>love</em> Rust</p>', 'I <*love* Rust'],
      ['<3', '<3'],
      ['< 3', '< 3'],
      ['<>', '<>'],
      ['<', '<'],
      ['</', '\\</'],
      ['before<a', 'before'],
      ['<p>before</>after', 'beforeafter'],
      ['<p>before</>', 'before'],
      ['<?pi?>after', 'after'],
      ['</3>after', 'after'],
      ['</>after', 'after'],
      ['<!foo>after', 'after'],
    ] as const) {
      await expectAtEverySplit(html, expected)
    }
  })

  it('closes malformed comments in the HTML comment end states', async () => {
    for (const [html, expected] of [
      ['before<!-->after', 'beforeafter'],
      ['before<!--->after', 'beforeafter'],
      ['before<!--x--!>after', 'beforeafter'],
      ['before<!--x--->after', 'beforeafter'],
      ['before<!--x', 'before'],
      ['before<!foo', 'before'],
      ['before<?pi', 'before'],
    ] as const) {
      await expectAtEverySplit(html, expected)
    }
  })

  it('keeps the first duplicate and malformed attribute values', () => {
    expect(parseAttributes('href=/first HREF=/second src=one SRC=two class=a CLASS=b id=one ID=two lang=js LANG=python')).toEqual({
      href: '/first',
      src: 'one',
      class: 'a',
      id: 'one',
      lang: 'js',
    })
    const unusualNames = parseAttributes('__proto__=first __PROTO__=second Ä=upper ä=lower')
    expect(Object.entries(unusualNames)).toEqual([
      ['__proto__', 'first'],
      ['Ä', 'upper'],
      ['ä', 'lower'],
    ])
    expect(parseAttributes('a="1"b=\'2\' c')).toEqual({ a: '1', b: '2', c: '' })
    expect(parseAttributes('a=b"c\'d<e=f`g')).toEqual({ a: 'b"c\'d<e=f`g' })
    expect(htmlToMarkdown('<a href=/first HREF=/second>link</a>')).toBe('[link](/first)')
    expect(htmlToMarkdown('<img src=/first SRC=/second alt=first ALT=second>')).toBe('![first](/first)')
  })

  it('keeps invalid opener text in one Tailwind-formatted run', async () => {
    const options = { plugins: { tailwind: true } }
    await expectAtEverySplit('<span class="italic">a<3 b</span>', '*a<3 b*', options)
    await expectAtEverySplit('<span class="italic">a< b</span>', '*a< b*', options)
    await expectAtEverySplit('<span class="italic">a<>b</span>', '*a<>b*', options)
  })
})
