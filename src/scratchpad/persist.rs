//! Persistence of the scratchpad: the scope becomes variables tagged by their real type, the functions become their source
//! text, and the restore recompiles the functions without ever replaying a script. See `src/scratchpad/persist.sdd`.

use std::collections::{BTreeMap, BTreeSet};

use rhai::{
    AST, ASTNode, Array, Blob, Dynamic, Engine as RhaiEngine, EvalAltResult, Expr, FnCallExpr, FnPtr,
    ImmutableString, Map, ParseError, Position, Scope, Stmt, Token, TokenizerControl,
};

use super::engine::ScratchpadEngine;
use super::error::{LimitKind, ScratchpadError, ScratchpadResult, ScriptPosition};
use crate::session::TaggedValue;

/// Deepest nesting of containers (arrays and tables) that is persisted; a container nested under this many others is refused.
///
/// The bound comes from what `SessionState::from_json` reads back: `serde_json` refuses more than 128 levels of JSON, and a
/// container takes two of them (`{"type": "array", "value": [...]}`). Measured with a probe: 63 nested containers read back
/// through `TaggedValue` alone, and 64 do not; through a session state, 62 read back and 63 do not. The bound is 60, two levels
/// below the measured state threshold, and the round trip of a session state at this depth is tested. Decided by the coordinator, to be confirmed.
const MAX_DATA_DEPTH: usize = 60;

/// The value at which Rhai saturates a line or a column of a position (`u16`).
const SATURATED_POSITION: usize = 65_535;

/// Names that Rhai handles on its own: a call to one of them is never reported at restore.
const RHAI_KEYWORDS: [&str; 9] = [
    "print",
    "debug",
    "type_of",
    "is_def_fn",
    "is_def_var",
    "eval",
    "call",
    "curry",
    "Fn",
];

/// Why a variable cannot be persisted. The reason is named in the warning, which is rendered to the LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnpersistableReason {
    /// An anonymous function: a closure.
    Closure,
    /// A named function pointer, such as `Fn("name")`.
    FunctionPointer,
    /// A value of a type that the tagged form does not represent, named by its Rhai type name.
    UnsupportedType(String),
    /// A floating point number that is not finite (NaN or infinity).
    NonFiniteFloat,
    /// An array or a table nested deeper than the persisted depth.
    TooDeep,
}

/// A variable that was not persisted, and why. The text is rendered to the LLM in the order of the variable names.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PersistWarning {
    variable: String,
    reason: UnpersistableReason,
}

impl PersistWarning {
    /// Name of the variable that was not persisted.
    #[must_use]
    pub fn variable(&self) -> &str {
        &self.variable
    }

    /// Why the variable was not persisted.
    #[must_use]
    pub const fn reason(&self) -> &UnpersistableReason {
        &self.reason
    }

    /// The text of the warning, in English, as rendered to the LLM.
    #[must_use]
    pub fn message(&self) -> String {
        match &self.reason {
            UnpersistableReason::Closure => format!(
                "warning: variable `{}` holds a closure and was not persisted; define a `fn` instead.",
                self.variable
            ),
            UnpersistableReason::FunctionPointer => format!(
                "warning: variable `{}` holds a function pointer and was not persisted; define a `fn` and call it by name instead.",
                self.variable
            ),
            UnpersistableReason::UnsupportedType(type_name) => format!(
                "warning: variable `{}` holds a value of type `{type_name}` that cannot be persisted and was not persisted.",
                self.variable
            ),
            UnpersistableReason::NonFiniteFloat => format!(
                "warning: variable `{}` holds a non-finite number and was not persisted.",
                self.variable
            ),
            UnpersistableReason::TooDeep => format!(
                "warning: variable `{}` nests deeper than {MAX_DATA_DEPTH} levels and was not persisted.",
                self.variable
            ),
        }
    }
}

/// The variables of a scope that can be persisted, and the warnings of those that cannot.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ScopeSnapshot {
    variables: BTreeMap<String, TaggedValue>,
    warnings: Vec<PersistWarning>,
}

impl ScopeSnapshot {
    /// The persistable variables, ordered by name.
    #[must_use]
    pub const fn variables(&self) -> &BTreeMap<String, TaggedValue> {
        &self.variables
    }

    /// The warnings of the excluded variables, ordered by name.
    #[must_use]
    pub fn warnings(&self) -> &[PersistWarning] {
        &self.warnings
    }
}

/// The source text of one function of a script, under its key `name/arity`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FunctionSource {
    key: String,
    text: String,
}

impl FunctionSource {
    /// The key of the function, `name/arity`, such as `scale/2`.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The source text of the function, documentation included, from its first comment to its closing brace.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A call that the restore could not match to a known name. Best effort: the call may still work.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RestoreWarning {
    function: String,
    call: String,
    line: Option<usize>,
}

impl RestoreWarning {
    /// The key of the function that makes the call, `name/arity`.
    #[must_use]
    pub fn function(&self) -> &str {
        &self.function
    }

    /// The called name, qualified by its module when it has one.
    #[must_use]
    pub fn call(&self) -> &str {
        &self.call
    }

    /// The line of the call in the text of the function, when it is known.
    #[must_use]
    pub const fn line(&self) -> Option<usize> {
        self.line
    }

    /// The text of the warning, as rendered to the LLM.
    #[must_use]
    pub fn message(&self) -> String {
        let place = self
            .line
            .map_or_else(String::new, |line| format!(" (line {line})"));
        format!(
            "function `{}` calls `{}`, which is not known{place}; this is a best-effort check, the call may still work.",
            self.function, self.call
        )
    }
}

/// The functions recompiled by a restore: their AST without any top-level statement, the keys of the broken sources
/// (sorted by key), and the warnings on the calls.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Recompiled {
    ast: AST,
    broken: Vec<String>,
    warnings: Vec<RestoreWarning>,
}

impl Recompiled {
    /// The AST of the restored functions. It holds no statement.
    #[must_use]
    pub const fn ast(&self) -> &AST {
        &self.ast
    }

    /// The keys of the sources that did not compile or did not define their function, sorted by key.
    #[must_use]
    pub fn broken(&self) -> &[String] {
        &self.broken
    }

    /// The calls that match no known name, sorted by function key then by line.
    #[must_use]
    pub fn warnings(&self) -> &[RestoreWarning] {
        &self.warnings
    }
}

/// Splits a scope into the variables that can be persisted and the warnings of those that cannot. A name bound twice
/// keeps its last value, as the scope does.
#[must_use]
pub fn snapshot_scope(scope: &Scope<'_>) -> ScopeSnapshot {
    let mut latest: BTreeMap<String, Dynamic> = BTreeMap::new();
    for (name, _constant, value) in scope.iter() {
        let _replaced = latest.insert(name.to_owned(), value);
    }
    let mut variables = BTreeMap::new();
    let mut warnings = Vec::new();
    for (name, value) in latest {
        match to_tagged(&value) {
            Ok(tagged) => {
                let _replaced = variables.insert(name, tagged);
            }
            Err(reason) => warnings.push(PersistWarning {
                variable: name,
                reason,
            }),
        }
    }
    ScopeSnapshot { variables, warnings }
}

