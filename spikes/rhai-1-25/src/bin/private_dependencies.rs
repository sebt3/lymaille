use rhai::module_resolvers::StaticModuleResolver;
use rhai::{Dynamic, Engine, ImmutableString, Module, Scope};

fn main() {
    dump_meta();
    // ---- S9: private dependencies of a toolset module ----
    let mut builder = Engine::new();
    // the dependency module, only known to the *builder* engine
    let dep_ast = builder.compile("fn secret() { 42 }").unwrap();
    let dep = Module::eval_ast_as_new(Scope::new(), &dep_ast, &builder).unwrap();
    let mut resolver = StaticModuleResolver::new();
    resolver.insert("dep", dep);
    builder.set_module_resolver(resolver);

    // the toolset module imports `dep` and exposes `f`
    let ts_ast = builder.compile("import \"dep\" as d;\nfn f() { d::secret() + 1 }\nfn g(x) { x * 2 }").unwrap();
    let ts = Module::eval_ast_as_new(Scope::new(), &ts_ast, &builder).unwrap();

    // the LLM-facing engine: knows only the toolset, not `dep`
    let mut llm = Engine::new();
    llm.register_static_module("ts", ts.into());

    println!("S9 ts::f() uses private dep      : {:?}", llm.eval::<i64>("ts::f()").map_err(|e| e.to_string()));
    println!("S9 ts::g(4)                      : {:?}", llm.eval::<i64>("ts::g(4)").map_err(|e| e.to_string()));
    println!("S9 LLM calls dep directly        : {:?}", llm.eval::<i64>("import \"dep\" as d; d::secret()").map_err(|e| e.to_string()));
    println!("S9 LLM calls d::secret (no import): {:?}", llm.eval::<i64>("d::secret()").map_err(|e| e.to_string()));
    println!("S9 LLM calls ts::secret          : {:?}", llm.eval::<i64>("ts::secret()").map_err(|e| e.to_string()));
    let meta = llm.gen_fn_metadata_to_json(false).unwrap();
    println!("S9 introspection mentions 'secret': {} ; mentions 'dep': {} ; mentions ts fn 'f': {}",
        meta.contains("secret"), meta.contains("\"dep\""), meta.contains("\"f\""));

}

#[allow(dead_code)]
fn dump_meta() {
    let mut builder = Engine::new();
    let dep_ast = builder.compile("fn secret() { 42 }").unwrap();
    let dep = Module::eval_ast_as_new(Scope::new(), &dep_ast, &builder).unwrap();
    let mut resolver = StaticModuleResolver::new();
    resolver.insert("dep", dep);
    builder.set_module_resolver(resolver);
    let ts_ast = builder.compile("import \"dep\" as d;\nfn f() { d::secret() + 1 }").unwrap();
    let ts = Module::eval_ast_as_new(Scope::new(), &ts_ast, &builder).unwrap();
    let mut llm = Engine::new();
    llm.register_static_module("ts", ts.into());
    let meta = llm.gen_fn_metadata_to_json(false).unwrap();
    let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
    println!("META modules: {}", serde_json::to_string(&v["modules"]).unwrap());
    let fns: Vec<_> = v["functions"].as_array().unwrap().iter().filter(|f| f["name"] == "secret" || f["name"] == "f").map(|f| f.to_string()).collect();
    println!("META global fns named secret/f: {:?}", fns);
}
