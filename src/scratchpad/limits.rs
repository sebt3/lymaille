//! Limits of the scratchpad sandbox: memory limits of Rhai, call and expression depths, render cap. There is deliberately no
//! limit of duration nor of operations: a long script is legitimate, and the host stops a runaway loop by cancellation.

use std::num::NonZeroUsize;

/// Default maximum size of a string, in bytes of UTF-8 (16 mebibytes).
const DEFAULT_MAX_STRING_BYTES: usize = 16 * 1024 * 1024;
/// Default maximum number of elements of an array.
const DEFAULT_MAX_ARRAY_LEN: usize = 1_000_000;
/// Default maximum number of entries of a map.
const DEFAULT_MAX_MAP_LEN: usize = 100_000;
/// Default cap of the rendered output, in characters.
const DEFAULT_RENDER_CAP_CHARS: usize = 8_000;
/// Depth of nested expressions, fixed.
const MAX_EXPR_DEPTH: usize = 256;
/// Depth of nested expressions inside a function, fixed.
const MAX_FUNCTION_EXPR_DEPTH: usize = 128;
/// Levels of nested calls, fixed.
const MAX_CALL_LEVELS: usize = 512;

/// Limits of one session's scratchpad. Values are copyable and built only by their constructors; they are derived from the
/// agent definition at each start, never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScratchpadLimits {
    max_string_bytes: usize,
    max_array_len: usize,
    max_map_len: usize,
    render_cap_chars: usize,
}

impl Default for ScratchpadLimits {
    fn default() -> Self {
        Self::new()
    }
}

