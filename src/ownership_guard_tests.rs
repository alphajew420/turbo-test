//! Budget interruption must survive private postmutation guards.
use super::*;

fn materialize_termination(scope: &mut v8::PinScope) {
    let source = v8::String::new(scope, "1").unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    scope.terminate_execution();
    let _ = script.run(scope);
}

fn interrupt_method(
    scope: &mut v8::PinScope,
    _: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    materialize_termination(scope);
}
fn getter(
    _: &mut v8::PinScope,
    _: v8::Local<v8::Name>,
    _: v8::PropertyCallbackArguments,
    _: v8::ReturnValue,
) {
}
fn interrupt_setter(
    scope: &mut v8::PinScope,
    _: v8::Local<v8::Name>,
    _: v8::Local<v8::Value>,
    _: v8::PropertyCallbackArguments,
    _: v8::ReturnValue<()>,
) {
    materialize_termination(scope);
}
#[test]
fn native_method_guard_preserves_materialized_termination() {
    super::tests::init_v8();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    install_with_html(scope, "<body>sentinel");
    let template = method_template(scope, "interrupt", interrupt_method);
    let function = template.get_function(scope).unwrap();
    let key = v8::String::new(scope, "interrupt").unwrap();
    context
        .global(scope)
        .create_data_property(scope, key.into(), function.into());
    v8::tc_scope!(let catch, scope);
    let source = v8::String::new(catch, "interrupt(); 'resumed'").unwrap();
    let result = v8::Script::compile(catch, source, None).and_then(|script| script.run(catch));
    assert!(result.is_none());
    assert!(catch.has_terminated() || catch.is_execution_terminating());
    catch.cancel_terminate_execution();
    catch.reset();
    reset();
}
#[test]
fn native_setter_guard_preserves_materialized_termination() {
    super::tests::init_v8();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    install_with_html(scope, "<body>sentinel");
    let template = v8::ObjectTemplate::new(scope);
    tmpl_accessor(scope, template, "value", getter, interrupt_setter);
    let object = template.new_instance(scope).unwrap();
    let key = v8::String::new(scope, "boxed").unwrap();
    context
        .global(scope)
        .create_data_property(scope, key.into(), object.into());
    v8::tc_scope!(let catch, scope);
    let source = v8::String::new(catch, "boxed.value=1; 'resumed'").unwrap();
    let result = v8::Script::compile(catch, source, None).and_then(|script| script.run(catch));
    assert!(result.is_none());
    assert!(catch.has_terminated() || catch.is_execution_terminating());
    catch.cancel_terminate_execution();
    catch.reset();
    reset();
}
