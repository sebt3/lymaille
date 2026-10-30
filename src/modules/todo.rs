//! Todo list of the session and the Rhai module `todo` (`agent-sessions/modules#FR-1`, `agent-sessions/modules#AC-1`).
//! The list is a shared handle: the Rhai module and the turn loop see the same items. Each change made through the module
//! emits one `TodoChanged` event, dated by the injected clock; reading emits nothing.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::sync::{Arc, Mutex, MutexGuard};

use rhai::{Array, Dynamic, EvalAltResult, ImmutableString, Map, Module, Position};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ModulesError, ModulesResult};
use crate::clock::Clock;
use crate::session::{Event, EventKind, EventSink, SessionId};

/// Text of the states accepted by `set_status`, in the order they are listed in the refusal.
const STATE_NAMES: &str = "pending, in_progress or done";

/// State of a todo. Written in snake case in the JSON form and in the Rhai module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started yet.
    Pending,
    /// Being worked on.
    InProgress,
    /// Finished.
    Done,
}

impl TodoStatus {
    /// Returns the text of the state: `pending`, `in_progress` or `done`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    /// Reads a state from its text. An unknown text is refused, and the refusal names it.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::InvalidTodo`] when the text is not one of the three states.
    pub fn parse(text: &str) -> ModulesResult<Self> {
        match text {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "done" => Ok(Self::Done),
            other => Err(ModulesError::InvalidTodo(format!(
                "unknown state `{other}`, expected {STATE_NAMES}"
            ))),
        }
    }
}

/// One todo of the list: an identifier, its text and its state.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    id: u64,
    text: String,
    status: TodoStatus,
}

impl TodoItem {
    /// Returns the identifier: an integer, increasing, never reused in the list.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Returns the text of the todo.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the state of the todo.
    #[must_use]
    pub const fn status(&self) -> TodoStatus {
        self.status
    }
}

/// Internal state of a list: the items in insertion order and the identifier of the next one.
#[derive(Debug)]
struct ListState {
    items: Vec<TodoItem>,
    next_id: u64,
}

/// Todo list of a session. It is a shared handle: clones see the same items.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TodoList {
    state: Arc<Mutex<ListState>>,
}

impl TodoList {
    /// Builds an empty list. The first identifier is 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ListState {
                items: Vec::new(),
                next_id: 1,
            })),
        }
    }

    /// Rebuilds a list from the JSON values of the session state. The next identifier follows the largest one read.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::InvalidTodo`] when a value is not a recognized todo, when two values share an identifier,
    /// or when a text is blank.
    pub fn from_values(values: &[Value]) -> ModulesResult<Self> {
        let mut items = Vec::with_capacity(values.len());
        for value in values {
            let Ok(item) = TodoItem::deserialize(value) else {
                return Err(ModulesError::InvalidTodo(
                    "a value is not a recognized todo".to_owned(),
                ));
            };
            validate_text(&item.text)?;
            items.push(item);
        }
        let mut seen = BTreeSet::new();
        for item in &items {
            if !seen.insert(item.id) {
                return Err(ModulesError::InvalidTodo(format!(
                    "two todos share the identifier {}",
                    item.id
                )));
            }
        }
        let next_id = match items.iter().map(|item| item.id).max() {
            None => 1,
            Some(largest) => largest
                .checked_add(1)
                .ok_or_else(|| ModulesError::InvalidTodo("the todo identifiers are exhausted".to_owned()))?,
        };
        Ok(Self {
            state: Arc::new(Mutex::new(ListState { items, next_id })),
        })
    }

    /// Adds a todo in the `pending` state and returns its identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::InvalidTodo`] when the text is blank, and [`ModulesError::Poisoned`] when the lock is poisoned.
    pub fn add(&self, text: &str) -> ModulesResult<u64> {
        validate_text(text)?;
        let mut state = self.lock()?;
        let id = state.next_id;
        state.next_id = id
            .checked_add(1)
            .ok_or_else(|| ModulesError::InvalidTodo("the todo identifiers are exhausted".to_owned()))?;
        state.items.push(TodoItem {
            id,
            text: text.to_owned(),
            status: TodoStatus::Pending,
        });
        drop(state);
        Ok(id)
    }

    /// Changes the state of the todo `id`.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::InvalidTodo`] when no todo has the identifier, and [`ModulesError::Poisoned`] when the lock
    /// is poisoned.
    pub fn set_status(&self, id: u64, status: TodoStatus) -> ModulesResult<()> {
        let mut state = self.lock()?;
        let item = state
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| unknown_identifier(id))?;
        item.status = status;
        drop(state);
        Ok(())
    }

    /// Returns a copy of the items, in insertion order.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::Poisoned`] when the lock is poisoned.
    pub fn items(&self) -> ModulesResult<Vec<TodoItem>> {
        Ok(self.lock()?.items.clone())
    }

    /// Returns the JSON values to hand to the session state, one per item, in insertion order.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::Poisoned`] when the lock is poisoned.
    pub fn to_values(&self) -> ModulesResult<Vec<Value>> {
        Ok(self
            .lock()?
            .items
            .iter()
            .map(|item| json!({"id": item.id, "text": item.text, "status": item.status.as_str()}))
            .collect())
    }

    /// Locks the state. A poisoned lock is reported, not recovered.
    fn lock(&self) -> ModulesResult<MutexGuard<'_, ListState>> {
        self.state.lock().map_err(|_poisoned| ModulesError::Poisoned)
    }
}