/// The tagged form of a value, or the reason it cannot be persisted.
///
/// # Errors
///
/// Returns the reason when the value is or contains a closure, a function pointer, a non-finite number, a value of an
/// unsupported type, or an array or table nested deeper than the persisted depth.
pub fn to_tagged(value: &Dynamic) -> Result<TaggedValue, UnpersistableReason> {
    tagged_at(value, 0)
}

/// Converts a value at a given number of enclosing containers. Containers are refused beyond [`MAX_DATA_DEPTH`].
fn tagged_at(value: &Dynamic, depth: usize) -> Result<TaggedValue, UnpersistableReason> {
    if value.is_shared() {
        return tagged_at(&value.flatten_clone(), depth);
    }
    if value.is_unit() {
        return Ok(TaggedValue::Unit);
    }
    if let Ok(flag) = value.as_bool() {
        return Ok(TaggedValue::Bool(flag));
    }
    if let Ok(number) = value.as_int() {
        return Ok(TaggedValue::Int(number));
    }
    if let Ok(number) = value.as_float() {
        return if number.is_finite() {
            Ok(TaggedValue::Float(number))
        } else {
            Err(UnpersistableReason::NonFiniteFloat)
        };
    }
    if let Ok(character) = value.as_char() {
        return Ok(TaggedValue::Char(character));
    }
    if let Some(text) = value.read_lock::<ImmutableString>() {
        return Ok(TaggedValue::String(text.to_string()));
    }
    if let Some(bytes) = value.read_lock::<Blob>() {
        return Ok(TaggedValue::Blob(bytes.clone()));
    }
    if let Some(items) = value.read_lock::<Array>() {
        return array_at(&items, depth);
    }
    if let Some(table) = value.read_lock::<Map>() {
        return table_at(&table, depth);
    }
    if let Some(pointer) = value.read_lock::<FnPtr>() {
        return Err(if pointer.is_anonymous() {
            UnpersistableReason::Closure
        } else {
            UnpersistableReason::FunctionPointer
        });
    }
    Err(UnpersistableReason::UnsupportedType(value.type_name().to_owned()))
}

/// The depth of the values inside a container at `depth`, or the refusal of the container.
const fn inner_depth(depth: usize) -> Result<usize, UnpersistableReason> {
    if depth >= MAX_DATA_DEPTH {
        return Err(UnpersistableReason::TooDeep);
    }
    Ok(depth.saturating_add(1))
}

fn array_at(items: &Array, depth: usize) -> Result<TaggedValue, UnpersistableReason> {
    let inner = inner_depth(depth)?;
    items
        .iter()
        .map(|item| tagged_at(item, inner))
        .collect::<Result<Vec<_>, _>>()
        .map(TaggedValue::Array)
}

fn table_at(table: &Map, depth: usize) -> Result<TaggedValue, UnpersistableReason> {
    let inner = inner_depth(depth)?;
    table
        .iter()
        .map(|(key, value)| tagged_at(value, inner).map(|tagged| (key.to_string(), tagged)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map(TaggedValue::Map)
}

/// The Rhai value of the same real type as the tag.
pub fn to_dynamic(value: &TaggedValue) -> Dynamic {
    match value {
        TaggedValue::Unit => Dynamic::UNIT,
        TaggedValue::Bool(flag) => Dynamic::from(*flag),
        TaggedValue::Int(number) => Dynamic::from(*number),
        TaggedValue::Float(number) => Dynamic::from(*number),
        TaggedValue::Char(character) => Dynamic::from(*character),
        TaggedValue::String(text) => Dynamic::from(text.clone()),
        TaggedValue::Blob(bytes) => Dynamic::from_blob(bytes.clone()),
        TaggedValue::Array(items) => Dynamic::from_array(items.iter().map(to_dynamic).collect()),
        TaggedValue::Map(table) => {
            let mut map = Map::new();
            for (key, item) in table {
                let _replaced = map.insert(key.as_str().into(), to_dynamic(item));
            }
            Dynamic::from_map(map)
        }
    }
}

/// Extracts the source text of each function of a script, in the order of definition.
///
/// # Errors
///
/// Returns a syntax error when the script does not compile or defines a method on a type, a limit error when a function
/// starts or ends at a position that Rhai cannot hold, and an internal error when the source of a function cannot be
/// delimited. In every case no function of the script is persisted.
pub fn extract_function_sources(
    engine: &ScratchpadEngine,
    script: &str,
) -> ScratchpadResult<Vec<FunctionSource>> {
    extract_verified(engine, script, check_against_ast)
}

/// Extracts the functions of a script, then passes them to `verify`, the self-check. Its failure fails the extraction.
fn extract_verified(
    engine: &ScratchpadEngine,
    script: &str,
    verify: impl FnOnce(&AST, &str, &LineIndex, &[Found]) -> ScratchpadResult<()>,
) -> ScratchpadResult<Vec<FunctionSource>> {
    let rhai = engine.rhai();
    let ast = rhai.compile(script).map_err(compile_error)?;
    let index = LineIndex::new(script);
    let found = scan(rhai, script, &index)?;
    verify(&ast, script, &index, &found)?;
    found
        .iter()
        .map(|function| -> ScratchpadResult<FunctionSource> {
            let text = script
                .get(function.start..function.end)
                .ok_or_else(|| internal("the source of a function cannot be delimited"))?;
            Ok(FunctionSource {
                key: function.key.clone(),
                text: text.to_owned(),
            })
        })
        .collect()
}

/// Recompiles each source separately, keeps the functions only, and reports the broken sources and the calls to
/// unknown names. No top-level statement is run.
#[must_use]
pub fn recompile_functions(engine: &ScratchpadEngine, sources: &BTreeMap<String, String>) -> Recompiled {
    let rhai = engine.rhai();
    let mut compiled: Vec<(&str, AST)> = Vec::new();
    let mut broken = Vec::new();
    for (key, text) in sources {
        match rhai.compile(text) {
            Ok(ast) if defines_only(&ast, key) => compiled.push((key.as_str(), ast)),
            _ => broken.push(key.clone()),
        }
    }
    // The names of the restored engine: every native of the granted groups and of the standard packages, then the
    // granted module paths of the catalogue.
    let mut known: BTreeSet<String> = engine
        .rhai()
        .gen_fn_signatures(true)
        .iter()
        .filter_map(|signature| signature.split('(').next())
        .map(str::to_owned)
        .collect();
    known.extend(
        engine
            .catalog()
            .functions()
            .iter()
            .map(|entry| entry.path().to_owned()),
    );
    known.extend(
        sources
            .keys()
            .filter_map(|key| split_key(key).map(|(name, _)| name.to_owned())),
    );

    let mut functions = AST::empty();
    let mut warnings = Vec::new();
    for (key, ast) in &compiled {
        collect_unknown_calls(ast, key, &known, &mut warnings);
        functions = functions.merge(&ast.clone_functions_only());
    }
    broken.sort();
    warnings.sort_by(|left, right| {
        left.function
            .cmp(&right.function)
            .then_with(|| left.line.cmp(&right.line))
    });
    Recompiled {
        ast: functions,
        broken,
        warnings,
    }
}

/// A scope holding the variables, in the order of their names.
#[must_use]
pub fn restore_scope(variables: &BTreeMap<String, TaggedValue>) -> Scope<'static> {
    let mut scope = Scope::new();
    for (name, value) in variables {
        let _pushed = scope.push_dynamic(name.as_str(), to_dynamic(value));
    }
    scope
}

/// A function of the script as the scanner read it: its key, the offsets of its text, and the offset of its closing brace.
struct Found {
    key: String,
    start: usize,
    end: usize,
    close: usize,
}

/// A function whose header and body the scanner is reading.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for the name after `fn`.
    Name,
    /// The name is read: waiting for `(`, `()` or a `.` that would make a method on a type.
    AfterName,
    /// Inside the parameter list.
    Params,
    /// The parameters are read: waiting for the opening brace of the body.
    BeforeBody,
    /// Inside the body.
    Body,
}

