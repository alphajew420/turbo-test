(function(){
  'use strict';
  const g=globalThis,ns=g[Symbol.for('ts')],native=g.__windowListenerNative;delete g.__windowListenerNative;
  const make=ns.makeNativeFunction,apply=Reflect.apply,StringValue=String,Exception=g.DOMException,ErrorType=TypeError,define=Object.defineProperty;
  const oldHost=g.__captureHostEventDispatch,BaseTarget=g.EventTarget,EventType=g.Event,document=g.document;
  const readEvent=event=>{const state=native('event',event);if(!state)throw new ErrorType('Argument must be an Event');return state;};
  const method=(object,name,fn,length)=>define(object,name,{value:make(fn,name,length),writable:true,configurable:true,enumerable:true});
  const getter=(object,name,fn)=>define(object,name,{get:make(fn,'get '+name,0),configurable:true,enumerable:true});
  const string=value=>{if(typeof value==='symbol')throw new ErrorType('Cannot convert a Symbol to a string');return StringValue(value);};
  if(!Object.getOwnPropertyDescriptor(g.ErrorEvent.prototype,'message')?.get){
    const states=new WeakMap(),read=states.get.bind(states),write=states.set.bind(states);
    const fields=['message','filename','lineno','colno','error'];
    class ErrorImplementation extends EventType{
      constructor(type,init={}){if(!arguments.length)throw new ErrorType('ErrorEvent requires a type');super(type,init);init=init??{};const colno=(+init.colno)>>>0,error=init.error??null,rawFilename=init.filename,filename=rawFilename===undefined?'':string(rawFilename),lineno=(+init.lineno)>>>0,rawMessage=init.message,message=rawMessage===undefined?'':string(rawMessage);write(this,{message,filename,lineno,colno,error});}
    }
    const ErrorCtor=make(ErrorImplementation,'ErrorEvent',1,true,true);ErrorCtor.prototype=ErrorImplementation.prototype;define(ErrorCtor.prototype,'constructor',{value:ErrorCtor,writable:true,configurable:true});define(ErrorCtor,'prototype',{writable:false});define(ErrorCtor.prototype,Symbol.toStringTag,{value:'ErrorEvent',configurable:true});
    for(const name of fields)getter(ErrorCtor.prototype,name,function(){const state=read(this);if(!state)throw new ErrorType('Illegal invocation');return state[name];});
    define(g,'ErrorEvent',{value:ErrorCtor,writable:true,configurable:true});
  }
  let signalGetter=Object.getOwnPropertyDescriptor(g.AbortSignal.prototype,'aborted')?.get;
  // Preserve an embedding runtime's genuine implementation. The standalone DOM
  // supplies a real abort primitive instead of its old empty constructor shells.
  if(!signalGetter){
    const Signal=make(function(){throw new ErrorType('Illegal constructor');},'AbortSignal',0,true,true);
    Object.setPrototypeOf(Signal.prototype,BaseTarget.prototype);define(Signal.prototype,Symbol.toStringTag,{value:'AbortSignal',configurable:true});define(Signal,'prototype',{writable:false});
    const read=signal=>{const record=native('signalGet',signal);if(!record)throw new ErrorType('Illegal invocation');return record;};
    getter(Signal.prototype,'aborted',function(){return read(this).aborted;});getter(Signal.prototype,'reason',function(){return read(this).reason;});
    method(Signal.prototype,'throwIfAborted',function(){const record=read(this);if(record.aborted)throw record.reason;},0);
    const Controller=make(function(){const signal=new BaseTarget();Object.setPrototypeOf(signal,Signal.prototype);native('signalSet',signal,{aborted:false,reason:undefined});native('controllerSet',this,{signal});},'AbortController',0,true,true);
    const controller=receiver=>{const record=native('controllerGet',receiver);if(!record)throw new ErrorType('Illegal invocation');return record;};
    getter(Controller.prototype,'signal',function(){return controller(this).signal;});
    method(Controller.prototype,'abort',function(reason){const signal=controller(this).signal,record=read(signal);if(record.aborted)return;record.aborted=true;record.reason=reason===undefined?new Exception('signal is aborted without reason','AbortError'):reason;oldHost(new EventType('abort'),signal);},0);
    define(Controller.prototype,Symbol.toStringTag,{value:'AbortController',configurable:true});define(Controller,'prototype',{writable:false});
    method(Signal,'abort',function(reason){const controller=new Controller();controller.abort(reason);return controller.signal;},0);
    define(g,'AbortSignal',{value:Signal,writable:true,configurable:true});define(g,'AbortController',{value:Controller,writable:true,configurable:true});
    signalGetter=Object.getOwnPropertyDescriptor(Signal.prototype,'aborted').get;
  }
  const localAborted=signal=>apply(signalGetter,signal,[]);
  const aborted=signal=>{const value=native("aborted",signal);if(typeof value!=="boolean")throw new ErrorType("Signal must be an AbortSignal");return value;};
  let signalAdd=g.AbortSignal.prototype.addEventListener,signalRemove=g.AbortSignal.prototype.removeEventListener;
  g.__captureWindowEventInterfaces=interfaces=>{const signal=interfaces.AbortSignal;if(signal&&typeof signal[0]==='function'){signalGetter=signal[0];signalAdd=signal[1];signalRemove=signal[2];}};
  function flatten(type,callback,options,remove=false,passiveDefault=false){
    type=string(type);
    if(callback!=null&&typeof callback!=='function'&&typeof callback!=='object')throw new ErrorType('Listener must be an object');
    let capture=false,once=false,passive=passiveDefault&&['touchstart','touchmove','wheel','mousewheel'].includes(type),signal=null;
    if(options!=null&&(typeof options==='object'||typeof options==='function')){
      capture=!!options.capture;
      if(!remove){once=!!options.once;const value=options.passive;if(value!==undefined)passive=!!value;signal=options.signal??null;if(signal!==null)aborted(signal);}
    }else capture=!!options;
    return {type,callback:callback??null,capture,once,passive,signal};
  }
  const registry=new Map(),get=registry.get.bind(registry),set=registry.set.bind(registry);
  function remove(record){record.removed=true;const list=get(record.type),index=list?.indexOf(record)??-1;if(index>=0)list.splice(index,1);if(record.signal&&record.abortHook)native('subscription',record.signal,record.abortHook,false);}
  function add(type,callback,options){
    if(this!==g)throw new ErrorType('Illegal invocation');if(arguments.length<2)throw new ErrorType('addEventListener requires 2 arguments');
    type=string(type);const record=flatten(type,callback,options,false,['touchstart','touchmove','wheel','mousewheel'].includes(type));
    if(record.callback===null||record.signal&&aborted(record.signal))return;
    const list=get(record.type)||[];if(list.some(value=>!value.removed&&!value.handler&&value.callback===record.callback&&value.capture===record.capture))return;
    record.removed=false;list.push(record);set(record.type,list);
    if(record.signal){record.abortHook=()=>remove(record);native('subscription',record.signal,record.abortHook,true);}
  }
  function removeListener(type,callback,options){
    if(this!==g)throw new ErrorType('Illegal invocation');if(arguments.length<2)throw new ErrorType('removeEventListener requires 2 arguments');
    const wanted=flatten(type,callback,options,true);for(const record of get(wanted.type)||[])if(!record.removed&&!record.handler&&record.callback===wanted.callback&&record.capture===wanted.capture)remove(record);
  }
  function invoke(event,capture,phase){
    const state=readEvent(event),records=(get(state.type)||[]).slice();state.currentTarget=g;state.eventPhase=phase;
    for(const record of records){if(state.stopImmediate)break;if(record.removed||record.capture!==capture||record.signal&&aborted(record.signal))continue;if(record.once)remove(record);state.passive=record.passive;
      try{if(record.handler){let args=[event],special=false;if(state.type==='error'){const values=native('error',event);if(values){args=values;special=true;}}const result=apply(record.callback,g,args);if(state.cancelable&&(special?result===true:result===false))state.defaultPrevented=true;}else if(typeof record.callback==='function')apply(record.callback,g,[event]);else apply(record.callback.handleEvent,record.callback,[event]);}catch(error){/* The embedding runtime still needs a complete listener-error reporter. */}finally{state.passive=false;}
    }
  }
  function dispatch(event,host=false){
    const state=readEvent(event);if(state.dispatching||!state.type)throw new Exception('The event is already being dispatched or uninitialized','InvalidStateError');
    state.dispatching=true;state.trusted=host;state.target=g;state.currentTarget=g;state.eventPhase=2;state.path=[g];
    const legacy=host&&(state.type==='load'||state.type==='pageshow');if(legacy)state.target=document;
    try{invoke(event,true,2);if(!state.stopImmediate)invoke(event,false,2);}finally{if(!legacy){state.currentTarget=null;state.eventPhase=0;state.path=[];state.dispatching=false;state.stop=false;state.stopImmediate=false;}state.passive=false;}
    return !state.defaultPrevented;
  }
  for(const type of 'abort blur cancel change click close contextmenu dblclick error focus hashchange input invalid keydown keypress keyup languagechange load message messageerror offline online pagehide pageshow popstate rejectionhandled reset resize scroll submit toggle unhandledrejection unload wheel beforeunload'.split(' ')){
    let current=null,record=null;define(g,'on'+type,{enumerable:true,configurable:true,get:make(function(){if(this!==g)throw new ErrorType('Illegal invocation');return current;},'get on'+type,0),set:make(function(value){if(this!==g)throw new ErrorType('Illegal invocation');value=typeof value==='function'?value:null;current=value;if(record){if(value===null){remove(record);record=null;}else record.callback=value;}else if(value){record={type,callback:value,capture:false,once:false,passive:false,signal:null,removed:false,handler:true};const list=get(type)||[];list.push(record);set(type,list);}},'set on'+type,1)});
  }
  method(g,'addEventListener',add,2);method(g,'removeEventListener',removeListener,2);method(g,'dispatchEvent',function(event){if(this!==g)throw new ErrorType('Illegal invocation');if(!arguments.length)throw new ErrorType('dispatchEvent requires 1 argument');return dispatch(event);},1);
  for(const name of ['addEventListener','removeEventListener','dispatchEvent'])ns.windowEventCallbacks[name]=g[name];
  g.__captureHostEventDispatch=(event,target)=>target===g?dispatch(event,true):oldHost(event,target);
  g.__captureWindowEventPhase=(event,capture)=>invoke(event,capture,capture?1:3);
  g.__captureListenerOptions=flatten;g.__captureSignalAborted=localAborted;g.__captureSignalSubscription=(signal,hook,subscribe)=>apply(subscribe?signalAdd:signalRemove,signal,['abort',hook,subscribe?{once:true}:undefined]);
})();
