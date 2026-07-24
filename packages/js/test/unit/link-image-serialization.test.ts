import { describe, expect, it } from 'vitest'
import { createPlugin, htmlToMarkdown } from '../../src/index'

describe('gfm link and image serialization', () => {
  it.each([
    ['<a href="">text</a>', '[text]()'],
    ['<a href="docs/a b">text</a>', '[text](<docs/a b>)'],
    [String.raw`<a href="docs/(a)\file">text</a>`, String.raw`[text](<docs/(a)\\file>)`],
    [String.raw`<a href="/x" title="say &quot;hi&quot; \ path">text</a>`, String.raw`[text](/x "say \"hi\" \\ path")`],
  ])('serializes a reparsable link for %s', (html, expected) => {
    expect(htmlToMarkdown(html)).toBe(expected)
  })

  it.each([
    [
      String.raw`<img src="/x.png" alt="a ] \ *bold* _em_ &#96;code&#96;">`,
      String.raw`![a \] \\ \*bold\* \_em\_ \`code\`](/x.png)`,
    ],
    [
      String.raw`<img src="/x.png" alt="alt" title="say &quot;hi&quot; \ path">`,
      String.raw`![alt](/x.png "say \"hi\" \\ path")`,
    ],
  ])('serializes a reparsable image for %s', (html, expected) => {
    expect(htmlToMarkdown(html)).toBe(expected)
  })

  it('serializes decoded controls and entity-shaped literals exactly once', () => {
    expect(htmlToMarkdown('<a href="https://example.test/a&#10;b&#127;?x=1&amp;y=2" title="line&#10;two &amp;copy;">link</a>'))
      .toBe(String.raw`[link](https://example.test/a%0Ab%7F?x=1&y=2 "line&#10;two \&copy;")`)
    expect(htmlToMarkdown('<img src="/i&#9;m" alt="line&#10;two &amp;copy;" title="t&#13;u &amp;reg;">'))
      .toBe(String.raw`![line&#10;two \&copy;](/i%09m "t&#13;u \&reg;")`)
  })

  it('escapes resource pipes inside GFM table cells', () => {
    const html = '<table><tr><td><a href="/a|b" title="t|u">link</a></td><td><img src="/i|m" alt="a|b" title="x|y"></td></tr></table>'
    expect(htmlToMarkdown(html)).toBe(String.raw`| [link](/a\|b "t\|u") | ![a\|b](/i\|m "x\|y") |
| --- | --- |`)
  })

  it('serializes values after plugin attribute mutation', () => {
    const mutateResources = createPlugin({
      processAttributes(node) {
        if (node.name === 'a') {
          node.attributes!.href = 'https://example.test/a\nb?x=1&copy;&y=2'
          node.attributes!.title = 'line\ntwo &reg;'
        }
        else if (node.name === 'img') {
          node.attributes!.src = '/i\u007Fm'
          node.attributes!.alt = 'a\nb &copy;'
          node.attributes!.title = 't\ru'
        }
      },
    })
    expect(htmlToMarkdown('<a href="/old">link</a><img src="/old">', { hooks: [mutateResources] }))
      .toBe(String.raw`[link](https://example.test/a%0Ab?x=1\&copy;&y=2 "line&#10;two \&reg;")![a&#10;b \&copy;](/i%7Fm "t&#13;u")`)
  })

  it('does not use autolink shorthand for destinations requiring serialization', () => {
    const unsafe = 'https://example.test/a\u007Fb'
    const mutateHref = createPlugin({
      processAttributes(node) {
        if (node.name === 'a')
          node.attributes!.href = unsafe
      },
      onNodeEnter(node) {
        if (node.name === 'span')
          return unsafe
      },
    })
    expect(htmlToMarkdown('<a href="/old"><span></span></a>', { hooks: [mutateHref] }))
      .toBe(`[${unsafe}](https://example.test/a%7Fb)`)
  })

  it('replaces controls that CommonMark cannot represent in resource text', () => {
    const controls = Array.from({ length: 32 }, (_, code) => code)
      .filter(code => code !== 9 && code !== 10 && code !== 12 && code !== 13)
      .concat(0x7F)
      .map(code => String.fromCharCode(code))
      .join('')
    const replacements = '\uFFFD'.repeat(29)

    expect(htmlToMarkdown(`<img src="/x" alt="a${controls}b" title="t${controls}u">`))
      .toBe(`![a${replacements}b](/x "t${replacements}u")`)
  })
})