/// The function being read by the scanner.
struct Pending {
    name: Option<String>,
    arity: usize,
    start: Position,
    fn_at: Position,
    phase: Phase,
}

/// A function read by the scanner, before its positions become offsets.
struct RawFunction {
    key: String,
    start: Position,
    close: Position,
}

/// The state of the token scanner. It tracks the braces to find the end of each top-level function.
#[derive(Default)]
struct Scanner {
    /// One entry per open brace; `true` for an interpolation brace of a backtick string.
    stack: Vec<bool>,
    /// Whether the previous token was an interpolated string, whose next brace opens an interpolation.
    prev_interp: bool,
    /// The first documentation comment of the run of comments before the next item.
    doc_start: Option<Position>,
    /// Where the next item starts, when it is `private`.
    item_start: Option<Position>,
    /// The function being read, if any.
    pending: Option<Pending>,
    /// The functions read so far, in the order of definition.
    found: Vec<RawFunction>,
}

impl Scanner {
    /// Reads one token that is not the end of the input.
    fn step(
        &mut self,
        token: &Token,
        position: Position,
        control: &TokenizerControl,
    ) -> ScratchpadResult<()> {
        let after_interp = std::mem::replace(
            &mut self.prev_interp,
            matches!(token, Token::InterpolatedString(_)),
        );
        if self.pending.is_some() {
            self.step_function(token, position, after_interp, control)
        } else {
            self.step_outside(token, position, after_interp, control)
        }
    }

    /// Reads a token outside any function: documentation comments, `private`, `fn` and the braces of other items.
    fn step_outside(
        &mut self,
        token: &Token,
        position: Position,
        after_interp: bool,
        control: &TokenizerControl,
    ) -> ScratchpadResult<()> {
        let top_level = self.stack.is_empty();
        if let Token::Comment(text) = token {
            if top_level && is_doc_comment(text) {
                let _kept = self.doc_start.get_or_insert(position);
            }
            return Ok(());
        }
        if let Token::LexError(error) = token {
            return Err(internal(&format!("the lexer failed: {error}")));
        }
        if top_level && matches!(token, Token::Private) {
            self.item_start = Some(self.doc_start.take().unwrap_or(position));
        } else if top_level && matches!(token, Token::Fn) {
            self.start_function(position);
        } else if matches!(token, Token::LeftBrace | Token::MapStart) {
            self.stack.push(after_interp);
            self.reset_item();
        } else if matches!(token, Token::RightBrace) {
            self.pop_brace(control)?;
            self.reset_item();
        } else {
            self.reset_item();
        }
        Ok(())
    }

    /// Starts the function whose `fn` keyword is at `fn_at`, starting its text at the documentation or `private` before it.
    fn start_function(&mut self, fn_at: Position) {
        let start = self
            .item_start
            .take()
            .or_else(|| self.doc_start.take())
            .unwrap_or(fn_at);
        self.pending = Some(Pending {
            name: None,
            arity: 0,
            start,
            fn_at,
            phase: Phase::Name,
        });
    }

    /// Forgets the documentation and the `private` keyword that precede no item.
    const fn reset_item(&mut self) {
        self.doc_start = None;
        self.item_start = None;
    }

    /// Closes the innermost brace; a brace of an interpolation hands the lexer back to the text of its string.
    fn pop_brace(&mut self, control: &TokenizerControl) -> ScratchpadResult<()> {
        let interpolation = self.stack.pop().ok_or_else(|| internal("unbalanced brace"))?;
        if interpolation {
            control.borrow_mut().is_within_text = true;
        }
        Ok(())
    }

    /// Reads one token of the header or the body of the function being read.
    fn step_function(
        &mut self,
        token: &Token,
        position: Position,
        after_interp: bool,
        control: &TokenizerControl,
    ) -> ScratchpadResult<()> {
        let mut pending = self
            .pending
            .take()
            .ok_or_else(|| internal("no function is being read"))?;
        match pending.phase {
            Phase::Name => {
                let Token::Identifier(name) = token else {
                    return Err(unexpected());
                };
                pending.name = Some(name.to_string());
                pending.phase = Phase::AfterName;
            }
            Phase::AfterName => {
                if matches!(token, Token::Period) {
                    return Err(typed_method(pending.fn_at));
                }
                if matches!(token, Token::Unit) {
                    pending.phase = Phase::BeforeBody;
                } else if matches!(token, Token::LeftParen) {
                    pending.phase = Phase::Params;
                } else {
                    return Err(unexpected());
                }
            }
            Phase::Params => {
                if matches!(token, Token::Identifier(_)) {
                    pending.arity = pending.arity.saturating_add(1);
                } else if matches!(token, Token::RightParen) {
                    pending.phase = Phase::BeforeBody;
                } else if !matches!(token, Token::Comma) {
                    return Err(unexpected());
                }
            }
            Phase::BeforeBody => {
                if !matches!(token, Token::LeftBrace) {
                    return Err(unexpected());
                }
                self.stack.push(false);
                pending.phase = Phase::Body;
            }
            Phase::Body => {
                if matches!(token, Token::LeftBrace | Token::MapStart) {
                    self.stack.push(after_interp);
                } else if matches!(token, Token::RightBrace) && self.closes_body(control)? {
                    return self.record(pending, position);
                }
            }
        }
        self.pending = Some(pending);
        Ok(())
    }

    /// Closes a brace of the body, and tells whether it was the closing brace of the body itself.
    fn closes_body(&mut self, control: &TokenizerControl) -> ScratchpadResult<bool> {
        self.pop_brace(control)?;
        Ok(self.stack.is_empty())
    }

    /// Records a function whose closing brace is at `close`.
    fn record(&mut self, pending: Pending, close: Position) -> ScratchpadResult<()> {
        let name = pending.name.ok_or_else(|| internal("a function has no name"))?;
        self.found.push(RawFunction {
            key: key_of(&name, pending.arity),
            start: pending.start,
            close,
        });
        Ok(())
    }

