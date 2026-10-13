//! Private phased Window dispatch and listener option capabilities.
use super::*;

struct WindowPhases(v8::Global<v8::Function>);
struct ListenerParser(v8::Global<v8::Function>);
struct SignalReader(v8::Global<v8::Function>);
struct WindowInterfaces(v8::Global<v8::Function>);
struct SignalSubscription(v8::Global<v8::Function>);
struct ErrorGetters(Vec<v8::Global<v8::Function>>);
fn records(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let operation = args.get(0).to_rust_string_lossy(scope);
    let Ok(object) = v8::Local::<v8::Object>::try_from(args.get(1)) else {
        return;
    };
    if operation == "subscription" {
        let Ok(hook) = v8::Local::<v8::Function>::try_from(args.get(2)) else {
            return;
        };
        let signal = v8::Global::new(scope, object);
        let hook = v8::Global::new(scope, hook);
        subscription(scope, &signal, &hook, args.get(3).is_true());
        return;
    }
    if operation == "aborted" {
        let Some(context) = object.get_creation_context(scope) else {
            return;
        };
        let Some(reader) = context.get_slot::<SignalReader>() else {
            return;
        };
        let reader = v8::Local::new(scope, &reader.0);
        let receiver = v8::undefined(scope).into();
        if let Some(value) = reader.call(scope, receiver, &[object.into()]) {
            rv.set(value);
        }
        return;
    }
    if operation == "error" {
        let Some(context) = object.get_creation_context(scope) else {
            return;
        };
        let Some(getters) = context.get_slot::<ErrorGetters>() else {
            return;
        };
        if getters.0.len() != 5 {
            return;
        }
        let scope = std::pin::pin!(v8::TryCatch::new(scope));
        let scope = &mut scope.init();
        let values = v8::Array::new(scope, 5);
        for (index, getter) in getters.0.iter().enumerate() {
            let getter = v8::Local::new(scope, getter);
            let Some(value) = getter.call(scope, object.into(), &[]) else {
                return;
            };
            values.set_index(scope, index as u32, value);
        }
        rv.set(values.into());
        return;
    }
    if operation == "event" {
        if let Some(record) = event_record(scope, object) {
            rv.set(record.into());
        }
        return;
    }
    let family = if operation.starts_with("signal") {
        "signal"
    } else {
        "controller"
    };
    let name = v8::String::new(scope, &format!("turbo-dom.Abort.{family}")).unwrap();
    let key = v8::Private::for_api(scope, Some(name));
    if operation.ends_with("Set") {
        object.set_private(scope, key, args.get(2));
    }
    if let Some(value) = object.get_private(scope, key) {
        rv.set(value);
    }
}
pub(super) fn install(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    bind_method(scope, global, "__windowListenerNative", records);
    run_js(scope, include_str!("event_listeners.js"));
    for (name, family) in [
        ("__captureWindowEventPhase", 0),
        ("__captureListenerOptions", 1),
        ("__captureSignalAborted", 2),
        ("__captureWindowEventInterfaces", 3),
        ("__captureSignalSubscription", 4),
    ] {
        let key = v8::String::new(scope, name).unwrap();
        if let Some(function) = global
            .get(scope, key.into())
            .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
        {
            let function = v8::Global::new(scope, function);
            let context = scope.get_current_context();
            match family {
                0 => {
                    context.set_slot(std::rc::Rc::new(WindowPhases(function)));
                }
                1 => {
                    context.set_slot(std::rc::Rc::new(ListenerParser(function)));
                }
                2 => {
                    context.set_slot(std::rc::Rc::new(SignalReader(function)));
                }
                3 => {
                    context.set_slot(std::rc::Rc::new(WindowInterfaces(function)));
                }
                _ => {
                    context.set_slot(std::rc::Rc::new(SignalSubscription(function)));
                }
            }
        }
        global.delete(scope, key.into());
    }
    capture_window_event_interfaces(scope);
}
fn owner<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Context>> {
    let owner = DOM_CONTEXTS
        .with(|contexts| contexts.borrow().get(&DOM_ID.with(|id| id.get())).cloned())?;
    Some(v8::Local::new(scope, &owner))
}
pub(super) fn window<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    if !DOM.with(|dom| {
        dom.borrow()
            .as_ref()
            .is_some_and(|dom| dom.browsing_context)
    }) {
        return None;
    }
    Some(owner(scope)?.global(scope))
}
pub(super) fn phase(scope: &mut v8::PinScope, event: v8::Local<v8::Object>, capture: bool) {
    let Some(context) = owner(scope) else {
        return;
    };
    let Some(dispatcher) = context.get_slot::<WindowPhases>() else {
        return;
    };
    let function = v8::Local::new(scope, &dispatcher.0);
    let flag = v8::Boolean::new(scope, capture);
    let receiver = v8::undefined(scope).into();
    function.call(scope, receiver, &[event.into(), flag.into()]);
}
pub(super) fn options<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments,
    remove: bool,
    passive: bool,
) -> Option<v8::Local<'s, v8::Object>> {
    let context = scope.get_current_context();
    let parser = context.get_slot::<ListenerParser>()?;
    let function = v8::Local::new(scope, &parser.0);
    let remove = v8::Boolean::new(scope, remove);
    let passive = v8::Boolean::new(scope, passive);
    let receiver = v8::undefined(scope).into();
    function
        .call(
            scope,
            receiver,
            &[
                args.get(0),
                args.get(1),
                args.get(2),
                remove.into(),
                passive.into(),
            ],
        )?
        .to_object(scope)
}
pub(super) fn aborted(scope: &mut v8::PinScope, signal: &v8::Global<v8::Object>) -> bool {
    let signal = v8::Local::new(scope, signal);
    let context = signal
        .get_creation_context(scope)
        .unwrap_or_else(|| scope.get_current_context());
    let Some(reader) = context.get_slot::<SignalReader>() else {
        return true;
    };
    let function = v8::Local::new(scope, &reader.0);
    let receiver = v8::undefined(scope).into();
    function
        .call(scope, receiver, &[signal.into()])
        .is_none_or(|value| value.is_true())
}

