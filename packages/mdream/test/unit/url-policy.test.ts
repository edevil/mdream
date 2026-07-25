import { describe, expect, it } from 'vitest'
import { engines, htmlToMarkdown, resolveEngine, streamHtmlToMarkdown } from '../utils/engines'

const dangerous = [
  'javascript:alert(1)',
  'JaVaScRiPt:alert(1)',
  '\tdata:text/html,payload',
  '\u007Ffile:///etc/passwd',
  'vbscript:msgbox(1)',
]

describe.each(engines)('url policy parity: $name', (engineConfig) => {
  it.each(dangerous)('rejects %j in strict mode', async (url) => {
    const engine = await resolveEngine(engineConfig.engine)
    expect(htmlToMarkdown(`<a href="${url}">safe text</a>`, { engine, urlPolicy: 'strict' })).toBe('safe text')
    expect(htmlToMarkdown(`<img src="${url}" alt="image">`, { engine, urlPolicy: 'strict' })).toBe('')
  })

  it('preserves dangerous schemes by default', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    expect(htmlToMarkdown('<a href="javascript:alert(1)">text</a>', { engine }))
      .toBe('[text](<javascript:alert(1)>)')
  })

  it('applies strict policy while streaming', async () => {
    const engine = await resolveEngine(engineConfig.engine)
    const input = '<p><a href="javascript:alert(1)">safe text</a> after</p>'
    const stream = new ReadableStream<string>({
      start(controller) {
        for (let index = 0; index < input.length; index += 3)
          controller.enqueue(input.slice(index, index + 3))
        controller.close()
      },
    })
    let output = ''
    for await (const chunk of streamHtmlToMarkdown(stream, { engine, urlPolicy: 'strict' }))
      output += chunk
    expect(output).toBe('safe text after')
  })
})
