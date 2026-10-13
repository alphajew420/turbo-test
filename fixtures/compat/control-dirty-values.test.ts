import { it, expect } from 'vitest';

it('separates text and textarea live values from their defaults', () => {
  document.body.innerHTML = '<input id="i" value="initial"><textarea id="t" value="ignored">initial text</textarea><input id="h" type="hidden" value="token">';
  const input = document.getElementById('i') as HTMLInputElement;
  expect(input.value).toBe('initial');
  input.value = 'edited'; input.defaultValue = 'new default';
  expect(input.value).toBe('edited'); expect(input.getAttribute('value')).toBe('new default');
  expect((input.cloneNode(true) as HTMLInputElement).value).toBe('edited');
  const text = document.getElementById('t') as HTMLTextAreaElement;
  expect(text.value).toBe('initial text'); text.value = 'live'; text.defaultValue = 'default';
  expect(text.value).toBe('live'); expect(text.textContent).toBe('default');
  expect((text.cloneNode(true) as HTMLTextAreaElement).value).toBe('live');
  const hidden = document.getElementById('h') as HTMLInputElement;
  hidden.value = 'new token'; expect(hidden.getAttribute('value')).toBe('new token');
});

it('uses default single selection and preserves explicit empty selection', () => {
  document.body.innerHTML = '<select id="s"><option disabled value="d">disabled</option><option value="a">A</option><option value="b">B</option></select><select id="m" multiple><option value="x">X</option></select>';
  const select = document.getElementById('s') as HTMLSelectElement;
  expect(select.value).toBe('a'); expect(select.selectedIndex).toBe(1);
  expect(select.options[1].selected).toBe(true);
  select.value = 'missing'; expect(select.value).toBe(''); expect(select.selectedIndex).toBe(-1);
  select.value = 'b'; expect(select.selectedIndex).toBe(2);
  select.selectedIndex = -1; expect(select.selectedOptions.length).toBe(0);
  expect((document.getElementById('m') as HTMLSelectElement).selectedIndex).toBe(-1);
});

it('document createElement is a nonconstructible method', () => {
  expect(document.createElement.length).toBe(1);
  expect(document.createElement.name).toBe('createElement');
  expect(()=>Reflect.construct(function(){}, [], document.createElement)).toThrow();
  expect(document.createElement('input').tagName).toBe('INPUT');
});

it('event constructors require a type and retain optional-init arities', () => {
  expect(Event.length).toBe(1); expect(CustomEvent.length).toBe(1);
  expect(CustomEvent.name).toBe('CustomEvent');
  expect(()=>new Event()).toThrow(); expect(()=>Event('click')).toThrow();
  expect(()=>new CustomEvent()).toThrow();
  expect(new Event(3).type).toBe('3');
  expect(new CustomEvent('test').detail).toBe(null);
  expect(new CustomEvent('test', {detail:42}).detail).toBe(42);
});
