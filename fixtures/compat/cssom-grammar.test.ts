import { describe, it, expect } from 'vitest';

describe('CSSOM grammar and stylesheet ownership', () => {
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