/// Capture trusted post-bootstrap interface getters without exposing a page setter.
pub fn capture_window_event_interfaces(scope: &mut v8::PinScope) {
    let context = scope.get_current_context();
    let Some(updater) = context.get_slot::<WindowInterfaces>() else {
        return;
    };
    let function = v8::Local::new(scope, &updater.0);
    let global = context.global(scope);
    let values = v8::Object::new(scope);
    for (interface, names) in [
        (
            "AbortSignal",
            &["aborted", "addEventListener", "removeEventListener"][..],
        ),
        (
            "ErrorEvent",
            &["message", "filename", "lineno", "colno", "error"][..],
        ),
    ] {
        let key = v8::String::new(scope, interface).unwrap();
        let Some(constructor) = global
            .get(scope, key.into())
            .and_then(|value| value.to_object(scope))
        else {
            continue;
        };
        let key = v8::String::new(scope, "prototype").unwrap();
        let Some(prototype) = constructor
            .get(scope, key.into())
            .and_then(|value| value.to_object(scope))
        else {
            continue;
        };
        let functions = v8::Array::new(scope, names.len() as i32);
        let mut getters = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let key = v8::String::new(scope, name).unwrap();
            let value = if interface == "AbortSignal" && index > 0 {
                prototype.get(scope, key.into())
            } else {
                prototype
                    .get_own_property_descriptor(scope, key.into())
                    .and_then(|value| value.to_object(scope))
                    .and_then(|descriptor| {
                        let key = v8::String::new(scope, "get").unwrap();
                        descriptor.get(scope, key.into())
                    })
            };
            if let Some(value) = value.filter(|value| value.is_function()) {
                functions.set_index(scope, index as u32, value);
                if interface == "ErrorEvent" {
                    let function = v8::Local::<v8::Function>::try_from(value).unwrap();
                    getters.push(v8::Global::new(scope, function));
                }
            }
        }
        if interface == "ErrorEvent" {
            context.set_slot(std::rc::Rc::new(ErrorGetters(getters)));
        }
        set_prop(scope, values, interface, functions.into());
    }
    let receiver = v8::undefined(scope).into();
    function.call(scope, receiver, &[values.into()]);
}