impl Default for TodoList {
    fn default() -> Self {
        Self::new()
    }
}

/// Refuses a blank text. The text itself is kept as given otherwise.
fn validate_text(text: &str) -> ModulesResult<()> {
    if text.trim().is_empty() {
        return Err(ModulesError::InvalidTodo(
            "the text of a todo must not be blank".to_owned(),
        ));
    }
    Ok(())
}

/// The refusal of an identifier that no todo carries. The text form also serves a script integer that is negative.
fn unknown_identifier<T: Display>(id: T) -> ModulesError {
    ModulesError::InvalidTodo(format!("unknown todo identifier {id}"))
}

/// Emits the `TodoChanged` events of a module, dated by its clock.
#[derive(Clone)]
struct Notifier {
    session: SessionId,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
}

impl Notifier {
    /// Emits one `TodoChanged` event for the session.
    fn changed(&self) {
        self.sink.emit(Event::new(
            self.session.clone(),
            self.clock.now(),
            EventKind::TodoChanged,
        ));
    }
}

/// Builds the Rhai module `todo` over a shared list: `add(text)`, `set_status(id, state)`, `list()` and `get(id)`. Changes
/// emit a `TodoChanged` event through `sink`, dated by `clock`, and a refused change emits nothing.
#[must_use]
pub fn todo_module(
    list: TodoList,
    session: SessionId,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
) -> Module {
    let notifier = Notifier { session, sink, clock };
    let mut module = Module::new();
    let add_list = list.clone();
    let add_notifier = notifier.clone();
    let _add = module.set_native_fn(
        "add",
        move |text: ImmutableString| -> Result<i64, Box<EvalAltResult>> {
            let id = add_list
                .add(text.as_str())
                .map_err(|error| Box::new(runtime_error(&error)))?;
            let value = identifier_value(id)?;
            add_notifier.changed();
            Ok(value)
        },
    );
    let status_list = list.clone();
    let _set_status = module.set_native_fn(
        "set_status",
        move |id: i64, state: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            let id = identifier_of(id)?;
            let status =
                TodoStatus::parse(state.as_str()).map_err(|error| Box::new(runtime_error(&error)))?;
            status_list
                .set_status(id, status)
                .map_err(|error| Box::new(runtime_error(&error)))?;
            notifier.changed();
            Ok(())
        },
    );
    let list_list = list.clone();
    let _list = module.set_native_fn("list", move || -> Result<Array, Box<EvalAltResult>> {
        let items = list_list
            .items()
            .map_err(|error| Box::new(runtime_error(&error)))?;
        items.iter().map(item_value).collect()
    });
    let _get = module.set_native_fn("get", move |id: i64| -> Result<Dynamic, Box<EvalAltResult>> {
        let wanted = identifier_of(id)?;
        let items = list.items().map_err(|error| Box::new(runtime_error(&error)))?;
        let item = items
            .iter()
            .find(|item| item.id == wanted)
            .ok_or_else(|| Box::new(runtime_error(&unknown_identifier(wanted))))?;
        item_value(item)
    });
    module
}

