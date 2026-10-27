//! Formatting functions that the LLM calls from its scripts: text, JSON, aligned table from a list of maps, extraction, and
//! truncation that keeps the start and the end. They are pure, always granted, and documented for introspection.

use rhai::{Array, Dynamic, Engine, EvalAltResult, FuncRegistration, ImmutableString, Map, Variant};

use super::render::{full_text, json_value};

/// Registers the formatting functions on an engine.
///
/// Each function has its documentation comment and the names of its parameters, so that introspection can tell the LLM
/// what it does: `to_text`, `to_json`, `to_json_pretty`, `to_table`, `pluck`, `get_path`, `grep_lines`, `head_lines`,
/// `tail_lines`, `truncate` and `truncate_ends`.
pub fn register_format_functions(engine: &mut Engine) {
    register_text_functions(engine);
    register_table_functions(engine);
    register_line_functions(engine);
}

/// Registers the text and JSON functions: `to_text`, `to_json`, `to_json_pretty`.
fn register_text_functions(engine: &mut Engine) {
    register(
        engine,
        "to_text",
        &["value: Dynamic", "string"],
        "/// Text of a value: a string as it is, compact JSON for an array or a map, the Rhai text of any other value.",
        |value: Dynamic| full_text(&value),
    );
    register(
        engine,
        "to_json",
        &["value: Dynamic", "string"],
        "/// Compact JSON of a value. Fails when JSON refuses the value (function, non-finite float, custom type).",
        |value: Dynamic| json_text(&value, false),
    );
    register(
        engine,
        "to_json_pretty",
        &["value: Dynamic", "string"],
        "/// JSON of a value indented with two spaces. Fails when JSON refuses the value.",
        |value: Dynamic| json_text(&value, true),
    );
}

/// Registers the table and extraction functions: `to_table`, `pluck`, `get_path`.
fn register_table_functions(engine: &mut Engine) {
    register(
        engine,
        "to_table",
        &["rows: array", "string"],
        "/// Aligned text table of an array of maps: a header line, a separator line, one line per map, columns separated by ` | `. Columns are the union of the keys. Fails when a row is not a map.",
        |rows: Array| table(&rows, None),
    );
    register(
        engine,
        "to_table",
        &["rows: array", "columns: array", "string"],
        "/// Aligned text table of an array of maps, with only the named columns, in the given order. Fails when a row is not a map or a column is not a string.",
        |rows: Array, columns: Array| table(&rows, Some(&columns)),
    );
    register(
        engine,
        "pluck",
        &["rows: array", "key: string", "array"],
        "/// Array of the values of `key` in each map of `rows`, the unit where a row has no such key.",
        |rows: Array, key: ImmutableString| pluck(&rows, &key),
    );
    register(
        engine,
        "get_path",
        &["value: Dynamic", "path: string", "Dynamic"],
        "/// Value at a dotted path such as `a.b.0.c` (numeric segments index arrays), or the unit when the path does not exist. Fails on an empty path.",
        |value: Dynamic, path: ImmutableString| get_path(&value, &path),
    );
}

/// Registers the line and truncation functions: `grep_lines`, `head_lines`, `tail_lines`, `truncate`, `truncate_ends`.
fn register_line_functions(engine: &mut Engine) {
    register(
        engine,
        "grep_lines",
        &["text: string", "needle: string", "string"],
        "/// Lines of `text` that contain `needle` (literal, case sensitive), joined by newlines.",
        |text: ImmutableString, needle: ImmutableString| grep_lines(&text, &needle),
    );
    register(
        engine,
        "head_lines",
        &["text: string", "count: i64", "string"],
        "/// First `count` lines of `text`, joined by newlines. Fails when `count` is negative.",
        |text: ImmutableString, count: i64| {
            count_argument("head_lines", count).map(|count| head_lines(&text, count))
        },
    );
    register(
        engine,
        "tail_lines",
        &["text: string", "count: i64", "string"],
        "/// Last `count` lines of `text`, joined by newlines. Fails when `count` is negative.",
        |text: ImmutableString, count: i64| {
            count_argument("tail_lines", count).map(|count| tail_lines(&text, count))
        },
    );
    register(
        engine,
        "truncate",
        &["text: string", "max_chars: i64", "string"],
        "/// `text` cut to `max_chars` characters, followed by `... [N more characters]` when it was cut. Fails when `max_chars` is negative.",
        |text: ImmutableString, max_chars: i64| {
            count_argument("truncate", max_chars).map(|max_chars| truncate(&text, max_chars))
        },
    );
    register(
        engine,
        "truncate_ends",
        &["text: string", "head_chars: i64", "tail_chars: i64", "string"],
        "/// Start and end of `text` around a notice of the omitted characters, when `text` is longer than both counts together; the unchanged text otherwise. Fails when a count is negative.",
        |text: ImmutableString, head: i64, tail: i64| -> Result<String, Box<EvalAltResult>> {
            let head = count_argument("truncate_ends", head)?;
            let tail = count_argument("truncate_ends", tail)?;
            Ok(truncate_ends(&text, head, tail))
        },
    );
}