#[derive(Clone)]
pub(super) struct Listener {
    pub callback: v8::Global<v8::Object>,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
    pub signal: Option<v8::Global<v8::Object>>,
    pub removed: std::rc::Rc<std::cell::Cell<bool>>,
    pub abort_hook: Option<v8::Global<v8::Function>>,
}

fn subscription(
    scope: &mut v8::PinScope,
    signal: &v8::Global<v8::Object>,
    hook: &v8::Global<v8::Function>,
    add: bool,
) {
    let signal = v8::Local::new(scope, signal);
    let Some(context) = signal.get_creation_context(scope) else {
        return;
    };
    let Some(dispatcher) = context.get_slot::<SignalSubscription>() else {
        return;
    };
    let function = v8::Local::new(scope, &dispatcher.0);
    let hook = v8::Local::new(scope, hook);
    let flag = v8::Boolean::new(scope, add);
    let receiver = v8::undefined(scope).into();
    function.call(scope, receiver, &[signal.into(), hook.into(), flag.into()]);
}
pub(super) fn detach(scope: &mut v8::PinScope, listener: &Listener) {
    if let (Some(signal), Some(hook)) = (&listener.signal, &listener.abort_hook) {
        subscription(scope, signal, hook, false);
    }
}
fn abort_listener(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(args.data()) else {
        return;
    };
    let Some(node) = data
        .get_index(scope, 0)
        .and_then(|value| value.to_object(scope))
    else {
        return;
    };
    let _dom = DomScope::for_object(scope, node);
    let Some(handle) = handle_of(scope, node) else {
        return;
    };
    let Some(kind) = data
        .get_index(scope, 1)
        .map(|value| value.to_rust_string_lossy(scope))
    else {
        return;
    };
    let Some(callback) = data.get_index(scope, 2) else {
        return;
    };
    let capture = data
        .get_index(scope, 3)
        .is_some_and(|value| value.is_true());
    let removed = LISTENERS.with(|listeners| {
        let mut removed = Vec::new();
        if let Some(list) = listeners.borrow_mut().get_mut(&(handle, kind)) {
            list.retain(|listener| {
                let matched = listener.capture == capture
                    && v8::Local::new(scope, &listener.callback).strict_equals(callback);
                if matched {
                    listener.removed.set(true);
                    removed.push(listener.clone());
                }
                !matched
            });
        }
        removed
    });
    for listener in removed {
        detach(scope, &listener);
    }
}
pub(super) fn attach(
    scope: &mut v8::PinScope,
    node: v8::Local<v8::Object>,
    kind: &str,
    listener: &mut Listener,
) {
    let Some(signal) = &listener.signal else {
        return;
    };
    let data = v8::Array::new(scope, 4);
    data.set_index(scope, 0, node.into());
    let kind = v8::String::new(scope, kind).unwrap();
    data.set_index(scope, 1, kind.into());
    let callback = v8::Local::new(scope, &listener.callback);
    data.set_index(scope, 2, callback.into());
    let capture = v8::Boolean::new(scope, listener.capture);
    data.set_index(scope, 3, capture.into());
    let Some(hook) = v8::Function::builder(abort_listener)
        .data(data.into())
        .build(scope)
    else {
        return;
    };
    let hook = v8::Global::new(scope, hook);
    subscription(scope, signal, &hook, true);
    listener.abort_hook = Some(hook);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_events_and_nodes_keep_their_native_window_owner() {
        super::super::tests::init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let parent = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, parent);
        super::super::install(scope);
        run_js(scope, "globalThis.hits=[];addEventListener('foreign',event=>hits.push('parent'));window.onerror=function(message,source,line,column,error){hits.push([message,source,line,column,error===child.failure,arguments.length]);return true};");
        let child = v8::Context::new(scope, Default::default());
        child.set_security_token(parent.get_security_token(scope));
        {
            let scope = &mut v8::ContextScope::new(scope, child);
            install_independent_html(scope, "<!doctype html><body><span id=target></span>");
            run_js(scope, "globalThis.hits=[];globalThis.node=document.getElementById('target');globalThis.failure=new Error('foreign');globalThis.errorEvent=new ErrorEvent('error',{message:'foreign',filename:'child.js',lineno:9,colno:3,error:failure,cancelable:true});addEventListener('foreign',function(event){hits.push([event.currentTarget===window,event.target===node,event.composedPath().at(-1)===window])},true);Object.defineProperty(document,'defaultView',{value:{}});Object.defineProperty(ErrorEvent.prototype,'message',{get(){throw new Error('guest getter')}});");
        }
        let scope = &mut v8::ContextScope::new(scope, parent);
        let global = parent.global(scope);
        let child_global = child.global(scope);
        set_prop(scope, global, "child", child_global.into());
        let source = v8::String::new(scope, "child.node.dispatchEvent(new Event('foreign',{bubbles:true}));const cancelled=!window.dispatchEvent(child.errorEvent);let signalled=0;const signal=new child.AbortController();const callback=()=>signalled++;addEventListener('foreign-signal',callback,{signal:signal.signal});signal.abort();addEventListener('foreign-signal',callback);dispatchEvent(new Event('foreign-signal'));JSON.stringify([hits,child.hits,cancelled,signalled]);").unwrap();
        let result = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        assert_eq!(
            result.to_rust_string_lossy(scope),
            r#"[[["foreign","child.js",9,3,true,5]],[[true,true,true]],true,1]"#
        );
        reset();
    }
    #[test]
    fn window_listener_failure_preserves_event_state() {
        fixture(
            r#"(()=>{let rows=[];window.addEventListener('direct',()=>{throw new Error('listener failure')},true);window.addEventListener('direct',e=>rows.push([e.currentTarget===window,e.eventPhase,e.isTrusted,e.composedPath().length]));const event=new Event('direct');window.dispatchEvent(event);if(JSON.stringify(rows)!=='[[true,2,false,1]]'||event.currentTarget!==null||event.eventPhase!==0)throw new Error('listener cleanup '+JSON.stringify(rows));window.addEventListener('immediate',e=>{rows.push('capture');e.stopImmediatePropagation()},true);window.addEventListener('immediate',()=>rows.push('bad'));window.dispatchEvent(new Event('immediate'));if(rows.at(-1)!=='capture')throw new Error('immediate');let brand;try{window.dispatchEvent(Object.create(Event.prototype))}catch(error){brand=error.name}if(brand!=='TypeError')throw new Error('forged event');const node=document.createElement('div'),NativeException=DOMException;let intercepted=0;DOMException=function(){intercepted++;return new Error('fake')};let native=false;try{node.dispatchEvent(new Event(''))}catch(error){native=error instanceof NativeException&&error.name==='InvalidStateError'}if(!native||intercepted)throw new Error('public constructor intercepted dispatch');return true})()"#,
        );
    }
    fn fixture(source: &str) {
        super::super::tests::init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        super::super::install(scope);
        let inert = parse_html_document(scope, "<!doctype html><body><div></div>", false);
        let global = context.global(scope);
        set_prop(scope, global, "inert", inert.into());
        let scope = std::pin::pin!(v8::TryCatch::new(scope));
        let scope = &mut scope.init();
        let source = v8::String::new(scope, source).unwrap();
        let result = v8::Script::compile(scope, source, None).and_then(|script| script.run(scope));
        if result.is_none() {
            panic!(
                "event fixture: {:?}",
                scope
                    .exception()
                    .map(|error| error.to_rust_string_lossy(scope))
            );
        }
        assert!(result.unwrap().is_true());
        reset();
    }
    #[test]
    fn window_document_node_phases_path_and_cleanup() {
        fixture(
            r#"(()=>{const node=document.createElement('span');document.body.append(node);let rows=[];const add=(target,name,capture)=>target.addEventListener('probe',function(e){rows.push([name,e.eventPhase,e.currentTarget===this,e.target===node,e.composedPath().at(-1)===window]);},capture);add(window,'window-capture',true);add(document,'document-capture',true);add(node,'target-bubble',false);add(node,'target-capture',true);add(document,'document-bubble',false);add(window,'window-bubble',false);const event=new Event('probe',{bubbles:true});node.dispatchEvent(event);const wanted=[['window-capture',1,true,true,true],['document-capture',1,true,true,true],['target-capture',2,true,true,true],['target-bubble',2,true,true,true],['document-bubble',3,true,true,true],['window-bubble',3,true,true,true]];if(JSON.stringify(rows)!==JSON.stringify(wanted))throw new Error(JSON.stringify(rows));if(event.currentTarget!==null||event.eventPhase!==0||event.composedPath().length!==0||event.target!==node)throw new Error('cleanup');return true})()"#,
        );
    }
    #[test]
    fn nonbubbling_resource_error_reaches_window_capture_load_excludes_window() {
        fixture(
            r#"(()=>{const node=document.createElement('script');document.body.append(node);let rows=[];for(const type of ['error','load'])for(const [target,name] of [[window,'window'],[document,'document'],[node,'node']])for(const capture of [true,false])target.addEventListener(type,e=>rows.push([type,name,capture,e.eventPhase]),capture);node.dispatchEvent(new Event('error'));node.dispatchEvent(new Event('load',{bubbles:true}));const wanted=[['error','window',true,1],['error','document',true,1],['error','node',true,2],['error','node',false,2],['load','document',true,1],['load','node',true,2],['load','node',false,2],['load','document',false,3]];if(JSON.stringify(rows)!==JSON.stringify(wanted))throw new Error(JSON.stringify(rows));return true})()"#,
        );
    }
    #[test]
    fn listeners_once_removal_passive_objects_and_stop_contracts() {
        fixture(
            r#"(()=>{const node=document.createElement('div');document.body.append(node);let rows=[];const second=()=>rows.push('second');node.addEventListener('remove',()=>{rows.push('first');node.removeEventListener('remove',second)});node.addEventListener('remove',second);node.dispatchEvent(new Event('remove'));let calls=0;const once=()=>{calls++;node.dispatchEvent(new Event('once'))};node.addEventListener('once',once,{once:true});node.dispatchEvent(new Event('once'));node.dispatchEvent(new Event('once'));if(calls!==1||rows.join()!=='first')throw new Error('once/removal '+rows+calls);node.addEventListener('passive',e=>{e.preventDefault();e.returnValue=false},{passive:true});const passive=new Event('passive',{cancelable:true});if(!node.dispatchEvent(passive)||passive.defaultPrevented)throw new Error('passive');let objectThis=false;const object={handleEvent(e){objectThis=this===object&&e.currentTarget===node}};node.addEventListener('object',object);node.dispatchEvent(new Event('object'));if(!objectThis)throw new Error('object');rows=[];node.addEventListener('stop',e=>{rows.push('target-capture');e.stopPropagation()},true);node.addEventListener('stop',()=>rows.push('target-bubble'));window.addEventListener('stop',()=>rows.push('window-bubble'));node.dispatchEvent(new Event('stop',{bubbles:true}));if(rows.join()!=='target-capture,target-bubble')throw new Error('target stop '+rows);rows=[];window.addEventListener('blocked',e=>{rows.push('window');e.stopPropagation()},true);document.addEventListener('blocked',()=>rows.push('document'),true);node.addEventListener('blocked',()=>rows.push('node'));node.dispatchEvent(new Event('blocked',{bubbles:true}));if(rows.join()!=='window')throw new Error('ancestor stop '+rows);return true})()"#,
        );
    }
    #[test]
    fn abort_signals_remove_native_and_window_listeners_before_readdition() {
        fixture(
            r#"(()=>{let rows=[];const node=document.createElement('div');document.body.append(node);for(const [target,name] of [[node,'node'],[window,'window']]){const controller=new AbortController(),callback=()=>rows.push(name);target.addEventListener('signal',callback,{signal:controller.signal});controller.abort('reason');target.addEventListener('signal',callback);target.dispatchEvent(new Event('signal'));if(!controller.signal.aborted||controller.signal.reason!=='reason')throw new Error('state');let reason;try{controller.signal.throwIfAborted()}catch(error){reason=error}if(reason!=='reason')throw new Error('reason');target.removeEventListener('signal',callback);target.addEventListener('signal',()=>rows.push('bad'),{signal:AbortSignal.abort()});target.dispatchEvent(new Event('signal'));}if(rows.join()!=='node,window')throw new Error(rows);let queried=0;const signal={get aborted(){queried++;return false}};try{node.addEventListener('x',()=>{},{signal});throw new Error('accepted fake')}catch(error){if(error.name!=='TypeError')throw error}if(queried)throw new Error('guest getter');return true})()"#,
        );
    }
    #[test]
    fn window_onerror_five_arguments_and_ordinary_handler_cancellation() {
        fixture(
            r#"(()=>{const failure=new Error('boom'),rows=[];window.addEventListener('error',()=>rows.push('before'));window.onerror=function(message,source,line,column,error){rows.push([message,source,line,column,error===failure,arguments.length,this===window]);return true};window.addEventListener('error',()=>rows.push('after'));const event=new ErrorEvent('error',{message:'boom',filename:'fixture.js',lineno:4,colno:7,error:failure,cancelable:true});if(window.dispatchEvent(event)||!event.defaultPrevented)throw new Error('onerror cancellation');if(JSON.stringify(rows)!==JSON.stringify(['before',['boom','fixture.js',4,7,true,5,true],'after']))throw new Error(JSON.stringify(rows));let ordinary;window.onerror=function(e){ordinary=[arguments.length,e instanceof Event];return false};const resource=new Event('error',{cancelable:true});if(window.dispatchEvent(resource)||JSON.stringify(ordinary)!=='[1,true]')throw new Error('ordinary error');window.onmessage=function(e){return false};const message=new Event('message',{cancelable:true});if(window.dispatchEvent(message))throw new Error('ordinary cancellation');window.onerror=null;let calls=0;const callback=()=>calls++;window.onmessage=callback;addEventListener('message',callback);dispatchEvent(new Event('message'));removeEventListener('message',callback);dispatchEvent(new Event('message'));if(calls!==3)throw new Error('handler listener identity '+calls);window.onmessage=null;return true})()"#,
        );
    }
    #[test]
    fn dispatch_receiver_options_and_detached_document_security() {
        fixture(
            r#"(()=>{let rows=[];const node=document.createElement('div');document.body.append(node);const options={get capture(){rows.push('capture');return false},get once(){rows.push('once');return true},get passive(){rows.push('passive');return true},get signal(){rows.push('signal');return null}};const fn=()=>{};node.addEventListener('x',fn,options);if(rows.join()!=='capture,once,passive,signal')throw new Error(rows);rows=[];node.removeEventListener('x',fn,options);if(rows.join()!=='capture')throw new Error('remove options '+rows);let count=0;addEventListener('bare',()=>count++);EventTarget.prototype.dispatchEvent.call(window,new Event('bare'));if(count!==1)throw new Error('bare');const fake={};Object.defineProperty(inert,'defaultView',{value:fake});window.addEventListener('inert',()=>count++);inert.body.firstChild.dispatchEvent(new Event('inert',{bubbles:true}));if(count!==1)throw new Error('inert window');let reached=0;window.addEventListener('shadow',()=>reached++);const host=document.createElement('div');document.body.append(host);const shadow=host.attachShadow({mode:'closed'}),inside=document.createElement('span');shadow.append(inside);inside.dispatchEvent(new Event('shadow',{bubbles:true,composed:false}));if(reached)throw new Error('shadow privacy');return true})()"#,
        );
    }
}
