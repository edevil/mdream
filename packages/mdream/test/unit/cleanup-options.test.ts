import { describe, expect, it } from 'vitest'
import { engines, htmlToMarkdown, resolveEngine, streamHtmlToMarkdown } from '../utils/engines'

describe.each(engines)('cleanup option parity: $name', (engineConfig) => {
  it.each([
    ['<h2><a href="#title">Title</a></h2>', '## Title'],
    ['<h2><a href="#other">Title</a></h2>', '## [Title](#other)'],
    ['<h2><a href="#title">Title</a> suffix</h2>', '## [Title](#title) suffix'],
    ['<h2><a href="#my%2Dheading">My Heading</a></h2>', '## My Heading'],
    ['<h2><a href="#same">Same</a></h2><h2><a href="#same">Same</a></h2>', '## Same\n\n## [Same](#same)'],
  ])('only removes a heading link to its actual slug', async (html, expected) => {
    const engine = await resolveEngine(engineConfig.engine)
    expect(htmlToMarkdown(html, { engine, clean: { selfLinkHeadings: true } })).toBe(expected)
  })

  it('collapses blank lines consistently while streaming', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = '<p>First</p><gap></gap><p>Second</p>'
    const plugins = { tagOverrides: { gap: { enter: '\n\n\n\n', exit: '', spacing: [0, 0] as [number, number], isInline: true } } }
    const expected = htmlToMarkdown(html, { engine, plugins, clean: { blankLines: true } })
    const input = new ReadableStream<string>({
      start(controller) {
        for (let index = 0; index < html.length; index += 3)
          controller.enqueue(html.slice(index, index + 3))
        controller.close()
      },
    })
    let output = ''
    for await (const chunk of streamHtmlToMarkdown(input, { engine, plugins, clean: { blankLines: true } }))
      output += chunk
    expect(output).toBe(expected)
  })
})
