//! Host-only event identity from the originating realm's private event records.
use super::*;

/// Detect genuine Events without consulting guest constructors, properties or brands.
pub fn is_native_event(scope: &mut v8::PinScope, event: v8::Local<v8::Object>) -> bool {
    event_record(scope, event).is_some()
}

/// Host-only mutable dispatch state from the genuine Event's originating realm.
/// Embeddings must keep this record inside private native/closure capabilities.
pub fn native_event_record<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: v8::Local<v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    event_record(scope, event)
}

/// Initialize an embedding-authorized opaque target without exposing a guest setter.
/// The embedding must authenticate ownership before calling this host-only seam.
pub fn register_native_event_target(
    scope: &mut v8::PinScope,
    target: v8::Local<v8::Object>,
) -> bool {
    let Some(context) = target.get_creation_context(scope) else {
        return false;
    };
    let scope = &mut v8::ContextScope::new(scope, context);
    if scope.is_execution_terminating() {
        return false;
    }
    let Some(name) = v8::String::new(scope, "turbo-dom.Abort.target") else {
        return false;
    };
    let key = v8::Private::for_api(scope, Some(name));
    if target
        .get_private(scope, key)
        .is_some_and(|value| value.is_object())
    {
        return true;
    }
    let record = v8::Object::new(scope);
    let listeners = v8::Array::new(scope, 0);
    let handlers = v8::Object::new(scope);
    let null = v8::null(scope);
    if record.set_prototype(scope, null.into()) != Some(true)
        || handlers.set_prototype(scope, null.into()) != Some(true)
    {
        return false;
    }
    for (name, value) in [
        ("listeners", listeners.into()),
        ("handlers", handlers.into()),
    ] {
        let Some(name) = v8::String::new(scope, name) else {
            return false;
        };
        if record.create_data_property(scope, name.into(), value) != Some(true) {
            return false;
        }
    }
    target.set_private(scope, key, record.into()) == Some(true)
}

/// Native EventTarget/Abort platform objects, without public instance properties.
pub fn is_native_event_target(scope: &mut v8::PinScope, event: v8::Local<v8::Object>) -> bool {
    private_platform_record(scope, event, "target")
}
pub fn is_native_abort_object(scope: &mut v8::PinScope, event: v8::Local<v8::Object>) -> bool {
    private_platform_record(scope, event, "signal")
        || private_platform_record(scope, event, "controller")
}
fn private_platform_record(
    scope: &mut v8::PinScope,
    event: v8::Local<v8::Object>,
    family: &str,
) -> bool {
    let name = v8::String::new(scope, &format!("turbo-dom.Abort.{family}")).unwrap();
    let key = v8::Private::for_api(scope, Some(name));
    event
        .get_private(scope, key)
        .is_some_and(|value| value.is_object())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_host_targets_are_initialized_privately_without_guest_brand_hooks() {
        super::super::tests::init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope,isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        super::super::install(scope);
        let code=v8::String::new(scope,"globalThis.hostTarget=Object.create(EventTarget.prototype);globalThis.hooks=0;Object.defineProperty(EventTarget,Symbol.hasInstance,{value(){hooks++;throw new Error('guest instanceof')}});Object.defineProperty(Object.prototype,'opaqueBrandProbe',{get(){hooks++;throw new Error('guest getter')}});for(const name of ['listeners','handlers'])Object.defineProperty(Object.prototype,name,{configurable:true,set(value){hooks++;globalThis.observedPrivate=value}});hostTarget;").unwrap();
        let target = v8::Script::compile(scope, code, None)
            .unwrap()
            .run(scope)
            .unwrap()
            .to_object(scope)
            .unwrap();
        assert!(!is_native_event_target(scope, target));
        assert!(register_native_event_target(scope, target));
        assert!(register_native_event_target(scope, target));
        assert!(is_native_event_target(scope, target));
        let code=v8::String::new(scope,"(()=>{let calls=0;hostTarget.addEventListener('probe',()=>calls++,{once:true});hostTarget.dispatchEvent(new Event('probe'));hostTarget.dispatchEvent(new Event('probe'));return calls===1&&hooks===0&&globalThis.observedPrivate===undefined&&Reflect.ownKeys(hostTarget).length===0})()").unwrap();
        assert!(v8::Script::compile(scope, code, None)
            .unwrap()
            .run(scope)
            .unwrap()
            .is_true());
        reset();
    }
    #[test]
    fn event_identity_uses_private_records_across_realms_and_ignores_forged_prototypes() {
        super::super::tests::init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let parent = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, parent);
        super::super::install(scope);
        let child = v8::Context::new(scope, Default::default());
        child.set_security_token(parent.get_security_token(scope));
        let foreign = {
            let scope = &mut v8::ContextScope::new(scope, child);
            install_independent_html(scope, "<!doctype html><body>");
            let source = v8::String::new(scope, "new Event('foreign')").unwrap();
            v8::Script::compile(scope, source, None)
                .unwrap()
                .run(scope)
                .unwrap()
                .to_object(scope)
                .unwrap()
        };
        let source = v8::String::new(scope,r#"(()=>{
          const real=new Event('real'),forged=Object.create(Event.prototype),plain={};
          for(const value of [real,forged,plain])Object.defineProperty(value,'constructor',{get(){throw new Error('guest constructor read')}});
          Object.defineProperty(Event,Symbol.hasInstance,{value(){throw new Error('guest instanceof')}});
          const target=new EventTarget(),controller=new AbortController();return [real,forged,plain,target,controller,controller.signal,Object.create(EventTarget.prototype),Object.create(AbortSignal.prototype)];
        })()"#).unwrap();
        let values = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        let values = v8::Local::<v8::Array>::try_from(values).unwrap();
        let genuine = values
            .get_index(scope, 0)
            .unwrap()
            .to_object(scope)
            .unwrap();
        let forged = values
            .get_index(scope, 1)
            .unwrap()
            .to_object(scope)
            .unwrap();
        let plain = values
            .get_index(scope, 2)
            .unwrap()
            .to_object(scope)
            .unwrap();
        assert!(is_native_event(scope, genuine));
        assert!(is_native_event(scope, foreign));
        assert!(!is_native_event(scope, forged));
        assert!(!is_native_event(scope, plain));
        let record = native_event_record(scope, genuine).unwrap();
        assert_eq!(get_str_prop(scope, record, "type").as_deref(), Some("real"));
        let record = native_event_record(scope, foreign).unwrap();
        assert_eq!(
            get_str_prop(scope, record, "type").as_deref(),
            Some("foreign")
        );
        assert!(native_event_record(scope, forged).is_none());
        assert!(native_event_record(scope, plain).is_none());
        let [target, controller, signal, forged_target, forged_signal] =
            [3, 4, 5, 6, 7].map(|index| {
                values
                    .get_index(scope, index)
                    .unwrap()
                    .to_object(scope)
                    .unwrap()
            });
        assert!(is_native_event_target(scope, target));
        assert!(is_native_event_target(scope, signal));
        assert!(is_native_abort_object(scope, controller));
        assert!(is_native_abort_object(scope, signal));
        assert!(!is_native_abort_object(scope, target));
        assert!(!is_native_event_target(scope, forged_target));
        assert!(!is_native_abort_object(scope, forged_signal));
        reset();
    }
}
