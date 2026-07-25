import type { ElementNode, TextNode } from '../types'
import { ELEMENT_NODE, TAG_HEAD, TAG_META, TAG_TITLE } from '../const'
import { createPlugin } from '../pluggable/plugin'

const MAX_FRONTMATTER_FIELDS = 64
const MAX_FRONTMATTER_VALUE_BYTES = 64 * 1024
const MAX_FRONTMATTER_BYTES = 256 * 1024
const UTF8_ENCODER = new TextEncoder()

export interface FrontmatterPluginOptions {
  /** Additional frontmatter fields to include */
  additionalFields?: Record<string, string>
  /** Meta tag names to extract (beyond the standard ones) */
  metaFields?: string[]
}

interface FrontmatterData {
  title?: string
  meta: Record<string, string>
}

function utf8Length(value: string): number {
  return UTF8_ENCODER.encode(value).byteLength
}

function additionalCandidates(fields: Record<string, string> | undefined): Array<[string, string]> {
  const candidates: Array<[string, string]> = []
  if (!fields)
    return candidates

  for (const key in fields) {
    if (!Object.hasOwn(fields, key))
      continue
    const value = fields[key]!
    if (key === 'title' || key === 'description' || key === 'meta'
      || utf8Length(key) > MAX_FRONTMATTER_VALUE_BYTES
      || utf8Length(value) > MAX_FRONTMATTER_VALUE_BYTES) {
      continue
    }
    const position = candidates.findIndex(([candidate]) => candidate >= key)
    if (position === -1) {
      if (candidates.length < MAX_FRONTMATTER_FIELDS)
        candidates.push([key, value])
    }
    else if (position < MAX_FRONTMATTER_FIELDS) {
      candidates.splice(position, 0, [key, value])
      if (candidates.length > MAX_FRONTMATTER_FIELDS)
        candidates.pop()
    }
  }
  return candidates
}

function yamlDoubleQuoted(value: string): string {
  let output = '"'
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index)
    if (code === 0x22)
      output += '\\"'
    else if (code === 0x5C)
      output += '\\\\'
    else if (code <= 0x1F || code === 0x2028 || code === 0x2029 || (code >= 0xD800 && code <= 0xDFFF))
      output += `\\u${code.toString(16).toUpperCase().padStart(4, '0')}`
    else
      output += value[index]
  }
  return `${output}"`
}

/**
 * A plugin that manages frontmatter generation from HTML head elements
 * Extracts metadata from meta tags and title and generates YAML frontmatter
 */
