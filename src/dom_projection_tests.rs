use super::*;

#[test]
fn replaced_document_preserves_traversal_and_equality_on_all_native_node_kinds() {
    super::tests::init_v8();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    for _ in 0..3 {
        install_independent_html(scope, "<!doctype html><body><div></div></body>");
        let code = v8::String::new(scope, r#"(()=>{
          const host=document.querySelector('div'),root=host.attachShadow({mode:'open'});
          root.innerHTML='<b>shadow</b>tail';
          const nodes=[root,document.createDocumentFragment(),document.createElement('section')];
          for(const node of nodes){
            if(node!==root)node.append(document.createElement('b'),document.createTextNode('tail'));
            if(node.lastChild!==node.childNodes.item(1)||node.lastChild.nodeName!=='#text'||node.lastChild.previousSibling!==node.firstChild)throw Error('native traversal');
            if(!node.isSameNode(node)||!node.isEqualNode(node)||node!==root&&!node.isEqualNode(node.cloneNode(true)))throw Error('native equality');
          }
          for(const name of ['lastChild','previousSibling','isSameNode','isEqualNode']){
            const descriptor=Object.getOwnPropertyDescriptor(Node.prototype,name);
            if(!descriptor||!descriptor.enumerable||!descriptor.configurable)throw Error('interface ownership '+name);
            const method=descriptor.get||descriptor.value;
            if(!Function.prototype.toString.call(method).includes('[native code]'))throw Error('native delegate '+name);
            let invalid=false;try{method.call({});}catch(error){invalid=error.name==='TypeError';}
            if(!invalid)throw Error('native receiver brand '+name);
          }
          return 'ok';
        })()"#).unwrap();
        v8::tc_scope!(let catch, scope);
        let result = v8::Script::compile(catch, code, None)
            .and_then(|script| script.run(catch))
            .unwrap_or_else(|| {
                panic!("{}", catch.exception().unwrap().to_rust_string_lossy(catch))
            });
        assert_eq!(result.to_rust_string_lossy(catch), "ok");
    }
    reset();
}
