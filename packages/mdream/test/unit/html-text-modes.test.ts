import { describe, expect, it } from 'vitest'
import { engines, resolveEngine } from '../utils/engines'
import { parseMarkdown } from '../utils/markdown'

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

describe.each(engines)('html tokenizer parity: $name', ({ engine: engineHandle }) => {
  it('matches self-close and text-mode semantics', async () => {
    const engine = await resolveEngine(engineHandle)
    const cases = [
      ['<strong/>inside</strong><p>after</p>', '**inside**\n\nafter'],
      ['<script/><p>hidden</p></script><p>shown</p>', 'shown'],
      ['<template/><b>hidden</b></template><p>shown</p>', 'shown'],
      ['before<![CDATA[hidden>shown]]><p>after</p>', 'beforeshown]]>\n\nafter'],
      ['<xmp>&amp; <b>literal</b></xmp>', '\\&amp; \\<b>literal\\</b>'],
      ['<textarea>A &amp; <b>x</b><!-- y --></textarea>', 'A & \\<b>x\\</b>\\<!-- y -->'],
      ['<plaintext>A &amp; </plaintext><p>still text</p>', 'A \\&amp; \\</plaintext>\\<p>still text\\</p>'],
      ['<noembed>hidden</noembed><p>shown</p>', 'shown'],
      ['before</br>after', 'before\\\nafter'],
      ['<div title="x\\">after</div><p>shown</p>', 'after\n\nshown'],
      ['<svg><title><', '<'],
      ['<svg><title><strong', ''],
      ['<svg><desc><', '<'],
      ['<svg><foreignObject><', '<'],
      ['<svg><desc><strong', ''],
      ['<svg><foreignObject><strong', ''],
      ['<svg><a href=/u =x/>after</svg>', '[](/u)after'],
      ['<xmp><script>alert(1)</script></xmp>', '\\<script>alert(1)\\</script>'],
      ['<svg><title><strong>one</strong><em>two</em></title></svg>', '**one***two*'],
      ['<svg><title>a</div><strong>b</strong></title></svg>', 'a**b**'],
      ['<svg><text><![CDATA[a<b>&amp;]]></text></svg>', 'a\\<b>\\&amp;'],
      ['<svg><text><![CDATA[&amp;]]></text></svg>', '\\&amp;'],
      ['<svg><text>a<![CDATA[b]]>c</text></svg>', 'abc'],
      ['<svg><text><![CDATA[&am]]><![CDATA[p;]]></text></svg>', '\\&amp;'],
      ['<svg><text><![CDATA[a]]> b</text></svg>', 'a b'],
      ['<svg>a</svg><svg><svg/>b</svg>', 'a b'],
      ['<svg><title><![CDATA[hidden]]></title></svg>', ''],
    ] as const
    for (const [html, expected] of cases)
      expect(engine.htmlToMarkdown(html), html).toBe(expected)
  })

  it('matches one-shot output at every streaming split', async () => {
    const engine = await resolveEngine(engineHandle)
    const html = '<xmp>&amp;<b>x</b></xmp><textarea>&amp;<!--x--></textarea><script/><p>hidden</p></script><template/><b>hidden</b></template><div title="x\\">after</div><p>shown</p><svg><a href=/u =x/>tail<text>x<![CDATA[&am]]><![CDATA[p;]]> y</text><title>a</div><strong>b</strong><'
    const expected = engine.htmlToMarkdown(html)
    for (let split = 0; split <= html.length; split++)
      expect(await streamConvert(engine, html, split), `split at ${split}`).toBe(expected)
  })

  it('preserves EOF less-than at SVG integration points', async () => {
    const engine = await resolveEngine(engineHandle)
    for (const html of ['<svg><desc><', '<svg><foreignObject><']) {
      for (let split = 0; split <= html.length; split++)
        expect(await streamConvert(engine, html, split), `${html} split at ${split}`).toBe('<')
    }
  })

  it('does not apply HTML voidness inside SVG foreign content', async () => {
    const engine = await resolveEngine(engineHandle)
    expect(engine.htmlToMarkdown('<svg><source>inside</source></svg>', {
      plugins: { tagOverrides: { source: { enter: '[', exit: ']', isInline: true } } },
    })).toBe('[inside]')

    const extracted: string[] = []
    engine.htmlToMarkdown('<svg><source>inside</source></svg>', {
      plugins: {
        tagOverrides: { source: 'source' },
        extraction: { source: element => extracted.push(element.textContent) },
      },
    })
    expect(extracted).toEqual(['inside'])
  })

  it('applies aliases whose source is a built-in tag', async () => {
    const engine = await resolveEngine(engineHandle)
    const html = '<strong>&amp; <b>literal</b></strong><p>after</p>'
    const expected = '\\&amp; \\<b>literal\\</b>\n\nafter'
    const options = { plugins: { tagOverrides: { strong: 'xmp' } } } as const
    expect(engine.htmlToMarkdown(html, options)).toBe(expected)
    for (let split = 0; split <= html.length; split++) {
      const stream = new ReadableStream<string>({
        start(controller) {
          controller.enqueue(html.slice(0, split))
          controller.enqueue(html.slice(split))
          controller.close()
        },
      })
      let output = ''
      for await (const chunk of engine.streamHtmlToMarkdown(stream, options))
        output += chunk
      expect(output, `split at ${split}`).toBe(expected)
    }

    const script = '<script><!--<script></script>--></script><p>after</p>'
    const scriptOptions = { plugins: { tagOverrides: { script: 'script' } } } as const
    for (let split = 0; split <= script.length; split++) {
      const stream = new ReadableStream<string>({
        start(controller) {
          controller.enqueue(script.slice(0, split))
          controller.enqueue(script.slice(split))
          controller.close()
        },
      })
      let output = ''
      for await (const chunk of engine.streamHtmlToMarkdown(stream, scriptOptions))
        output += chunk
      expect(output, `identity script alias split at ${split}`).toBe('after')
    }
  })

  it('applies aliases to synthetic tags', async () => {
    const engine = await resolveEngine(engineHandle)
    expect(engine.htmlToMarkdown('before</br>after', {
      plugins: { tagOverrides: { br: 'strong' } },
    })).toBe('beforeafter')
    expect(engine.htmlToMarkdown('before<![CDATA[hidden]]>after', {
      plugins: { tagOverrides: { '#cdata-section': 'br' } },
    })).toBe('before\\\nafter')
    expect(engine.htmlToMarkdown(`${'<![CDATA[hidden]]>'.repeat(600)}<p>after</p>`, {
      plugins: { tagOverrides: { '#cdata-section': 'meta' } },
    })).toBe('after')
  })

  it('keeps literal text-mode payloads inert when Markdown is reparsed', async () => {
    const engine = await resolveEngine(engineHandle)
    for (const html of [
      '<xmp>&amp; <script>alert(1)</script><b>bold</b></xmp>',
      '<xmp><script>alert(1)</script><b>bold</b></xmp>',
      '<plaintext>&amp; <script>alert(1)</script><b>bold</b>',
      '<textarea>&lt;script&gt;alert(1)&lt;/script&gt;<b>bold</b></textarea>',
    ]) {
      const ast = parseMarkdown(engine.htmlToMarkdown(html))
      const types: string[] = []
      const visit = (node: { type?: string, children?: unknown[] }) => {
        if (node.type)
          types.push(node.type)
        for (const child of node.children || [])
          visit(child as { type?: string, children?: unknown[] })
      }
      visit(ast)
      expect(types, html).not.toContain('html')
      expect(types, html).not.toContain('link')
      expect(types, html).not.toContain('strong')
    }
  })
})
