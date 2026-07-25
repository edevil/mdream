import type { MdreamOptions } from '../../src/types'
import { describe, expect, it } from 'vitest'
import { ConfigurationError, htmlToMarkdown, streamHtmlToMarkdown } from '../../src'

function unreadStream(pulls: { count: number }): ReadableStream<string> {
  return new ReadableStream({
    pull(controller) {
      pulls.count++
      controller.enqueue('<p>consumed</p>')
      controller.close()
    },
  })
}

async function collect(stream: AsyncIterable<string>): Promise<string> {
  let output = ''
  for await (const chunk of stream)
    output += chunk
  return output
}

describe('option validation', () => {
  it.each([
    { clean: { fragments: true } },
    { clean: true },
    { plugins: { isolateMain: true } },
    { plugins: { frontmatter: true } },
    { plugins: { extraction: { p: () => {} } } },
  ] satisfies Partial<MdreamOptions>[])('rejects unsupported streaming options before reading input', (options) => {
    const pulls = { count: 0 }
    expect(() => streamHtmlToMarkdown(unreadStream(pulls), options)).toThrow(ConfigurationError)
    expect(pulls.count).toBe(0)
    expect(() => htmlToMarkdown('<p>body</p>', options)).not.toThrow()
  })

  it('discards a large fallback prefix when a late main is found one-shot', () => {
    const html = `<h1>Fallback</h1><p>${'prefix '.repeat(20_000)}</p><main><h1>Real</h1><p>body</p></main>`
    expect(htmlToMarkdown(html, { plugins: { isolateMain: true } })).toBe('# Real\n\nbody')
  })

  it('applies partial, empty, nested, spacing, and whitespace overrides', async () => {
    const options: Partial<MdreamOptions> = {
      plugins: {
        tagOverrides: {
          strong: { enter: '[' },
          em: { exit: ']' },
          x: { enter: '', exit: '', spacing: [0, 0], isInline: true, collapsesInnerWhiteSpace: true },
        },
      },
    }
    const html = '<p><strong>a<em>b</em></strong><x>  c   d  </x></p>'
    const expected = '[a*b]** c d'
    expect(htmlToMarkdown(html, options)).toBe(expected)

    const input = new ReadableStream<string>({
      start(controller) {
        controller.enqueue(html.slice(0, 15))
        controller.enqueue(html.slice(15))
        controller.close()
      },
    })
    expect(await collect(streamHtmlToMarkdown(input, options))).toBe(expected)
  })

  it('keeps paired literal overrides out of built-in serializer rewriting', () => {
    expect(htmlToMarkdown('<a href="https://example.com"><strong>x</strong></a>', {
      plugins: {
        tagOverrides: {
          a: { enter: '<literal>', exit: '</literal>' },
        },
      },
    })).toBe('<literal>**x**</literal>')
  })

  it('rejects invalid aliases, unsafe partials, and malformed runtime values without panic', () => {
    expect(() => htmlToMarkdown('<x>body</x>', {
      plugins: { tagOverrides: { x: 'not-a-real-tag' } },
    })).toThrow('Unknown tag alias')
    expect(() => htmlToMarkdown('<a href="/x">body</a>', {
      plugins: { tagOverrides: { a: { enter: '[' } } },
    })).toThrow('must override both enter and exit')
    expect(() => htmlToMarkdown('<x>body</x>', {
      plugins: { tagOverrides: { x: { spacing: [1] } as any } },
    })).toThrow('invalid spacing')

    const aliases = ['', 'A'.repeat(10_000), '\0', '😀', '__proto__', 'constructor']
    for (const alias of aliases) {
      expect(() => htmlToMarkdown('<x>body</x>', {
        plugins: { tagOverrides: { x: alias } },
      })).toThrow(ConfigurationError)
    }
  })
})
