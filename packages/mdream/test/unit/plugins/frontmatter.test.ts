import { describe, expect, it } from 'vitest'
import { parseAllDocuments } from 'yaml'
import { engines, htmlToMarkdown, resolveEngine, streamHtmlToMarkdown } from '../../utils/engines'

function parseFrontmatter(markdown: string): Record<string, unknown> {
  expect(markdown.startsWith('---\n')).toBe(true)
  expect(markdown.match(/^---$/gm)).toHaveLength(2)
  const envelopeEnd = markdown.indexOf('\n---\n', 4)
  expect(envelopeEnd).toBeGreaterThan(3)
  const documents = parseAllDocuments(markdown.slice(4, envelopeEnd))
  expect(documents).toHaveLength(1)
  expect(documents[0]!.errors).toEqual([])
  return documents[0]!.toJS() as Record<string, unknown>
}

async function streamed(html: string, chunkSize: number, engine: Parameters<typeof resolveEngine>[0], options: Record<string, unknown>): Promise<string> {
  const input = new ReadableStream<string>({
    start(controller) {
      for (let offset = 0; offset < html.length; offset += chunkSize)
        controller.enqueue(html.slice(offset, offset + chunkSize))
      controller.close()
    },
  })
  let output = ''
  for await (const chunk of streamHtmlToMarkdown(input, { ...options, engine }))
    output += chunk
  return output
}