export function frontmatterPlugin(options: FrontmatterPluginOptions = {}) {
  const additionalFields = additionalCandidates(options.additionalFields)
  const metaFields = new Set([
    'description',
    'keywords',
    'author',
    'date',
    'og:title',
    'og:description',
    'twitter:title',
    'twitter:description',
    ...(options.metaFields || []),
  ])

  // Metadata collection
  const frontmatter: FrontmatterData = { meta: Object.create(null) }
  let retainedBytes = 0
  let fieldCount = 0
  let metadataCount = 0
  let inHead = false

  function setMetadata(target: Record<string, string>, key: string, value: string): void {
    const keyBytes = utf8Length(key)
    const valueBytes = utf8Length(value)
    if (keyBytes > MAX_FRONTMATTER_VALUE_BYTES || valueBytes > MAX_FRONTMATTER_VALUE_BYTES)
      return

    const previous = target[key]
    const previousBytes = previous === undefined ? 0 : keyBytes + utf8Length(previous)
    const nextBytes = retainedBytes - previousBytes + keyBytes + valueBytes
    if ((previous === undefined && metadataCount >= MAX_FRONTMATTER_FIELDS - 1) || nextBytes > MAX_FRONTMATTER_BYTES)
      return

    if (previous === undefined) {
      fieldCount++
      metadataCount++
    }
    retainedBytes = nextBytes
    target[key] = value
  }

  function setTitle(value: string): void {
    const valueBytes = utf8Length(value)
    if (!value || valueBytes > MAX_FRONTMATTER_VALUE_BYTES)
      return
    const previousBytes = frontmatter.title === undefined ? 0 : 'title'.length + utf8Length(frontmatter.title)
    const nextBytes = retainedBytes - previousBytes + 'title'.length + valueBytes
    if ((frontmatter.title === undefined && fieldCount >= MAX_FRONTMATTER_FIELDS) || nextBytes > MAX_FRONTMATTER_BYTES)
      return
    if (frontmatter.title === undefined)
      fieldCount++
    retainedBytes = nextBytes
    frontmatter.title = value
  }

  function getStructuredData(): Record<string, string> | undefined {
    const result: Record<string, string> = {}
    for (const [key, value] of selectedAdditionalFields(retainedBytes, fieldCount))
      result[key] = value
    if (frontmatter.title)
      result.title = frontmatter.title
    for (const [k, v] of Object.entries(frontmatter.meta)) {
      result[k] = v
    }
    return Object.keys(result).length > 0 ? result : undefined
  }

  const plugin = createPlugin({
    onNodeEnter(node: any): string | undefined {
      if (node.excludedFromMarkdown)
        return

      // Track when we enter the head section
      if (node.tagId === TAG_HEAD) {
        inHead = true
        return
      }

      // Process title tag inside head
      if (inHead && node.type === ELEMENT_NODE && node.tagId === TAG_TITLE) {
        // Title will be processed in processTextNode
        return
      }

      // Process meta tags inside head
      if (inHead && node.type === ELEMENT_NODE && node.tagId === TAG_META) {
        const elementNode = node as ElementNode
        const { name, property, content } = elementNode.attributes || {}

        // Check for valid meta tags
        const metaName = property || name
        if (metaName && content && metaFields.has(metaName)) {
          setMetadata(frontmatter.meta, metaName, content)
        }

        // Don't output anything for meta tags
        return undefined
      }
    },

    onNodeExit(node: any, state: any) {
      if (node.excludedFromMarkdown)
        return undefined

      // Handle exiting the head tag
      if (node.type === ELEMENT_NODE && node.tagId === TAG_HEAD) {
        inHead = false
        if (state.options?.format === 'text')
          return undefined

        // Generate frontmatter as we exit the head
        if (fieldCount > 0 || additionalFields.length > 0) {
          const frontmatterContent = generateFrontmatter()
          if (frontmatterContent) {
            state.buffer.push(frontmatterContent)
            state.lastContentCache = frontmatterContent
          }
        }
      }

      return undefined
    },

    processTextNode(node: TextNode) {
      if (node.parent?.excludedFromMarkdown)
        return

      // Only process if we're in the head section
      if (!inHead) {
        return
      }

      // Handle text inside title tag
      const parent = node.parent
      if (parent && parent.tagId === TAG_TITLE && node.value) {
        setTitle(node.value.trim())
        return { content: '', skip: true }
      }
    },
  } as any)

  // Attach getter to the plugin for structured data access
  ;(plugin as any).getFrontmatter = getStructuredData

  return plugin

  /**
   * Generate YAML frontmatter string from collected metadata
   */
  function generateFrontmatter(): string {
    if (fieldCount === 0 && additionalFields.length === 0) {
      return ''
    }

    const yamlLines: string[] = []
    if (frontmatter.title)
      yamlLines.push(`title: ${yamlDoubleQuoted(frontmatter.title)}`)

    for (const [key, value] of selectedAdditionalFields(retainedBytes, fieldCount))
      yamlLines.push(`${yamlDoubleQuoted(key)}: ${yamlDoubleQuoted(value)}`)

    const metaEntries = Object.entries(frontmatter.meta).sort(([a], [b]) => a.localeCompare(b))
    if (metaEntries.length > 0) {
      yamlLines.push('meta:')
      for (const [key, value] of metaEntries)
        yamlLines.push(`  ${yamlDoubleQuoted(key)}: ${yamlDoubleQuoted(value)}`)
    }

    if (yamlLines.length === 0)
      return ''

    return `---\n${yamlLines.join('\n')}\n---\n\n`
  }

  function selectedAdditionalFields(initialBytes: number, initialCount: number): Array<[string, string]> {
    const selected: Array<[string, string]> = []
    const capturedKeys = new Set(Object.keys(frontmatter.meta))
    let bytes = initialBytes
    let count = initialCount
    for (const [key, value] of additionalFields) {
      const keyBytes = utf8Length(key)
      const valueBytes = utf8Length(value)
      if (capturedKeys.has(key)
        || count >= MAX_FRONTMATTER_FIELDS
        || bytes + keyBytes + valueBytes > MAX_FRONTMATTER_BYTES) {
        continue
      }
      selected.push([key, value])
      bytes += keyBytes + valueBytes
      count++
    }
    return selected
  }
}