/// Wraps a refusal of the list as a Rhai runtime error that carries its message.
fn runtime_error(error: &ModulesError) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(error.to_string()), Position::NONE)
}

/// Reads a script identifier. A negative number names no todo.
fn identifier_of(id: i64) -> Result<u64, Box<EvalAltResult>> {
    u64::try_from(id)
        .ok()
        .ok_or_else(|| Box::new(runtime_error(&unknown_identifier(id))))
}

/// Gives a todo identifier to a script. Identifiers that do not fit an integer of the script are refused.
fn identifier_value(id: u64) -> Result<i64, Box<EvalAltResult>> {
    i64::try_from(id).map_err(|error| Box::new(runtime_error(&ModulesError::InvalidTodo(error.to_string()))))
}

/// The map `#{id, text, status}` of a todo.
fn item_value(item: &TodoItem) -> Result<Dynamic, Box<EvalAltResult>> {
    let id = identifier_value(item.id)?;
    let fields = [
        ("id", Dynamic::from(id)),
        ("text", Dynamic::from(item.text.clone())),
        ("status", Dynamic::from(item.status.as_str().to_owned())),
    ];
    Ok(Dynamic::from(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect::<Map>(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use rhai::{Engine as RhaiEngine, EvalAltResult};
    use serde_json::json;

    use super::{TodoItem, TodoList, TodoStatus, todo_module};
    use crate::clock::{ManualClock, Timestamp};
    use crate::session::{Event, EventKind, EventSink, RecordingSink, SessionId};

    /// A list, a recording sink, a manual clock and a Rhai engine with the `todo` module registered on the list.
    struct Fixture {
        list: TodoList,
        sink: Arc<RecordingSink>,
        engine: RhaiEngine,
    }

    fn fixture() -> Fixture {
        fixture_with(TodoList::new())
    }

    fn fixture_with(list: TodoList) -> Fixture {
        let sink = Arc::new(RecordingSink::new());
        let recording: Arc<RecordingSink> = Arc::clone(&sink);
        let shared: Arc<dyn EventSink> = recording;
        let clock = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let session = SessionId::new("s-1").expect("a non-empty identifier");
        let module = todo_module(list.clone(), session, shared, clock);
        let mut engine = RhaiEngine::new();
        let _registered = engine.register_static_module("todo", Arc::new(module));
        Fixture { list, sink, engine }
    }

    fn changed_count(sink: &RecordingSink) -> usize {
        sink.events()
            .iter()
            .filter(|event: &&Event| event.kind == EventKind::TodoChanged)
            .count()
    }

    fn error_text<T: std::fmt::Debug>(result: Result<T, Box<EvalAltResult>>) -> String {
        match result {
            Ok(value) => panic!("the script must fail, it returned {value:?}"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn adding_a_todo_emits_one_changed_event() {
        let fixture = fixture();
        let _added = fixture
            .engine
            .eval::<i64>(r#"todo::add("check the pods")"#)
            .expect("adding a todo succeeds");
        let items = fixture.list.items().expect("the list is readable");
        assert_eq!(items.len(), 1, "one todo must be in the list");
        let item = items.first().expect("one todo was checked above");
        assert_eq!(item.status(), TodoStatus::Pending, "a new todo is pending");
        assert_eq!(item.text(), "check the pods");
        assert_eq!(
            changed_count(&fixture.sink),
            1,
            "exactly one TodoChanged must be emitted"
        );
    }

    #[test]
    fn changed_event_is_dated_by_the_clock_and_names_the_session() {
        let fixture = fixture();
        let _added = fixture
            .engine
            .eval::<i64>(r#"todo::add("check the pods")"#)
            .expect("adding a todo succeeds");
        let events = fixture.sink.events();
        let event = events.first().expect("one event was emitted");
        assert_eq!(
            event.at,
            Timestamp::from_millis(1_000),
            "the instant comes from the injected clock"
        );
        assert_eq!(event.session.as_str(), "s-1");
    }

    #[test]
    fn changing_a_state_survives_a_round_trip_through_values() {
        let fixture = fixture();
        fixture
            .engine
            .eval::<()>(r#"todo::add("write the report"); todo::set_status(1, "done")"#)
            .expect("the script succeeds");
        assert_eq!(
            changed_count(&fixture.sink),
            2,
            "the add and the status change each emit"
        );
        let values = fixture.list.to_values().expect("the list converts");
        let restored = TodoList::from_values(&values).expect("the values are read back");
        let items = restored.items().expect("the restored list is readable");
        let item = items.first().expect("the restored list keeps the todo");
        assert_eq!(
            item.status(),
            TodoStatus::Done,
            "the state survives the round trip"
        );
        assert_eq!(item.text(), "write the report");
    }

    #[test]
    fn reading_emits_nothing() {
        let list = TodoList::new();
        assert_eq!(list.add("first").expect("adding succeeds"), 1);
        assert_eq!(list.add("second").expect("adding succeeds"), 2);
        let fixture = fixture_with(list);
        let count = fixture
            .engine
            .eval::<i64>("todo::list().len()")
            .expect("list() is readable");
        assert_eq!(count, 2, "list() returns both todos");
        let text = fixture
            .engine
            .eval::<String>("todo::get(1).text")
            .expect("get() is readable");
        assert_eq!(text, "first", "get() returns the first todo");
        assert!(
            fixture.sink.events().is_empty(),
            "reading must not emit any event"
        );
    }

    #[test]
    fn unknown_state_is_refused_with_its_name() {
        let list = TodoList::new();
        assert_eq!(list.add("only").expect("adding succeeds"), 1);
        let fixture = fixture_with(list.clone());
        let message = error_text(fixture.engine.eval::<()>(r#"todo::set_status(1, "fini")"#));
        assert!(
            message.contains("fini"),
            "the error must name the refused state: {message}"
        );
        let items = list.items().expect("the list is readable");
        assert_eq!(
            items.first().map(TodoItem::status),
            Some(TodoStatus::Pending),
            "the todo is unchanged"
        );
        assert_eq!(
            fixture.sink.events(),
            Vec::new(),
            "a refused change emits nothing"
        );
    }

    #[test]
    fn unknown_identifier_is_refused_with_its_number() {
        let fixture = fixture();
        let message = error_text(fixture.engine.eval::<()>(r#"todo::set_status(7, "done")"#));
        assert!(
            message.contains('7'),
            "the error must name the identifier 7: {message}"
        );
        assert_eq!(
            fixture.sink.events(),
            Vec::new(),
            "a refused change emits nothing"
        );
    }

    #[test]
    fn blank_text_is_refused_and_leaves_the_list_empty() {
        let fixture = fixture();
        let message = error_text(fixture.engine.eval::<i64>(r#"todo::add("   ")"#));
        assert!(
            message.contains("blank"),
            "the error must say the text is blank: {message}"
        );
        assert!(
            fixture.list.items().expect("readable").is_empty(),
            "the list stays empty"
        );
        assert_eq!(
            fixture.sink.events(),
            Vec::new(),
            "a refused change emits nothing"
        );
    }

    #[test]
    fn identifiers_are_not_reused_after_a_restore() {
        let list = TodoList::new();
        for text in ["one", "two", "three"] {
            let _id = list.add(text).expect("adding succeeds");
        }
        let values = list.to_values().expect("the list converts");
        let restored = TodoList::from_values(&values).expect("the values are read back");
        let fourth = restored.add("four").expect("adding succeeds");
        assert!(
            fourth > 3,
            "the fourth identifier {fourth} must follow the third one (3)"
        );
    }

    #[test]
    fn identifiers_follow_the_largest_one_read_back() {
        let values = vec![
            json!({"id": 1, "text": "one", "status": "pending"}),
            json!({"id": 5, "text": "five", "status": "pending"}),
        ];
        let restored = TodoList::from_values(&values).expect("the values are read back");
        let next = restored.add("six").expect("adding succeeds");
        assert_eq!(next, 6, "the next identifier must follow the largest one (5)");
    }

    #[test]
    fn unreadable_value_is_refused() {
        let values = vec![json!({"text": "no identifier", "status": "pending"})];
        let error = TodoList::from_values(&values).expect_err("a value without identifier is refused");
        assert!(
            matches!(error, crate::modules::ModulesError::InvalidTodo(_)),
            "the refusal must be InvalidTodo, got {error:?}"
        );
    }

    #[test]
    fn empty_values_give_an_empty_list() {
        let list = TodoList::from_values(&[]).expect("no value is a valid list");
        assert_eq!(list.items().expect("readable"), Vec::new());
        assert_eq!(
            list.add("first").expect("adding succeeds"),
            1,
            "the first identifier is 1"
        );
    }

    #[test]
    fn values_with_an_unknown_field_or_state_are_refused() {
        let extra = vec![json!({"id": 1, "text": "a", "status": "pending", "extra": true})];
        assert!(
            TodoList::from_values(&extra).is_err(),
            "an unknown field is refused"
        );
        let state = vec![json!({"id": 1, "text": "a", "status": "fini"})];
        assert!(
            TodoList::from_values(&state).is_err(),
            "an unknown state is refused"
        );
    }

    #[test]
    fn duplicate_identifiers_and_blank_texts_are_refused() {
        let duplicate = vec![
            json!({"id": 1, "text": "a", "status": "pending"}),
            json!({"id": 1, "text": "b", "status": "done"}),
        ];
        assert!(
            TodoList::from_values(&duplicate).is_err(),
            "two todos with one identifier are refused"
        );
        let blank = vec![json!({"id": 1, "text": " ", "status": "pending"})];
        assert!(TodoList::from_values(&blank).is_err(), "a blank text is refused");
    }

    #[test]
    fn values_keep_insertion_order_and_identifiers() {
        let list = TodoList::new();
        assert_eq!(list.add("first").expect("adding succeeds"), 1);
        assert_eq!(list.add("second").expect("adding succeeds"), 2);
        let values = list.to_values().expect("the list converts");
        assert_eq!(values[0], json!({"id": 1, "text": "first", "status": "pending"}));
        assert_eq!(values[1], json!({"id": 2, "text": "second", "status": "pending"}));
    }

    #[test]
    fn states_have_their_text() {
        assert_eq!(
            TodoStatus::parse("in_progress").expect("known").as_str(),
            "in_progress"
        );
        assert!(TodoStatus::parse("fini").is_err(), "an unknown state is refused");
    }

    #[test]
    fn poisoned_lock_is_reported_not_recovered() {
        let list = TodoList::new();
        let poisoner = list.clone();
        let handle = thread::spawn(move || {
            let _guard = poisoner.state.lock().expect("the lock is fresh");
            panic!("poisoning the lock on purpose");
        });
        assert!(handle.join().is_err(), "the poisoning thread must panic");
        assert!(matches!(
            list.items(),
            Err(crate::modules::ModulesError::Poisoned)
        ));
        assert!(matches!(
            list.add("x"),
            Err(crate::modules::ModulesError::Poisoned)
        ));
    }
}
