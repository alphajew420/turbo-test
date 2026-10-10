import { describe, it, expect } from 'vitest';

describe('native DOM interface ownership', () => {
  it('keeps native shadow-root identity, ownership and scoped queries', () => {
    const host=document.createElement('div');document.body.appendChild(host);
    const root=host.attachShadow({mode:'open'});
    expect(root.nodeType).toBe(11);
    expect(Object.getPrototypeOf(root)).toBe(ShadowRoot.prototype);
    expect(Object.getPrototypeOf(ShadowRoot.prototype)).toBe(DocumentFragment.prototype);
    expect(root.host).toBe(host);
    expect(root.mode).toBe('open');
    expect(root.ownerDocument).toBe(document);
    expect(host.shadowRoot).toBe(root);
    expect(root.parentNode).toBe(null);
    root.innerHTML='<span id="native-shadow">text</span>';
    const span=root.querySelector('#native-shadow');
    expect(span.textContent).toBe('text');
    expect(document.querySelector('#native-shadow')).toBe(null);
    expect(span.getRootNode()).toBe(root);
    expect(span.getRootNode({composed:true})).toBe(document);
    expect(()=>root.cloneNode(true)).toThrow();
    expect(()=>host.attachShadow({mode:'open'})).toThrow();
    expect(()=>document.body.appendChild(root)).toThrow();
    const closedHost=document.createElement('div'),closed=closedHost.attachShadow({mode:'closed'});
    expect(closed.mode).toBe('closed');expect(closed.host).toBe(closedHost);expect(closedHost.shadowRoot).toBe(null);
    host.remove();
  });

  it('owns node constants and event methods on interfaces with real listener state', () => {
    const element=document.createElement('div');
    expect(Object.hasOwn(element,'ELEMENT_NODE')).toBe(false);
    expect(element.ELEMENT_NODE).toBe(Node.ELEMENT_NODE);
    expect(Node.ELEMENT_NODE).toBe(1);
    expect(Node.DOCUMENT_FRAGMENT_NODE).toBe(11);
    const constant=Object.getOwnPropertyDescriptor(Node.prototype,'TEXT_NODE');
    expect(constant.value).toBe(3);expect(constant.writable).toBe(false);expect(constant.configurable).toBe(false);expect(constant.enumerable).toBe(true);
    expect(element.__cascadeProp).toBe(undefined);
    expect(element.activeElement).toBe(undefined);
    expect(element.checkValidity).toBe(undefined);
    expect(Object.hasOwn(element,'addEventListener')).toBe(false);
    expect(element.addEventListener).toBe(EventTarget.prototype.addEventListener);
    let dom=0,custom=0,windowEvents=0;
    element.addEventListener('event',()=>dom++);
    element.dispatchEvent(new Event('event'));
    class Bus extends EventTarget {}
    const bus=new Bus();bus.addEventListener('event',()=>custom++);bus.dispatchEvent(new Event('event'));
    const listener=()=>windowEvents++;
    EventTarget.prototype.addEventListener.call(window,'event',listener);
    EventTarget.prototype.dispatchEvent.call(window,new Event('event'));
    EventTarget.prototype.removeEventListener.call(window,'event',listener);
    expect([dom,custom,windowEvents]).toEqual([1,1,1]);
    expect(()=>EventTarget.prototype.addEventListener.call({},'event',listener)).toThrow();
    expect(()=>EventTarget()).toThrow();
    const input=document.createElement('input');
    expect(input.checkValidity()).toBe(true);
    expect(Object.hasOwn(input,'checkValidity')).toBe(false);
  });

  it('uses Web IDL constructor inheritance and property flags', () => {
    for(const [child,parent] of [[Node,EventTarget],[Element,Node],[HTMLElement,Element],[HTMLDivElement,HTMLElement],[Document,Node],[HTMLDocument,Document],[DocumentFragment,Node],[Text,CharacterData],[HTMLAudioElement,HTMLMediaElement]]){
      expect(Object.getPrototypeOf(child)).toBe(parent);
      expect(Object.getOwnPropertyDescriptor(child,'prototype').writable).toBe(false);
      expect(Object.getOwnPropertyDescriptor(globalThis,child.name).enumerable).toBe(false);
      expect(()=>child()).toThrow();
      expect(()=>new child()).toThrow();
    }
  });

  it('uses real fragments that flatten children when inserted', () => {
    const fragment=document.createDocumentFragment();
    expect(fragment.nodeType).toBe(11);
    expect(fragment.nodeName).toBe('#document-fragment');
    expect(Object.getPrototypeOf(fragment)).toBe(DocumentFragment.prototype);
    expect(fragment instanceof Node).toBe(true);
    expect(fragment instanceof Element).toBe(false);
    const span=document.createElement('span');span.textContent='fragment';
    const text=document.createTextNode(' text');
    fragment.append(span,text);
    expect(fragment.children.length).toBe(1);
    expect(fragment.firstElementChild).toBe(span);
    expect(fragment.lastElementChild).toBe(span);
    expect(fragment.childElementCount).toBe(1);
    expect(fragment.querySelector('span')).toBe(span);
    expect(fragment.textContent).toBe('fragment text');
    const clone=fragment.cloneNode(true);
    expect(clone.nodeType).toBe(11);
    expect(clone.textContent).toBe('fragment text');
    const parent=document.createElement('div');
    expect(parent.appendChild(fragment)).toBe(fragment);
    expect(fragment.childNodes.length).toBe(0);
    expect(parent.firstChild).toBe(span);
    expect(parent.childElementCount).toBe(1);
    expect(parent.textContent).toBe('fragment text');
    const second=document.createDocumentFragment();second.append(document.createTextNode('before'));
    parent.insertBefore(second,span);
    expect(second.childNodes.length).toBe(0);
    expect(parent.textContent).toBe('beforefragment text');
  });

  it('constructs native-backed class delegates with subclass state and new.target', () => {
    const make=globalThis[Symbol.for('ts')].makeNativeFunction;
    class State { value; target; constructor(value){if(value==='bad')throw new RangeError('bad');this.value=value;this.target=new.target;} }
    const Wrapped=make(State,'State',1,true,true);
    Wrapped.prototype=State.prototype;
    class Child extends Wrapped {}
    const original=Reflect.construct;
    try{
      Reflect.construct=()=>{throw new Error('modified');};
      const instance=new Child(7);
      expect(instance.value).toBe(7);
      expect(instance.target).toBe(Child);
      expect(instance instanceof Child).toBe(true);
      expect(instance instanceof Wrapped).toBe(true);
      expect(instance instanceof State).toBe(true);
      expect(()=>Wrapped(1)).toThrow();
      expect(()=>new Wrapped('bad')).toThrow('bad');
    }finally{Reflect.construct=original;}
  });

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
