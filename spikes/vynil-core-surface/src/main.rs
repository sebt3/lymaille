use vynil_core::Script;

fn main() {
    let s = Script::new_bare(vec![]);
    let mut names: Vec<String> = s
        .engine
        .gen_fn_signatures(false)
        .iter()
        .filter_map(|x| x.split('(').next().map(|n| n.trim().to_string()))
        .filter(|n| !n.contains(' ') && !n.starts_with('$') && !n.starts_with("op") && !n.contains("::"))
        .collect();
    names.sort();
    names.dedup();
    // keep only names that look like vynil-core helpers (snake_case with letters), drop Rhai builtins heuristically
    let builtins = ["print", "debug", "type_of", "len", "push", "pop", "is_empty", "to_string", "to_debug", "keys", "values", "contains", "get", "set", "parse_int", "parse_float", "to_int", "to_float", "to_array", "to_blob"];
    let helpers: Vec<_> = names.iter().filter(|n| !builtins.contains(&n.as_str())).collect();
    for probe in ["get_env", "sleep", "log_info", "log_debug", "file_read", "file_write", "read_dir", "shell_run", "sha256", "json_encode", "base64_encode", "url_encode", "basename"] {
        println!("{probe:14} registered: {}", names.iter().any(|n| n == probe));
    }
    println!("total distinct callable names: {}", names.len());
    let json = s.engine.gen_fn_metadata_to_json(false).expect("metadata feature must be enabled");
    println!("metadata feature unified, json bytes: {}", json.len());
    let _ = helpers;
}