describe.each(engines)('frontmatter plugin $name', (engineConfig) => {
  it('extracts title and description from head', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `
      <html>
        <head>
          <title>Test Page Title</title>
          <meta name="description" content="This is a test page description">
        </head>
        <body>
          <h1>Main Content</h1>
          <p>This is the main content of the page.</p>
        </body>
      </html>
    `

    const markdown = htmlToMarkdown(html, {
      plugins: { frontmatter: true },
      engine,
    })

    expect(markdown).toContain('---')
    expect(markdown).toContain('title: "Test Page Title"')
    expect(markdown).toContain('meta:')
    expect(markdown).toContain('  "description": "This is a test page description"')
    expect(markdown).toContain('---\n\n')
    expect(markdown).toContain('# Main Content')
    expect(markdown).toContain('This is the main content of the page.')
  })

  it('includes additional frontmatter fields', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `
      <html>
        <head>
          <title>Test Page</title>
        </head>
        <body>
          <p>Content</p>
        </body>
      </html>
    `

    const markdown = htmlToMarkdown(html, {
      plugins: { frontmatter: {
        additionalFields: {
          layout: 'post',
          date: '2025-05-10',
        },
      } },
      engine,
    })

    expect(markdown).toContain('title: "Test Page"')
    expect(markdown).toContain('"layout": "post"')
    expect(markdown).toContain('"date": "2025-05-10"')
  })

  it('correctly formats frontmatter values', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `
      <html>
        <head>
          <title>Title with "quotes"</title>
          <meta name="keywords" content="key1, key2, key3">
          <meta name="author" content="John Doe">
        </head>
        <body>
          <p>Content</p>
        </body>
      </html>
    `

    const markdown = htmlToMarkdown(html, {
      plugins: { frontmatter: true },
      engine,
    })

    expect(markdown).toContain('title: "Title with \\"quotes\\""')
    expect(markdown).toContain('"keywords": "key1, key2, key3"')
    expect(markdown).toContain('"author": "John Doe"')
  })

  it('extracts social media meta tags', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `
      <html>
        <head>
          <title>Page Title</title>
          <meta property="og:title" content="OG Title">
          <meta property="og:description" content="OG Description">
          <meta name="twitter:title" content="Twitter Title">
          <meta name="twitter:description" content="Twitter Description">
        </head>
        <body>
          <p>Content</p>
        </body>
      </html>
    `

    const markdown = htmlToMarkdown(html, {
      plugins: { frontmatter: true },
      engine,
    })

    expect(markdown).toContain('meta:')
    expect(markdown).toContain('"og:title": "OG Title"')
    expect(markdown).toContain('"og:description": "OG Description"')
    expect(markdown).toContain('"twitter:title": "Twitter Title"')
    expect(markdown).toContain('"twitter:description": "Twitter Description"')
  })

  it('receives structured frontmatter via callback', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `
      <html>
        <head>
          <title>My Page</title>
          <meta name="description" content="A test page">
          <meta name="author" content="Jane">
        </head>
        <body><p>Content</p></body>
      </html>
    `

    let frontmatter: Record<string, string> | undefined
    htmlToMarkdown(html, {
      plugins: { frontmatter: (fm) => { frontmatter = fm } },
      engine,
    })

    expect(frontmatter).toBeDefined()
    expect(frontmatter!.title).toBe('My Page')
    expect(frontmatter!.description).toBe('A test page')
    expect(frontmatter!.author).toBe('Jane')
  })

  it('receives structured frontmatter via onExtract with config', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `
      <html>
        <head><title>Page</title></head>
        <body><p>Content</p></body>
      </html>
    `

    let frontmatter: Record<string, string> | undefined
    htmlToMarkdown(html, {
      plugins: {
        frontmatter: {
          additionalFields: { layout: 'post', category: 'blog' },
          onExtract: (fm) => { frontmatter = fm },
        },
      },
      engine,
    })

    expect(frontmatter).toBeDefined()
    expect(frontmatter!.title).toBe('Page')
    expect(frontmatter!.layout).toBe('post')
    expect(frontmatter!.category).toBe('blog')
  })

  it('supports custom meta fields', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const html = `
      <html>
        <head>
          <title>Test Page</title>
          <meta name="custom-field" content="Custom Value">
          <meta name="another-field" content="Another Value">
        </head>
        <body>
          <p>Content</p>
        </body>
      </html>
    `

    const markdown = htmlToMarkdown(html, {
      plugins: { frontmatter: { metaFields: ['custom-field', 'another-field'] } },
      engine,
    })

    expect(markdown).toContain('meta:')
    expect(markdown).toContain('  "another-field": "Another Value"')
    expect(markdown).toContain('  "custom-field": "Custom Value"')
  })

  it('round-trips hostile and implicit-looking values through a YAML parser', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const hostileKey = 'key\n---\ninjected'
    const hostileValue = '---\nowned: true\n...\r\t"\\\0\u0001\u001F\u2028\u2029'
    const implicitValues = {
      bool: 'true',
      nil: 'null',
      number: '123.40',
      date: '2025-05-10',
      map: '{ owned: true }',
      array: '[one, two]',
    }
    const markdown = htmlToMarkdown('<head></head><body><p>Safe body</p></body>', {
      plugins: {
        frontmatter: {
          additionalFields: {
            [hostileKey]: hostileValue,
            ...implicitValues,
          },
        },
      },
      engine,
    })

    const parsed = parseFrontmatter(markdown)
    expect(parsed[hostileKey]).toBe(hostileValue)
    for (const [key, value] of Object.entries(implicitValues)) {
      expect(parsed[key]).toBe(value)
      expect(typeof parsed[key]).toBe('string')
    }
    expect(markdown).toContain('Safe body')
  })

  it('uses the last valid HTML metadata and keeps it when a later duplicate is oversized', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const oversized = 'x'.repeat(64 * 1024 + 1)
    const html = `<head><title>first</title><title>last</title><meta name="author" content="first"><meta name="author" content="last"><meta name="author" content="${oversized}"></head><p>Body</p>`
    const options = { plugins: { frontmatter: true } }
    const expected = htmlToMarkdown(html, { ...options, engine })

    expect(parseFrontmatter(expected)).toMatchObject({
      title: 'last',
      meta: { author: 'last' },
    })
    for (const chunkSize of [4096, html.length])
      expect(await streamed(html, chunkSize, engine, options), `chunk size ${chunkSize}`).toBe(expected)

    const shortHtml = '<head><title>first</title><title>last</title><meta name="author" content="first"><meta name="author" content="last"></head><p>Body</p>'
    const shortExpected = htmlToMarkdown(shortHtml, { ...options, engine })
    for (const chunkSize of [1, 2, 7, shortHtml.length])
      expect(await streamed(shortHtml, chunkSize, engine, options), `duplicate chunk size ${chunkSize}`).toBe(shortExpected)
  })

  it('drops oversized and excess optional metadata without dropping the body', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const additionalFields = Object.fromEntries([
      ['oversized', 'x'.repeat(64 * 1024 + 1)],
      ...Array.from({ length: 80 }, (_, index) => [`field-${index.toString().padStart(2, '0')}`, `value-${index}`]),
    ])
    const html = '<head></head><body><p>Body after metadata</p></body>'
    const options = { plugins: { frontmatter: { additionalFields } } }
    const expected = htmlToMarkdown(html, { ...options, engine })
    const parsed = parseFrontmatter(expected)

    expect(parsed.oversized).toBeUndefined()
    expect(Object.keys(parsed)).toHaveLength(64)
    expect(expected).toContain('Body after metadata')
    expect(await streamed(html, 5, engine, options)).toBe(expected)
  })

  it('enforces the aggregate metadata byte limit', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const additionalFields = Object.fromEntries(
      Array.from({ length: 8 }, (_, index) => [`aggregate-${index}`, 'x'.repeat(40 * 1024)]),
    )
    const markdown = htmlToMarkdown('<head></head><p>Body</p>', {
      plugins: { frontmatter: { additionalFields } },
      engine,
    })

    expect(Object.keys(parseFrontmatter(markdown))).toHaveLength(6)
    expect(markdown).toContain('Body')
  })
})
