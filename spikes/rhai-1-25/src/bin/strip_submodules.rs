use rhai::module_resolvers::StaticModuleResolver;
use rhai::{Engine, Module, Scope};

fn main() {
    let mut builder = Engine::new();
    let dep_ast = builder.compile("fn secret() { 42 }").unwrap();
    let dep = Module::eval_ast_as_new(Scope::new(), &dep_ast, &builder).unwrap();
    let mut resolver = StaticModuleResolver::new();
    resolver.insert("dep", dep);
    builder.set_module_resolver(resolver);
    let ts_ast = builder.compile("import \"dep\" as d;\nfn f() { d::secret() + 1 }\nfn g(x) { x * 2 }\nfn _hidden() { 1 }").unwrap();
    let ts = Module::eval_ast_as_new(Scope::new(), &ts_ast, &builder).unwrap();

    // variant 1: shadow each sub-module by an empty one
    let mut clean = ts.clone();
    let names: Vec<String> = clean.iter_sub_modules().map(|(k, _)| k.to_string()).collect();
    for n in &names { clean.set_sub_module(n.as_str(), Module::new()); }
    println!("variant1 sub-modules: {} (empty: {})", clean.iter_sub_modules().count(), clean.iter_sub_modules().all(|(_, m)| m.is_empty()));
    let mut llm = Engine::new();
    llm.register_static_module("ts", clean.into());
    println!("ts::f() = {:?}", llm.eval::<i64>("ts::f()").map_err(|e| e.to_string()));
    println!("ts::g(4) = {:?}", llm.eval::<i64>("ts::g(4)").map_err(|e| e.to_string()));
    println!("d::secret() = {:?}", llm.eval::<i64>("d::secret()").map_err(|e| e.to_string()));
    let meta = llm.gen_fn_metadata_to_json(false).unwrap();
    println!("meta mentions secret: {} ; dep: {} ; f: {}", meta.contains("secret"), meta.contains("\"d\""), meta.contains("\"f\""));
    println!("{}", meta);
}