/// Registers one function with its parameter names, its return type (last) and its documentation comment, in the global
/// namespace.
fn register<A, const N: usize, const X: bool, R, const F: bool, FUNC>(
    engine: &mut Engine,
    name: &str,
    params: &[&str],
    comment: &str,
    function: FUNC,
) where
    R: Variant + Clone,
    FUNC: rhai::RhaiNativeFunc<A, N, X, R, F> + Send + Sync + 'static,
    A: 'static,
{
    let _metadata = FuncRegistration::new(name)
        .with_params_info(params)
        .with_comments([comment])
        .register_into_engine(engine, function);
}

/// Count given by the script: refused when negative. The error is raised to the script.
fn count_argument(function: &str, count: i64) -> Result<usize, Box<EvalAltResult>> {
    usize::try_from(count).map_err(|error| {
        Box::<EvalAltResult>::from(format!("{function}: the count must not be negative ({error})"))
    })
}

/// JSON text of a value, compact or indented.
fn json_text(value: &Dynamic, pretty: bool) -> Result<String, Box<EvalAltResult>> {
    let json = json_value(value)
        .ok_or_else(|| Box::<EvalAltResult>::from("to_json: the value cannot be encoded as JSON"))?;
    let text = if pretty {
        serde_json::to_string_pretty(&json)
    } else {
        serde_json::to_string(&json)
    };
    text.map_err(|error| Box::<EvalAltResult>::from(format!("to_json: {error}")))
}

/// Aligned table of maps. Without columns, the columns are the union of the keys in order of first appearance.
fn table(rows: &Array, columns: Option<&Array>) -> Result<String, Box<EvalAltResult>> {
    let maps = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            row.read_lock::<Map>()
                .map(|map| map.clone())
                .ok_or_else(|| Box::<EvalAltResult>::from(format!("to_table: row {index} is not a map")))
        })
        .collect::<Result<Vec<Map>, Box<EvalAltResult>>>()?;
    let names = match columns {
        Some(columns) => columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                column
                    .read_lock::<ImmutableString>()
                    .map(|name| name.to_string())
                    .ok_or_else(|| {
                        Box::<EvalAltResult>::from(format!("to_table: column {index} is not a string"))
                    })
            })
            .collect::<Result<Vec<String>, Box<EvalAltResult>>>()?,
        None => union_of_keys(&maps),
    };
    if names.is_empty() {
        return Ok(String::new());
    }
    let cells: Vec<Vec<String>> = maps
        .iter()
        .map(|map| {
            names
                .iter()
                .map(|name| map.get(name.as_str()).map_or_else(String::new, cell_text))
                .collect()
        })
        .collect();
    let mut widths: Vec<usize> = names.iter().map(|name| name.chars().count()).collect();
    for row in &cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut lines = vec![format_row(&names, &widths), separator(&widths)];
    lines.extend(cells.iter().map(|row| format_row(row, &widths)));
    Ok(lines.join("\n"))
}

