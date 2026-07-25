import { describe, expect, it } from 'vitest'
import { TAG_BLOCKQUOTE, TEXT_NODE } from '../../src/const'
import { htmlToMarkdown, streamHtmlToMarkdown } from '../../src/index'
import { parseHtml } from '../../src/parse'

const LIMIT = 512

function nested(tag: string, depth: number, content: string): string {
  return `${`<${tag}>`.repeat(depth)}${content}${`</${tag}>`.repeat(depth)}`
}

async function streamConvert(chunks: string[]): Promise<string> {
  const stream = new ReadableStream<string>({
    start(controller) {
      for (const chunk of chunks)
        controller.enqueue(chunk)
      controller.close()
    },
  })
  let output = ''
  for await (const chunk of streamHtmlToMarkdown(stream))
    output += chunk
  return output
}

describe('element depth limit', () => {
  it.each([255, 256, 511, 512, 513])('preserves the subtree and later siblings at depth %i', (depth) => {
    expect(htmlToMarkdown(`${nested('div', depth, 'inside')}<p>after</p>`)).toBe('inside\n\nafter')
  })

  it.each([255, 256, 511, 512])('does not wrap repeated-tag counters at depth %i', (depth) => {
    const html = nested('blockquote', depth, 'sentinel')
    const output = htmlToMarkdown(html)
    expect(output.endsWith('sentinel')).toBe(true)
    expect(output.match(/> /g)).toHaveLength(depth)

    const textEvent = parseHtml(html).events.find(event => event.node.type === TEXT_NODE)
    expect(textEvent?.node.parent?.depthMap[TAG_BLOCKQUOTE]).toBe(depth)
  })

  it('handles 100,000 repeated starts without growing the real node chain', () => {
    const html = `<p>before</p>${'<div>'.repeat(100_000)}inside`
    expect(htmlToMarkdown(html)).toBe('before\n\ninside')
    expect(parseHtml(html).events).toHaveLength(LIMIT + 3)
  })

  it('does not enter overflow for self-closing elements at the limit', () => {
    const html = `${'<div>'.repeat(LIMIT)}<br>kept${'</div>'.repeat(LIMIT)}<p>after</p>`
    const output = htmlToMarkdown(html)
    expect(output).toContain('kept')
    expect(output).toContain('after')
  })

  it('applies implied-end recovery before checking the limit', () => {
    expect(htmlToMarkdown('<p>item'.repeat(1_000)).match(/item/g)).toHaveLength(1_000)
  })

  it('recovers streamed output after 100,000 repeated root closes', async () => {
    const chunks = ['<p>before</p>', '<div>'.repeat(100_000), 'inside', '</div>'.repeat(100_000), '<p>after</p>']
    expect(await streamConvert(chunks)).toBe('before\n\ninside\n\nafter')
  }, 15_000)

  it('suppresses inert and raw text in overflow, then resumes', () => {
    const html = `${'<div>'.repeat(LIMIT)}<section>before<script>hidden-script</script><template><style>hidden-style</style><b>hidden-template</b></template>visible</section>${'</div>'.repeat(LIMIT)}<p>after</p>`
    const output = htmlToMarkdown(html)
    expect(output).toContain('before')
    expect(output).toContain('visible')
    expect(output).toContain('after')
    expect(output).not.toContain('hidden-script')
    expect(output).not.toContain('hidden-style')
    expect(output).not.toContain('hidden-template')
  })

  it('does not let raw text close an outer inert overflow frame', () => {
    const html = `${'<div>'.repeat(LIMIT)}<template><script>"</template>"; hidden</script>still-hidden</template>visible</div><p>after</p>`
    const output = htmlToMarkdown(html)
    expect(output).not.toContain('hidden')
    expect(output).toContain('visible')
    expect(output).toContain('after')
  })

  it('ignores malformed closes without unbalancing capped output', () => {
    const html = `${'<div>'.repeat(LIMIT)}<section>inside</bogus><span>still</section><p>after</p>`
    const output = htmlToMarkdown(html)
    expect(output).toContain('inside')
    expect(output).toContain('still')
    expect(output).toContain('after')
  })

  it('does not leave overflow active after a skipped CDATA override', () => {
    const html = `${'<div>'.repeat(LIMIT)}<![CDATA[hidden]]><p>kept</p>`
    const output = htmlToMarkdown(html, {
      plugins: {
        tagOverrides: {
          '#cdata-section': { enter: '[', exit: ']', isInline: true, spacing: [0, 0] },
        },
      },
    })
    expect(output).not.toContain('hidden')
    expect(output).toContain('kept')
  })

  it('preserves text from deep pre, blockquote, list, and table contexts', () => {
    for (const [context, sentinel] of [
      ['<pre>*pre*</pre>', 'pre'],
      ['<blockquote>quote</blockquote>', 'quote'],
      ['<ul><li>item</li></ul>', 'item'],
      ['<table><tr><td>cell</td></tr></table>', 'cell'],
    ]) {
      const html = `${'<div>'.repeat(LIMIT)}<section>${context}</section>${'</div>'.repeat(LIMIT)}<p>after</p>`
      const output = htmlToMarkdown(html)
      expect(output).toContain(sentinel)
      expect(output).toContain('after')
    }
  })

  it('matches one-shot output at every split during overflow recovery', async () => {
    const html = `${'<i>'.repeat(LIMIT)}<section>before<script>hidden-script</script><style>hidden-style</style><template>inert</template>inside</section><p>after</p>`
    const expected = htmlToMarkdown(html)
    for (let split = 0; split <= html.length; split++)
      expect(await streamConvert([html.slice(0, split), html.slice(split)]), `split ${split}`).toBe(expected)
  })
})
