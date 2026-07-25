import { describe, expect, it } from 'vitest'
import { htmlToMarkdown, streamHtmlToMarkdown } from '../../src'

async function stream(html: string, options: Parameters<typeof streamHtmlToMarkdown>[1]): Promise<string> {
  const input = new ReadableStream<string>({
    start(controller) {
      for (let index = 0; index < html.length; index += 3)
        controller.enqueue(html.slice(index, index + 3))
      controller.close()
    },
  })
  let output = ''
  for await (const chunk of streamHtmlToMarkdown(input, options))
    output += chunk
  return output
}

describe('cleanup options', () => {
  it('collapses blank lines only when enabled', async () => {
    const html = '<p>First</p><gap></gap><p>Second</p>'
    const plugins = { tagOverrides: { gap: { enter: '\n\n\n\n', exit: '', spacing: [0, 0] as [number, number], isInline: true } } }
    const uncleaned = htmlToMarkdown(html, { plugins })
    const cleaned = htmlToMarkdown(html, { plugins, clean: { blankLines: true } })
    expect(uncleaned).toContain('\n\n\n')
    expect(cleaned).not.toContain('\n\n\n')
    expect(await stream(html, { plugins, clean: { blankLines: true } })).toBe(cleaned)
    expect(await stream(html, { plugins, clean: { blankLines: false } })).toBe(uncleaned)
  })

  it.each([
    ['<h2><a href="#title">Title</a></h2>', '## Title'],
    ['<h2><a href="#other">Title</a></h2>', '## [Title](#other)'],
    ['<h2><a href="#title">Title</a> suffix</h2>', '## [Title](#title) suffix'],
    ['<h2><a href="#my%2Dheading">My Heading</a></h2>', '## My Heading'],
    ['<h2><a href="#same">Same</a></h2><h2><a href="#same">Same</a></h2>', '## Same\n\n## [Same](#same)'],
    ['<h2><a href="#same">Same</a></h2><h2><a href="#same-1">Same</a></h2>', '## Same\n\n## Same'],
  ])('only removes a heading link to its actual slug', (html, expected) => {
    expect(htmlToMarkdown(html, { clean: { selfLinkHeadings: true } })).toBe(expected)
  })
})
