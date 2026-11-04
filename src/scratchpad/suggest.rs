//! Closest candidates of a name that is not found, taken among the granted elements only: a function, a module or a variable
//! the script cannot reach is never suggested, even when it is the closest name. The suggestions turn a bare Rhai error into
//! something the LLM can act on, without revealing anything it was not granted.

use super::error::{NotFoundKind, ScratchpadError};

/// Most suggestions rendered for one error.
const MAX_SUGGESTIONS: usize = 3;
/// Largest distance of a suggestion, before the length of the requested name is taken into account.
const MAX_DISTANCE: usize = 3;
/// Longest name compared, in characters: longer names are cut, which keeps the distance computation bounded.
const MAX_NAME_CHARS: usize = 128;

/// Elements a not-found name can be compared with: the visible functions with their signatures, the granted module names
/// and the variables of the scope.
///
/// Usage lines can be attached on top of the function candidates with [`CandidateSet::with_usage`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CandidateSet {
    functions: Vec<(String, String)>,
    modules: Vec<String>,
    variables: Vec<String>,
    usage: Vec<(String, String)>,
}

impl CandidateSet {
    /// Builds the set from the call paths with their signatures of the visible functions, the granted module names and the
    /// scope variable names.
    #[must_use]
    pub const fn new(functions: Vec<(String, String)>, modules: Vec<String>, variables: Vec<String>) -> Self {
        Self {
            functions,
            modules,
            variables,
            usage: Vec::new(),
        }
    }

    /// Attaches the usage lines of the visible functions: call paths paired with the usage text read from
    /// `super::introspect::FunctionEntry::usage`. A path that is not a function candidate of the set is dropped, so that no
    /// usage text can smuggle an ungranted path into a suggestion. Chainable; without this call the set carries no usage.
    #[must_use]
    pub fn with_usage(mut self, usages: Vec<(String, String)>) -> Self {
        self.usage = usages
            .into_iter()
            .filter(|(path, _)| self.functions.iter().any(|(candidate, _)| candidate == path))
            .collect();
        self
    }
}

/// Closest candidates of `name`, from the nearest to the furthest, ties in alphabetical order, without duplicates, at most
/// `max` of them. Nothing for an empty name or when no candidate is near enough.
#[must_use]
pub fn nearest(name: &str, candidates: &[String], max: usize) -> Vec<String> {
    ranked(name, candidates.iter().map(String::as_str))
        .into_iter()
        .take(max)
        .map(|(_, candidate)| candidate.to_owned())
        .collect()
}

/// Completes a not-found error with its closest candidates taken from `candidates`.
///
/// Other errors are returned unchanged. A variable of the scope that a script function could not read gets the hint to pass
/// it as an argument.
#[must_use]
pub fn suggest_for(error: ScratchpadError, candidates: &CandidateSet) -> ScratchpadError {
    let ScratchpadError::NotFound {
        kind, name, function, ..
    } = &error
    else {
        return error;
    };
    let suggestions = suggestions_for(*kind, name, candidates);
    let hint = (*kind == NotFoundKind::Variable && function.is_some() && candidates.variables.contains(name))
        .then(|| format!("functions cannot see scope variables: pass `{name}` as an argument"));
    let error = error.with_suggestions(suggestions);
    match hint {
        Some(hint) => error.with_hint(hint),
        None => error,
    }
}

/// Suggestions for a not-found name of the given kind.
fn suggestions_for(kind: NotFoundKind, name: &str, candidates: &CandidateSet) -> Vec<String> {
    match kind {
        NotFoundKind::Function => function_suggestions(name, candidates),
        NotFoundKind::Module => module_suggestions(name, candidates),
        NotFoundKind::Variable => first_names(ranked(name, candidates.variables.iter().map(String::as_str))),
    }
}

/// Functions near `name`. A candidate at distance zero is shown with its signature, so that the LLM sees the parameters it
/// has to pass, followed by its usage line when the set carries one.
fn function_suggestions(name: &str, candidates: &CandidateSet) -> Vec<String> {
    ranked(name, candidates.functions.iter().map(|(path, _)| path.as_str()))
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(distance, path)| {
            if distance == 0 {
                call_form(path, candidates)
            } else {
                path.to_owned()
            }
        })
        .collect()
}

