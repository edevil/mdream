import { describe, expect, it } from 'vitest'
import { engines, resolveEngine } from '../../utils/engines'
import { parseMarkdown } from '../../utils/markdown'

describe.each(engines)('resource serialization AST $name', (engineConfig) => {
  it('preserves decoded resource values through a GFM parse', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const markdown = engine.htmlToMarkdown(
      '<a href="https://example.test/a&#10;b?x=&amp;copy;&amp;y=2" title="line&#10;two &amp;reg;">link</a><img src="/i&#127;m" alt="a&#10;b &amp;copy;" title="t&#13;u">',
    )

    expect(parseMarkdown(markdown)).toMatchObject({
      type: 'root',
      children: [{
        type: 'paragraph',
        children: [
          {
            type: 'link',
            url: 'https://example.test/a%0Ab?x=&copy;&y=2',
            title: 'line\ntwo &reg;',
            children: [{ type: 'text', value: 'link' }],
          },
          {
            type: 'image',
            url: '/i%7Fm',
            title: 't\ru',
            alt: 'a\nb &copy;',
          },
        ],
      }],
    })
  })

  it('replaces controls that GFM resource text cannot represent', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const controls = Array.from({ length: 32 }, (_, code) => code)
      .filter(code => code !== 9 && code !== 10 && code !== 12 && code !== 13)
      .concat(0x7F)
      .map(code => String.fromCharCode(code))
      .join('')
    const replacements = '\uFFFD'.repeat(29)
    const markdown = engine.htmlToMarkdown(
      `<a href="/x" title="a${controls}b">link</a><img src="/i" alt="c${controls}d" title="e${controls}f">`,
    )

    expect(parseMarkdown(markdown)).toMatchObject({
      children: [{
        children: [
          { type: 'link', title: `a${replacements}b` },
          { type: 'image', alt: `c${replacements}d`, title: `e${replacements}f` },
        ],
      }],
    })
  })

  it('keeps resource pipes inside their GFM table cells', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const markdown = engine.htmlToMarkdown(
      '<table><tr><td><a href="/a|b" title="t|u">link</a></td><td><img src="/i|m" alt="a|b" title="x|y"></td></tr></table>',
    )

    expect(parseMarkdown(markdown)).toMatchObject({
      type: 'root',
      children: [{
        type: 'table',
        children: [{
          type: 'tableRow',
          children: [
            { type: 'tableCell', children: [{ type: 'link', url: '/a|b', title: 't|u' }] },
            { type: 'tableCell', children: [{ type: 'image', url: '/i|m', title: 'x|y', alt: 'a|b' }] },
          ],
        }],
      }],
    })
  })
})
