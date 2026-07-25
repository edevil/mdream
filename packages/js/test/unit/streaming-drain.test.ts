import type { ParseState } from '../../src/parse'
import type { MdreamOptions, TransformPlugin } from '../../src/types'
import { describe, expect, it } from 'vitest'
import { createPlugin, htmlToMarkdown, streamHtmlToMarkdown } from '../../src/index'
import { createMarkdownProcessor } from '../../src/markdown-processor'
import { finalizeParse, parseHtmlStream } from '../../src/parse'

function chunkedStream(html: string, chunkSize: number): ReadableStream<string> {
  return new ReadableStream({
    start(controller) {
      for (let offset = 0; offset < html.length; offset += chunkSize)
        controller.enqueue(html.slice(offset, offset + chunkSize))
      controller.close()
    },
  })
}

async function streamChunks(chunks: string[], options: Partial<MdreamOptions> = {}): Promise<string> {
  const stream = new ReadableStream<string>({
    start(controller) {
      for (const chunk of chunks)
        controller.enqueue(chunk)
      controller.close()
    },
  })
  let markdown = ''
  for await (const chunk of streamHtmlToMarkdown(stream, options))
    markdown += chunk
  return markdown
}

async function streamedOutputChunks(chunks: string[], options: Partial<MdreamOptions> = {}): Promise<string[]> {
  const stream = new ReadableStream<string>({
    start(controller) {
      for (const chunk of chunks)
        controller.enqueue(chunk)
      controller.close()
    },
  })
  const output: string[] = []
  for await (const chunk of streamHtmlToMarkdown(stream, options))
    output.push(chunk)
  return output
}

async function streamConvert(html: string, chunkSize: number, options: Partial<MdreamOptions> = {}): Promise<string> {
  let markdown = ''
  for await (const chunk of streamHtmlToMarkdown(chunkedStream(html, chunkSize), options))
    markdown += chunk
  return markdown
}

const BLOCK_NEWLINE_HTML = [
  '<div class="wrap">\n\t\t\t\t        ',
  '<form action="https://ex.example/act?x=1&amp;id=42" class="foo bar wrap" data-flag method="post">',
  '<a aria-controls="dd"\n aria-expanded="false"\n class="btn menu-btn"\n data-dropdown="dd"\n href="#">\n ',
  '<span>Alpha Beta Gamma</a>\n<li>\n </li>\n</form>',
  '<div class="badges"><a href="/other-link/" target="_blank" class="bp"> Delta</a></div></div>',
].join('')