impl ScratchpadLimits {
    /// Limits with the default values: 16 MiB strings, 1 000 000 array elements, 100 000 map entries, 8 000 rendered
    /// characters.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_string_bytes: DEFAULT_MAX_STRING_BYTES,
            max_array_len: DEFAULT_MAX_ARRAY_LEN,
            max_map_len: DEFAULT_MAX_MAP_LEN,
            render_cap_chars: DEFAULT_RENDER_CAP_CHARS,
        }
    }

    /// Sets the maximum size of a string, in bytes. Returns nothing for zero: Rhai reads zero as unlimited.
    #[must_use]
    pub fn with_max_string_bytes(self, bytes: usize) -> Option<Self> {
        NonZeroUsize::new(bytes).map(|_| Self {
            max_string_bytes: bytes,
            ..self
        })
    }

    /// Sets the maximum number of elements of an array. Returns nothing for zero.
    #[must_use]
    pub fn with_max_array_len(self, len: usize) -> Option<Self> {
        NonZeroUsize::new(len).map(|_| Self {
            max_array_len: len,
            ..self
        })
    }

    /// Sets the maximum number of entries of a map. Returns nothing for zero.
    #[must_use]
    pub fn with_max_map_len(self, len: usize) -> Option<Self> {
        NonZeroUsize::new(len).map(|_| Self {
            max_map_len: len,
            ..self
        })
    }

    /// Sets the cap of the rendered output, in characters. Zero is accepted; a value above the platform's `usize` saturates.
    #[must_use]
    pub fn with_render_cap_chars(self, chars: u64) -> Self {
        Self {
            render_cap_chars: usize::try_from(chars).unwrap_or(usize::MAX),
            ..self
        }
    }

    /// Maximum size of a string, in bytes.
    #[must_use]
    pub const fn max_string_bytes(&self) -> usize {
        self.max_string_bytes
    }

    /// Maximum number of elements of an array.
    #[must_use]
    pub const fn max_array_len(&self) -> usize {
        self.max_array_len
    }

    /// Maximum number of entries of a map.
    #[must_use]
    pub const fn max_map_len(&self) -> usize {
        self.max_map_len
    }

    /// Cap of the rendered output, in characters.
    #[must_use]
    pub const fn render_cap_chars(&self) -> usize {
        self.render_cap_chars
    }

    /// Applies the memory and depth limits to an engine: string, array and map sizes, expression depths, call levels. Nothing
    /// else is set on the engine: no operation limit, no duration limit.
    pub fn apply_to(&self, engine: &mut rhai::Engine) {
        let _strings = engine.set_max_string_size(self.max_string_bytes);
        let _arrays = engine.set_max_array_size(self.max_array_len);
        let _maps = engine.set_max_map_size(self.max_map_len);
        let _expressions = engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_FUNCTION_EXPR_DEPTH);
        let _calls = engine.set_max_call_levels(MAX_CALL_LEVELS);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{MAX_CALL_LEVELS, MAX_EXPR_DEPTH, MAX_FUNCTION_EXPR_DEPTH, ScratchpadLimits};
    use crate::scratchpad::{LimitKind, ScratchpadError, ScriptErrorClass};
    use rhai::{Dynamic, Engine};

    fn engine_with(limits: &ScratchpadLimits) -> Engine {
        let mut engine = Engine::new();
        limits.apply_to(&mut engine);
        engine
    }

    #[test]
    fn defaults() {
        let limits = ScratchpadLimits::new();
        assert_eq!(limits.max_string_bytes(), 16_777_216, "string limit");
        assert_eq!(limits.max_array_len(), 1_000_000, "array limit");
        assert_eq!(limits.max_map_len(), 100_000, "map limit");
        assert_eq!(limits.render_cap_chars(), 8_000, "render cap");
    }

    #[test]
    fn overrides() {
        let limits = ScratchpadLimits::new()
            .with_max_string_bytes(1000)
            .expect("1000 is not zero")
            .with_render_cap_chars(500);
        assert_eq!(limits.max_string_bytes(), 1000, "string limit");
        assert_eq!(limits.render_cap_chars(), 500, "render cap");
        assert_eq!(limits.max_array_len(), 1_000_000, "array limit kept");
        assert_eq!(limits.max_map_len(), 100_000, "map limit kept");
    }

    #[test]
    fn zero_memory_limit_refused() {
        assert!(
            ScratchpadLimits::new().with_max_string_bytes(0).is_none(),
            "string"
        );
        assert!(ScratchpadLimits::new().with_max_array_len(0).is_none(), "array");
        assert!(ScratchpadLimits::new().with_max_map_len(0).is_none(), "map");
    }

    #[test]
    fn zero_render_cap_accepted() {
        let limits = ScratchpadLimits::new().with_render_cap_chars(0);
        assert_eq!(limits.render_cap_chars(), 0, "a cap of zero is reported");
    }

    #[test]
    fn render_cap_conversion_never_panics() {
        let limits = ScratchpadLimits::new().with_render_cap_chars(u64::MAX);
        assert_eq!(
            limits.render_cap_chars(),
            usize::try_from(u64::MAX).unwrap_or(usize::MAX),
            "the cap saturates to the largest usize"
        );
    }

    #[test]
    fn limits_are_applied_to_an_engine() {
        let engine = engine_with(&ScratchpadLimits::new());
        assert_eq!(engine.max_string_size(), 16_777_216, "string size");
        assert_eq!(engine.max_array_size(), 1_000_000, "array size");
        assert_eq!(engine.max_map_size(), 100_000, "map size");
        assert_eq!(engine.max_expr_depth(), MAX_EXPR_DEPTH, "expression depth");
        assert_eq!(
            engine.max_function_expr_depth(),
            MAX_FUNCTION_EXPR_DEPTH,
            "function expression depth"
        );
        assert_eq!(engine.max_call_levels(), MAX_CALL_LEVELS, "call levels");
    }

    #[test]
    fn no_operation_or_time_limit() {
        let engine = engine_with(&ScratchpadLimits::new());
        assert_eq!(engine.max_operations(), 0, "no maximum of operations is reported");
        let result = engine.eval::<i64>("let i = 0; while i < 5_000_000 { i += 1; } i");
        assert_eq!(
            result.ok(),
            Some(5_000_000),
            "a loop of five million iterations completes"
        );
    }

    #[test]
    fn string_over_the_limit_stops_the_script() {
        let limits = ScratchpadLimits::new()
            .with_max_string_bytes(1000)
            .expect("1000 is not zero");
        let engine = engine_with(&limits);
        let result = engine.eval::<Dynamic>("let s = \"\"; for i in 0..2000 { s += \"x\"; } s");
        match result {
            Err(error) => assert!(
                matches!(*error, rhai::EvalAltResult::ErrorDataTooLarge(..)),
                "expected a data-too-large error, got {error:?}"
            ),
            Ok(value) => panic!("the string must exceed the limit, it returned {value}"),
        }
    }

    #[test]
    fn native_result_over_the_limit_fails() {
        let limits = ScratchpadLimits::new()
            .with_max_string_bytes(1000)
            .expect("1000 is not zero");
        let mut engine = engine_with(&limits);
        let _registered = engine.register_fn("big", || "x".repeat(2000));
        let result = engine.eval::<Dynamic>("big()");
        match result {
            Err(error) => assert!(
                matches!(*error, rhai::EvalAltResult::ErrorDataTooLarge(..)),
                "expected a data-too-large error, got {error:?}"
            ),
            Ok(value) => panic!("the native result must be refused, it returned {value}"),
        }
    }

    #[test]
    fn call_depth_exceeded() {
        // The 512 levels of calls overflow the 2 MiB stack of a test thread in a debug build (open question in
        // limits.sdd): the evaluation runs on a thread with a larger stack, so that the test checks the limit itself.
        let outcome = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let engine = engine_with(&ScratchpadLimits::new());
                let result = engine.eval::<Dynamic>("fn r(n) { r(n + 1) } r(0)");
                match result {
                    Err(error) => assert!(
                        matches!(*error, rhai::EvalAltResult::ErrorStackOverflow(..)),
                        "expected a stack overflow error, got {error:?}"
                    ),
                    Ok(value) => panic!("the recursion must overflow, it returned {value}"),
                }
            })
            .expect("the evaluation thread starts");
        assert!(
            outcome.join().is_ok(),
            "the evaluation thread ends without a panic"
        );
    }

    #[test]
    fn limits_are_per_engine() {
        let small = ScratchpadLimits::new()
            .with_max_string_bytes(1000)
            .expect("1000 is not zero");
        let large = ScratchpadLimits::new()
            .with_max_string_bytes(1_048_576)
            .expect("1 MiB is not zero");
        let script = "let s = \"\"; for i in 0..100000 { s += \"x\"; } s.len()";
        let first = engine_with(&small).eval::<i64>(script);
        let second = engine_with(&large).eval::<i64>(script);
        let refused = match first {
            Ok(value) => panic!("the small engine must refuse the string, it returned {value}"),
            Err(error) => ScratchpadError::from_rhai(&error),
        };
        assert_eq!(
            refused.class(),
            ScriptErrorClass::Limit,
            "the small engine refuses with a limit, got {refused:?}"
        );
        assert!(
            matches!(
                refused,
                ScratchpadError::Limit {
                    kind: LimitKind::StringSize,
                    ..
                }
            ),
            "the refusal is the string size limit, got {refused:?}"
        );
        assert_eq!(second.ok(), Some(100_000), "the large engine accepts the string");
    }

    /// Blocks the thread for `ms` milliseconds, as a native function waiting on the outside world does.
    #[expect(
        clippy::disallowed_methods,
        reason = "test only: a native call that really blocks its thread, to check that no duration limit stops a script"
    )]
    fn block_the_thread(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    // Scenario: no time limit over two real minutes
    #[test]
    #[ignore = "two real minutes: run with cargo test -- --ignored"]
    fn no_time_limit_over_two_real_minutes() {
        let mut engine = engine_with(&ScratchpadLimits::new());
        let _nap = engine.register_fn("nap", |ms: i64| block_the_thread(u64::try_from(ms).unwrap_or(0)));
        #[expect(
            clippy::disallowed_methods,
            reason = "test only: measures the real duration of the script, which no limit of duration may cut"
        )]
        let started = std::time::Instant::now();
        let result = engine.eval::<i64>("let i = 0; while i < 5 { nap(25000); i += 1; } i");
        let elapsed = started.elapsed();
        assert_eq!(result.ok(), Some(5), "five naps of 25 seconds complete");
        assert_eq!(engine.max_operations(), 0, "no maximum of operations is reported");
        assert!(
            elapsed >= Duration::from_secs(120),
            "the script ran for two real minutes, it took {elapsed:?}"
        );
    }
}
