//! Typed error of the scratchpad module. Rhai errors are classified into the variants below, with their position, so that
//! the text rendered to the LLM names the cause and the line, and so that the audit counts them by class without keeping
//! their text.

use rhai::{Dynamic, EvalAltResult, ParseErrorType, Position};

/// Position of a script error, one-based: line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScriptPosition {
    line: usize,
    column: usize,
}

impl ScriptPosition {
    /// Builds a position from a line and a column, both starting at one. Returns nothing when either is zero.
    #[must_use]
    pub const fn new(line: usize, column: usize) -> Option<Self> {
        if line == 0 || column == 0 {
            return None;
        }
        Some(Self { line, column })
    }

    /// Line of the position, starting at one.
    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    /// Column of the position, starting at one.
    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }

    /// Converts a position of Rhai. Returns nothing when Rhai does not know the position.
    fn from_rhai(position: Position) -> Option<Self> {
        Self::new(position.line()?, position.position()?)
    }
}

/// Kind of a name that was not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotFoundKind {
    /// A function, named without its argument types.
    Function,
    /// A module.
    Module,
    /// A variable.
    Variable,
}

impl std::fmt::Display for NotFoundKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Function => "function",
            Self::Module => "module",
            Self::Variable => "variable",
        })
    }
}

/// Kind of a limit that was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitKind {
    /// Depth of the calls of script functions.
    CallDepth,
    /// Depth of nested expressions.
    ExpressionDepth,
    /// Size of a string.
    StringSize,
    /// Size of an array.
    ArraySize,
    /// Size of a map.
    MapSize,
    /// Any other limit (operations, modules, variables).
    Other,
}

impl std::fmt::Display for LimitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CallDepth => "call depth",
            Self::ExpressionDepth => "expression depth",
            Self::StringSize => "string size",
            Self::ArraySize => "array size",
            Self::MapSize => "map size",
            Self::Other => "other",
        })
    }
}

/// Class of a scratchpad error, for the quality signals and the audit: no text is kept with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScriptErrorClass {
    /// The script does not compile.
    Syntax,
    /// The script fails while it runs.
    Runtime,
    /// A function, a module or a variable is not found.
    NotFound,
    /// A limit of the sandbox is exceeded.
    Limit,
    /// The run was cancelled by the host.
    Cancelled,
    /// The engine cannot be built.
    Engine,
    /// An internal failure of the scratchpad.
    Internal,
}

/// Error of the scratchpad module. The texts are rendered to the LLM and name the cause and the position.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScratchpadError {
    /// The script does not compile. The position is kept when Rhai knows it.
    #[error("syntax error{}: {message}", location(.position.as_ref()))]
    Syntax {
        /// Message of Rhai, without the position.
        message: String,
        /// Position of the error, when known.
        position: Option<ScriptPosition>,
    },
    /// The script fails while it runs: `throw`, failing native function, wrong type of argument.
    #[error("runtime error{}: {message}", location(.position.as_ref()))]
    Runtime {
        /// Message of the error, without the position.
        message: String,
        /// Position of the error, when known.
        position: Option<ScriptPosition>,
    },
    /// A function, a module or a variable is not found.
    #[error("{kind} not found{}: {name}{}{}", location(.position.as_ref()), suggestions_text(.suggestions.as_slice()), hint_text(.hint.as_deref()))]
    NotFound {
        /// Kind of the name that is missing.
        kind: NotFoundKind,
        /// Name that was asked for, without the argument types of a function.
        name: String,
        /// Position of the call, when known.
        position: Option<ScriptPosition>,
        /// Name of the script function where the error occurred, when it occurred in one.
        function: Option<String>,
        /// Closest candidates, in order. Empty by default.
        suggestions: Vec<String>,
        /// Advice to the LLM on how to fix the call, when the cause is known (for example a scope variable read by a
        /// function). Absent by default.
        hint: Option<String>,
    },
    /// A limit of the sandbox is exceeded.
    #[error("limit exceeded ({kind}){}: {message}", location(.position.as_ref()))]
    Limit {
        /// Kind of the limit.
        kind: LimitKind,
        /// Message of the error, without the position.
        message: String,
        /// Position of the error, when known.
        position: Option<ScriptPosition>,
    },
    /// The run was interrupted by the cancellation of the host.
    #[error("script cancelled")]
    Cancelled,
    /// The inspection pad does not run what it reads: a function of a shell module, or a write to a variable of its
    /// target. Carries the name of the function. Its class is `Runtime`. See `inspect.rs`.
    #[error("{name} is not executed: the inspection does not run it")]
    InspectionOnly {
        /// Name of the function, `module::function`.
        name: String,
    },
    /// The engine cannot be built: a group that cannot be granted, a registration that fails.
    #[error("scratchpad engine failed: {message}")]
    Engine {
        /// Text of the failure.
        message: String,
    },
    /// An internal failure: a blocking task interrupted by a panic, an unrecoverable poisoned lock.
    #[error("scratchpad internal failure: {message}")]
    Internal {
        /// Text of the failure.
        message: String,
    },
}

