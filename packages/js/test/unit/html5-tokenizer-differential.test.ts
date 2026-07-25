import type { ElementNode } from '../../src/types'
import rehypeParse from 'rehype-parse'
import { unified } from 'unified'
import { describe, expect, it } from 'vitest'
import { ELEMENT_NODE, NodeEventEnter } from '../../src/const'
import { htmlToMarkdown } from '../../src/index'
import { parseHtml } from '../../src/parse'

interface Html5Node {
  type?: string
  tagName?: string
  value?: string
  properties?: Record<string, unknown>
  children?: Html5Node[]
}

const html5 = unified().use(rehypeParse, { fragment: true })

function textContent(node: Html5Node): string {
  if (node.type === 'text')
    return node.value || ''
  return node.children?.map(textContent).join('') || ''
}

function findElement(node: Html5Node, tagName: string): Html5Node | undefined {
  if (node.tagName === tagName)
    return node
  for (const child of node.children || []) {
    const match = findElement(child, tagName)
    if (match)
      return match
  }
}

describe('html5 tokenizer differential fixtures', () => {
  it('matches visible text from invalid starts and malformed comments', () => {
    for (const source of [
      'I <3 Rust',
      'I < 3 Rust',
      'I <> Rust',
      'before<!-->after',
      'before<!--->after',
      'before<!--x--!>after',
      'before<!--x--->after',
      'before<!--x',
      'before<!foo>after',
      'before<!foo',
      'before<?pi?>after',
      'before<?pi',
    ]) {
      const tree = html5.parse(source) as Html5Node
      expect(htmlToMarkdown(source), source).toBe(textContent(tree))
    }
  })

  it('matches first-wins attributes from an HTML5 parser', () => {
    const source = '<a href=/first HREF=/second class=one CLASS=two id=first ID=second lang=js LANG=python>x</a><img src=/first SRC=/second>'
    const html5Tree = html5.parse(source) as Html5Node
    const html5Anchor = findElement(html5Tree, 'a')!
    const html5Image = findElement(html5Tree, 'img')!
    const events = parseHtml(source).events
    const anchor = events.find(event => event.type === NodeEventEnter
      && event.node.type === ELEMENT_NODE
      && (event.node as ElementNode).name === 'a')!.node as ElementNode
    const image = events.find(event => event.type === NodeEventEnter
      && event.node.type === ELEMENT_NODE
      && (event.node as ElementNode).name === 'img')!.node as ElementNode

    expect(anchor.attributes.href).toBe(html5Anchor.properties!.href)
    expect([anchor.attributes.class]).toEqual(html5Anchor.properties!.className)
    expect(anchor.attributes.id).toBe(html5Anchor.properties!.id)
    expect(anchor.attributes.lang).toBe(html5Anchor.properties!.lang)
    expect(image.attributes.src).toBe(html5Image.properties!.src)
  })
})