/// How a function at distance zero is rendered: the signature of the visible function at `path`, or its path when the set
/// holds no signature for it, extended with `; call it as: {usage}` when the set carries a usage line for it.
fn call_form(path: &str, candidates: &CandidateSet) -> String {
    let signature = candidates
        .functions
        .iter()
        .find(|(candidate, _)| candidate.as_str() == path)
        .map_or_else(|| path.to_owned(), |(_, signature)| signature.clone());
    match candidates
        .usage
        .iter()
        .find(|(candidate, _)| candidate.as_str() == path)
        .map(|(_, usage)| usage.as_str())
    {
        Some(usage) => format!("{signature}; call it as: {usage}"),
        None => signature,
    }
}

/// Modules near `name`, among the granted names; when none is near, among the modules of the visible function paths.
fn module_suggestions(name: &str, candidates: &CandidateSet) -> Vec<String> {
    let granted = ranked(name, candidates.modules.iter().map(String::as_str));
    if !granted.is_empty() {
        return first_names(granted);
    }
    let visible = candidates
        .functions
        .iter()
        .filter_map(|(path, _)| path.rsplit_once("::").map(|(module, _)| module));
    first_names(ranked(name, visible))
}

/// The first suggestions of a ranked list, as names.
fn first_names(ranked: Vec<(usize, &str)>) -> Vec<String> {
    ranked
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, name)| name.to_owned())
        .collect()
}

