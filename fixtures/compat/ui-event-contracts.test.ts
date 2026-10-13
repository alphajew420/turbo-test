// @vitest-environment jsdom
import {it, expect} from 'vitest';

it('inherits the UI event interfaces and keeps input fields private and readonly',()=>{
 for(const Constructor of [UIEvent,MouseEvent,KeyboardEvent,InputEvent,FocusEvent,PointerEvent,WheelEvent]){
  const event=new Constructor('probe',{key:'a',clientX:4,data:'a',arbitrary:17});
  expect(event instanceof Event).toBe(true);expect(event instanceof UIEvent).toBe(true);
  expect(Reflect.ownKeys(event)).toEqual(['isTrusted']);expect('arbitrary' in event).toBe(false);
  expect(()=>Constructor('probe')).toThrow(TypeError);expect(()=>new Constructor()).toThrow(TypeError);
  expect(Constructor.length).toBe(1);expect(Function.prototype.toString.call(Constructor).includes('[native code]')).toBe(true);
 }
 expect(new PointerEvent('probe') instanceof MouseEvent).toBe(true);expect(new WheelEvent('probe') instanceof MouseEvent).toBe(true);
 const key=new KeyboardEvent('keydown',{key:'a',code:'KeyA',keyCode:65,shiftKey:true});
 const getter=Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'key').get;
 expect(getter.call(key)).toBe('a');expect(()=>getter.call({})).toThrow(TypeError);
 expect(Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'key').set).toBe(undefined);
 expect(key.which).toBe(65);expect(key.getModifierState('Shift')).toBe(true);
 expect(new KeyboardEvent('probe',{which:17}).which).toBe(0);
 expect(new InputEvent('input').data).toBe(null);expect(new InputEvent('input',{data:'x'}).getTargetRanges()).toEqual([]);
});
it('derives mouse coordinates and legacy button codes from the private event record',()=>{
 const mouse=new MouseEvent('mousedown',{clientX:8,clientY:12,button:2,buttons:2});
 expect([mouse.x,mouse.y,mouse.pageX,mouse.pageY,mouse.offsetX,mouse.offsetY]).toEqual([8,12,8,12,8,12]);
 expect(mouse.which).toBe(3);expect(mouse.isTrusted).toBe(false);
 mouse.initMouseEvent('click',true,true,null,1,20,30,4,5,true,false,false,false,1,null);
 expect([mouse.type,mouse.detail,mouse.clientX,mouse.clientY,mouse.which,mouse.ctrlKey]).toEqual(['click',1,4,5,2,true]);
 expect(()=>new MouseEvent('probe',{relatedTarget:{}})).toThrow(TypeError);
 expect(()=>new UIEvent('probe',{view:{}})).toThrow(TypeError);
 expect(()=>new KeyboardEvent('probe',{key:Symbol()})).toThrow(TypeError);
});