/// Result of the scratchpad module.
pub type ScratchpadResult<T> = Result<T, ScratchpadError>;

/// Text of a position for a message: ` at line L, column C`, or nothing when the position is unknown.
fn location(position: Option<&ScriptPosition>) -> String {
    position.map_or_else(String::new, |position| {
        format!(" at line {}, column {}", position.line(), position.column())
    })
}

/// Text of the closest candidates: `. Closest candidates: a, b`, or nothing when there is none.
fn suggestions_text(suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        String::new()
    } else {
        format!(". Closest candidates: {}", suggestions.join(", "))
    }
}

/// Text of the hint: `. {hint}`, or nothing when there is none.
fn hint_text(hint: Option<&str>) -> String {
    hint.map_or_else(String::new, |hint| format!(". {hint}"))
}

impl ScratchpadError {
    /// Classifies an evaluation error of Rhai. Wrapper errors of function and module calls are unwrapped to their cause.
    #[must_use]
    pub fn from_rhai(error: &EvalAltResult) -> Self {
        classify(error)
    }

    /// Class of the error.
    #[must_use]
    pub const fn class(&self) -> ScriptErrorClass {
        match self {
            Self::Syntax { .. } => ScriptErrorClass::Syntax,
            Self::Runtime { .. } | Self::InspectionOnly { .. } => ScriptErrorClass::Runtime,
            Self::NotFound { .. } => ScriptErrorClass::NotFound,
            Self::Limit { .. } => ScriptErrorClass::Limit,
            Self::Cancelled => ScriptErrorClass::Cancelled,
            Self::Engine { .. } => ScriptErrorClass::Engine,
            Self::Internal { .. } => ScriptErrorClass::Internal,
        }
    }

    /// Adds the closest candidates to a not-found error. Has no effect on the other variants.
    #[must_use]
    pub fn with_suggestions(mut self, suggestions: Vec<String>) -> Self {
        if let Self::NotFound {
            suggestions: slot, ..
        } = &mut self
        {
            *slot = suggestions;
        }
        self
    }

    /// Adds a hint to a not-found error. Has no effect on the other variants.
    #[must_use]
    pub fn with_hint(mut self, hint: String) -> Self {
        if let Self::NotFound { hint: slot, .. } = &mut self {
            *slot = Some(hint);
        }
        self
    }

    /// Gives the position `fallback` to the error when it has none of its own.
    const fn or_position(mut self, fallback: Option<ScriptPosition>) -> Self {
        if let Self::Syntax { position, .. }
        | Self::Runtime { position, .. }
        | Self::NotFound { position, .. }
        | Self::Limit { position, .. } = &mut self
            && position.is_none()
        {
            *position = fallback;
        }
        self
    }

    /// Records that the error occurred in the script function `name`: the innermost function is kept for a not-found
    /// error, the others get the function in front of their message.
    fn within_function(mut self, name: &str) -> Self {
        if let Self::NotFound { function, .. } = &mut self {
            if function.is_none() {
                *function = Some(name.to_owned());
            }
        } else {
            self.prefix_message(&format!("in function {name}: "));
        }
        self
    }

    /// Records that the error occurred while loading the module `name`, in front of its message.
    fn within_module(mut self, name: &str) -> Self {
        self.prefix_message(&format!("in module {name}: "));
        self
    }

    /// Puts `prefix` in front of the message of an error that has one.
    fn prefix_message(&mut self, prefix: &str) {
        if let Self::Syntax { message, .. } | Self::Runtime { message, .. } | Self::Limit { message, .. } =
            self
        {
            message.insert_str(0, prefix);
        }
    }
}