describe('streaming drain parity', () => {
  it.each([
    '<div>Alpha</div>',
    '<div>Alpha</div><div>Beta</div>',
    '<div>Alpha</div><em></em>',
    '<div>Alpha</div><em></em><div>Beta</div>',
    '<div>Alpha</div><em></em> tail',
    '<p>before <strong></strong><em>after</em></p>',
    '<blockquote><p>quote</p></blockquote><p>after</p>',
    '<ul><li>alpha</li><li>beta</li></ul>',
    '<ul><li><a href="/t">Schedule</a> New<br> <div>Domain Services</div></li></ul>',
    '<p>before<br></p>',
    '<details><summary>Title</summary><p>Body</p></details>',
    '<pre><code>alpha\n\n</code></pre>',
    '<pre><code>const x = `hi $' + '{y}`;</code></pre>',
    '<p>use <code>a`b</code> here</p>',
    '<table><tr><td>a`b</td><td>c\\d</td></tr></table>',
    '<p>text with <a href="/x">a [bracket] link</a> end</p>',
    '<ol><li>one<pre><code>cmd</code></pre></li><li>two</li></ol>',
    '<ul><li>one<pre><code>cmd</code></pre></li><li>two</li></ul>',
    '<summary>text <svg></svg></summary>',
    '<details><summary>text <svg><polyline points="1 2"></polyline></svg></summary><p>b</p></details>',
    '<h3>Set priority</h3><a class="anchor-link" href="#x"></a><p>The value.</p>',
    '<h2>Section</h2><a href="/x"><svg></svg></a><p>Body text.</p>',
    '<a href="https://example.com">https://example.com</a>',
    '<dl><dt>MPN:</dt><dd>D100</dd><dt>Availability:</dt><dd>Ships</dd></dl>',
    '<address><p>One</p><p>Two</p></address>',
    '<p>before</p><script>var x = 1; if (a < b) { y(); }</script><p>after</p>',
    '<script>a()</script><script>b()</script><p>ok</p>',
    '<p>x</p><script>let s = "</scr" + "ipt>end";</script><p>y</p>',
    '<p>one</p><script>\n  line1\n  line2\n</script><p>two</p>',
    '<p>answered on <span>03 Apr 2013,&nbsp;</span><span>09:53 AM</span></p>',
  ])('matches one-shot bytes at every chunk width: %s', async (html) => {
    const expected = htmlToMarkdown(html)

    for (let chunkSize = 1; chunkSize <= html.length; chunkSize++) {
      expect(await streamConvert(html, chunkSize), `chunkSize=${chunkSize}`).toBe(expected)
    }
  })

  it('keeps block newline context across drained chunks', async () => {
    const expected = htmlToMarkdown(BLOCK_NEWLINE_HTML)

    for (const chunkSize of [1, 3, 7, 16, 40])
      expect(await streamConvert(BLOCK_NEWLINE_HTML, chunkSize), `chunkSize=${chunkSize}`).toBe(expected)
  })

  it.each([
    ['nested quotes', '<blockquote><p>outer</p><blockquote><blockquote><p>inner</p></blockquote></blockquote><p>tail</p></blockquote>', {}],
    ['quote in list', '<ul><li>before<blockquote><p>quoted</p><ul><li>nested</li></ul></blockquote>after</li></ul>', {}],
    ['list in quote', '<blockquote><ol><li>one</li><li><strong>two</strong></li></ol></blockquote>', {}],
    ['hard breaks', '<blockquote><p>first<br>second<br><br>fourth</p></blockquote>', {}],
    ['links and markers', '<blockquote><p><a href="/x">link</a> <strong>bold</strong> <em>em</em> <code>a`b</code></p></blockquote>', {}],
    ['fenced code', '<blockquote><pre><code>alpha\n```\nomega</code></pre></blockquote>', {}],
    ['wrapping', '<blockquote><p>The quick brown fox jumps over the lazy dog and keeps running every day.</p></blockquote>', { wrapWidth: 24 }],
    ['wrapped list', '<blockquote><ul><li>a b c</li></ul></blockquote>', { wrapWidth: 8 }],
    ['wrapped quote in list', '<ul><li><blockquote><span>a b </span><span>c d e</span></blockquote></li></ul>', { wrapWidth: 5 }],
    ['bare text siblings', 'x<blockquote>y</blockquote>z', {}],
    ['inline sibling whitespace', '<blockquote><span>a </span><span>b</span></blockquote>', {}],
    ['hard break after space', '<blockquote><span>a </span><br>b</blockquote>', {}],
    ['nested quote in list', '<ul><li><blockquote><p>a</p><blockquote>b</blockquote><p>c</p></blockquote></li></ul>', {}],
  ] as const)('keeps streamed blockquote %s exact at every split', async (_name, html, options) => {
    const expected = htmlToMarkdown(html, options)
    for (let split = 0; split <= html.length; split++) {
      expect(
        await streamChunks([html.slice(0, split), html.slice(split)], options),
        `split=${split}`,
      ).toBe(expected)
    }
  })

  it('yields a 2 MiB quote before close and retains only frame context', () => {
    const content = 'a'.repeat(2 * 1024 * 1024)
    const processor = createMarkdownProcessor()
    const parseState: ParseState = {
      depthMap: processor.state.depthMap,
      depth: 0,
    }
    const handleEvent = processor.processEvent

    let leftover = parseHtmlStream('<blockquote><p>', parseState, handleEvent)
    let beforeClose = processor.getMarkdownChunk()
    for (let offset = 0; offset < content.length; offset += 8 * 1024) {
      leftover = parseHtmlStream(leftover + content.slice(offset, offset + 8 * 1024), parseState, handleEvent)
      beforeClose += processor.getMarkdownChunk()
      expect(processor.state.buffer.reduce((length, fragment) => length + fragment.length, 0)).toBeLessThan(32)
    }
    leftover = parseHtmlStream(`${leftover}</p>`, parseState, handleEvent)
    beforeClose += processor.getMarkdownChunk()
    expect(beforeClose.length).toBe(content.length + 2)
    expect(beforeClose.startsWith('> ')).toBe(true)
    expect(beforeClose.endsWith('a')).toBe(true)

    leftover = parseHtmlStream(`${leftover}</blockquote>`, parseState, handleEvent)
    finalizeParse(leftover, parseState, handleEvent)
    expect(beforeClose + processor.getMarkdownChunk(true)).toBe(`> ${content}`)
  }, 30_000)

  it('keeps blockquote cursors valid across plugin fragment rewrites', async () => {
    function coalescingPlugin(): TransformPlugin {
      return createPlugin({
        onNodeEnter(node, state) {
          if (node.name === 'span')
            state.buffer.splice(0, state.buffer.length, state.buffer.join(''))
        },
      })
    }
    const html = '<blockquote><p>before <span>inside</span> after</p></blockquote>'
    const expected = htmlToMarkdown(html, { hooks: [coalescingPlugin()] })

    for (let split = 0; split <= html.length; split++) {
      expect(await streamChunks(
        [html.slice(0, split), html.slice(split)],
        { hooks: [coalescingPlugin()] },
      ), `split=${split}`).toBe(expected)
    }
  })

  it('preserves cumulative plugin buffer fragments and array identity', async () => {
    let buffer: string[] | undefined
    const plugin = createPlugin({
      beforeNodeProcess(_event, state) {
        buffer ||= state.buffer
        expect(state.buffer).toBe(buffer)
      },
    })
    const html = '<strong>Alpha</strong><em>Beta</em><span>Gamma</span>'

    expect(await streamChunks([
      '<strong>Alpha</strong>',
      '<em>Beta</em>',
      '<span>Gamma</span>',
    ], { hooks: [plugin] })).toBe(htmlToMarkdown(html, { hooks: [createPlugin({})] }))
    expect(buffer).toContain('Alpha')
    expect(buffer).toContain('Beta')
    expect(buffer).toContain('Gamma')
    expect(buffer!.length).toBeGreaterThan(3)
  })

  it('drains plugin appends and mutations of the un-emitted tail', async () => {
    function tailPlugin(): TransformPlugin {
      let div = 0
      return createPlugin({
        onNodeEnter(node, state) {
          if (node.name !== 'div' || ++div !== 2)
            return
          const tail = state.buffer.length - 1
          state.buffer[tail] = `${state.buffer[tail]!.replace(/\n+$/, '')} changed\n\n`
          state.buffer.push('appended')
        },
      })
    }
    const html = '<div>Alpha</div><div>Beta</div>'
    const expected = htmlToMarkdown(html, { hooks: [tailPlugin()] })
    const actual = await streamChunks(['<div>Alpha</div>', '<div>Beta</div>'], { hooks: [tailPlugin()] })

    expect(actual).toBe(expected)
    expect(actual).toContain('changed')
    expect(actual).toContain('appended')
  })

  it('rebases the emitted cursor after a plugin coalesces historical fragments', async () => {
    function coalescingPlugin(): TransformPlugin {
      let div = 0
      return createPlugin({
        onNodeEnter(node, state) {
          if (node.name !== 'div' || ++div !== 2)
            return
          state.buffer.splice(0, state.buffer.length, state.buffer.join(''))
        },
      })
    }
    const html = '<div>Alpha</div><div>Beta</div>'

    expect(await streamChunks(['<div>Alpha</div>', '<div>Beta</div>'], { hooks: [coalescingPlugin()] }))
      .toBe(htmlToMarkdown(html, { hooks: [coalescingPlugin()] }))
  })

  it('restores active rewrite anchors after a plugin coalesces fragments', async () => {
    function coalescingPlugin(): TransformPlugin {
      return createPlugin({
        onNodeEnter(node, state) {
          if (node.name === 'span')
            state.buffer.splice(0, state.buffer.length, state.buffer.join(''))
        },
      })
    }
    const html = '<code>alpha<span>beta</span>gamma</code>'
    const expected = htmlToMarkdown(html, { hooks: [coalescingPlugin()] })

    expect(await streamChunks(['<code>alpha', '<span>beta</span>', 'gamma</code>'], { hooks: [coalescingPlugin()] }))
      .toBe(expected)
  })

  it('preserves active anchors when plugins change earlier fragment lengths', async () => {
    function rewritingPlugin(): TransformPlugin {
      return createPlugin({
        onNodeEnter(node, state) {
          if (node.name !== 'span')
            return
          const text = state.buffer.indexOf('X')
          state.buffer[text] = 'QX'
        },
      })
    }
    const html = '<a href="/x"><code>X<span>a</span></code></a>'
    const expected = htmlToMarkdown(html, { hooks: [rewritingPlugin()] })

    expect(expected).toContain('QX')
    expect(await streamChunks(['<a href="/x"><code>X', '<span>a</span>', '</code></a>'], { hooks: [rewritingPlugin()] }))
      .toBe(expected)
  })

  it('tracks active anchors through length edits followed by coalescing', async () => {
    function rewritingPlugin(): TransformPlugin {
      return createPlugin({
        onNodeEnter(node, state) {
          if (node.name !== 'span')
            return
          const text = state.buffer.indexOf('X')
          state.buffer[text] = 'QX'
          state.buffer.splice(0, state.buffer.length, state.buffer.join(''))
        },
      })
    }
    const html = '<a href="/x">X<code><span>a</span></code></a>'
    const expected = htmlToMarkdown(html, { hooks: [rewritingPlugin()] })

    expect(expected).toContain('QX')
    expect(await streamChunks(['<a href="/x">X<code>', '<span>a</span>', '</code></a>'], { hooks: [rewritingPlugin()] }))
      .toBe(expected)
  })

  it('does not join the cumulative plugin buffer while draining', async () => {
    let protectedBuffer: string[] | undefined
    const plugin = createPlugin({
      beforeNodeProcess(_event, state) {
        if (protectedBuffer)
          return
        protectedBuffer = state.buffer
        Object.defineProperty(state.buffer, 'join', {
          value: () => {
            throw new Error('cumulative buffer joined')
          },
        })
      },
    })
    const chunks = Array.from({ length: 128 }, (_, index) => `<span>${index}</span>`)
    const html = chunks.join('')

    expect(await streamChunks(chunks, { hooks: [plugin] })).toBe(htmlToMarkdown(html))
    expect(protectedBuffer!.length).toBe(chunks.length)
  })

  it.each([
    '<p>before <strong></strong><em>after</em></p>',
    '<p>use <code>a`b``c</code> after</p>',
    '<pre><code>before\n```\nafter</code></pre>',
    '<p>UTF-16 🎉 boundary</p>',
  ])('keeps held marker and code boundaries at every chunk width: %s', async (html) => {
    const expected = htmlToMarkdown(html)
    const noOpPlugin = createPlugin({})

    for (let chunkSize = 1; chunkSize <= html.length; chunkSize++) {
      expect(await streamConvert(html, chunkSize, { hooks: [noOpPlugin] }), `chunkSize=${chunkSize}`)
        .toBe(expected)
    }
  })

  it.each([
    ['code span', '<code>', '</code>', 'a````b'],
    ['backtick fence', '<pre><code>', '</code></pre>', 'a\n````\nb'],
    ['tilde fence', '<pre><code class="language-a`b">', '</code></pre>', 'a\n~~~~\nb'],
  ])('sizes %s delimiters split across fragment boundaries', async (_name, open, close, content) => {
    const html = `${open}${content}${close}`
    const chunks = [open, ...content, close]

    expect(await streamChunks(chunks)).toBe(htmlToMarkdown(html))
  })

  it('keeps the longest line-leading fence marker run', async () => {
    const html = '<pre><code>````\n`\nafter</code></pre>'
    const markdown = await streamChunks(['<pre><code>', '``', '``\n', '`\n', 'after', '</code></pre>'])

    expect(markdown).toBe(htmlToMarkdown(html))
    expect(markdown.startsWith('`````\n')).toBe(true)
    expect(markdown.endsWith('\n`````')).toBe(true)
  })

  it('never yields between UTF-16 surrogate halves', async () => {
    let span = 0
    const plugin = createPlugin({
      onNodeEnter(node, state) {
        if (node.name !== 'span')
          return
        state.buffer.push(++span === 1 ? '\uD83C' : '\uDF89')
      },
    })
    const output = await streamedOutputChunks(['<span></span>', '<span></span>'], { hooks: [plugin] })

    expect(output.join('')).toBe('🎉')
    for (const chunk of output) {
      const first = chunk.charCodeAt(0)
      const last = chunk.charCodeAt(chunk.length - 1)
      expect(first >= 0xDC00 && first <= 0xDFFF).toBe(false)
      expect(last >= 0xD800 && last <= 0xDBFF).toBe(false)
    }
  })

  it('retains a surrogate half across plugin-free compaction', async () => {
    const options: Partial<MdreamOptions> = {
      plugins: {
        tagOverrides: {
          'x-high': { enter: 'A\uD83C' },
          'x-low': { enter: '\uDF89' },
        },
      },
    }
    const output = await streamedOutputChunks(['<x-high></x-high>', '<x-low></x-low>'], options)

    expect(output.join('')).toBe('A🎉')
    expect(output).toEqual(['A', '🎉'])
  })

  it('compacts wrapped output without losing line context', async () => {
    const html = '<p>alpha beta gamma delta epsilon zeta eta theta iota kappa</p>'
    const expected = htmlToMarkdown(html, { wrapWidth: 12 })

    for (let chunkSize = 1; chunkSize <= html.length; chunkSize++)
      expect(await streamConvert(html, chunkSize, { wrapWidth: 12 }), `chunkSize=${chunkSize}`).toBe(expected)
  })

  it('counts surrogate pairs split across fragments as one wrap column', async () => {
    const options: Partial<MdreamOptions> = {
      wrapWidth: 4,
      plugins: {
        tagOverrides: {
          'x-high': { enter: '\uD83C' },
          'x-low': { enter: '\uDF89' },
        },
      },
    }
    const html = '<x-high></x-high><x-low></x-low><span>a </span><span>a</span>'

    expect(await streamChunks(['<x-high></x-high>', '<x-low></x-low>', '<span>a </span>', '<span>a</span>'], options))
      .toBe(htmlToMarkdown(html, options))
  })

  it('carries a high surrogate through compacted wrap context', async () => {
    const options: Partial<MdreamOptions> = {
      wrapWidth: 11,
      plugins: {
        tagOverrides: {
          'x-high': { enter: '\uD83C' },
          'x-tail': { enter: '\uDF89abcdef ' },
        },
      },
    }
    const html = '<x-high></x-high><x-tail></x-tail><span>a b</span>'

    expect(await streamChunks(['<x-high></x-high>', '<x-tail></x-tail>', '<span>a b</span>'], options))
      .toBe(htmlToMarkdown(html, options))
  })

  it.runIf(process.env.MDREAM_STRESS_TESTS === '1').each([4, 8, 16])('keeps held %d MiB nodes frame-linear', async (sizeMiB) => {
    const content = 'a'.repeat(sizeMiB * 1024 * 1024)
    const fixtures = [
      `<code>${content}</code>`,
      `<pre><code>${content}</code></pre>`,
      `<a href="/target">${content}</a>`,
      `<blockquote>${content}</blockquote>`,
    ]

    for (const html of fixtures)
      expect(await streamConvert(html, 8 * 1024)).toBe(htmlToMarkdown(html))
  }, 15 * 60_000)
})
