import { describe, expect, it } from 'vitest'
import { engines, resolveEngine } from '../utils/engines'

async function streamConvert(engine: Awaited<ReturnType<typeof resolveEngine>>, html: string, split: number): Promise<string> {
  const stream = new ReadableStream<string>({
    start(controller) {
      controller.enqueue(html.slice(0, split))
      controller.enqueue(html.slice(split))
      controller.close()
    },
  })
  let output = ''
  for await (const chunk of engine.streamHtmlToMarkdown(stream))
    output += chunk
  return output
}

describe.each(engines)('malformed HTML tokenizer parity: $name', ({ engine: engineHandle }) => {
  it('matches HTML recovery and streaming boundaries', async () => {
    const engine = await resolveEngine(engineHandle)
    const cases = [
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
      ['before<!-->after', 'beforeafter'],
      ['before<!--->after', 'beforeafter'],
      ['before<!--x--!>after', 'beforeafter'],
      ['before<!--x--->after', 'beforeafter'],
      ['before<!--x', 'before'],
      ['before<!foo', 'before'],
      ['before<?pi', 'before'],
      ['<a href=/first HREF=/second>link</a>', '[link](/first)'],
      ['<img src=/first SRC=/second alt=first ALT=second>', '![first](/first)'],
    ] as const
    for (const [html, expected] of cases) {
      expect(engine.htmlToMarkdown(html), html).toBe(expected)
      for (let split = 0; split <= html.length; split++)
        expect(await streamConvert(engine, html, split), `${html} split at ${split}`).toBe(expected)
    }
  })

  it('keeps invalid opener text in one Tailwind-formatted run', async () => {
    const engine = await resolveEngine(engineHandle)
    for (const [html, expected] of [
      ['<span class="italic">a<3 b</span>', '*a<3 b*'],
      ['<span class="italic">a< b</span>', '*a< b*'],
      ['<span class="italic">a<>b</span>', '*a<>b*'],
    ] as const) {
      expect(engine.htmlToMarkdown(html, { plugins: { tailwind: true } }), html).toBe(expected)
    }
  })
})
