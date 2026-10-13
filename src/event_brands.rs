//! Host-only event identity from the originating realm's private event records.
use super::*;

/// Detect genuine Events without consulting guest constructors, properties or brands.
pub fn is_native_event(scope: &mut v8::PinScope, event: v8::Local<v8::Object>) -> bool {
    event_record(scope, event).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
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
          return [real,forged,plain];
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
        reset();
    }
}
