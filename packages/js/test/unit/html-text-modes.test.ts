import type { ElementNode, MdreamOptions, TextNode } from '../../src/types'
import { describe, expect, it } from 'vitest'
import { ELEMENT_NODE, NodeEventEnter, NodeEventExit } from '../../src/const'
import { htmlToMarkdown, streamHtmlToMarkdown } from '../../src/index'
import { parseHtml } from '../../src/parse'

async function streamConvert(chunks: string[], options: Partial<MdreamOptions> = {}): Promise<string> {
  const stream = new ReadableStream<string>({
    start(controller) {
      for (const chunk of chunks)
        controller.enqueue(chunk)
      controller.close()
    },
  })
  let output = ''
  for await (const chunk of streamHtmlToMarkdown(stream, options))
    output += chunk
  return output
}

describe('html self-closing and text modes', () => {
  it('ignores a solidus on ordinary HTML and honors intrinsic voids and overrides', () => {
    expect(htmlToMarkdown('<strong/>inside</strong><p>after</p>')).toBe('**inside**\n\nafter')
    expect(htmlToMarkdown('before<br>after<img src=x>tail')).toBe('before\\\nafter![](x)tail')
    expect(htmlToMarkdown(`${'<keygen>'.repeat(600)}<p>after</p>`)).toBe('after')
    expect(htmlToMarkdown('<widget>after', {
      plugins: { tagOverrides: { widget: { enter: '[', exit: ']', isInline: true, isSelfClosing: true } } },
    })).toBe('[]after')
    expect(htmlToMarkdown('<STRONG>after', {
      plugins: { tagOverrides: { strong: { enter: '[', exit: ']', isInline: true, isSelfClosing: true } } },
    })).toBe('[]after')
  })

  it('distinguishes an unquoted-value solidus from a self-closing marker', () => {
    const { events } = parseHtml('<div data=x/><span>one</span></div><div data=x=/><span>equals</span></div><div data=/><span>slash</span></div><div data=x /><span>two</span></div>')
    const divs = events.filter(event => event.type === NodeEventEnter && event.node.type === ELEMENT_NODE && (event.node as ElementNode).name === 'div').map(event => event.node as ElementNode)
    const spans = events.filter(event => event.type === NodeEventEnter && event.node.type === ELEMENT_NODE && (event.node as ElementNode).name === 'span').map(event => event.node as ElementNode)
    expect(divs.map(div => div.attributes.data)).toEqual(['x/', 'x=/', '/', 'x'])
    expect(spans.map(span => span.parent)).toEqual(divs)

    const malformed = parseHtml('<svg><a href=/u =x/>after</svg>').events
    const after = malformed.find(event => event.type === NodeEventEnter && event.node.type !== ELEMENT_NODE && (event.node as TextNode).value === 'after')
    expect(after?.node.parent?.name).toBe('svg')
    expect(htmlToMarkdown('<svg><a href=/u =x/>after</svg>')).toBe('[](/u)after')
  })

  it('keeps script and template open until real end tags', () => {
    expect(htmlToMarkdown('<script/><p>hidden</p></script><p>shown</p>')).toBe('shown')
    const extracted: string[] = []
    expect(htmlToMarkdown('<template/><strong class=target>hidden</strong></template><p>shown</p>', {
      plugins: { extraction: { '.target': element => extracted.push(element.textContent) } },
    })).toBe('shown')
    expect(extracted).toEqual(['hidden'])
    expect(htmlToMarkdown('before<![CDATA[hidden>shown]]><p>after</p>')).toBe('beforeshown]]>\n\nafter')
  })

  it('uses RAWTEXT, RCDATA, and plaintext tokenization', () => {
    expect(htmlToMarkdown('<xmp>&amp; <b>literal</b></xmp>')).toBe('\\&amp; \\<b>literal\\</b>')
    expect(htmlToMarkdown('<xmp><script>alert(1)</script></xmp>')).toBe('\\<script>alert(1)\\</script>')
    expect(htmlToMarkdown('<textarea>A &amp; <b>x</b><!-- y --></textarea>')).toBe('A & \\<b>x\\</b>\\<!-- y -->')
    expect(htmlToMarkdown('<title>A &amp; <b>x</b></title>')).toBe('A & \\<b>x\\</b>')
    expect(htmlToMarkdown('<plaintext>A &amp; </plaintext><p>still text</p>')).toBe('A \\&amp; \\</plaintext>\\<p>still text\\</p>')
    expect(htmlToMarkdown('<style>&amp;<b>x</b></style><iframe>x</iframe><noframes>x</noframes><noembed>x</noembed><p>shown</p>')).toBe('shown')
  })

  it('keeps text-mode bytes literal in the event tree', () => {
    const { events } = parseHtml('<xmp>&amp; <b>x</b></xmp><textarea>&amp;<!--x--></textarea>')
    const texts = events
      .filter(event => event.type === NodeEventEnter && event.node.type !== ELEMENT_NODE)
      .map(event => (event.node as TextNode).value)
    expect(texts).toEqual(['&amp; <b>x</b>', '&<!--x-->'])
  })

  it('commits a leading less-than leftover at text-mode EOF', () => {
    expect(htmlToMarkdown('<xmp><')).toBe('<')
    expect(htmlToMarkdown('<textarea><')).toBe('<')
    expect(htmlToMarkdown('<plaintext><')).toBe('<')
    expect(htmlToMarkdown('<svg><title><')).toBe('<')
    expect(htmlToMarkdown('<svg><title><strong')).toBe('')
    expect(htmlToMarkdown('<svg><desc><')).toBe('<')
    expect(htmlToMarkdown('<svg><foreignObject><')).toBe('<')
    expect(htmlToMarkdown('<svg><desc><strong')).toBe('')
    expect(htmlToMarkdown('<svg><foreignObject><strong')).toBe('')
  })

  it('supports syntactic self-close only for the supported SVG contract', () => {
    const { events } = parseHtml('<svg><path /><text>foreign</text></svg><div /><span>html</span></div>')
    const elements = events
      .filter(event => event.node.type === ELEMENT_NODE)
      .map(event => `${event.type === NodeEventEnter ? 'enter' : event.type === NodeEventExit ? 'exit' : '?'}:${(event.node as ElementNode).name}`)
    expect(elements).toEqual([
      'enter:svg',
      'enter:path',
      'exit:path',
      'enter:text',
      'exit:text',
      'exit:svg',
      'enter:div',
      'enter:span',
      'exit:span',
      'exit:div',
    ])
    expect(htmlToMarkdown('<svg><foreignObject><strong/>inside</strong></foreignObject></svg>')).toBe('**inside**')
    const svgIntegration = '<svg><desc><strong/>inside</strong></desc><title><em/>title</em></title></svg>'
    expect(htmlToMarkdown(svgIntegration)).toBe('**inside***title*')
    expect(htmlToMarkdown('<svg><title><strong>one</strong><em>two</em></title></svg>')).toBe('**one***two*')
    expect(htmlToMarkdown('<svg><title>a</div><strong>b</strong></title></svg>')).toBe('a**b**')
    expect(htmlToMarkdown('<svg><text><![CDATA[a<b>&amp;]]></text></svg>')).toBe('a\\<b>\\&amp;')
    expect(htmlToMarkdown('<svg><text><![CDATA[&amp;]]></text></svg>')).toBe('\\&amp;')
    expect(htmlToMarkdown('<svg><text>a<![CDATA[b]]>c</text></svg>')).toBe('abc')
    expect(htmlToMarkdown('<svg><text><![CDATA[&am]]><![CDATA[p;]]></text></svg>')).toBe('\\&amp;')
    expect(htmlToMarkdown('<svg><text><![CDATA[a]]> b</text></svg>')).toBe('a b')
    expect(htmlToMarkdown('<svg>a</svg><svg><svg/>b</svg>')).toBe('a b')
    expect(htmlToMarkdown('<svg><text>a<![CDATA[b]]>c</text></svg>', { wrapWidth: 80 })).toBe('abc')
    expect(htmlToMarkdown('<svg><title><![CDATA[hidden]]></title></svg>')).toBe('')
    const sourceEvents = parseHtml('<svg><source><strong>inside</strong></source></svg>').events
    const source = sourceEvents.find(event => event.type === NodeEventEnter && event.node.type === ELEMENT_NODE && (event.node as ElementNode).name === 'source')?.node
    const strong = sourceEvents.find(event => event.type === NodeEventEnter && event.node.type === ELEMENT_NODE && (event.node as ElementNode).name === 'strong')?.node
    expect(strong?.parent).toBe(source)
  })

  it('does not treat backslashes as HTML quote escapes', async () => {
    const html = '<div title="x\\">after</div><p>shown</p>'
    expect(htmlToMarkdown(html)).toBe('after\n\nshown')
    for (let split = 0; split <= html.length; split++)
      expect(await streamConvert([html.slice(0, split), html.slice(split)]), `split at ${split}`).toBe('after\n\nshown')
  })

  it('uses target text modes for aliases and only closes them by alias name', async () => {
    expect(htmlToMarkdown('<x>hidden</iframe><p>still hidden</p></x><p>shown</p>', {
      plugins: { tagOverrides: { x: 'iframe' } },
    })).toBe('shown')

    const cases = [
      ['<x>&amp; <b>literal</b></x><p>after</p>', '\\&amp; \\<b>literal\\</b>\n\nafter', 'x', 'xmp'],
      ['<x>A &amp; </x><p>still text</p>', 'A \\&amp; \\</x>\\<p>still text\\</p>', 'x', 'plaintext'],
      ['<x>&amp; <b>x</b></x>', '& \\<b>x\\</b>', 'x', 'textarea'],
      ['<strong>&amp; <b>literal</b></strong><p>after</p>', '\\&amp; \\<b>literal\\</b>\n\nafter', 'strong', 'xmp'],
    ] as const
    for (const [html, expected, source, target] of cases) {
      const options: Partial<MdreamOptions> = { plugins: { tagOverrides: { [source]: target } } }
      expect(htmlToMarkdown(html, options)).toBe(expected)
      for (let split = 0; split <= html.length; split++)
        expect(await streamConvert([html.slice(0, split), html.slice(split)], options), `${source}->${target} split at ${split}`).toBe(expected)
    }

    const script = '<script><!--<script></script>--></script><p>after</p>'
    const options: Partial<MdreamOptions> = { plugins: { tagOverrides: { script: 'script' } } }
    for (let split = 0; split <= script.length; split++)
      expect(await streamConvert([script.slice(0, split), script.slice(split)], options), `identity script alias split at ${split}`).toBe('after')
  })

  it('matches whole input at every streaming split', async () => {
    const html = '<p>before</p><xmp>&amp; <b>x</b></xmp><textarea>A &amp; <i>y</i><!--z--></textarea><style><p>hidden</p></style><script/><p>hidden</p></script><template/><b>hidden</b></template><svg><text>x<![CDATA[&am]]><![CDATA[p;]]> y</text><title>a</div><strong>b</strong></title></svg><p>after</p>'
    const expected = await streamConvert([html])
    expect(expected).toBe(htmlToMarkdown(html))
    for (let split = 0; split <= html.length; split++) {
      expect(await streamConvert([html.slice(0, split), html.slice(split)]), `split at ${split}`).toBe(expected)
    }
  })

  it('recovers an end br as a start tag', () => {
    expect(htmlToMarkdown('before</br>after')).toBe('before\\\nafter')
    expect(htmlToMarkdown('before</br>after', {
      plugins: { tagOverrides: { br: 'strong' } },
    })).toBe('beforeafter')
    expect(htmlToMarkdown('before<![CDATA[hidden]]>after', {
      plugins: { tagOverrides: { '#cdata-section': 'br' } },
    })).toBe('before\\\nafter')
    expect(htmlToMarkdown(`${'<![CDATA[hidden]]>'.repeat(600)}<p>after</p>`, {
      plugins: { tagOverrides: { '#cdata-section': 'meta' } },
    })).toBe('after')

    const events = parseHtml('<svg>before</br><strong>after</strong></svg>').events
    const svg = events.find(event => event.type === NodeEventEnter && event.node.type === ELEMENT_NODE && (event.node as ElementNode).name === 'svg')?.node
    const strong = events.find(event => event.type === NodeEventEnter && event.node.type === ELEMENT_NODE && (event.node as ElementNode).name === 'strong')?.node
    expect(strong?.parent).toBe(svg)
  })
})