/// Classifies one Rhai error. Wrapper errors of function and module calls are unwrapped: the cause decides the class, and
/// its own position wins over the position of the call.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "EvalAltResult is non_exhaustive in Rhai: a wildcard arm is required, and it is the Runtime fallback of the spec"
)]
fn classify(error: &EvalAltResult) -> ScratchpadError {
    match error {
        EvalAltResult::ErrorInFunctionCall(name, _source, inner, position) => classify(inner)
            .or_position(script_position(*position))
            .within_function(name),
        EvalAltResult::ErrorInModule(name, inner, position) => classify(inner)
            .or_position(script_position(*position))
            .within_module(name),
        EvalAltResult::ErrorParsing(kind, position) => parsing(kind, *position),
        EvalAltResult::ErrorFunctionNotFound(signature, position) => {
            let name = signature
                .split_once(" (")
                .map_or(signature.as_str(), |(name, _)| name);
            not_found(NotFoundKind::Function, name, *position)
        }
        EvalAltResult::ErrorModuleNotFound(name, position) => {
            not_found(NotFoundKind::Module, name, *position)
        }
        EvalAltResult::ErrorVariableNotFound(name, position) => {
            not_found(NotFoundKind::Variable, name, *position)
        }
        EvalAltResult::ErrorStackOverflow(position) => limit(LimitKind::CallDepth, error, *position),
        EvalAltResult::ErrorDataTooLarge(type_name, position) => {
            limit(data_kind(type_name), error, *position)
        }
        EvalAltResult::ErrorTooManyOperations(position)
        | EvalAltResult::ErrorTooManyVariables(position)
        | EvalAltResult::ErrorTooManyModules(position) => limit(LimitKind::Other, error, *position),
        EvalAltResult::ErrorTerminated(..) => ScratchpadError::Cancelled,
        // An inspection shell raises its refusal as a runtime error whose value is the refusal itself: the value is the
        // only way to tell it from a `throw` of the script, which cannot build one.
        EvalAltResult::ErrorRuntime(value, position) => value
            .clone()
            .try_cast::<ScratchpadError>()
            .unwrap_or_else(|| ScratchpadError::Runtime {
                message: runtime_message(value),
                position: script_position(*position),
            }),
        // `EvalAltResult` is `#[non_exhaustive]`: the wildcard arm is the `Runtime` fallback of the spec.
        _ => ScratchpadError::Runtime {
            message: leaf_message(error),
            position: script_position(error.position()),
        },
    }
}

/// Converts a Rhai position, when Rhai knows it.
fn script_position(position: Position) -> Option<ScriptPosition> {
    ScriptPosition::from_rhai(position)
}

/// Class of a syntax error: the expression depth limit of the parser is a limit, the other errors are syntax errors.
fn parsing(kind: &ParseErrorType, position: Position) -> ScratchpadError {
    let message = kind.to_string();
    if matches!(kind, ParseErrorType::ExprTooDeep) {
        ScratchpadError::Limit {
            kind: LimitKind::ExpressionDepth,
            message,
            position: script_position(position),
        }
    } else {
        ScratchpadError::Syntax {
            message,
            position: script_position(position),
        }
    }
}

/// Not-found error for `name`, without suggestions.
fn not_found(kind: NotFoundKind, name: &str, position: Position) -> ScratchpadError {
    ScratchpadError::NotFound {
        kind,
        name: name.to_owned(),
        position: script_position(position),
        function: None,
        suggestions: Vec::new(),
        hint: None,
    }
}

/// Limit error whose message is the text of Rhai without its position.
fn limit(kind: LimitKind, error: &EvalAltResult, position: Position) -> ScratchpadError {
    ScratchpadError::Limit {
        kind,
        message: leaf_message(error),
        position: script_position(position),
    }
}

/// Kind of a size limit from the type named by Rhai: `Length of string`, `Size of array/BLOB`, `Size of object map`.
fn data_kind(type_name: &str) -> LimitKind {
    if type_name.contains("string") {
        LimitKind::StringSize
    } else if type_name.contains("array") {
        LimitKind::ArraySize
    } else if type_name.contains("map") {
        LimitKind::MapSize
    } else {
        LimitKind::Other
    }
}

/// Message of a runtime error raised by `throw` or by a native function: the value thrown, or a generic text when empty.
fn runtime_message(value: &Dynamic) -> String {
    let text = value.to_string();
    if value.is_unit() || text.is_empty() {
        "no message was given".to_owned()
    } else {
        text
    }
}