/// Keys of the maps, each once, in order of first appearance.
fn union_of_keys(maps: &[Map]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for map in maps {
        for key in map.keys() {
            let name = key.to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// Text of a table cell: a string as it is, any other value as its text; a newline is shown as two characters.
fn cell_text(value: &Dynamic) -> String {
    let text = value
        .read_lock::<ImmutableString>()
        .map_or_else(|| full_text(value), |text| text.to_string());
    text.replace('\n', "\\n")
}

/// One table line: every cell but the last is padded to its column width, the columns are separated by ` | `.
fn format_row(cells: &[String], widths: &[usize]) -> String {
    let last = cells.len().saturating_sub(1);
    cells
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(index, (cell, width))| {
            if index == last {
                cell.clone()
            } else {
                format!("{cell:<width$}")
            }
        })
        .collect::<Vec<String>>()
        .join(" | ")
}

/// Separator line under the header: dashes under each column, joined by `-+-`.
fn separator(widths: &[usize]) -> String {
    widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<String>>()
        .join("-+-")
}

/// Values of `key` in each map of `rows`, the unit where a row has no such key or is not a map.
fn pluck(rows: &Array, key: &str) -> Array {
    rows.iter()
        .map(|row| {
            row.read_lock::<Map>()
                .and_then(|map| map.get(key).cloned())
                .unwrap_or(Dynamic::UNIT)
        })
        .collect()
}

/// Value at a dotted path. The unit when the path does not exist.
fn get_path(value: &Dynamic, path: &str) -> Result<Dynamic, Box<EvalAltResult>> {
    if path.is_empty() {
        return Err(Box::<EvalAltResult>::from("get_path: the path is empty"));
    }
    let mut current = value.clone();
    for segment in path.split('.') {
        match step(&current, segment) {
            Some(next) => current = next,
            None => return Ok(Dynamic::UNIT),
        }
    }
    Ok(current)
}

/// One step of a path: the item of an array at a numeric segment, the value of a key of a map.
fn step(value: &Dynamic, segment: &str) -> Option<Dynamic> {
    if let Some(items) = value.read_lock::<Array>() {
        let index = segment.parse::<usize>().ok()?;
        return items.get(index).cloned();
    }
    let map = value.read_lock::<Map>()?;
    map.get(segment).cloned()
}

/// Lines of `text` that contain `needle`.
fn grep_lines(text: &str, needle: &str) -> String {
    text.lines()
        .filter(|line| line.contains(needle))
        .collect::<Vec<&str>>()
        .join("\n")
}

/// First `count` lines of `text`.
fn head_lines(text: &str, count: usize) -> String {
    text.lines().take(count).collect::<Vec<&str>>().join("\n")
}

/// Last `count` lines of `text`.
fn tail_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .skip(lines.len().saturating_sub(count))
        .copied()
        .collect::<Vec<&str>>()
        .join("\n")
}

/// `text` cut to `max_chars` characters, with the count of the characters left out.
fn truncate(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}... [{} more characters]", total.saturating_sub(max_chars))
}

/// Start and end of `text` around a notice, when `text` is longer than the two counts together.
fn truncate_ends(text: &str, head: usize, tail: usize) -> String {
    let total = text.chars().count();
    let kept = head.saturating_add(tail);
    if total <= kept {
        return text.to_owned();
    }
    let omitted = total.saturating_sub(kept);
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(total.saturating_sub(tail)).collect();
    format!("{start}\n[... {omitted} characters omitted ...]\n{end}")
}

#[cfg(test)]
mod tests {
    use super::register_format_functions;
    use rhai::{Dynamic, Engine};

    fn engine() -> Engine {
        let mut engine = Engine::new();
        register_format_functions(&mut engine);
        engine
    }

