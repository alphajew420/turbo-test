import { describe, it, expect } from 'vitest';

describe('native DOM interface ownership', () => {
  it('keeps distinct interface inheritance and branding', () => {
    expect(Object.getPrototypeOf(Node.prototype)).toBe(EventTarget.prototype);
    expect(Object.getPrototypeOf(Element.prototype)).toBe(Node.prototype);
    expect(Object.getPrototypeOf(HTMLElement.prototype)).toBe(Element.prototype);
    expect(Object.getPrototypeOf(HTMLDivElement.prototype)).toBe(HTMLElement.prototype);
    expect(Object.getPrototypeOf(Document.prototype)).toBe(Node.prototype);
    expect(Object.getPrototypeOf(HTMLDocument.prototype)).toBe(Document.prototype);
    expect(Object.getPrototypeOf(Text.prototype)).toBe(CharacterData.prototype);
    expect(Object.getPrototypeOf(CharacterData.prototype)).toBe(Node.prototype);
    const element=document.createElement('div');
    expect(Object.getPrototypeOf(element)).toBe(HTMLDivElement.prototype);
    expect(Object.prototype.toString.call(element)).toBe('[object HTMLDivElement]');
    expect(Object.prototype.toString.call(document)).toBe('[object HTMLDocument]');
    expect(Object.prototype.toString.call(document.createTextNode('text'))).toBe('[object Text]');
  });

  it('exposes receiver-safe native accessors on their owning interfaces', () => {
    const element=document.createElement('div');
    expect(Object.hasOwn(element,'id')).toBe(false);
    expect(Object.hasOwn(element,'nodeType')).toBe(false);
    const id=Object.getOwnPropertyDescriptor(Element.prototype,'id');
    const type=Object.getOwnPropertyDescriptor(Node.prototype,'nodeType');
    expect(id.enumerable).toBe(true);
    expect(id.configurable).toBe(true);
    expect(id.get.name).toBe('get id');
    expect(id.get.length).toBe(0);
    expect(id.set.name).toBe('set id');
    expect(id.set.length).toBe(1);
    expect(Object.hasOwn(id.get,'prototype')).toBe(false);
    expect(()=>id.get.call({})).toThrow();
    id.set.call(element,'native-id');
    expect(element.getAttribute('id')).toBe('native-id');
    expect(id.get.call(element)).toBe('native-id');
    expect(type.get.call(element)).toBe(1);
    element.innerHTML='<span>live</span>';
    expect(element.firstChild.parentNode).toBe(element);
    expect(element.textContent).toBe('live');
  });

  it('shares native prototype methods while preserving tree and expando identity', () => {
    const first=document.createElement('div'),second=document.createElement('div');
    expect(Object.hasOwn(first,'appendChild')).toBe(false);
    expect(Object.hasOwn(first,'getAttribute')).toBe(false);
    expect(first.appendChild).toBe(Node.prototype.appendChild);
    expect(first.getAttribute).toBe(Element.prototype.getAttribute);
    first.__marker=42;
    const text=document.createTextNode('live');
    first.appendChild(text);
    second.appendChild(first);
    expect(second.firstChild).toBe(first);
    expect(second.firstChild.__marker).toBe(42);
    expect(first.firstChild).toBe(text);
    expect(text.parentNode).toBe(first);
    expect(first.cloneNode(true).textContent).toBe('live');
  });
});
