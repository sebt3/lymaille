use rhai::{Dynamic, Engine, Scope, AST};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Probe<T>(PhantomData<T>);
trait Fallback {
    fn is_ser(&self) -> bool {
        false
    }
}
impl<T> Fallback for Probe<T> {}
impl<T: serde::Serialize> Probe<T> {
    fn is_ser(&self) -> bool {
        true
    }
}
fn assert_send_sync<T: Send + Sync>() {}

fn main() {
    // S0: sync
    assert_send_sync::<Engine>();
    assert_send_sync::<AST>();
    assert_send_sync::<Scope>();
    assert_send_sync::<Dynamic>();
    println!("S0 Engine/AST/Scope/Dynamic are Send+Sync: ok");

    // S1: serialization of scope values, incl. closures / fn pointers
    let engine = Engine::new();
    let mut scope = Scope::new();
    engine
        .run_with_scope(
            &mut scope,
            r#"
        let n = 42; let fl = 1.5; let s = "hi"; let c = 'x'; let b = true; let u = ();
        let arr = [1, 2, 3]; let m = #{a: 1, b: "x"}; let bl = blob(3, 7);
        let f = |x| x + 1;
        let p = Fn("foo");
        let k = 5; let g = |x| x + k;
    "#,
        )
        .unwrap();
    for (name, _c, value) in scope.iter() {
        match serde_json::to_string(&value) {
            Ok(j) => println!("S1 {name} [{}] -> {j}", value.type_name()),
            Err(e) => println!("S1 {name} [{}] -> ERR {e}", value.type_name()),
        }
    }

    // S8: roundtrip of types through JSON
    println!("S8 roundtrip:");
    for (name, _c, value) in scope.iter() {
        if let Ok(j) = serde_json::to_string(&value) {
            match serde_json::from_str::<Dynamic>(&j) {
                Ok(back) => {
                    let same = value.type_name() == back.type_name();
                    println!("   {name}: {} -> {}{}", value.type_name(), back.type_name(), if same { "" } else { "   <-- TYPE CHANGED" });
                }
                Err(e) => println!("   {name}: deserialize ERR {e}"),
            }
        }
    }

    // S3: AST serializable? function metadata and doc comments
    println!("S3 AST: Serialize? {}", Probe::<AST>(PhantomData).is_ser());
    let ast = engine
        .compile("/// Adds one to x.\n/// Second line.\nfn inc(x) { x + 1 }\nprivate fn hidden(a, b) { a }\nfn two(a, b) { a + b }")
        .unwrap();
    for f in ast.iter_functions() {
        println!("S3 fn {}({}) access={:?} comments={:?}", f.name, f.params.join(","), f.access, f.comments);
    }

    // S4: size limits vs native results
    let mut e = Engine::new();
    e.set_max_string_size(100);
    e.set_max_array_size(10);
    e.register_fn("big", || "x".repeat(10_000));
    e.register_fn("bigarr", || vec![Dynamic::from(1_i64); 1000]);
    println!("S4 native fn returns 10k string: {:?}", e.eval::<Dynamic>("let a = big(); a.len()").map_err(|e| e.to_string()));
    println!("S4 native fn returns 1000-array: {:?}", e.eval::<Dynamic>("let a = bigarr(); a.len()").map_err(|e| e.to_string()));
    println!("S4 script builds big string: {:?}", e.eval::<Dynamic>("let s = \"x\"; s.pad(500, 'x'); s.len()").map_err(|e| e.to_string()));

    // S5: cancellation via on_progress
    let flag = Arc::new(AtomicBool::new(false));
    let mut e = Engine::new();
    let f2 = flag.clone();
    e.on_progress(move |_| if f2.load(Ordering::Relaxed) { Some(Dynamic::from("cancelled")) } else { None });
    e.register_fn("blocking_sleep", || std::thread::sleep(Duration::from_millis(1500)));
    let f3 = flag.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        f3.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let r = e.run("loop { }");
    println!("S5 loop{{}} cancelled: {:?} after {:?}", r.map_err(|e| e.to_string()), t.elapsed());
    flag.store(false, Ordering::Relaxed);
    let f4 = flag.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        f4.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let r = e.run("blocking_sleep(); let x = 1; loop { x += 1; }");
    println!("S5 cancel during native blocking call: {:?} after {:?}", r.map_err(|e| e.to_string()), t.elapsed());

    // S6: introspection with metadata feature
    let mut e = Engine::new();
    e.register_fn("json_encode", |v: Dynamic| v.to_string());
    let ast = e.compile("/// Doubles.\nfn dbl(x) { x * 2 }").unwrap();
    let sigs = e.gen_fn_signatures(false);
    println!("S6 gen_fn_signatures has json_encode: {}", sigs.iter().any(|s| s.contains("json_encode")));
    let js = e.gen_fn_metadata_with_ast_to_json(&ast, false).unwrap();
    println!("S6 metadata json has script fn 'dbl': {} ; doc comment 'Doubles': {}", js.contains("\"dbl\""), js.contains("Doubles"));

    // S7: engine shared across threads
    let e = Arc::new(Engine::new());
    let hs: Vec<_> = (0..4)
        .map(|i| {
            let e = e.clone();
            std::thread::spawn(move || e.eval::<i64>(&format!("let a = {i}; a * 10")).unwrap())
        })
        .collect();
    let rs: Vec<i64> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    println!("S7 shared engine across threads: {:?}", rs);
}