    /// Ends the scan: no function may be open and every brace must be closed.
    fn finish(self) -> ScratchpadResult<Vec<RawFunction>> {
        if self.pending.is_some() || !self.stack.is_empty() {
            return Err(internal("the script ends inside a function"));
        }
        Ok(self.found)
    }
}

/// Scans the tokens of a script that compiled, and returns its functions in the order of definition.
fn scan(rhai: &RhaiEngine, script: &str, index: &LineIndex) -> ScratchpadResult<Vec<Found>> {
    let inputs = [script];
    let (tokens, control) = rhai.lex(&inputs);
    let mut scanner = Scanner::default();
    for (token, position) in tokens {
        if matches!(token, Token::EOF) {
            break;
        }
        scanner.step(&token, position, &control)?;
    }
    scanner
        .finish()?
        .into_iter()
        .map(|function| -> ScratchpadResult<Found> {
            let start = offset_of(index, script, function.start)?;
            let close = offset_of(index, script, function.close)?;
            let end = close
                .checked_add(1)
                .ok_or_else(|| internal("a function ends past the script"))?;
            Ok(Found {
                key: function.key,
                start,
                end,
                close,
            })
        })
        .collect()
}

/// Checks that the functions read by the scanner are the functions of the AST: the same keys, and for each key the same
/// closing brace. Closures, which Rhai names `anon$…`, are not functions of the script.
fn check_against_ast(ast: &AST, script: &str, index: &LineIndex, found: &[Found]) -> ScratchpadResult<()> {
    let mut expected: BTreeMap<String, usize> = BTreeMap::new();
    for definition in ast.iter_fn_def() {
        if is_closure(&definition.name) {
            continue;
        }
        let close = offset_of(index, script, definition.body.end_position())?;
        let _replaced = expected.insert(key_of(&definition.name, definition.params.len()), close);
    }
    let mut extracted: BTreeMap<String, usize> = BTreeMap::new();
    for function in found {
        let _replaced = extracted.insert(function.key.clone(), function.close);
    }
    if expected != extracted {
        return Err(internal("the functions of the script and their sources disagree"));
    }
    Ok(())
}

/// Whether the AST defines exactly the function of the key, not a method on a type.
fn defines_only(ast: &AST, key: &str) -> bool {
    let Some((name, arity)) = split_key(key) else {
        return false;
    };
    let mut definitions = ast.iter_functions().filter(|function| !is_closure(function.name));
    match (definitions.next(), definitions.next()) {
        (Some(function), None) => {
            function.name == name && function.params.len() == arity && function.this_type.is_none()
        }
        _ => false,
    }
}

/// Records each call of the AST whose name is not known, with the function key and the line of the call.
fn collect_unknown_calls(ast: &AST, key: &str, known: &BTreeSet<String>, warnings: &mut Vec<RestoreWarning>) {
    let _complete = ast.walk(&mut |path: &[ASTNode<'_>]| {
        if let Some((call, position)) = path.last().and_then(call_of) {
            let name = qualified_name(call);
            // An operator (`+`, `==`, `in`…) is marked by Rhai itself: it is not a call by name.
            let operator = call.op_token.is_some();
            if !operator && !known.contains(&name) && !RHAI_KEYWORDS.contains(&call.name.as_str()) {
                warnings.push(RestoreWarning {
                    function: key.to_owned(),
                    call: name,
                    line: position.line(),
                });
            }
        }
        true
    });
}

/// The call of a node of the AST, when it is one: a statement call, a function call or a method call.
fn call_of<'node>(node: &'node ASTNode<'_>) -> Option<(&'node FnCallExpr, Position)> {
    if let ASTNode::Stmt(Stmt::FnCall(call, position)) = node {
        return Some((call.as_ref(), *position));
    }
    if let ASTNode::Expr(Expr::FnCall(call, position) | Expr::MethodCall(call, position)) = node {
        return Some((call.as_ref(), *position));
    }
    None
}

/// The name of a call, qualified by its module path when it has one: `grafana::loki_query`.
fn qualified_name(call: &FnCallExpr) -> String {
    let mut name = String::new();
    for segment in &call.namespace.path {
        name.push_str(segment.name.as_str());
        name.push_str("::");
    }
    name.push_str(call.name.as_str());
    name
}

/// Whether a name is the name of a closure, which Rhai gives to every anonymous function.
fn is_closure(name: &str) -> bool {
    name.starts_with("anon$")
}

/// Whether a comment is a documentation comment: `///` (but not `////`) or `/**` (but not `/***`).
fn is_doc_comment(text: &str) -> bool {
    (text.starts_with("///") && !text.starts_with("////"))
        || (text.starts_with("/**") && !text.starts_with("/***"))
}

/// The key of a function: its name and its number of parameters, such as `scale/2`.
fn key_of(name: &str, arity: usize) -> String {
    format!("{name}/{arity}")
}

/// The name and the arity of a key, or nothing when the key is not one.
fn split_key(key: &str) -> Option<(&str, usize)> {
    let (name, arity) = key.rsplit_once('/')?;
    Some((name, arity.parse::<usize>().ok()?))
}

/// Offsets in bytes of the start of each line of a script. Rhai numbers lines and columns from one.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(script: &str) -> Self {
        let mut starts = vec![0];
        for (byte, character) in script.char_indices() {
            if character == '\n' {
                starts.push(byte.saturating_add(1));
            }
        }
        Self { starts }
    }

    /// The byte offset of a position, a column being a count of characters. Nothing when the position is outside the
    /// script.
    fn offset(&self, script: &str, position: Position) -> Option<usize> {
        let line = position.line()?;
        let column = position.position()?;
        let start = *self.starts.get(line.checked_sub(1)?)?;
        let text = script.get(start..)?;
        let mut current = 1usize;
        for (byte, _character) in text.char_indices() {
            if current == column {
                return start.checked_add(byte);
            }
            current = current.checked_add(1)?;
        }
        (current == column).then_some(script.len())
    }
}

/// The byte offset of a position of the script. Rhai saturates its positions at [`SATURATED_POSITION`]: such a
/// position is refused rather than located wrongly.
fn offset_of(index: &LineIndex, script: &str, position: Position) -> ScratchpadResult<usize> {
    let (Some(line), Some(column)) = (position.line(), position.position()) else {
        return Err(internal("a position of the script is missing"));
    };
    if line >= SATURATED_POSITION || column >= SATURATED_POSITION {
        return Err(ScratchpadError::Limit {
            kind: LimitKind::Other,
            message: "a line or a column reaches 65 535 characters, so the function cannot be located"
                .to_owned(),
            position: ScriptPosition::new(line, column),
        });
    }
    index
        .offset(script, position)
        .ok_or_else(|| internal("a position lies outside the script"))
}

/// The scratchpad error of a script that does not compile.
fn compile_error(error: ParseError) -> ScratchpadError {
    let ParseError(kind, position) = error;
    ScratchpadError::from_rhai(&EvalAltResult::ErrorParsing(*kind, position))
}

