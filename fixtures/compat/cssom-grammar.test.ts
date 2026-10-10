import { describe, it, expect } from 'vitest';

describe('CSSOM grammar and stylesheet ownership', () => {
  it('exposes nonconstructible native-backed list members with Web IDL flags', () => {
    for(const C of [CSSRuleList,StyleSheetList]){
      const item=Object.getOwnPropertyDescriptor(C.prototype,'item');
      const length=Object.getOwnPropertyDescriptor(C.prototype,'length');
      expect(item.enumerable).toBe(true);expect(item.configurable).toBe(true);expect(item.writable).toBe(true);
      expect(item.value.name).toBe('item');expect(item.value.length).toBe(1);
      expect(length.get.name).toBe('get length');expect(length.get.length).toBe(0);
      expect(length.enumerable).toBe(true);expect(length.configurable).toBe(true);
      for(const fn of [item.value,length.get]){
        expect(Object.hasOwn(fn,'prototype')).toBe(false);
        expect(Function.prototype.toString.call(fn).includes('[native code]')).toBe(true);
        expect(()=>new fn()).toThrow();
        expect(()=>fn.call({})).toThrow();
      }
      expect(Object.getOwnPropertyDescriptor(C,'prototype').writable).toBe(false);
      expect(Object.getOwnPropertyDescriptor(globalThis,C.name).enumerable).toBe(false);
    }
  });

  it('retains branded live list identity across replacement and tree changes', () => {
    const sheet=new CSSStyleSheet(),rules=sheet.cssRules;
    sheet.replaceSync('a {} b {}');
    expect(rules).toBe(sheet.cssRules);
    expect(rules.length).toBe(2);
    expect(rules.item(0)).toBe(rules[0]);
    expect(Array.from(rules,r=>r.selectorText)).toEqual(['a','b']);
    const styles=document.styleSheets,initial=styles.length;
    const style=document.createElement('style');style.textContent='sentinel {}';document.head.appendChild(style);
    expect(styles).toBe(document.styleSheets);
    expect(styles instanceof StyleSheetList).toBe(true);
    expect(Array.isArray(styles)).toBe(false);
    expect(styles.length).toBe(initial+1);
    expect(styles.item(initial)).toBe(style.sheet);
    expect(Object.getOwnPropertyDescriptor(rules,'0').writable).toBe(false);
    expect(()=>new CSSRuleList()).toThrow();
    style.remove();expect(styles.length).toBe(initial);
  });
  it('recovers invalid selectors and known property values', () => {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync('a { color: red; width: garbage; } ??? { color: blue; } sentinel {}');
    expect(Array.from(sheet.cssRules, rule => rule.selectorText)).toEqual(['a', 'sentinel']);
    expect(sheet.cssRules[0].style.getPropertyValue('width')).toBe('');
    expect(sheet.cssRules[0].style.getPropertyValue('color')).toBe('red');
  });

  it('keeps escaped strings and nested grouping boundaries', () => {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync('/* { */ @media screen { a[data-x="}"] { content: "a;b}"; --token: url("a;b"); } }');
    const rule = sheet.cssRules[0].cssRules[0];
    expect(rule.selectorText).toBe('a[data-x="}"]');
    expect(rule.style.getPropertyValue('content')).toBe('"a;b}"');
    expect(rule.style.getPropertyValue('--token')).toBe('url("a;b")');
    expect(rule.parentRule).toBe(sheet.cssRules[0]);
    expect(rule.parentStyleSheet).toBe(sheet);
  });

  it('accepts deferred variable values and rejects invalid insertions', () => {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync('a { width: var(--width); height: inherit; }');
    expect(sheet.cssRules[0].style.getPropertyValue('width')).toBe('var(--width)');
    expect(sheet.cssRules[0].style.getPropertyValue('height')).toBe('inherit');
    expect(() => sheet.insertRule('??? {}')).toThrow();
  });

  it('attaches fetched sheets in document order and detaches on removal', () => {
    const style = document.createElement('style'); style.textContent='first {}';
    const link = document.createElement('link'); link.rel='stylesheet';
    document.head.append(style, link);
    const sheet = globalThis[Symbol.for('ts')].attachLinkedStyleSheet(link, '@import "child.css"; sentinel {}', 'https://example.test/main.css');
    expect(sheet.ownerNode).toBe(link);
    expect(sheet.href).toBe('https://example.test/main.css');
    expect(sheet.cssRules[0].type).toBe(3);
    expect(sheet.cssRules[0].href).toBe('child.css');
    expect(Array.from(document.styleSheets).slice(-2)).toEqual([style.sheet,sheet]);
    link.remove();
    expect(link.sheet).toBe(null);
    expect(sheet.ownerNode).toBe(null);
    style.remove();
  });
});
it('stores style/import rule state privately behind native interface accessors',()=>{
  const sheet=new CSSStyleSheet();sheet.replaceSync('sentinel { color: red; }');const rule=sheet.cssRules[0];
  expect(Reflect.ownKeys(rule)).toEqual([]);expect(Object.getPrototypeOf(rule)).toBe(CSSStyleRule.prototype);
  expect(rule.parentStyleSheet).toBe(sheet);expect(rule.parentRule).toBe(null);expect(rule.type).toBe(1);
  expect(rule.cssText).toBe('sentinel { color: red; }');rule.cssText='ignored { color: blue; }';expect(rule.selectorText).toBe('sentinel');
  rule.selectorText='changed';rule.style='color: blue';expect(rule.cssText).toBe('changed { color: blue; }');
  rule.selectorText='[';expect(rule.selectorText).toBe('changed');
  const descriptor=Object.getOwnPropertyDescriptor(CSSRule.prototype,'cssText');expect(descriptor.enumerable).toBe(true);expect(descriptor.configurable).toBe(true);expect(descriptor.get.name).toBe('get cssText');expect(descriptor.get.length).toBe(0);
  expect(()=>descriptor.get.call({})).toThrow(TypeError);expect(()=>new descriptor.get()).toThrow(TypeError);expect(()=>new CSSStyleRule()).toThrow(TypeError);
  const linked=document.createElement('style');linked.textContent='@import "child.css";';document.head.appendChild(linked);
  const imported=linked.sheet.cssRules[0];expect(Object.getPrototypeOf(imported)).toBe(CSSImportRule.prototype);expect(Reflect.ownKeys(imported)).toEqual([]);expect(imported.href).toBe('child.css');expect(imported.styleSheet).toBe(null);linked.remove();
});
it('keeps internal computed-style CSS reads out of target-installed accessor instrumentation',()=>{
  const style=document.createElement('style');style.textContent='.computed { color: red; }';document.head.appendChild(style);
  const element=document.createElement('div');element.className='computed';document.body.appendChild(element);
  const rules=Object.getOwnPropertyDescriptor(CSSStyleSheet.prototype,'cssRules'),text=Object.getOwnPropertyDescriptor(CSSRule.prototype,'cssText');let reads=0;
  Object.defineProperty(CSSStyleSheet.prototype,'cssRules',{...rules,get(){reads++;return rules.get.call(this);}});Object.defineProperty(CSSRule.prototype,'cssText',{...text,get(){reads++;return text.get.call(this);}});
  try{expect(getComputedStyle(element).getPropertyValue('color')).toBe('red');expect(reads).toBe(0);expect(style.sheet.cssRules[0].cssText).toBe('.computed { color: red; }');expect(reads).toBe(2);}
  finally{Object.defineProperty(CSSStyleSheet.prototype,'cssRules',rules);Object.defineProperty(CSSRule.prototype,'cssText',text);element.remove();style.remove();}
});
