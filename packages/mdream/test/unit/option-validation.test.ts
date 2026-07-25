import type { EngineOptions } from '@mdream/js'
import { describe, expect, it } from 'vitest'
import { engines, resolveEngine } from '../utils/engines'

async function collect(stream: AsyncIterable<string>): Promise<string> {
  let output = ''
  for await (const chunk of stream)
    output += chunk
  return output
}

describe.each(engines)('option validation parity: $name', (engineConfig) => {
  it.each([
    { clean: { fragments: true } },
    { clean: true },
    { plugins: { isolateMain: true } },
    { plugins: { frontmatter: true } },
    { plugins: { extraction: { p: () => {} } } },
  ] satisfies Partial<EngineOptions>[])('rejects unsupported streaming options before accessing input', async (options) => {
    const engine = await resolveEngine(engineConfig.engine)
    let getReaderCalls = 0
    const input = {
      getReader() {
        getReaderCalls++
        throw new Error('input was accessed')
      },
    } as unknown as ReadableStream<string>

    await expect(
      Promise.resolve()
        .then(() => collect(engine.streamHtmlToMarkdown(input, options))),
    )
      .rejects
      .toThrow(/unsupported streaming option/i)
    expect(getReaderCalls).toBe(0)
    expect(() => engine.htmlToMarkdown('<p>body</p>', options)).not.toThrow()
  })

  it('uses a late explicit main instead of an already-rendered fallback', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `<h1>Fallback</h1><p>${'prefix '.repeat(20_000)}</p><main><h1>Real</h1><p>body</p></main>`
    expect(engine.htmlToMarkdown(html, { plugins: { isolateMain: true } })).toBe('# Real\n\nbody')
  })

  it('keeps valid partial and literal overrides identical', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const options: Partial<EngineOptions> = {
      plugins: {
        tagOverrides: {
          strong: { enter: '[' },
          em: { exit: ']' },
          x: { enter: '', exit: '', spacing: [0, 0], isInline: true, collapsesInnerWhiteSpace: true },
        },
      },
    }
    expect(engine.htmlToMarkdown('<strong>a<em>b</em></strong><x>  c   d  </x>', options))
      .toBe('[a*b]** c d')
  })

  it('rejects invalid aliases and stateful partial overrides', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    expect(() => engine.htmlToMarkdown('<x>body</x>', {
      plugins: { tagOverrides: { x: 'not-a-real-tag' } },
    })).toThrow(/unknown tag alias/i)
    expect(() => engine.htmlToMarkdown('<a href="/x">body</a>', {
      plugins: { tagOverrides: { a: { enter: '[' } } },
    })).toThrow(/both enter and exit/i)
  })
})