/// Candidates near `name` with their distance, sorted by distance then by name, without duplicates. Empty for an empty name.
fn ranked<'candidate>(
    name: &str,
    candidates: impl Iterator<Item = &'candidate str>,
) -> Vec<(usize, &'candidate str)> {
    let requested = bounded(name);
    if requested.is_empty() {
        return Vec::new();
    }
    let qualified = requested.contains("::");
    let requested_chars: Vec<char> = requested.chars().collect();
    let threshold = requested_chars.len().div_euclid(3).clamp(1, MAX_DISTANCE);
    let mut scored: Vec<(usize, &'candidate str)> = candidates
        .map(|candidate| (distance(&requested_chars, candidate, qualified), candidate))
        .filter(|(distance, _)| *distance <= threshold)
        .collect();
    scored.sort_unstable();
    scored.dedup_by(|left, right| left.1 == right.1);
    scored
}

/// Distance of a requested name to a candidate. A qualified name is compared to the whole path; an unqualified one also to
/// the last segment of the path, the smaller distance counting.
fn distance(requested: &[char], candidate: &str, qualified: bool) -> usize {
    let whole = levenshtein(requested, &bounded_chars(candidate));
    if qualified {
        return whole;
    }
    let last = candidate.rsplit("::").next().unwrap_or(candidate);
    whole.min(levenshtein(requested, &bounded_chars(last)))
}

/// Edit distance between two sequences of characters, with the classic two-row dynamic programming.
fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, left_char) in (1..).zip(left) {
        let mut current: Vec<usize> = vec![row];
        for (right_char, (above_left, above)) in
            right.iter().zip(previous.iter().zip(previous.iter().skip(1)))
        {
            let cost = usize::from(left_char != right_char);
            let left_cell = current.last().copied().unwrap_or(0);
            current.push(
                above
                    .saturating_add(1)
                    .min(left_cell.saturating_add(1))
                    .min(above_left.saturating_add(cost)),
            );
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}

/// The name cut to the longest compared length.
fn bounded(name: &str) -> String {
    name.chars().take(MAX_NAME_CHARS).collect()
}

/// The characters of a name cut to the longest compared length.
fn bounded_chars(name: &str) -> Vec<char> {
    name.chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::{CandidateSet, levenshtein, nearest, suggest_for};
    use crate::scratchpad::{NotFoundKind, ScratchpadError};

    fn grafana() -> CandidateSet {
        CandidateSet::new(
            vec![
                (
                    "grafana::loki_query".to_owned(),
                    "grafana::loki_query(query: string, range: string) -> map".to_owned(),
                ),
                (
                    "grafana::prom_query".to_owned(),
                    "grafana::prom_query(query: string) -> map".to_owned(),
                ),
            ],
            vec!["grafana".to_owned(), "k8s".to_owned()],
            vec!["pods".to_owned(), "nodes".to_owned()],
        )
    }

    fn not_found(kind: NotFoundKind, name: &str, function: Option<&str>) -> ScratchpadError {
        ScratchpadError::NotFound {
            kind,
            name: name.to_owned(),
            position: None,
            function: function.map(str::to_owned),
            suggestions: Vec::new(),
            hint: None,
        }
    }

    /// Suggestions of an error, which must be a not-found error.
    fn suggestions(error: ScratchpadError) -> Vec<String> {
        let ScratchpadError::NotFound { suggestions, .. } = error else {
            panic!("expected a not-found error");
        };
        suggestions
    }

    #[test]
    fn typo_in_a_function_name() {
        let error = suggest_for(
            not_found(NotFoundKind::Function, "grafana::loki_queryy", None),
            &grafana(),
        );
        assert_eq!(
            suggestions(error),
            vec!["grafana::loki_query".to_owned()],
            "closest"
        );
    }

    #[test]
    fn unqualified_name_matches_the_last_segment() {
        let error = suggest_for(not_found(NotFoundKind::Function, "loki_querry", None), &grafana());
        assert_eq!(
            suggestions(error),
            vec!["grafana::loki_query".to_owned()],
            "last segment"
        );
    }

    #[test]
    fn no_suggestion_names_an_ungranted_function() {
        let candidates = CandidateSet::new(
            vec![(
                "grafana::loki_query".to_owned(),
                "grafana::loki_query() -> map".to_owned(),
            )],
            vec!["grafana".to_owned()],
            Vec::new(),
        );
        let error = suggest_for(
            not_found(NotFoundKind::Function, "grafana::loki_queryy", None),
            &candidates,
        );
        let texts = suggestions(error);
        assert_eq!(
            texts,
            vec!["grafana::loki_query".to_owned()],
            "the granted one only"
        );
        assert!(
            !format!("{texts:?}").contains("other"),
            "the ungranted module is nowhere"
        );
    }

    #[test]
    fn dependency_of_a_toolset_is_never_suggested() {
        let candidates = CandidateSet::new(
            vec![("ts::f".to_owned(), "ts::f() -> int".to_owned())],
            vec!["ts".to_owned()],
            Vec::new(),
        );
        let error = suggest_for(not_found(NotFoundKind::Function, "ts::secrt", None), &candidates);
        assert!(
            suggestions(error).is_empty(),
            "the dependency `secret` is not visible"
        );
    }

    #[test]
    fn ungranted_group_function_is_never_suggested() {
        let candidates = CandidateSet::new(
            vec![(
                "json_encode".to_owned(),
                "json_encode(value: Dynamic) -> string".to_owned(),
            )],
            Vec::new(),
            Vec::new(),
        );
        let error = suggest_for(not_found(NotFoundKind::Function, "get_envv", None), &candidates);
        assert!(
            suggestions(error).is_empty(),
            "get_env is not granted, so not suggested"
        );
    }

    #[test]
    fn unknown_module_suggests_the_granted_module() {
        let error = suggest_for(not_found(NotFoundKind::Module, "grafan", None), &grafana());
        assert_eq!(suggestions(error), vec!["grafana".to_owned()], "the module");
    }

    #[test]
    fn arity_mismatch_shows_the_signature() {
        let error = suggest_for(
            not_found(NotFoundKind::Function, "grafana::loki_query", None),
            &grafana(),
        );
        assert_eq!(
            suggestions(error),
            vec!["grafana::loki_query(query: string, range: string) -> map".to_owned()],
            "the signature at distance zero"
        );
    }

    #[test]
    fn threshold_excludes_far_names() {
        let error = suggest_for(not_found(NotFoundKind::Function, "zzzzzzzz", None), &grafana());
        assert!(suggestions(error).is_empty(), "no candidate is near enough");
    }

    #[test]
    fn at_most_three_suggestions_ordered_by_distance_then_name() {
        let candidates = CandidateSet::new(
            ["f5", "f3", "f1", "f4", "f2"]
                .iter()
                .map(|name| ((*name).to_owned(), format!("{name}()")))
                .collect(),
            Vec::new(),
            Vec::new(),
        );
        let error = suggest_for(not_found(NotFoundKind::Function, "f0", None), &candidates);
        assert_eq!(
            suggestions(error),
            vec!["f1".to_owned(), "f2".to_owned(), "f3".to_owned()],
            "three of the five, alphabetical"
        );
    }

    #[test]
    fn variable_typo() {
        let error = suggest_for(not_found(NotFoundKind::Variable, "podz", None), &grafana());
        assert_eq!(suggestions(error), vec!["pods".to_owned()], "the variable");
    }

    #[test]
    fn function_cannot_see_scope_variables() {
        let error = suggest_for(not_found(NotFoundKind::Variable, "pods", Some("f")), &grafana());
        let ScratchpadError::NotFound { hint, .. } = &error else {
            panic!("expected a not-found error");
        };
        assert_eq!(
            hint.as_deref(),
            Some("functions cannot see scope variables: pass `pods` as an argument"),
            "the hint tells to pass the variable"
        );
        assert!(
            error.to_string().contains("pass `pods` as an argument"),
            "the text carries the hint: {error}"
        );
    }

    #[test]
    fn very_long_and_empty_names() {
        let long = "a".repeat(10_000);
        assert!(
            nearest(&long, &["grafana::loki_query".to_owned()], 3).is_empty(),
            "long name"
        );
        assert!(nearest("", &["a".to_owned()], 3).is_empty(), "empty name");
        let error = suggest_for(not_found(NotFoundKind::Function, &long, None), &grafana());
        assert!(suggestions(error).is_empty(), "long name: nothing near");
        let empty = suggest_for(not_found(NotFoundKind::Function, "", None), &grafana());
        assert!(suggestions(empty).is_empty(), "empty name: nothing");
    }

    #[test]
    fn order_of_candidates_does_not_matter() {
        let forward = grafana();
        let backward = CandidateSet::new(
            forward.functions.iter().rev().cloned().collect(),
            forward.modules.iter().rev().cloned().collect(),
            forward.variables.iter().rev().cloned().collect(),
        );
        let error = not_found(NotFoundKind::Function, "grafana::loki_queryy", None);
        assert_eq!(
            suggest_for(error.clone(), &forward),
            suggest_for(error, &backward),
            "the same result"
        );
    }

    /// Hint of a not-found error, which must be one.
    fn hint_of(error: &ScratchpadError) -> Option<String> {
        let ScratchpadError::NotFound { hint, .. } = error else {
            panic!("expected a not-found error");
        };
        hint.clone()
    }

    // Scenario: un candidat est proche quand sa distance vaut au plus le tiers de la longueur du nom
    #[test]
    fn threshold_is_a_third_of_the_name_length() {
        assert_eq!(
            nearest("abcdefghi", &["abcdef".to_owned()], 3),
            vec!["abcdef".to_owned()],
            "nine characters: a distance of three is near"
        );
        assert!(
            nearest("abcdefgh", &["abcde".to_owned()], 3).is_empty(),
            "eight characters: a distance of three is too far"
        );
        assert!(
            nearest("abcde", &["abcxy".to_owned()], 3).is_empty(),
            "five characters: a distance of two is too far"
        );
        assert_eq!(
            nearest("ab", &["ax".to_owned()], 3),
            vec!["ax".to_owned()],
            "a short name keeps a threshold of one"
        );
    }

    // Scenario: les noms sont comparés sur leurs 128 premiers caractères
    #[test]
    fn names_are_compared_on_their_first_128_characters() {
        let prefix_124 = "a".repeat(124);
        let first_requested = format!("{prefix_124}xyzqTTTT");
        let first_candidate = format!("{prefix_124}abcrTTTT");
        assert!(
            nearest(&first_requested, std::slice::from_ref(&first_candidate), 3).is_empty(),
            "four differences within the first 128 characters: too far"
        );
        let prefix_125 = "a".repeat(125);
        let second_requested = format!("{prefix_125}xyzQTTT");
        let second_candidate = format!("{prefix_125}abcWTTT");
        assert_eq!(
            nearest(&second_requested, std::slice::from_ref(&second_candidate), 3),
            vec![second_candidate.clone()],
            "three differences within the first 128 characters, a fourth one beyond them: near"
        );
    }

    // Scenario: un indice n'explique qu'une variable de scope lue dans une fonction
    #[test]
    fn hint_only_for_a_scope_variable_read_in_a_function() {
        let unknown = suggest_for(not_found(NotFoundKind::Variable, "zzz", Some("f")), &grafana());
        assert_eq!(
            hint_of(&unknown),
            None,
            "a name that is not a scope variable gets no hint"
        );
        let top_level = suggest_for(not_found(NotFoundKind::Variable, "pods", None), &grafana());
        assert_eq!(hint_of(&top_level), None, "outside a function there is no hint");
        let function = suggest_for(not_found(NotFoundKind::Function, "pods", Some("f")), &grafana());
        assert_eq!(hint_of(&function), None, "a function name is not a variable");
    }

    // Scenario: les modules des fonctions visibles sont nommés une seule fois chacun
    #[test]
    fn module_fallback_names_each_module_once() {
        let candidates = CandidateSet::new(
            vec![
                (
                    "grafana::loki_query".to_owned(),
                    "grafana::loki_query() -> map".to_owned(),
                ),
                (
                    "grafana::prom_query".to_owned(),
                    "grafana::prom_query() -> map".to_owned(),
                ),
            ],
            Vec::new(),
            Vec::new(),
        );
        let error = suggest_for(not_found(NotFoundKind::Module, "grafan", None), &candidates);
        assert_eq!(
            suggestions(error),
            vec!["grafana".to_owned()],
            "one module, though two visible functions live in it"
        );
    }

    #[test]
    fn other_errors_are_unchanged() {
        let error = ScratchpadError::Cancelled;
        assert_eq!(
            suggest_for(error.clone(), &grafana()),
            error,
            "not a not-found error"
        );
    }

    // suggest.sdd, Must: the granted module names come first; the modules of the visible paths are only a fallback.
    #[test]
    fn granted_module_without_visible_function_is_suggested() {
        let candidates = CandidateSet::new(Vec::new(), vec!["mcp".to_owned()], Vec::new());
        let error = suggest_for(not_found(NotFoundKind::Module, "mcpp", None), &candidates);
        assert_eq!(
            suggestions(error),
            vec!["mcp".to_owned()],
            "a granted module is suggested even when it exposes no visible function"
        );
    }

    // suggest.sdd, Must: for a qualified name, the distance is computed on the whole path, not on the last segment.
    #[test]
    fn qualified_name_is_not_matched_on_its_last_segment() {
        let error = suggest_for(
            not_found(NotFoundKind::Function, "x::loki_query", None),
            &grafana(),
        );
        assert!(
            suggestions(error).is_empty(),
            "the whole path is too far, so the last segment alone must not match"
        );
    }

    // suggest.sdd, Must: the distance is the Levenshtein edit distance, where a deletion and an insertion both cost one.
    #[test]
    fn edit_distance_counts_one_deletion_and_is_symmetric() {
        let abc: Vec<char> = "abc".chars().collect();
        let ab: Vec<char> = "ab".chars().collect();
        assert_eq!(levenshtein(&abc, &ab), 1, "deleting one character costs one");
        assert_eq!(
            levenshtein(&ab, &abc),
            1,
            "inserting one character costs one, as deleting does"
        );
    }

    /// A set with one map-taking function and its usage line, for the `call it as:` scenarios.
    fn sandbox_with_usage() -> CandidateSet {
        CandidateSet::new(
            vec![(
                "sandbox::read_file".to_owned(),
                "sandbox::read_file(arguments: Map)".to_owned(),
            )],
            vec!["sandbox".to_owned()],
            Vec::new(),
        )
        .with_usage(vec![(
            "sandbox::read_file".to_owned(),
            "sandbox::read_file(#{ path: \"\u{2026}\" })".to_owned(),
        )])
    }

    // Scenario: un nombre d'arguments incorrect sur une fonction à table montre l'appel attendu
    #[test]
    fn a_wrong_arity_on_a_map_function_shows_the_expected_call() {
        let error = suggest_for(
            not_found(NotFoundKind::Function, "sandbox::read_file", None),
            &sandbox_with_usage(),
        );
        assert_eq!(
            suggestions(error),
            vec![
                "sandbox::read_file(arguments: Map); call it as: sandbox::read_file(#{ path: \"\u{2026}\" })"
                    .to_owned()
            ],
            "the signature and the expected call at distance zero"
        );
    }

    // Scenario: l'usage d'une fonction proche n'est pas montré
    #[test]
    fn the_usage_of_a_near_function_is_not_shown() {
        let error = suggest_for(
            not_found(NotFoundKind::Function, "sandbox::read_fil", None),
            &sandbox_with_usage(),
        );
        assert_eq!(
            suggestions(error),
            vec!["sandbox::read_file".to_owned()],
            "the bare path at a nonzero distance, no signature and no usage"
        );
    }

    // Scenario: un usage donné pour un chemin inconnu est ignoré
    #[test]
    fn a_usage_given_for_an_unknown_path_is_ignored() {
        let candidates = CandidateSet::new(
            vec![(
                "sandbox::read_file".to_owned(),
                "sandbox::read_file(arguments: Map)".to_owned(),
            )],
            vec!["sandbox".to_owned()],
            Vec::new(),
        )
        .with_usage(vec![(
            "other::secret".to_owned(),
            "other::secret(#{ token: \"\u{2026}\" })".to_owned(),
        )]);
        let without_usage = CandidateSet::new(
            vec![(
                "sandbox::read_file".to_owned(),
                "sandbox::read_file(arguments: Map)".to_owned(),
            )],
            vec!["sandbox".to_owned()],
            Vec::new(),
        );
        assert_eq!(
            candidates, without_usage,
            "a path that is not a function candidate is dropped"
        );
        let error = suggest_for(
            not_found(NotFoundKind::Function, "sandbox::read_fil", None),
            &candidates,
        );
        assert!(
            !format!("{error}").contains("other::secret"),
            "the unknown path is nowhere in the message: {error}"
        );
    }
}