    #[test]
    fn structured_object_as_json() {
        let result = engine().eval::<String>("let v = #{pods: [#{name: \"a\"}]}; to_json(v)");
        assert_eq!(
            result.ok().as_deref(),
            Some(r#"{"pods":[{"name":"a"}]}"#),
            "compact JSON"
        );
    }

    #[test]
    fn pretty_json() {
        let result = engine().eval::<String>("to_json_pretty(#{a: 1})");
        assert_eq!(
            result.ok().as_deref(),
            Some("{\n  \"a\": 1\n}"),
            "two spaces of indent"
        );
    }

    #[test]
    fn list_of_maps_as_aligned_table() {
        let script = "to_table([#{name: \"api\", ready: 2}, #{name: \"db\", ready: 10}])";
        let result = engine().eval::<String>(script);
        assert_eq!(
            result.ok().as_deref(),
            Some("name | ready\n-----+------\napi  | 2\ndb   | 10"),
            "aligned table"
        );
    }

    #[test]
    fn table_with_chosen_columns() {
        let script = "to_table([#{name: \"api\", ready: 2}, #{name: \"db\", ready: 10}], [\"ready\"])";
        let result = engine().eval::<String>(script);
        assert_eq!(
            result.ok().as_deref(),
            Some("ready\n-----\n2\n10"),
            "only the ready column"
        );
    }

    #[test]
    fn missing_keys_and_newlines_in_cells() {
        let script = "to_table([#{a: \"x\\ny\"}, #{b: 1}])";
        let result = engine().eval::<String>(script);
        assert_eq!(
            result.ok().as_deref(),
            Some("a    | b\n-----+--\nx\\ny | \n     | 1"),
            "newline shown as two characters, missing cells empty"
        );
    }

    #[test]
    fn table_of_non_maps_refused() {
        let result = engine().eval::<String>("to_table([#{a: 1}, 5])");
        match result {
            Err(error) => assert!(
                error.to_string().contains('1'),
                "the error names index 1: {error}"
            ),
            Ok(value) => panic!("a row that is not a map must be refused, got {value}"),
        }
    }

    #[test]
    fn empty_table() {
        let without = engine().eval::<String>("to_table([])");
        let with = engine().eval::<String>("to_table([], [\"a\"])");
        assert_eq!(without.ok().as_deref(), Some(""), "empty without columns");
        assert_eq!(
            with.ok().as_deref(),
            Some("a\n-"),
            "header and separator with columns"
        );
    }

    #[test]
    fn table_width_counts_characters() {
        let script = "to_table([#{c: \"\u{e9}\u{e9}\", d: 1}, #{c: \"abcd\", d: 2}])";
        let result = engine().eval::<String>(script);
        assert_eq!(
            result.ok().as_deref(),
            Some("c    | d\n-----+--\n\u{e9}\u{e9}   | 1\nabcd | 2"),
            "the short cell is padded to four characters, not to its byte length"
        );
    }

    #[test]
    fn extraction() {
        let script = "let rows = [#{n: \"a\", v: #{x: 1}}, #{n: \"b\"}]; \
                      [pluck(rows, \"n\"), get_path(rows[0], \"v.x\"), get_path(rows[1], \"v.x\")]";
        let values = match engine().eval::<rhai::Array>(script) {
            Ok(values) => values,
            Err(error) => panic!("the extraction must succeed: {error}"),
        };
        let names: Vec<String> = values
            .first()
            .and_then(|value| value.clone().try_cast::<rhai::Array>())
            .map_or_else(Vec::new, |array| {
                array.into_iter().map(|value| value.to_string()).collect()
            });
        assert_eq!(
            names,
            vec!["a".to_owned(), "b".to_owned()],
            "pluck returns the names"
        );
        assert_eq!(
            values.get(1).map(ToString::to_string),
            Some("1".to_owned()),
            "path found"
        );
        assert!(
            values.get(2).is_some_and(Dynamic::is_unit),
            "missing path gives the unit"
        );
    }

    #[test]
    fn path_through_an_array_index() {
        let result = engine().eval::<i64>("get_path(#{items: [#{id: 7}]}, \"items.0.id\")");
        assert_eq!(result.ok(), Some(7), "index 0 then key id");
    }

    #[test]
    fn line_helpers() {
        let text = "one\nerr two\nthree\nerr four\nfive";
        let script_grep = engine().eval::<String>(&format!("grep_lines({text:?}, \"err\")"));
        let script_head = engine().eval::<String>(&format!("head_lines({text:?}, 2)"));
        let script_tail = engine().eval::<String>(&format!("tail_lines({text:?}, 2)"));
        assert_eq!(
            script_grep.ok().as_deref(),
            Some("err two\nerr four"),
            "matching lines"
        );
        assert_eq!(
            script_head.ok().as_deref(),
            Some("one\nerr two"),
            "first two lines"
        );
        assert_eq!(
            script_tail.ok().as_deref(),
            Some("err four\nfive"),
            "last two lines"
        );
    }

    #[test]
    fn truncate_keeps_the_start() {
        let long = "a".repeat(100);
        let truncated = engine().eval::<String>(&format!("truncate({long:?}, 10)"));
        assert_eq!(
            truncated.ok(),
            Some(format!("{}... [90 more characters]", "a".repeat(10))),
            "first 10 characters and the count"
        );
        let short = engine().eval::<String>("truncate(\"0123456789\", 10)");
        assert_eq!(
            short.ok().as_deref(),
            Some("0123456789"),
            "a text of 10 characters is unchanged"
        );
    }

    #[test]
    fn truncate_ends_keeps_both_ends() {
        let text = format!("{}{}", "h".repeat(20), "m".repeat(950));
        let text = format!("{text}{}", "t".repeat(30));
        let result = engine().eval::<String>(&format!("truncate_ends({text:?}, 20, 30)"));
        match result {
            Ok(value) => {
                assert!(
                    value.starts_with(&"h".repeat(20)),
                    "starts with the first 20 characters"
                );
                assert!(
                    value.ends_with(&"t".repeat(30)),
                    "ends with the last 30 characters"
                );
                assert!(
                    value.contains("950 characters omitted"),
                    "mentions 950 omitted characters: {value}"
                );
            }
            Err(error) => panic!("truncate_ends must succeed: {error}"),
        }
    }

    #[test]
    fn negative_counts_refused() {
        let result = engine().eval::<String>("head_lines(\"a\\nb\", -1)");
        assert!(result.is_err(), "a negative count is a script error");
    }

    #[test]
    fn documented_for_introspection() {
        let engine = engine();
        let json = engine.gen_fn_metadata_to_json(false);
        let parsed: serde_json::Value = match json.map(|text| serde_json::from_str(&text)) {
            Ok(Ok(value)) => value,
            _ => panic!("the metadata must be JSON"),
        };
        let functions = parsed
            .get("functions")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for name in [
            "to_text",
            "to_json",
            "to_json_pretty",
            "to_table",
            "pluck",
            "get_path",
            "grep_lines",
            "head_lines",
            "tail_lines",
            "truncate",
            "truncate_ends",
        ] {
            let entries: Vec<&serde_json::Value> = functions
                .iter()
                .filter(|entry| entry.get("name").and_then(serde_json::Value::as_str) == Some(name))
                .collect();
            assert!(!entries.is_empty(), "`{name}` is registered");
            for entry in entries {
                let comments = entry.get("docComments").and_then(serde_json::Value::as_array);
                assert!(
                    comments.is_some_and(|c| !c.is_empty()),
                    "`{name}` has a documentation comment"
                );
                let params = entry.get("params").and_then(serde_json::Value::as_array);
                assert!(
                    params.is_some_and(|p| p.iter().all(|param| param.get("name").is_some())),
                    "`{name}` has named parameters"
                );
            }
        }
    }

    #[test]
    fn format_functions_do_not_panic() {
        let engine = engine();
        let scripts = [
            "to_text(\"\")",
            "to_table([])",
            "to_table([], [])",
            "pluck([], \"\")",
            "get_path(#{}, \"a\")",
            "grep_lines(\"\", \"\")",
            "head_lines(\"\", 0)",
            "tail_lines(\"\", 0)",
            "head_lines(\"abc\", 9223372036854775807)",
            "tail_lines(\"abc\", 9223372036854775807)",
            "truncate(\"\", 0)",
            "truncate(\"abc\", 9223372036854775807)",
            "truncate_ends(\"\", 0, 0)",
            "truncate_ends(\"abc\", 9223372036854775807, 9223372036854775807)",
        ];
        for script in scripts {
            // Either a value or a script error: the call must return, never panic.
            let _outcome = engine.eval::<Dynamic>(script);
        }
    }
}
