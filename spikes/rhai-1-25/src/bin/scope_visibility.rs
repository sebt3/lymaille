use rhai::{Dynamic, Engine, Scope};

fn main() {
    let engine = Engine::new();

    // (a) same script: fn defined then reading a variable defined before the call
    println!("A same script : {:?}", engine.eval::<i64>("let x = 3; fn f() { x } f()").map_err(|e| e.to_string()));

    // (b) realistic path: functions kept as a growing AST, merged with each new script
    let fns = engine.compile("fn f() { x }\nfn g(a) { a + 1 }").unwrap();
    let mut scope = Scope::new();
    let s1 = engine.compile("let x = 3; x").unwrap();
    println!("B1 define x   : {:?}", engine.eval_ast_with_scope::<i64>(&mut scope, &fns.merge(&s1)).map_err(|e| e.to_string()));
    let s2 = engine.compile("f()").unwrap();
    println!("B2 call f() reading outer x (merged AST): {:?}", engine.eval_ast_with_scope::<i64>(&mut scope, &fns.merge(&s2)).map_err(|e| e.to_string()));
    let s3 = engine.compile("g(x)").unwrap();
    println!("B3 g(x) arg passing                    : {:?}", engine.eval_ast_with_scope::<i64>(&mut scope, &fns.merge(&s3)).map_err(|e| e.to_string()));

    // (c) call_fn on a host-owned scope: NOT the path used for LLM scripts
    let mut sc = Scope::new();
    sc.push("x", 3_i64);
    println!("C call_fn      : {:?}", engine.call_fn::<Dynamic>(&mut sc, &fns, "f", ()).map_err(|e| e.to_string()));

    // (d) global:: constants
    println!("D global:: const: {:?}", engine.eval::<i64>("const C = 7; fn g() { global::C } g()").map_err(|e| e.to_string()));
}