/// The refusal of a method on a type, at the position of its `fn`.
fn typed_method(fn_at: Position) -> ScratchpadError {
    ScratchpadError::Syntax {
        message: "a method on a type (`fn int.double()`) cannot be persisted; define a plain `fn` instead"
            .to_owned(),
        position: ScriptPosition::new(fn_at.line().unwrap_or(0), fn_at.position().unwrap_or(0)),
    }
}

/// The refusal of a header that is not a function header: the scanner and Rhai disagree, which is a defect of the crate.
fn unexpected() -> ScratchpadError {
    internal("the header of a function cannot be read")
}

/// An internal failure of the persistence.
fn internal(message: &str) -> ScratchpadError {
    ScratchpadError::Internal {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rhai::{AST, Blob, Dynamic, EvalAltResult, Module, Scope, Shared};
    use std::cell::Cell;

    use crate::clock::Timestamp;
    use crate::session::{AgentRef, SessionId, SessionState};

    use super::{
        Found, LineIndex, PersistWarning, RestoreWarning, UnpersistableReason, check_against_ast,
        extract_function_sources, extract_verified, recompile_functions, restore_scope, snapshot_scope,
        to_dynamic, to_tagged,
    };
    use crate::scratchpad::{
        GrantedModule, Grants, LimitKind, ScratchpadEngine, ScratchpadError, ScratchpadLimits,
        ScratchpadResult,
    };
    use crate::session::TaggedValue;

    fn engine() -> ScratchpadEngine {
        ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .build()
            .expect("an engine without grants builds")
    }

    /// An engine with a `tick` function that increments the returned counter.
    fn counting_engine(counter: &Arc<AtomicUsize>) -> ScratchpadEngine {
        let counter = Arc::clone(counter);
        ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .with_registration(move |engine| {
                let _tick = engine.register_fn("tick", move || {
                    let _previous = counter.fetch_add(1, Ordering::SeqCst);
                });
                Ok(())
            })
            .build()
            .expect("the counting engine builds")
    }

    fn run(engine: &ScratchpadEngine, scope: &mut Scope<'_>, script: &str) -> Dynamic {
        engine
            .rhai()
            .eval_with_scope::<Dynamic>(scope, script)
            .expect("the test script runs")
    }

    fn scope_of(engine: &ScratchpadEngine, script: &str) -> Scope<'static> {
        let mut scope = Scope::new();
        let _value = run(engine, &mut scope, script);
        scope
    }

    fn warning_names(warnings: &[PersistWarning]) -> Vec<&str> {
        warnings.iter().map(PersistWarning::variable).collect()
    }

    fn sources(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, text)| ((*key).to_owned(), (*text).to_owned()))
            .collect()
    }

    fn keys(functions: &[super::FunctionSource]) -> Vec<&str> {
        functions.iter().map(super::FunctionSource::key).collect()
    }

    fn nested_arrays(levels: usize) -> Dynamic {
        let mut value = Dynamic::from(1i64);
        for _ in 0..levels {
            value = Dynamic::from_array(vec![value]);
        }
        value
    }

    #[test]
    fn variable_types_survive_round_trip() {
        let engine = engine();
        let mut scope = Scope::new();
        let _char = scope.push("x", 'x');
        let _blob = scope.push("b", Blob::from([1u8, 2, 255]));
        let _int = scope.push("i", 7i64);
        let _float = scope.push("f", 7.0f64);
        let _string = scope.push("s", "x".to_owned());
        let _map = run(
            &engine,
            &mut scope,
            "let m = #{list: [1, 'c', `t`], nested: #{ok: true}}; m",
        );
        let snapshot = snapshot_scope(&scope);
        assert!(
            snapshot.warnings().is_empty(),
            "no variable of this scope is excluded"
        );
        let restored = restore_scope(snapshot.variables());

        assert_eq!(
            restored.get_value::<char>("x"),
            Some('x'),
            "the char `x` must come back as the char `x`"
        );
        assert_eq!(
            restored.get_value::<Blob>("b"),
            Some(vec![1, 2, 255]),
            "the blob must come back as a blob"
        );
        assert_eq!(
            restored.get_value::<i64>("i"),
            Some(7),
            "the integer must come back as an integer"
        );
        assert_eq!(
            restored.get_value::<f64>("f"),
            Some(7.0),
            "the float must come back as a float"
        );
        assert_eq!(
            restored.get_value::<String>("s").as_deref(),
            Some("x"),
            "the string must come back as a string"
        );
        let original = scope.get_value::<Dynamic>("m").expect("the map is in the scope");
        let back = restored.get_value::<Dynamic>("m").expect("the map is restored");
        assert_eq!(
            to_tagged(&back),
            to_tagged(&original),
            "the map holding an array must come back equal"
        );
    }

    #[test]
    fn closure_is_excluded_with_warning() {
        let engine = engine();
        let scope = scope_of(&engine, "let n = 3; let f = |x| x + 1;");
        let snapshot = snapshot_scope(&scope);
        assert!(
            snapshot.variables().contains_key("n"),
            "the plain variable `n` is persisted"
        );
        assert!(
            !snapshot.variables().contains_key("f"),
            "the closure `f` is not persisted"
        );
        assert_eq!(
            warning_names(snapshot.warnings()),
            vec!["f"],
            "exactly one warning names `f`"
        );
        assert_eq!(
            snapshot.warnings()[0].message(),
            "warning: variable `f` holds a closure and was not persisted; define a `fn` instead.",
            "the warning says closure and suggests a fn"
        );
    }

    #[test]
    fn function_pointer_is_excluded_with_warning() {
        let engine = engine();
        let scope = scope_of(&engine, "fn foo() { 1 } let p = Fn(\"foo\");");
        let snapshot = snapshot_scope(&scope);
        assert_eq!(
            warning_names(snapshot.warnings()),
            vec!["p"],
            "the warning names `p`"
        );
        assert!(
            snapshot.warnings()[0]
                .message()
                .contains("holds a function pointer"),
            "the warning says function pointer, got {}",
            snapshot.warnings()[0].message()
        );
        assert!(
            !snapshot.variables().contains_key("p"),
            "the pointer is not persisted"
        );
    }

    #[test]
    fn closure_nested_in_container_excludes_the_whole_variable() {
        let engine = engine();
        let scope = scope_of(&engine, "let m = #{ok: 1, cb: |x| x}; let a = [1, |x| x];");
        let snapshot = snapshot_scope(&scope);
        assert!(
            !snapshot.variables().contains_key("m"),
            "the table holding a closure is not persisted"
        );
        assert!(
            !snapshot.variables().contains_key("a"),
            "the array holding a closure is not persisted"
        );
        assert_eq!(
            warning_names(snapshot.warnings()),
            vec!["a", "m"],
            "each variable has its warning, by name"
        );
    }

    #[test]
    fn captured_variable_stays_persistable() {
        let engine = engine();
        let scope = scope_of(&engine, "let k = 5; let g = |x| x + k;");
        let snapshot = snapshot_scope(&scope);
        assert_eq!(
            snapshot.variables().get("k"),
            Some(&TaggedValue::Int(5)),
            "`k` is persisted with 5"
        );
        assert_eq!(
            warning_names(snapshot.warnings()),
            vec!["g"],
            "only the closure `g` is excluded"
        );
    }

    #[test]
    fn unsupported_and_non_finite_values_are_excluded() {
        let engine = engine();
        let mut scope = scope_of(&engine, "let t = timestamp();");
        let _nan = scope.push("nan", f64::NAN);
        let snapshot = snapshot_scope(&scope);
        assert_eq!(
            warning_names(snapshot.warnings()),
            vec!["nan", "t"],
            "both are excluded"
        );
        let instant = snapshot
            .warnings()
            .iter()
            .find(|warning| warning.variable() == "t");
        assert!(
            matches!(
                instant.map(PersistWarning::reason),
                Some(UnpersistableReason::UnsupportedType(_))
            ),
            "the instant is an unsupported type"
        );
        let nan = snapshot
            .warnings()
            .iter()
            .find(|warning| warning.variable() == "nan");
        assert_eq!(
            nan.map(PersistWarning::message).as_deref(),
            Some("warning: variable `nan` holds a non-finite number and was not persisted."),
            "the NaN has its own warning text"
        );
    }

    #[test]
    fn warnings_are_ordered_by_variable_name() {
        let engine = engine();
        let scope = scope_of(&engine, "let b = |x| x; let a = |x| x;");
        let snapshot = snapshot_scope(&scope);
        assert_eq!(
            warning_names(snapshot.warnings()),
            vec!["a", "b"],
            "the warnings come in name order"
        );
    }

    #[test]
    fn excluded_variable_stays_usable_until_the_session_ends() {
        let engine = engine();
        let defining = engine
            .rhai()
            .compile("let f = |x| x + 1;")
            .expect("the definition compiles");
        let mut scope = Scope::new();
        let _definition = engine
            .rhai()
            .eval_ast_with_scope::<Dynamic>(&mut scope, &defining)
            .expect("the definition runs");
        let snapshot = snapshot_scope(&scope);
        assert_eq!(
            warning_names(snapshot.warnings()),
            vec!["f"],
            "the closure is excluded"
        );
        assert!(scope.contains("f"), "the live scope still holds the closure");
        let call = engine.rhai().compile("f.call(1)").expect("the call compiles");
        let full = defining.merge(&call);
        let value: i64 = engine
            .rhai()
            .eval_ast_with_scope(&mut scope, &full)
            .expect("the live closure still runs after the snapshot");
        assert_eq!(value, 2, "the closure still computes 2");
    }

    #[test]
    fn restore_keeps_the_rest_of_the_scope() {
        let engine = engine();
        let scope = scope_of(&engine, "let f = |x| x; let a = 1; let b = 2; let c = 3;");
        let restored = restore_scope(snapshot_scope(&scope).variables());
        assert!(
            restored.get_value::<Dynamic>("f").is_none(),
            "the closure variable is absent"
        );
        assert_eq!(restored.get_value::<i64>("a"), Some(1), "`a` is restored");
        assert_eq!(restored.get_value::<i64>("b"), Some(2), "`b` is restored");
        assert_eq!(restored.get_value::<i64>("c"), Some(3), "`c` is restored");
    }

    #[test]
    fn data_at_the_depth_limit_round_trips_through_the_session_state() {
        let limit = nested_arrays(60);
        let tagged = to_tagged(&limit).expect("the deepest persisted value is accepted");
        let mut state = SessionState::new(
            SessionId::new("depth-test").expect("the identifier is valid"),
            AgentRef::new("agent", "fingerprint"),
            "context",
            BTreeMap::new(),
            Timestamp::from_millis(0),
        );
        let _previous = state.variables_mut().insert("deep".to_owned(), tagged.clone());
        let text = state.to_json().expect("the state serializes");
        let restored = SessionState::from_json(&text).expect("the state reads back at the depth limit");
        assert_eq!(
            restored.variables().get("deep"),
            Some(&tagged),
            "the value at the depth limit comes back equal through to_json and from_json"
        );
    }

    #[test]
    fn data_one_level_beyond_the_limit_is_excluded() {
        let beyond = nested_arrays(61);
        assert_eq!(
            to_tagged(&beyond),
            Err(UnpersistableReason::TooDeep),
            "61 nested containers are refused"
        );
        let mut scope = Scope::new();
        let _deep = scope.push("deep", beyond);
        let snapshot = snapshot_scope(&scope);
        assert!(
            !snapshot.variables().contains_key("deep"),
            "the variable is not persisted"
        );
        assert_eq!(
            snapshot.warnings().first().map(PersistWarning::reason),
            Some(&UnpersistableReason::TooDeep),
            "the reason is the depth"
        );
        assert_eq!(
            snapshot
                .warnings()
                .first()
                .map(PersistWarning::message)
                .as_deref(),
            Some("warning: variable `deep` nests deeper than 60 levels and was not persisted."),
            "the text names the limit of 60 levels"
        );
    }

    #[test]
    fn unsupported_type_message_names_the_type() {
        let engine = engine();
        let scope = scope_of(&engine, "let t = timestamp();");
        let snapshot = snapshot_scope(&scope);
        assert_eq!(
            snapshot
                .warnings()
                .first()
                .map(PersistWarning::message)
                .as_deref(),
            Some(
                "warning: variable `t` holds a value of type `timestamp` that cannot be persisted and was not persisted."
            ),
            "the text names the Rhai type of the value"
        );
        assert_eq!(
            snapshot.warnings().first().map(PersistWarning::reason),
            Some(&UnpersistableReason::UnsupportedType("timestamp".to_owned())),
            "the reason carries the type name"
        );
    }

    #[test]
    fn position_column_bounds_are_65534_accepted_and_65535_refused() {
        let engine = engine();
        // `fn g() { "xxx" }`: the closing brace sits at column k + 13.
        let at = |column: usize| format!("fn g() {{ \"{}\" }}", "x".repeat(column.saturating_sub(13)));
        assert!(
            extract_function_sources(&engine, &at(65_534)).is_ok(),
            "a closing brace at column 65 534 is located"
        );
        assert!(
            matches!(
                extract_function_sources(&engine, &at(65_535)),
                Err(ScratchpadError::Limit {
                    kind: LimitKind::Other,
                    ..
                })
            ),
            "a closing brace at column 65 535 is refused"
        );
    }

    #[test]
    fn position_line_bounds_are_65534_accepted_and_65535_refused() {
        let engine = engine();
        let at = |line: usize| format!("{}fn g() {{ 1 }}", "\n".repeat(line.saturating_sub(1)));
        assert!(
            extract_function_sources(&engine, &at(65_534)).is_ok(),
            "line 65 534 is located"
        );
        assert!(
            matches!(
                extract_function_sources(&engine, &at(65_535)),
                Err(ScratchpadError::Limit {
                    kind: LimitKind::Other,
                    ..
                })
            ),
            "line 65 535 is refused"
        );
    }

    #[test]
    fn a_tab_is_one_column() {
        let engine = engine();
        let script = "\t\tfn g() { 1 }";
        let functions = extract_function_sources(&engine, script).expect("the script extracts");
        assert_eq!(
            functions[0].text(),
            "fn g() { 1 }",
            "the text starts at the keyword"
        );
        let index = LineIndex::new(script);
        assert_eq!(
            index.offset(script, rhai::Position::new(1, 3)),
            Some(2),
            "column 3 is the keyword after two tabs"
        );
    }

    #[test]
    fn standard_surface_is_not_reported_at_restore() {
        let engine = engine();
        let source =
            "fn f(v) { let n = v.len() + 1 * 2 == 3; print(n); let m = v.map(|x| x + 1); to_string(n) }";
        let recompiled = recompile_functions(&engine, &sources(&[("f/1", source)]));
        assert!(recompiled.broken().is_empty(), "the source is not broken");
        assert!(
            recompiled.warnings().is_empty(),
            "operators, methods, print, to_string and closures are not reported, got {:?}",
            recompiled.warnings()
        );
    }

    #[test]
    fn operators_are_not_reported_even_when_no_native_has_their_name() {
        let engine = engine();
        let source =
            "fn f(a, b) { let c = a in b; let d = !a; let e = a ?? b; let g = a && b || !b; [c, d, e, g] }";
        let recompiled = recompile_functions(&engine, &sources(&[("f/2", source)]));
        assert!(recompiled.broken().is_empty(), "the source compiles");
        assert!(
            recompiled.warnings().is_empty(),
            "got {:?}",
            recompiled.warnings()
        );
    }

    #[test]
    fn a_real_unknown_call_gives_exactly_one_warning() {
        let engine = engine();
        let source = "fn f(v) { let n = v.len() + 1; ghost(n); v.map(|x| x + 1) }";
        let recompiled = recompile_functions(&engine, &sources(&[("f/1", source)]));
        assert_eq!(
            recompiled
                .warnings()
                .iter()
                .map(RestoreWarning::call)
                .collect::<Vec<_>>(),
            vec!["ghost"],
            "exactly one warning names the unknown call"
        );
    }

    #[test]
    fn the_self_check_is_run_by_the_extraction() {
        let engine = engine();
        let called = Cell::new(false);
        let refused = extract_verified(
            &engine,
            "fn f() { 1 }",
            |_ast: &AST, _script: &str, _index: &LineIndex, found: &[Found]| {
                called.set(true);
                assert_eq!(
                    found.len(),
                    1,
                    "the check receives the functions read by the scanner"
                );
                Err(ScratchpadError::Internal {
                    message: "refused by the check".to_owned(),
                })
            },
        );
        assert!(called.get(), "the extraction calls its self-check");
        assert!(
            matches!(refused, Err(ScratchpadError::Internal { .. })),
            "a failing check fails the extraction"
        );
        let accepted = extract_verified(
            &engine,
            "fn f() { 1 }",
            |_ast: &AST, _script: &str, _index: &LineIndex, _found: &[Found]| Ok(()),
        );
        assert_eq!(
            keys(&accepted.expect("a passing check keeps the functions")),
            vec!["f/0"]
        );
    }

    #[test]
    fn a_value_round_trips_through_dynamic() {
        let value = TaggedValue::Map(BTreeMap::from([
            ("c".to_owned(), TaggedValue::Char('x')),
            ("u".to_owned(), TaggedValue::Unit),
        ]));
        let back = to_tagged(&to_dynamic(&value));
        assert_eq!(back, Ok(value), "a map of a char and a unit comes back equal");
    }

    #[test]
    fn extraction_keeps_source_and_documentation() {
        let engine = engine();
        let functions = extract_function_sources(&engine, "/// Doubles a.\nfn f(a) { a * 2 }\nlet x = 3;")
            .expect("the script extracts");
        assert_eq!(keys(&functions), vec!["f/1"], "one source under the key `f/1`");
        assert_eq!(
            functions[0].text(),
            "/// Doubles a.\nfn f(a) { a * 2 }",
            "the text is exactly the two lines of the function with its comment"
        );
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_confuse_the_extraction() {
        let engine = engine();
        let script = "fn g() {\n  let s = \"}\";\n  // }\n  let m = #{k: 1};\n  s\n}\nlet after = 1;";
        let functions = extract_function_sources(&engine, script).expect("the script extracts");
        assert_eq!(keys(&functions), vec!["g/0"], "one function");
        assert_eq!(
            functions[0].text(),
            "fn g() {\n  let s = \"}\";\n  // }\n  let m = #{k: 1};\n  s\n}",
            "the source holds the whole function"
        );
    }

    #[test]
    fn two_functions_and_overloads() {
        let engine = engine();
        let functions = extract_function_sources(
            &engine,
            "fn f(a) { a }\nfn f(a, b) { a + b }\nprivate fn h() { 1 }",
        )
        .expect("the script extracts");
        assert_eq!(
            keys(&functions),
            vec!["f/1", "f/2", "h/0"],
            "the keys are f/1, f/2 and h/0"
        );
        assert_eq!(
            functions[2].text(),
            "private fn h() { 1 }",
            "the private function keeps its keyword"
        );
    }

    #[test]
    fn interpolation_and_closures_do_not_break_the_extraction() {
        let engine = engine();
        let script = "fn tpl(l) {\n  `${ if l > 0 { \"+\" } else { \"-\" } }${ l }`\n}\nfn after() { 2 }";
        let functions = extract_function_sources(&engine, script).expect("the script extracts");
        assert_eq!(
            keys(&functions),
            vec!["tpl/1", "after/0"],
            "both functions are found"
        );
        assert_eq!(
            functions[0].text(),
            "fn tpl(l) {\n  `${ if l > 0 { \"+\" } else { \"-\" } }${ l }`\n}",
            "the interpolated text is kept whole"
        );
        let closures =
            extract_function_sources(&engine, "fn g(v) { v.map(|x| { x + 1 }) }").expect("it extracts");
        assert_eq!(
            keys(&closures),
            vec!["g/1"],
            "the closure is not a function of the script"
        );
    }

    #[test]
    fn nested_function_is_a_syntax_error_of_rhai() {
        let engine = engine();
        let result = extract_function_sources(&engine, "if true { fn inner() { 1 } }");
        assert!(
            matches!(result, Err(ScratchpadError::Syntax { .. })),
            "Rhai refuses a function inside a block at compile time, got {result:?}"
        );
    }

    #[test]
    fn self_check_refuses_a_disagreement_between_scanner_and_ast() {
        let engine = engine();
        let script = "fn f(a) { a }";
        let ast = engine.rhai().compile(script).expect("the script compiles");
        let index = LineIndex::new(script);
        let agreeing = [Found {
            key: "f/1".to_owned(),
            start: 0,
            end: 13,
            close: 12,
        }];
        assert!(
            check_against_ast(&ast, script, &index, &agreeing).is_ok(),
            "a scanner that reads the AST's function passes the check"
        );
        let disagreeing = [Found {
            key: "g/1".to_owned(),
            start: 0,
            end: 13,
            close: 12,
        }];
        assert!(
            matches!(
                check_against_ast(&ast, script, &index, &disagreeing),
                Err(ScratchpadError::Internal { .. })
            ),
            "a scanner that reads another function is an internal failure"
        );
    }

    #[test]
    fn typed_method_is_refused_at_extraction() {
        let engine = engine();
        let result = extract_function_sources(&engine, "fn int.double() { this * 2 }");
        assert!(
            matches!(result, Err(ScratchpadError::Syntax { .. })),
            "a method on a type is a syntax error, got {result:?}"
        );
    }

    #[test]
    fn position_beyond_the_u16_limit_is_refused() {
        let engine = engine();
        let script = format!("let s = \"{}\"; fn lg() {{ 1 }}", "x".repeat(70_000));
        let result: ScratchpadResult<_> = extract_function_sources(&engine, &script);
        assert!(
            matches!(
                result,
                Err(ScratchpadError::Limit {
                    kind: LimitKind::Other,
                    ..
                })
            ),
            "a position at 65 535 or more is a limit error, got {result:?}"
        );
    }

    #[test]
    fn functions_reused_after_recompilation() {
        let engine = engine();
        let recompiled = recompile_functions(&engine, &sources(&[("f/1", "fn f(a) { a * 2 }")]));
        assert!(recompiled.broken().is_empty(), "nothing is broken");
        let mut scope = Scope::new();
        let _x = scope.push("x", 3i64);
        let call = engine.rhai().compile("f(x)").expect("the call compiles");
        let full = recompiled.ast().clone().merge(&call);
        let result: i64 = engine
            .rhai()
            .eval_ast_with_scope(&mut scope, &full)
            .expect("the restored function runs");
        assert_eq!(result, 6, "f(3) is 6");
    }

    #[test]
    fn top_level_statements_are_never_replayed() {
        let counter = Arc::new(AtomicUsize::new(0));
        let engine = counting_engine(&counter);
        let recompiled = recompile_functions(&engine, &sources(&[("f/0", "fn f() { 1 }\ntick();")]));
        assert!(
            recompiled
                .ast()
                .iter_functions()
                .any(|function| function.name == "f"),
            "f is restored"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "the counter is not incremented"
        );
    }

    #[test]
    fn source_that_no_longer_compiles_is_broken_alone() {
        let engine = engine();
        let recompiled = recompile_functions(
            &engine,
            &sources(&[("ok/0", "fn ok() { 1 }"), ("bad/0", "fn bad( { }")]),
        );
        assert_eq!(recompiled.broken(), ["bad/0"], "bad/0 is reported broken");
        assert!(
            recompiled
                .ast()
                .iter_functions()
                .any(|function| function.name == "ok"),
            "ok is kept"
        );
    }

    fn grafana_engine(with_loki_query: bool) -> ScratchpadEngine {
        let mut module = Module::new();
        if with_loki_query {
            let _native = module
                .set_native_fn("loki_query", |_text: &str| -> Result<i64, Box<EvalAltResult>> {
                    Ok(1)
                });
        }
        let granted = GrantedModule::new("grafana", Shared::new(module)).expect("the module name is valid");
        let grants = Grants::new().with_module(granted).expect("the module is granted");
        ScratchpadEngine::builder(ScratchpadLimits::new(), grants)
            .build()
            .expect("the engine builds")
    }

    #[test]
    fn vanished_toolset_api_is_a_warning_and_the_rest_is_kept() {
        let engine = grafana_engine(false);
        let recompiled = recompile_functions(
            &engine,
            &sources(&[
                ("list/0", "fn list() { grafana::loki_query(\"x\") }"),
                ("other/0", "fn other() { 1 }"),
            ]),
        );
        assert!(recompiled.broken().is_empty(), "nothing is broken");
        assert_eq!(
            recompiled
                .warnings()
                .iter()
                .map(|warning| (warning.function(), warning.call()))
                .collect::<Vec<_>>(),
            vec![("list/0", "grafana::loki_query")],
            "one warning names the call"
        );
        assert!(
            recompiled
                .ast()
                .iter_functions()
                .any(|function| function.name == "other"),
            "other is kept"
        );
    }

    #[test]
    fn still_valid_toolset_api_gives_no_warning() {
        let engine = grafana_engine(true);
        let recompiled = recompile_functions(
            &engine,
            &sources(&[("list/0", "fn list() { grafana::loki_query(\"x\") }")]),
        );
        assert!(recompiled.broken().is_empty(), "nothing is broken");
        assert!(recompiled.warnings().is_empty(), "no warning is given");
    }

    #[test]
    fn unknown_unqualified_call_is_a_warning_not_a_broken_source() {
        let engine = engine();
        let recompiled = recompile_functions(&engine, &sources(&[("g/0", "fn g() { nothing_here() }")]));
        assert!(recompiled.broken().is_empty(), "the source is not broken");
        assert_eq!(
            recompiled
                .warnings()
                .iter()
                .map(RestoreWarning::call)
                .collect::<Vec<_>>(),
            vec!["nothing_here"],
            "one warning names the call"
        );
    }

    #[test]
    fn unknown_call_is_reported_with_its_line() {
        let engine = engine();
        let recompiled = recompile_functions(&engine, &sources(&[("f/0", "fn f() {\n  1;\n  ghost(2)\n}")]));
        let warning = recompiled.warnings().first().expect("one warning");
        assert_eq!(warning.function(), "f/0", "the function is named by its key");
        assert_eq!(warning.call(), "ghost", "the call is named");
        assert_eq!(warning.line(), Some(3), "the line is 3");
    }

    #[test]
    fn source_defining_a_method_or_another_arity_is_broken() {
        let engine = engine();
        let recompiled = recompile_functions(
            &engine,
            &sources(&[
                ("f/2", "fn f(a) { a }"),
                ("double/0", "fn int.double() { this * 2 }"),
            ]),
        );
        assert_eq!(
            recompiled.broken(),
            ["double/0", "f/2"],
            "a wrong arity and a method are broken"
        );
    }

    #[test]
    fn a_rebound_name_keeps_its_last_value() {
        let mut scope = Scope::new();
        let _first = scope.push("a", 1i64);
        let _second = scope.push("a", "x".to_owned());
        let snapshot = snapshot_scope(&scope);
        assert_eq!(
            snapshot.variables().get("a"),
            Some(&TaggedValue::String("x".to_owned())),
            "the scope holds the last value of the name"
        );
    }

    #[test]
    fn source_defining_another_function_than_its_key_is_broken() {
        let engine = engine();
        let recompiled = recompile_functions(&engine, &sources(&[("f/1", "fn g(a) { a }")]));
        assert_eq!(recompiled.broken(), ["f/1"], "f/1 is broken");
    }
}
