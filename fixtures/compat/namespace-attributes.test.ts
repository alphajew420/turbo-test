import { describe, expect, it } from 'vitest';

describe('namespace-aware native elements and attributes', () => {
  it('keeps namespace URI, prefix, local name and qualified name through cloning', () => {
    const node = document.createElementNS('urn:example', 'p:Mixed');
    node.setAttributeNS('urn:first', 'one:value', 'first');
    node.setAttributeNS('urn:second', 'two:value', 'second');
    node.setAttribute('value', 'plain');
    const copy = node.cloneNode(true) as Element;
    expect([copy.namespaceURI, copy.prefix, copy.localName, copy.tagName]).toEqual([
      'urn:example', 'p', 'Mixed', 'p:Mixed',
    ]);
    expect([copy.getAttributeNS('urn:first', 'value'), copy.getAttributeNS('urn:second', 'value'),
      copy.getAttributeNS(null, 'value'), copy.getAttributeNS('urn:absent', 'value')]).toEqual([
      'first', 'second', 'plain', null,
    ]);
    expect(copy instanceof Element).toBe(true);
    expect(copy instanceof HTMLElement).toBe(false);
  });

  it('uses live native Attr identity and rejects an attribute already owned by another element', () => {
    const owner = document.createElement('div');
    const other = document.createElement('div');
    const attribute = document.createAttributeNS('urn:attribute', 'a:value');
    attribute.value = 'first';
    expect(owner.setAttributeNodeNS(attribute)).toBe(null);
    expect(owner.getAttributeNodeNS('urn:attribute', 'value')).toBe(attribute);
    expect(owner.attributes[0]).toBe(attribute);
    expect(attribute instanceof Attr).toBe(true);
    expect(attribute instanceof Node).toBe(true);
    expect(attribute.nodeType).toBe(Node.ATTRIBUTE_NODE);
    attribute.value = 'live';
    expect(owner.getAttributeNS('urn:attribute', 'value')).toBe('live');
    let error = '';
    try { other.setAttributeNode(attribute); } catch (caught) { error = (caught as DOMException).name; }
    expect(error).toBe('InUseAttributeError');
    expect(owner.removeAttributeNode(attribute)).toBe(attribute);
    expect([attribute.ownerElement, attribute.value]).toEqual([null, 'live']);
    other.setAttributeNode(attribute);
    expect(attribute.ownerElement).toBe(other);
  });

  it('keeps NamedNodeMap live, branded, iterable and backed by the same Attr nodes', () => {
    const owner = document.createElement('div');
    const map = owner.attributes;
    expect(map).toBe(owner.attributes);
    expect(Array.isArray(map)).toBe(false);
    expect(map instanceof NamedNodeMap).toBe(true);
    owner.setAttribute('first', 'one');
    expect(map.length).toBe(1);
    expect(map.item(0)).toBe(owner.getAttributeNode('first'));
    expect(map.getNamedItem('first')).toBe(map[0]);
    const replacement = document.createAttribute('first');
    replacement.value = 'replaced';
    const old = map.setNamedItem(replacement)!;
    expect([old.ownerElement, old.value]).toEqual([null, 'one']);
    expect(Array.from(map)).toEqual([replacement]);
    expect(map.removeNamedItem('first')).toBe(replacement);
    expect(map.length).toBe(0);
    let rejected = false;
    try { Object.getOwnPropertyDescriptor(NamedNodeMap.prototype, 'length')!.get!.call({}); }
    catch (error) { rejected = error instanceof TypeError; }
    expect(rejected).toBe(true);
  });

  it('validates namespace combinations and resolves namespace declarations in the native tree', () => {
    const node = document.createElementNS('urn:element', 'p:parent');
    const child = document.createElementNS(null, 'child');
    node.appendChild(child);
    node.setAttributeNS('http://www.w3.org/2000/xmlns/', 'xmlns:q', 'urn:lookup');
    expect([child.lookupNamespaceURI('q'), child.lookupPrefix('urn:lookup'),
      node.lookupNamespaceURI('p'), child.lookupNamespaceURI('xml')]).toEqual([
      'urn:lookup', 'q', 'urn:element', 'http://www.w3.org/XML/1998/namespace',
    ]);
    const failures: string[] = [];
    for (const action of [() => node.setAttributeNS(null, 'p:value', 'bad'),
      () => document.createElementNS('urn:wrong', 'xml:item'),
      () => node.setAttributeNS('urn:wrong', 'xmlns:q', 'bad')]) {
      try { action(); } catch (error) { failures.push((error as DOMException).name); }
    }
    expect(failures).toEqual(['NamespaceError', 'NamespaceError', 'NamespaceError']);
  });
});