/// Text of an error of Rhai without the position that Rhai appends to it.
fn leaf_message(error: &EvalAltResult) -> String {
    let text = error.to_string();
    let position = error.position();
    if position.is_none() {
        return text;
    }
    let suffix = format!(" ({position})");
    text.strip_suffix(&suffix)
        .map_or_else(|| text.clone(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{LimitKind, NotFoundKind, ScratchpadError, ScriptErrorClass, ScriptPosition};
    use rhai::{Dynamic, Engine, EvalAltResult, FuncRegistration, Module, ParseErrorType, Position};

    #[test]
    fn inspection_only_is_a_runtime_error_naming_its_function() {
        let error = ScratchpadError::InspectionOnly {
            name: "grafana::query".to_owned(),
        };
        assert_eq!(
            error.class(),
            ScriptErrorClass::Runtime,
            "an inspection refusal is a runtime error"
        );
        assert_eq!(
            error.to_string(),
            "grafana::query is not executed: the inspection does not run it",
            "the text names the function and says that the inspection does not run it"
        );
    }

    #[test]
    fn inspection_marker_travels_through_rhai_as_its_error() {
        let marker = ScratchpadError::InspectionOnly {
            name: "grafana::query".to_owned(),
        };
        let raised = EvalAltResult::ErrorRuntime(Dynamic::from(marker.clone()), Position::NONE);
        assert_eq!(
            ScratchpadError::from_rhai(&raised),
            marker,
            "a refusal raised by an inspection shell is classified back to its own error"
        );
    }

    fn position() -> Option<ScriptPosition> {
        ScriptPosition::new(3, 5)
    }

    /// Evaluates a script on an engine that has a module `grafana` exposing `loki_query`.
    fn evaluate(script: &str) -> EvalAltResult {
        let mut engine = Engine::new();
        let mut module = Module::new();
        let _hash = FuncRegistration::new("loki_query")
            .set_into_module(&mut module, |text: rhai::ImmutableString| text.to_string());
        let _engine = engine.register_static_module("grafana", module.into());
        match engine.eval::<Dynamic>(script) {
            Ok(value) => panic!("the script `{script}` must fail, it returned {value}"),
            Err(error) => *error,
        }
    }

    /// Error of a script that must fail on the engine of the test.
    fn failure(script: &str) -> EvalAltResult {
        let engine = Engine::new();
        match engine.eval::<Dynamic>(script) {
            Ok(value) => panic!("the script `{script}` must fail, it returned {value}"),
            Err(error) => *error,
        }
    }

    #[test]
    fn display_texts_are_stable() {
        let cases = [
            (
                ScratchpadError::Syntax {
                    message: "unexpected end".to_owned(),
                    position: position(),
                },
                "syntax error at line 3, column 5: unexpected end",
            ),
            (
                ScratchpadError::Runtime {
                    message: "boom".to_owned(),
                    position: position(),
                },
                "runtime error at line 3, column 5: boom",
            ),
            (
                ScratchpadError::NotFound {
                    kind: NotFoundKind::Function,
                    name: "grafana::loki_queryy".to_owned(),
                    position: position(),
                    function: None,
                    suggestions: Vec::new(),
                    hint: None,
                },
                "function not found at line 3, column 5: grafana::loki_queryy",
            ),
            (
                ScratchpadError::NotFound {
                    kind: NotFoundKind::Module,
                    name: "nomod".to_owned(),
                    position: position(),
                    function: None,
                    suggestions: vec!["grafana".to_owned(), "k8s".to_owned()],
                    hint: None,
                },
                "module not found at line 3, column 5: nomod. Closest candidates: grafana, k8s",
            ),
            (
                ScratchpadError::NotFound {
                    kind: NotFoundKind::Variable,
                    name: "unknown_var".to_owned(),
                    position: position(),
                    function: None,
                    suggestions: Vec::new(),
                    hint: None,
                },
                "variable not found at line 3, column 5: unknown_var",
            ),
            (
                ScratchpadError::Limit {
                    kind: LimitKind::CallDepth,
                    message: "stack overflow".to_owned(),
                    position: position(),
                },
                "limit exceeded (call depth) at line 3, column 5: stack overflow",
            ),
            (ScratchpadError::Cancelled, "script cancelled"),
            (
                ScratchpadError::Engine {
                    message: "group Fs".to_owned(),
                },
                "scratchpad engine failed: group Fs",
            ),
            (
                ScratchpadError::Internal {
                    message: "task panicked".to_owned(),
                },
                "scratchpad internal failure: task panicked",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected, "the text of {error:?}");
        }
    }

    #[test]
    fn hint_is_appended_after_the_suggestions() {
        let error = ScratchpadError::NotFound {
            kind: NotFoundKind::Variable,
            name: "x".to_owned(),
            position: position(),
            function: Some("f".to_owned()),
            suggestions: Vec::new(),
            hint: None,
        }
        .with_hint("functions cannot see scope variables: pass `x` as an argument".to_owned());
        assert_eq!(
            error.to_string(),
            "variable not found at line 3, column 5: x. functions cannot see scope variables: pass `x` as an argument",
            "the hint text"
        );
    }

    #[test]
    fn display_omits_unknown_position() {
        let syntax = ScratchpadError::Syntax {
            message: "unexpected end".to_owned(),
            position: None,
        };
        let limit = ScratchpadError::Limit {
            kind: LimitKind::StringSize,
            message: "too large".to_owned(),
            position: None,
        };
        assert_eq!(syntax.to_string(), "syntax error: unexpected end", "no location");
        assert_eq!(
            limit.to_string(),
            "limit exceeded (string size): too large",
            "no location"
        );
    }

    #[test]
    fn syntax_error_carries_its_position() {
        let error = failure("let x = ;");
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Syntax {
            position: Some(position),
            ..
        } = &classified
        else {
            panic!("expected a syntax error with a position on line 1, got {classified:?}");
        };
        assert_eq!(position.line(), 1, "the error is on line 1");
        assert_eq!(classified.class(), ScriptErrorClass::Syntax, "the class");
    }

    #[test]
    fn runtime_error_from_throw() {
        let error = failure("throw \"boom\"");
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Runtime { message, .. } = &classified else {
            panic!("expected a runtime error, got {classified:?}");
        };
        assert!(
            message.contains("boom"),
            "the message `{message}` carries the thrown text"
        );
    }

    #[test]
    fn function_not_found_without_argument_types() {
        let error = evaluate("grafana::loki_queryy(\"x\")");
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::NotFound { kind, name, .. } = &classified else {
            panic!("expected a function not found error, got {classified:?}");
        };
        assert_eq!(*kind, NotFoundKind::Function, "the kind");
        assert_eq!(name, "grafana::loki_queryy", "the name without argument types");
    }

    #[test]
    fn module_and_variable_not_found() {
        let module_error = ScratchpadError::from_rhai(&evaluate("nomod::f()"));
        let ScratchpadError::NotFound {
            kind: module_kind,
            name: module_name,
            ..
        } = &module_error
        else {
            panic!("expected a module not found error, got {module_error:?}");
        };
        assert_eq!(*module_kind, NotFoundKind::Module, "the kind of the module error");
        assert_eq!(module_name, "nomod", "the module name");
        let variable_error = ScratchpadError::from_rhai(&evaluate("unknown_var + 1"));
        let ScratchpadError::NotFound {
            kind: variable_kind,
            name: variable_name,
            ..
        } = &variable_error
        else {
            panic!("expected a variable not found error, got {variable_error:?}");
        };
        assert_eq!(
            *variable_kind,
            NotFoundKind::Variable,
            "the kind of the variable error"
        );
        assert_eq!(variable_name, "unknown_var", "the variable name");
    }

    #[test]
    fn call_depth_exceeded_is_a_limit() {
        let mut engine = Engine::new();
        let _limits = engine.set_max_call_levels(8);
        let error = match engine.eval::<Dynamic>("fn r(n) { r(n + 1) } r(0)") {
            Ok(value) => panic!("the recursion must overflow, it returned {value}"),
            Err(error) => *error,
        };
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Limit { kind, .. } = &classified else {
            panic!("expected a call depth limit, got {classified:?}");
        };
        assert_eq!(*kind, LimitKind::CallDepth, "the kind");
    }

    #[test]
    fn oversized_data_is_a_limit_of_the_matching_kind() {
        let mut engine = Engine::new();
        let _limits = engine.set_max_string_size(1000);
        let error = match engine.eval::<Dynamic>("let s = \"x\"; loop { s += s; }") {
            Ok(value) => panic!("the string must exceed the limit, it returned {value}"),
            Err(error) => *error,
        };
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Limit { kind, .. } = &classified else {
            panic!("expected a string size limit, got {classified:?}");
        };
        assert_eq!(*kind, LimitKind::StringSize, "the kind");
    }

    #[test]
    fn termination_is_a_cancellation() {
        let mut engine = Engine::new();
        let _hook = engine.on_progress(|_| Some(Dynamic::from("stop")));
        let error = match engine.eval::<Dynamic>("loop { }") {
            Ok(value) => panic!("the loop must be stopped, it returned {value}"),
            Err(error) => *error,
        };
        let classified = ScratchpadError::from_rhai(&error);
        assert_eq!(
            classified,
            ScratchpadError::Cancelled,
            "the termination is a cancellation"
        );
        assert_eq!(classified.class(), ScriptErrorClass::Cancelled, "the class");
    }

    #[test]
    fn inner_position_and_function_name_are_kept() {
        let script = "fn f() {\n    missing_fn()\n}\nf()";
        let error = failure(script);
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::NotFound {
            kind: NotFoundKind::Function,
            position: Some(position),
            function: Some(function),
            ..
        } = &classified
        else {
            panic!("expected a function not found error in `f`, got {classified:?}");
        };
        assert_eq!(
            position.line(),
            2,
            "the inner call is on line 2, not on the outer call"
        );
        assert_eq!(function, "f", "the function where the error occurred");
    }

    #[test]
    fn suggestions_are_appended() {
        let error = ScratchpadError::NotFound {
            kind: NotFoundKind::Function,
            name: "grafana::loki_queryy".to_owned(),
            position: None,
            function: None,
            suggestions: Vec::new(),
            hint: None,
        }
        .with_suggestions(vec!["grafana::loki_query".to_owned()]);
        assert!(
            error
                .to_string()
                .ends_with(". Closest candidates: grafana::loki_query"),
            "text: {error}"
        );
    }

    #[test]
    fn zero_position_refused() {
        assert_eq!(ScriptPosition::new(0, 5), None, "a zero line is refused");
        assert_eq!(ScriptPosition::new(3, 0), None, "a zero column is refused");
        assert!(ScriptPosition::new(1, 1).is_some(), "one and one are accepted");
    }

    #[test]
    fn each_listed_rhai_variant_is_classified() {
        let cases: [(EvalAltResult, ScriptErrorClass); 6] = [
            (
                EvalAltResult::ErrorParsing(ParseErrorType::UnexpectedEOF, Position::new(1, 9)),
                ScriptErrorClass::Syntax,
            ),
            (
                EvalAltResult::ErrorParsing(ParseErrorType::ExprTooDeep, Position::new(1, 2)),
                ScriptErrorClass::Limit,
            ),
            (
                EvalAltResult::ErrorDataTooLarge("Size of object map".to_owned(), Position::new(1, 2)),
                ScriptErrorClass::Limit,
            ),
            (
                EvalAltResult::ErrorTooManyOperations(Position::new(1, 2)),
                ScriptErrorClass::Limit,
            ),
            (
                EvalAltResult::ErrorStackOverflow(Position::new(1, 2)),
                ScriptErrorClass::Limit,
            ),
            (
                EvalAltResult::ErrorArithmetic("division by zero".to_owned(), Position::new(1, 2)),
                ScriptErrorClass::Runtime,
            ),
        ];
        for (error, expected) in cases {
            let classified = ScratchpadError::from_rhai(&error);
            assert_eq!(classified.class(), expected, "the class of variant {error:?}");
        }
    }

    #[test]
    fn map_size_limit_is_kept_per_type() {
        let error = EvalAltResult::ErrorDataTooLarge("Size of object map".to_owned(), Position::new(1, 2));
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Limit { kind, .. } = &classified else {
            panic!("expected a map size limit, got {classified:?}");
        };
        assert_eq!(*kind, LimitKind::MapSize, "the kind");
    }

    #[test]
    fn module_wrapper_is_unwrapped_to_its_cause() {
        let cause = EvalAltResult::ErrorRuntime(Dynamic::from("inside"), Position::new(4, 7));
        let wrapped =
            EvalAltResult::ErrorInModule("grafana".to_owned(), Box::new(cause), Position::new(1, 1));
        let classified = ScratchpadError::from_rhai(&wrapped);
        let ScratchpadError::Runtime {
            message,
            position: Some(position),
        } = &classified
        else {
            panic!("expected the runtime cause of the module, got {classified:?}");
        };
        assert!(
            message.contains("inside"),
            "the message `{message}` is the one of the cause"
        );
        assert_eq!(position.line(), 4, "the position of the cause");
    }

    #[test]
    fn too_many_variables_and_modules_are_other_limits() {
        for error in [
            EvalAltResult::ErrorTooManyVariables(Position::new(1, 2)),
            EvalAltResult::ErrorTooManyModules(Position::new(1, 2)),
        ] {
            let classified = ScratchpadError::from_rhai(&error);
            let ScratchpadError::Limit { kind, .. } = &classified else {
                panic!("expected a limit for {error:?}, got {classified:?}");
            };
            assert_eq!(*kind, LimitKind::Other, "the kind of {error:?}");
        }
    }

    #[test]
    fn array_size_is_an_array_limit() {
        let mut engine = Engine::new();
        let _limits = engine.set_max_array_size(10);
        let error = match engine.eval::<Dynamic>("let a = []; loop { a.push(1); }") {
            Ok(value) => panic!("the array must exceed the limit, it returned {value}"),
            Err(error) => *error,
        };
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Limit { kind, .. } = &classified else {
            panic!("expected an array size limit, got {classified:?}");
        };
        assert_eq!(*kind, LimitKind::ArraySize, "the kind");
    }

    // Scenario: un enveloppeur de module nomme le module devant le message
    #[test]
    fn module_wrapper_names_the_module_in_its_message() {
        let cause = EvalAltResult::ErrorRuntime(Dynamic::from("inside"), Position::new(4, 7));
        let wrapped =
            EvalAltResult::ErrorInModule("grafana".to_owned(), Box::new(cause), Position::new(1, 1));
        let classified = ScratchpadError::from_rhai(&wrapped);
        let ScratchpadError::Runtime { message, .. } = &classified else {
            panic!("expected the runtime cause of the module, got {classified:?}");
        };
        assert_eq!(
            message, "in module grafana: inside",
            "the module prefix, then the cause"
        );
    }

    // Scenario: la fonction la plus interne d'un appel imbriqué est conservée
    #[test]
    fn innermost_function_of_two_nested_levels_is_kept() {
        let script = "fn g() {\n    missing_fn()\n}\nfn f() {\n    g()\n}\nf()";
        let error = failure(script);
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::NotFound {
            function: Some(function),
            position: Some(position),
            ..
        } = &classified
        else {
            panic!("expected a function not found error in a function, got {classified:?}");
        };
        assert_eq!(function, "g", "the innermost function, not the outer one");
        assert_eq!(position.line(), 2, "the position of the inner call");
    }

    // Scenario: le message ne répète pas la position de l'erreur
    #[test]
    fn runtime_message_does_not_repeat_the_position() {
        let error = failure("let a = 1 / 0; a");
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Runtime { message, position } = &classified else {
            panic!("expected a runtime error for the division, got {classified:?}");
        };
        assert_eq!(
            message, "Division by zero: 1 / 0",
            "the message of Rhai, without the position it appends to its text"
        );
        assert!(
            !message.contains("position"),
            "no position in the message: {message}"
        );
        assert!(position.is_some(), "the position is kept apart from the message");
    }

    // Scenario: chaque genre de limite a son propre texte
    #[test]
    fn each_limit_kind_has_its_text() {
        let cases = [
            (LimitKind::CallDepth, "limit exceeded (call depth): m"),
            (LimitKind::ExpressionDepth, "limit exceeded (expression depth): m"),
            (LimitKind::StringSize, "limit exceeded (string size): m"),
            (LimitKind::ArraySize, "limit exceeded (array size): m"),
            (LimitKind::MapSize, "limit exceeded (map size): m"),
            (LimitKind::Other, "limit exceeded (other): m"),
        ];
        for (kind, expected) in cases {
            let error = ScratchpadError::Limit {
                kind,
                message: "m".to_owned(),
                position: None,
            };
            assert_eq!(error.to_string(), expected, "the text of {kind:?}");
        }
    }

    #[test]
    fn non_not_found_errors_in_a_function_name_the_function() {
        let error = failure("fn f() { throw \"boom\" } f()");
        let classified = ScratchpadError::from_rhai(&error);
        let ScratchpadError::Runtime { message, .. } = &classified else {
            panic!("expected a runtime error inside `f`, got {classified:?}");
        };
        assert!(
            message.starts_with("in function f: "),
            "the message `{message}` names the function `f` in front"
        );
        assert!(
            message.contains("boom"),
            "the message keeps the thrown text: {message}"
        );
    }
}
