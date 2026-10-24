//! Deliberate faults, one per lint. Never fix them: that is their purpose.
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

pub fn undocumented() {}
mod hidden {
    pub fn unreachable_one() {}
}
pub enum Exhaustive {
    A,
    B,
}
pub struct NoDebug;
#[derive(Debug)]
pub struct NoCopy {
    pub a: u8,
}
pub enum Variants {
    Small(u8),
    Big([u8; 200]),
}
enum Local {
    A,
    B,
    C,
}

fn unwrap_it() -> u8 {
    Some(1_u8).unwrap()
}
fn expect_it() -> u8 {
    Some(1_u8).expect("x")
}
fn panic_it() {
    panic!("x");
}
fn todo_it() {
    todo!();
}
fn unimplemented_it() {
    unimplemented!();
}
fn unreachable_it() {
    unreachable!();
}
fn dbg_it() {
    dbg!(1_u8);
}
fn print_it() {
    println!("x");
}
fn eprint_it() {
    eprintln!("x");
}
fn index_it(v: &[u8], i: usize) -> u8 {
    v[i]
}
fn arith_it(a: u32, b: u32) -> u32 {
    a + b
}
fn division_it(a: u32, b: u32) -> u32 {
    a / b
}
fn modulo_it(a: i32, b: i32) -> i32 {
    a % b
}
fn cast_it(a: u32) -> u64 {
    a as u64
}
fn trivial_cast_it() -> u8 {
    1_u8 as u8
}
#[allow(dead_code)]
fn allow_it() {}
#[expect(dead_code)]
fn expect_without_reason() {}
fn underscore_it() {
    let _ = "1".parse::<u8>();
}
fn str_to_string_it() -> String {
    "a".to_string()
}
fn wildcard_it(l: &Local) -> u8 {
    match l {
        Local::A => 1,
        _ => 2,
    }
}
fn hash_iter_it() {
    for _ in HashMap::<u8, u8>::new() {}
}
fn env_it() -> bool {
    std::env::var("X").is_ok()
}
fn fs_it() -> bool {
    std::fs::read_to_string("x").is_ok()
}
fn clock_it() -> std::time::SystemTime {
    std::time::SystemTime::now()
}
fn command_it() -> std::process::Command {
    std::process::Command::new("x")
}
fn pass_by_value_it(s: String) -> usize {
    s.len()
}
fn const_it() -> u8 {
    1
}
async fn not_send_it() {
    let r = Rc::new(1_u8);
    async {}.await;
    drop(r);
}
fn many_args_it(a: u8, b: u8, c: u8, d: u8, e: u8, f: u8) -> u8 {
    a ^ b ^ c ^ d ^ e ^ f
}
fn shadow_it() -> usize {
    let a = 1_u8;
    let a = "x";
    a.len()
}
fn suffix_it() -> u8 {
    1u8
}
fn unused_result_it() {
    BTreeMap::<u8, u8>::new().insert(1, 2);
}
fn qualified_it() {
    let _m: std::collections::BTreeMap<u8, u8> = std::collections::BTreeMap::new();
}
fn single_use_it<'a>(x: &'a str) -> usize {
    x.len()
}
fn unicode_it() -> u8 {
    let é = 1_u8;
    é
}
fn unsafe_it() -> u8 {
    let p = &1_u8 as *const u8;
    unsafe { *p }
}
fn string_slice_it(s: &str) -> &str {
    &s[0..1]
}
fn get_unwrap_it(v: &[u8]) -> u8 {
    *v.get(0).unwrap()
}
fn format_push_it(s: &mut String) {
    s.push_str(&format!("x"));
}
fn then_some_it(c: bool) -> Option<u8> {
    if c { Some(1) } else { None }
}
fn map_err_it() -> Result<u8, ()> {
    "1".parse::<u8>().map_err(|_| ())
}
fn try_err_it() -> Result<u8, u8> {
    Err(1)?;
    Ok(0)
}
fn use_debug_it() {
    eprintln!("{:?}", 1_u8);
}
fn exit_it() {
    std::process::exit(1);
}
fn forget_it() {
    std::mem::forget(String::new());
}
