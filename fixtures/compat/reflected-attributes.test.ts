// @vitest-environment jsdom
import { it, expect } from 'vitest';

it('reflects enumerated editing attributes without changing the stored native attribute',()=>{
  const el=document.createElement('div');
  const cases=[['dir','LTR','ltr',''],['inputMode','NUMERIC','numeric',''],['enterKeyHint','Go','go',''],['virtualKeyboardPolicy','manual','manual','']];
  for(const [name,raw,expected,invalid]of cases){const attr=name.toLowerCase();expect(el[name]).toBe('');el.setAttribute(attr,raw);expect(el[name]).toBe(expected);el[name]='junk';expect(el.getAttribute(attr)).toBe('junk');expect(el[name]).toBe(invalid);}
  el.slot='assigned';el.elementTiming='hero';expect(el.getAttribute('slot')).toBe('assigned');expect(el.getAttribute('elementtiming')).toBe('hero');
  expect(Object.hasOwn(Element.prototype,'slot')).toBe(true);expect(Object.hasOwn(HTMLElement.prototype,'inputMode')).toBe(true);expect(Object.keys(el)).toEqual([]);
  const get=Object.getOwnPropertyDescriptor(HTMLElement.prototype,'dir').get;expect(()=>get.call({})).toThrow(TypeError);expect(()=>new get()).toThrow(TypeError);expect(()=>el.dir=Symbol()).toThrow(TypeError);
  expect(el.cloneNode().getAttribute('slot')).toBe('assigned');
});
it('uses attribute presence for booleans and preserves the hidden until-found state',()=>{
  const el=document.createElement('div');for(const name of ['autofocus','inert']){expect(el[name]).toBe(false);el.setAttribute(name,'false');expect(el[name]).toBe(true);el[name]=false;expect(el.hasAttribute(name)).toBe(false);el[name]='yes';expect(el.getAttribute(name)).toBe('');}
  expect(el.hidden).toBe(false);el.hidden='UNTIL-FOUND';expect(el.hidden).toBe('until-found');expect(el.getAttribute('hidden')).toBe('until-found');
  el.hidden='anything';expect(el.hidden).toBe(true);expect(el.getAttribute('hidden')).toBe('');el.hidden=null;expect(el.hidden).toBe(false);expect(()=>el.hidden=Symbol()).toThrow(TypeError);
});
it('inherits spellcheck and translation through actual ancestors',()=>{
  const parent=document.createElement('div'),child=document.createElement('span');parent.appendChild(child);parent.spellcheck=false;parent.translate=false;
  expect(child.spellcheck).toBe(false);expect(child.translate).toBe(false);child.setAttribute('spellcheck','junk');child.setAttribute('translate','junk');expect(child.spellcheck).toBe(false);expect(child.translate).toBe(false);
  child.spellcheck=true;child.translate=true;expect(child.getAttribute('spellcheck')).toBe('true');expect(child.getAttribute('translate')).toBe('yes');expect(child.spellcheck).toBe(true);expect(child.translate).toBe(true);
  const password=document.createElement('input');password.type='password';expect(password.spellcheck).toBe(false);password.spellcheck=true;expect(password.spellcheck).toBe(true);
  parent.writingSuggestions='false';child.removeAttribute('writingsuggestions');expect(child.writingSuggestions).toBe('false');child.writingSuggestions='junk';expect(child.writingSuggestions).toBe('true');expect(child.getAttribute('writingsuggestions')).toBe('junk');
});
it('resolves auto capitalization and correction through the associated form',()=>{
  const form=document.createElement('form'),input=document.createElement('input');form.autocapitalize='characters';form.autocorrect=false;form.appendChild(input);
  expect(input.autocapitalize).toBe('characters');expect(input.autocorrect).toBe(false);input.autocapitalize='off';expect(input.autocapitalize).toBe('none');input.autocorrect=true;expect(input.getAttribute('autocorrect')).toBe('on');
  input.autocapitalize='on';expect(input.autocapitalize).toBe('sentences');input.autocapitalize='junk';expect(input.autocapitalize).toBe('sentences');input.autocapitalize='';expect(input.autocapitalize).toBe('');
  const div=document.createElement('div');form.appendChild(div);expect(div.autocapitalize).toBe('');expect(div.autocorrect).toBe(true);
});
it('computes default draggable state from actual image and link attributes',()=>{
  const image=document.createElement('img'),link=document.createElement('a'),div=document.createElement('div');expect(image.draggable).toBe(true);expect(link.draggable).toBe(false);link.href='/target';expect(link.draggable).toBe(true);expect(div.draggable).toBe(false);
  image.draggable=false;expect(image.getAttribute('draggable')).toBe('false');expect(image.draggable).toBe(false);div.draggable='x';expect(div.getAttribute('draggable')).toBe('true');expect(div.cloneNode().draggable).toBe(true);
});
