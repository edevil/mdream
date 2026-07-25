import type { ParseState } from './parse'
import type { EngineOptions, NodeEvent, TagHandler, TransformPlugin } from './types'
import { createMarkdownProcessor } from './markdown-processor'
import { finalizeParse, parseHtmlStream } from './parse'

function createBlankLineCleaner(enabled: boolean): (chunk: string) => string {
  let newlineRun = 0
  return (chunk) => {
    if (!enabled)
      return chunk
    let output = ''
    for (const character of chunk) {
      if (character === '\n') {
        newlineRun++
        if (newlineRun <= 2)
          output += character
      }
      else {
        newlineRun = 0
        output += character
      }
    }
    return output
  }
}

/**
 * Creates a markdown stream from an HTML stream
 * @param htmlStream - ReadableStream of HTML content (as Uint8Array or string)
 * @param options - Configuration options for conversion
 * @param resolvedPlugins - Pre-resolved plugin instances
 * @param tagOverrideHandlers - Tag override handlers from declarative config
 * @returns An async generator yielding markdown chunks
 */
export async function* streamHtmlToMarkdown(
  htmlStream: ReadableStream<Uint8Array | string> | null,
  options: EngineOptions = {},
  resolvedPlugins: TransformPlugin[] = [],
  tagOverrideHandlers?: Map<string, TagHandler>,
): AsyncIterable<string> {
  if (!htmlStream) {
    throw new Error('Invalid HTML stream provided')
  }
  const decoder = new TextDecoder()
  const reader = htmlStream.getReader()

  const processor = createMarkdownProcessor(options, resolvedPlugins, tagOverrideHandlers)
  const clean = options.clean
  const cleanChunk = createBlankLineCleaner(
    options.format !== 'text' && (clean === true || (typeof clean === 'object' && clean.blankLines === true)),
  )
  const parseState: ParseState = {
    depthMap: processor.state.depthMap,
    depth: 0,
    resolvedPlugins,
    tagOverrideHandlers,
    plainText: processor.state.plainText,
  }
  const handleEvent: (event: NodeEvent) => void = resolvedPlugins.length
    ? processor.processEventWithPlugins
    : processor.processEvent

  let remainingHtml = ''

  try {
    while (true) {
      const { done, value } = await reader.read()

      if (done) {
        break
      }

      // Process the HTML chunk
      const decoded = typeof value === 'string'
        ? decoder.decode() + value
        : decoder.decode(value, { stream: true })
      const htmlContent = `${remainingHtml}${decoded}`

      remainingHtml = parseHtmlStream(htmlContent, parseState, handleEvent)

      const chunk = cleanChunk(processor.getMarkdownChunk())
      if (chunk) {
        yield chunk
      }
    }
    // Process any remaining HTML, then commit trailing text and close any
    // elements left open at end of input.
    const decoderTail = decoder.decode()
    const finalHtml = remainingHtml + decoderTail
    const leftover = finalHtml
      ? parseHtmlStream(finalHtml, parseState, handleEvent)
      : ''
    finalizeParse(leftover, parseState, handleEvent)

    // Emit any final content
    const finalChunk = cleanChunk(processor.getMarkdownChunk(true))
    if (finalChunk) {
      yield finalChunk
    }
  }
  finally {
    reader.releaseLock()
  }
}
