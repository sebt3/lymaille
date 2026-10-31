//! Registry of the runs of sub-agents: a start that hands back a handle at once, the end of a run as an event, a synchronous
//! wait as sugar over that event, cancellation propagated to the children of the caller, resumption of an interrupted
//! run on its saved session, and the batch driver that repeats turns until a result is accepted
//! (`agent-sessions/delegation#FR-1`, `FR-3`, `FR-6`, `FR-7`, `FR-14`, `AC-1`, `AC-3`, `AC-10`). See `./run.sdd`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::watch;

use super::error::{DelegationError, DelegationResult};
use super::result::{ResultCollector, SubagentOutcome, SubagentStatus};
use super::sources::EventHub;
use crate::agents::InferenceProvider;
use crate::clock::Clock;
use crate::context::CompactionStrategy;
use crate::runtime::TurnRunner;
use crate::session::{CancelHandle, Event, EventKind, EventSink, Outcome, RunId, SessionId, SessionStore};

/// Source of the notices the registry sends to the event hub.
const SUBAGENT_SOURCE: &str = "subagent";

/// Name of the notice sent when a run is over, in the source `subagent`.
const ENDED: &str = "ended";

/// Number of turns a batch run may take without an accepted result (`agent-sessions/delegation#FR-4`).
const BATCH_MAX_TURNS: u32 = 8;

/// Fixed message given to a batch sub-agent whose turn ended without an accepted result.
const REMINDER: &str = "No result has been accepted yet. Submit your result with the result::submit function; corrections are made in this same run.";

/// Reason of a run cancelled by its caller or by the registry.
const CANCELLED_REASON: &str = "the run was cancelled";

/// Reason of a run whose sub-agent the host failed to launch.
const LAUNCH_FAILED_REASON: &str = "the host failed to launch the sub-agent";

/// Reason of a run that ended, without an explicit failure, with no accepted result.
const NO_RESULT_REASON: &str = "the run ended without a result";

/// Reason of a batch run whose turn failed.
const TURN_FAILED_REASON: &str = "a turn of the sub-agent failed";

/// Reason of a batch run that reached its turn limit without an accepted result.
const NO_RESULT_IN_TURNS_REASON: &str = "no result was accepted within the turn limit";

/// Reason of a run interrupted by the restore of its main session.
const INTERRUPTED_REASON: &str = "the run was interrupted by a restore of the session";

/// Reason of a run whose launcher reported an interruption outside a restore: such an outcome is `Failed`.
const OUTSIDE_RESTORE_REASON: &str = "the sub-agent reported an interruption outside a restore";

/// Reason of a run whose collector cannot be read (a poisoned lock): it is reported as a failure, never as the raw outcome.
const UNREADABLE_REASON: &str = "the outcome of the run could not be read";

/// Host side of a sub-agent.
///
/// The host assembles the sub-agent of a request (definition, profile, provider, grants without a delegation module,
/// turn runner) and runs it to its end: the library cannot build a sub-agent alone.
pub trait SubagentLauncher: Send + Sync + 'static {
    /// Runs the sub-agent of `request` until its end and returns its outcome. Once the run is concluded, the registry
    /// reads the outcome of the collector of the request, so this value only tells how the launch went.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Launch`] when the sub-agent cannot be assembled or run.
    fn launch(
        &self,
        request: LaunchRequest,
    ) -> impl Future<Output = DelegationResult<SubagentOutcome>> + Send;
}

/// What the host needs to run one sub-agent: its name, its task, its run, its session, the cancellation of the run and
/// the collector of its result.
///
/// The task is the input of the run: it never goes into an event, an error or a notice, and `Debug` leaves it out.
#[non_exhaustive]
pub struct LaunchRequest {
    subagent: String,
    task: String,
    run: RunId,
    session: SessionId,
    resumed: bool,
    cancel: CancelHandle,
    collector: Arc<ResultCollector>,
}

impl LaunchRequest {
    /// Name of the sub-agent, as the definition of the main agent declares it.
    #[must_use]
    pub const fn subagent(&self) -> &str {
        self.subagent.as_str()
    }

    /// Task given by the main agent.
    #[must_use]
    pub const fn task(&self) -> &str {
        self.task.as_str()
    }

    /// Identifier of the run.
    #[must_use]
    pub const fn run(&self) -> &RunId {
        &self.run
    }

    /// Identifier of the session of the sub-agent: a new one for a fresh run, the saved one to resume.
    #[must_use]
    pub const fn session(&self) -> &SessionId {
        &self.session
    }

    /// Whether the run resumes the saved session of an interrupted run.
    #[must_use]
    pub const fn resumed(&self) -> bool {
        self.resumed
    }

    /// Cancellation of the run: a child of the handle of the caller, cancelled with it.
    #[must_use]
    pub const fn cancel(&self) -> &CancelHandle {
        &self.cancel
    }

    /// Collector of the result of the run.
    #[must_use]
    pub const fn collector(&self) -> &Arc<ResultCollector> {
        &self.collector
    }
}

impl fmt::Debug for LaunchRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchRequest")
            .field("subagent", &self.subagent)
            .field("run", &self.run)
            .field("resumed", &self.resumed)
            .finish_non_exhaustive()
    }
}

/// Handle of a started run: its identifier, which the caller keeps to wait for the run, read its status or cancel it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunHandle {
    run: RunId,
}

impl RunHandle {
    /// Identifier of the run.
    #[must_use]
    pub const fn run(&self) -> &RunId {
        &self.run
    }
}

/// What the main session persists about a run that is not concluded: the run, its sub-agent and the session of the
/// sub-agent, which a restore needs to report the interruption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunRecord {
    run: RunId,
    subagent: String,
    session: SessionId,
}

impl RunRecord {
    /// Builds a record of `run`, run by `subagent` in `session`.
    #[must_use]
    pub const fn new(run: RunId, subagent: String, session: SessionId) -> Self {
        Self {
            run,
            subagent,
            session,
        }
    }

    /// Identifier of the run.
    #[must_use]
    pub const fn run(&self) -> &RunId {
        &self.run
    }

    /// Name of the sub-agent.
    #[must_use]
    pub const fn subagent(&self) -> &str {
        self.subagent.as_str()
    }

    /// Identifier of the session of the sub-agent.
    #[must_use]
    pub const fn session(&self) -> &SessionId {
        &self.session
    }
}

/// Progress of a run as the registry reports it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RunProgress {
    /// The run is not concluded yet.
    Running,
    /// The run is concluded with this outcome.
    Ended(SubagentOutcome),
}

/// Sends the events of the runs to the host, and their notices to the event hub.
struct Reporter {
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    hub: Arc<EventHub>,
    parent: SessionId,
}

impl Reporter {
    /// Reports the end of a run: `RunEnded` when its status has an outcome, then the notice `ended`.
    fn ended(&self, run: &RunId, status: SubagentStatus) {
        if let Some(outcome) = event_outcome(status) {
            self.emit(EventKind::RunEnded {
                run: run.clone(),
                outcome,
            });
        }
        self.notify(run, status_label(status));
    }

    /// Reports a run interrupted by a restore: `RunInterrupted` with the session of the sub-agent, then the notice `ended`
    /// with the status `interrupted`.
    fn interrupted(&self, run: &RunId, session: &SessionId) {
        self.emit(EventKind::RunInterrupted {
            run: run.clone(),
            session: session.clone(),
        });
        self.notify(run, status_label(SubagentStatus::Interrupted));
    }

    /// Emits an event of the main session, at the instant of the clock.
    fn emit(&self, kind: EventKind) {
        self.sink
            .emit(Event::new(self.parent.clone(), self.clock.now(), kind));
    }

    /// Notifies the event hub. The notice carries the identifier of the run and its status, never the task nor the result.
    /// A notice the hub cannot record (poisoned lock) is dropped: the run is concluded all the same.
    fn notify(&self, run: &RunId, status: &str) {
        let data = json!({"run": run.as_str(), "status": status});
        let _notified = self.hub.notify(SUBAGENT_SOURCE, ENDED, data, self.clock.now());
    }
}

/// Outcome of a run as the event of its end carries it. An interrupted run has no outcome: its event is `RunInterrupted`.
const fn event_outcome(status: SubagentStatus) -> Option<Outcome> {
    match status {
        SubagentStatus::Valid => Some(Outcome::Valid),
        SubagentStatus::Failed => Some(Outcome::Failed),
        SubagentStatus::Cancelled => Some(Outcome::Cancelled),
        SubagentStatus::Interrupted => None,
    }
}

/// Status as the notice of the event hub names it.
const fn status_label(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Valid => "valid",
        SubagentStatus::Failed => "failed",
        SubagentStatus::Cancelled => "cancelled",
        SubagentStatus::Interrupted => "interrupted",
    }
}

/// One run as the registry holds it: its collector, its cancellation, and the signal that wakes its waiters.
struct Entry {
    run: RunId,
    subagent: String,
    session: SessionId,
    /// Whether the run comes from a restore (interrupted, its session may be resumed).
    restored: bool,
    collector: Arc<ResultCollector>,
    cancel: CancelHandle,
    /// Set to `true` once the run is concluded and reported.
    done: watch::Sender<bool>,
    /// The outcomes reported in place of the raw ones of the collector.
    normalized: Normalized,
}

/// Runs of the registry, by identifier, the restored runs whose session is not resumed yet, and the counter that numbers
/// the new runs.
#[derive(Default)]
struct Table {
    entries: BTreeMap<RunId, Arc<Entry>>,
    /// Identifiers of the restored runs not resumed yet: `records` keeps them until a resume takes them.
    pending: BTreeSet<RunId>,
    last_run: u64,
}

/// Registry of the runs of the sub-agents of one main session.
pub struct RunRegistry<L> {
    launcher: Arc<L>,
    declared: BTreeMap<String, (Value, u32)>,
    reporter: Arc<Reporter>,
    table: Mutex<Table>,
}

impl<L> fmt::Debug for RunRegistry<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunRegistry").finish_non_exhaustive()
    }
}

impl<L: SubagentLauncher> RunRegistry<L> {
    /// Builds the registry of the sub-agents of the main session `parent`. `declared` maps the name of each sub-agent the
    /// main agent may delegate to to its result schema and its number of corrections; its keys are the list of allowed
    /// names. Each schema is compiled here, once, to be checked.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidSchema`] when a schema does not compile as a JSON Schema.
    #[expect(
        clippy::too_many_arguments,
        reason = "six arguments, the signature of src/delegation/run.sdd (Accepts of @RunRegistry::new): the launcher, the declared sub-agents, the sink, the clock, the hub and the parent session; a struct would add a public type the spec does not name"
    )]
    pub fn new(
        launcher: Arc<L>,
        declared: BTreeMap<String, (Value, u32)>,
        sink: Arc<dyn EventSink>,
        clock: Arc<dyn Clock>,
        hub: Arc<EventHub>,
        parent: SessionId,
    ) -> DelegationResult<Self> {
        for (subagent, (schema, corrections)) in &declared {
            let _checked = ResultCollector::new(subagent, schema, *corrections)?;
        }
        Ok(Self {
            launcher,
            declared,
            reporter: Arc::new(Reporter {
                sink,
                clock,
                hub,
                parent,
            }),
            table: Mutex::new(Table::default()),
        })
    }

    /// Starts a run of `subagent` on `task`, under a child of `cancel`, and returns its handle at once; the run goes on in a
    /// detached task.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::UnknownSubagent`] when `subagent` is not allowed, in which case no run is created, and
    /// [`DelegationError::Poisoned`] when the lock of the registry is poisoned.
    pub fn start(&self, subagent: &str, task: &str, cancel: &CancelHandle) -> DelegationResult<RunHandle> {
        self.launch_run(subagent, task, cancel, None)
    }

    /// Starts a run of `subagent` on `task` that resumes the saved `session` of an interrupted run, under a child of
    /// `cancel`. The restored run is consumed: it is no longer listed by [`RunRegistry::records`], and its session cannot
    /// be resumed again.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`RunRegistry::start`], and [`DelegationError::UnknownSession`] or
    /// [`DelegationError::DuplicateRun`] when `session` may not be resumed for `subagent`.
    pub fn resume(
        &self,
        subagent: &str,
        task: &str,
        cancel: &CancelHandle,
        session: &SessionId,
    ) -> DelegationResult<RunHandle> {
        if !self.declared.contains_key(subagent) {
            return Err(DelegationError::UnknownSubagent {
                name: subagent.to_owned(),
            });
        }
        self.launch_run(subagent, task, cancel, Some(session.clone()))
    }

    /// Registers a run and spawns it. `resume` is the saved session to resume, if any.
    fn launch_run(
        &self,
        subagent: &str,
        task: &str,
        cancel: &CancelHandle,
        resume: Option<SessionId>,
    ) -> DelegationResult<RunHandle> {
        let Some((schema, corrections)) = self.declared.get(subagent) else {
            return Err(DelegationError::UnknownSubagent {
                name: subagent.to_owned(),
            });
        };
        // Checked before anything is registered: the spawn below needs the runtime, and a refusal must leave no entry behind.
        let _runtime = Handle::try_current().map_err(|_absent| DelegationError::NoRuntime)?;
        let collector = Arc::new(ResultCollector::new(subagent, schema, *corrections)?);
        let child = cancel.child();
        let resumed = resume.is_some();
        let entry = {
            // The check of a resumption and the consumption of the restored run happen under the same lock as the insertion.
            let mut table = self.lock()?;
            let (run, session, taken) = if let Some(session) = resume {
                let taken = check_resumable(&table, subagent, &session)?;
                (next_run_id(&mut table)?, session, Some(taken))
            } else {
                let run = next_run_id(&mut table)?;
                let session = session_of(&self.reporter.parent, &run)?;
                (run, session, None)
            };
            let entry = Arc::new(Entry {
                run: run.clone(),
                subagent: subagent.to_owned(),
                session,
                restored: false,
                normalized: Normalized::new(subagent, schema, *corrections)?,
                collector: Arc::clone(&collector),
                cancel: child.clone(),
                done: watch::channel(false).0,
            });
            if let Some(taken) = taken {
                let _was_pending = table.pending.remove(&taken);
            }
            let _previous = table.entries.insert(run, Arc::clone(&entry));
            entry
        };
        let request = LaunchRequest {
            subagent: subagent.to_owned(),
            task: task.to_owned(),
            run: entry.run.clone(),
            session: entry.session.clone(),
            resumed,
            cancel: child,
            collector,
        };
        let handle = RunHandle {
            run: entry.run.clone(),
        };
        spawn(
            Arc::clone(&self.launcher),
            Arc::clone(&self.reporter),
            entry,
            request,
        );
        Ok(handle)
    }

    /// Waits for the end of a run and returns its outcome. It is sugar over the end event: the wait wakes when the run is
    /// concluded and reported.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::UnknownRun`] for an unknown identifier, and [`DelegationError::Poisoned`] when a lock is
    /// poisoned.
    pub async fn wait(&self, run: &RunId) -> DelegationResult<SubagentOutcome> {
        let entry = self.find(run)?;
        let mut done = entry.done.subscribe();
        // The sender lives in `entry`, which this call holds: the wait can only end on the signal.
        let _signalled = done.wait_for(|finished| *finished).await.is_ok();
        outcome_of(&entry).ok_or(DelegationError::Launch)
    }

    /// Progress of a run: `Running`, or the outcome once it is concluded.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::UnknownRun`] for an unknown identifier, and [`DelegationError::Poisoned`] when a lock is
    /// poisoned.
    pub fn status(&self, run: &RunId) -> DelegationResult<RunProgress> {
        let entry = self.find(run)?;
        let outcome = outcome_of(&entry);
        Ok(outcome.map_or(RunProgress::Running, RunProgress::Ended))
    }

    /// Cancels a run and, through its children, the sub-agents it runs. It does not wait for the end of the run.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::UnknownRun`] for an unknown identifier, and [`DelegationError::Poisoned`] when the lock of
    /// the registry is poisoned.
    pub fn cancel(&self, run: &RunId) -> DelegationResult<()> {
        self.find(run)?.cancel.cancel();
        Ok(())
    }

    /// Records to persist with the main session: the runs in progress (those whose collector is not concluded), and the
    /// restored runs not resumed yet, which a later restore needs as much as the runs in progress.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when a lock is poisoned.
    pub fn records(&self) -> DelegationResult<Vec<RunRecord>> {
        let table = self.lock()?;
        let mut records = Vec::new();
        for entry in table.entries.values() {
            let listed = if entry.restored {
                table.pending.contains(&entry.run)
            } else {
                outcome_of(entry).is_none()
            };
            if listed {
                records.push(RunRecord::new(
                    entry.run.clone(),
                    entry.subagent.clone(),
                    entry.session.clone(),
                ));
            }
        }
        Ok(records)
    }

    /// Restores the runs of a main session from their records. Every record is checked first (sub-agent still allowed,
    /// identifier new and not repeated), so that nothing is changed or emitted when one is refused. Each run is then
    /// registered as interrupted, with the session of its sub-agent, and reported by `RunInterrupted` and the notice
    /// `ended`. An interrupted run is not resumed: the main agent starts a new run on its session.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::UnknownSubagent`] for a record whose sub-agent is not allowed,
    /// [`DelegationError::DuplicateRun`] for an identifier already known or repeated, and
    /// [`DelegationError::Poisoned`] when a lock is poisoned. Nothing is changed nor emitted when a record is refused.
    ///
    /// Each restored run is registered with its counter moved past its number, so that new runs are numbered after it.
    pub fn restore(&self, records: Vec<RunRecord>) -> DelegationResult<()> {
        let mut table = self.lock()?;
        let mut restored = Vec::with_capacity(records.len());
        let mut seen = BTreeSet::new();
        for record in records {
            let Some((schema, corrections)) = self.declared.get(record.subagent()) else {
                return Err(DelegationError::UnknownSubagent {
                    name: record.subagent().to_owned(),
                });
            };
            if table.entries.contains_key(record.run()) || !seen.insert(record.run().clone()) {
                return Err(DelegationError::DuplicateRun {
                    id: record.run().as_str().to_owned(),
                });
            }
            let collector = ResultCollector::new(record.subagent(), schema, *corrections)?;
            let _concluded = collector.conclude_as(
                SubagentStatus::Interrupted,
                INTERRUPTED_REASON,
                Some(record.session().clone()),
            )?;
            restored.push(Arc::new(Entry {
                run: record.run().clone(),
                subagent: record.subagent().to_owned(),
                session: record.session().clone(),
                restored: true,
                normalized: Normalized::new(record.subagent(), schema, *corrections)?,
                collector: Arc::new(collector),
                cancel: CancelHandle::new(),
                done: watch::channel(true).0,
            }));
        }
        for entry in &restored {
            if let Some(number) = run_number(&entry.run) {
                table.last_run = table.last_run.max(number);
            }
        }
        for entry in &restored {
            let _previous = table.entries.insert(entry.run.clone(), Arc::clone(entry));
            let _new_pending = table.pending.insert(entry.run.clone());
        }
        drop(table);
        for entry in &restored {
            self.reporter.interrupted(&entry.run, &entry.session);
        }
        Ok(())
    }

    /// Run registered under `run`.
    fn find(&self, run: &RunId) -> DelegationResult<Arc<Entry>> {
        let entry = self.lock()?.entries.get(run).cloned();
        entry.ok_or_else(|| DelegationError::UnknownRun {
            id: run.as_str().to_owned(),
        })
    }

    /// Locks the table. A poisoned lock is reported, never recovered: the table may hold a half-written run.
    fn lock(&self) -> DelegationResult<MutexGuard<'_, Table>> {
        self.table.lock().map_err(|_poisoned| DelegationError::Poisoned)
    }
}

/// Checks that `session` may be resumed for `subagent` (`agent-sessions/delegation#AC-10`) and returns the identifier of the
/// restored run that holds it. The session must be the one of a restored run of that sub-agent, else `UnknownSession`. No
/// run in progress may already hold it: the refusal is `DuplicateRun`, naming that run, so a session is resumed once while
/// that run lives. The restored run must not have been resumed already, else `UnknownSession`.
fn check_resumable(table: &Table, subagent: &str, session: &SessionId) -> DelegationResult<RunId> {
    let unknown = || DelegationError::UnknownSession {
        id: session.as_str().to_owned(),
    };
    let holds = |entry: &Entry| entry.subagent == subagent && entry.session == *session;
    if !table.entries.values().any(|entry| entry.restored && holds(entry)) {
        return Err(unknown());
    }
    for entry in table.entries.values() {
        if !entry.restored && holds(entry) && outcome_of(entry).is_none() {
            return Err(DelegationError::DuplicateRun {
                id: entry.run.as_str().to_owned(),
            });
        }
    }
    table
        .entries
        .values()
        .find(|entry| entry.restored && holds(entry) && table.pending.contains(&entry.run))
        .map(|entry| entry.run.clone())
        .ok_or_else(unknown)
}

/// Next identifier of a run: `run-<n>` with `n` increasing, skipping any identifier the table already knows (a restored
/// run keeps its identifier).
fn next_run_id(table: &mut Table) -> DelegationResult<RunId> {
    loop {
        let Some(number) = table.last_run.checked_add(1) else {
            // The numbering is exhausted: the last identifier tried is known, so it is refused as a duplicate.
            let last = table.last_run;
            return Err(DelegationError::DuplicateRun {
                id: format!("run-{last}"),
            });
        };
        table.last_run = number;
        // The text is never empty: the refusal of an empty identifier is reported as a launch failure.
        let Some(run) = RunId::new(format!("run-{number}")) else {
            return Err(DelegationError::Launch);
        };
        if !table.entries.contains_key(&run) {
            return Ok(run);
        }
    }
}

/// Session of a fresh sub-agent of `run` in the main session `parent`: `<main session>-<run>`, so that two main sessions
/// never give the same sub-agent session. The refusal of an empty identifier is reported as a launch failure, as for
/// [`next_run_id`].
fn session_of(parent: &SessionId, run: &RunId) -> DelegationResult<SessionId> {
    SessionId::new(format!("{}-{}", parent.as_str(), run.as_str())).ok_or(DelegationError::Launch)
}

/// Number of a run identifier of the form `run-<n>`, if it has that form.
fn run_number(run: &RunId) -> Option<u64> {
    run.as_str().strip_prefix("run-")?.parse::<u64>().ok()
}

/// Outcomes a run reports in place of the raw ones of its collector (`agent-sessions/delegation#FR-14`), built once, with
/// their fixed reasons: `failed` for an interruption outside a restore, `cancelled` for a cancellation that carried a
/// session (which it then loses), `unreadable` when the collector cannot be read.
struct Normalized {
    failed: SubagentOutcome,
    cancelled: SubagentOutcome,
    unreadable: SubagentOutcome,
}

impl Normalized {
    /// Builds the fixed outcomes of a sub-agent with `schema` and `corrections` corrections.
    fn new(subagent: &str, schema: &Value, corrections: u32) -> DelegationResult<Self> {
        let failed = ResultCollector::new(subagent, schema, corrections)?;
        fail(&failed, OUTSIDE_RESTORE_REASON)?;
        let cancelled = ResultCollector::new(subagent, schema, corrections)?;
        conclude_cancelled(&cancelled)?;
        let unreadable = ResultCollector::new(subagent, schema, corrections)?;
        fail(&unreadable, UNREADABLE_REASON)?;
        Ok(Self {
            failed: concluded(&failed)?,
            cancelled: concluded(&cancelled)?,
            unreadable: concluded(&unreadable)?,
        })
    }
}

/// Outcome of a concluded collector.
fn concluded(collector: &ResultCollector) -> DelegationResult<SubagentOutcome> {
    collector.outcome()?.ok_or(DelegationError::Launch)
}

/// Outcome a run reports, from the outcome `raw` its collector holds. A restored run keeps its `Interrupted` outcome. A
/// live run never reports `Interrupted` nor a cancellation that carries a session: those become the fixed outcomes of
/// `normalized`. A collector that cannot be read gives the fixed failure, never a raw outcome.
fn resolve(
    normalized: &Normalized,
    restored: bool,
    raw: DelegationResult<Option<SubagentOutcome>>,
) -> Option<SubagentOutcome> {
    match raw {
        Err(_poisoned) => Some(normalized.unreadable.clone()),
        Ok(None) => None,
        Ok(Some(outcome)) if restored => Some(outcome),
        Ok(Some(outcome)) => Some(match outcome.status() {
            SubagentStatus::Interrupted => normalized.failed.clone(),
            SubagentStatus::Cancelled if outcome.session().is_some() => normalized.cancelled.clone(),
            SubagentStatus::Valid | SubagentStatus::Failed | SubagentStatus::Cancelled => outcome,
        }),
    }
}

/// Outcome of a run as the registry reports it, `None` while it is not concluded.
fn outcome_of(entry: &Entry) -> Option<SubagentOutcome> {
    resolve(&entry.normalized, entry.restored, entry.collector.outcome())
}

/// Spawns the run in a detached task. The task concludes the run and reports it even when the launcher panics or the task
/// is stopped, so that no wait stays open. The finisher is built before the spawn: a task dropped before its first poll
/// drops its future, and with it the finisher, which then concludes the run.
fn spawn<L: SubagentLauncher>(
    launcher: Arc<L>,
    reporter: Arc<Reporter>,
    entry: Arc<Entry>,
    request: LaunchRequest,
) {
    let finisher = Finisher {
        entry: Some(entry),
        reporter,
    };
    drop(tokio::spawn(async move {
        let mut finisher = finisher;
        let returned = launcher.launch(request).await;
        finisher.finish(Some(returned));
    }));
}

/// Concludes a run and reports it. Its drop concludes an abandoned run.
struct Finisher {
    entry: Option<Arc<Entry>>,
    reporter: Arc<Reporter>,
}

impl Finisher {
    /// Concludes the run from what its launcher returned (`None` when it was abandoned), reports its end when the collector
    /// holds an outcome, and wakes the waiters. Only the first call has an effect.
    fn finish(&mut self, returned: Option<DelegationResult<SubagentOutcome>>) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        let _settled = settle(&entry, returned);
        if let Some(outcome) = outcome_of(&entry) {
            self.reporter.ended(&entry.run, outcome.status());
        }
        let _previous = entry.done.send_replace(true);
    }
}

impl Drop for Finisher {
    fn drop(&mut self) {
        self.finish(None);
    }
}

/// Concludes a run that its collector has not concluded. An outcome already set stands. Otherwise the cancellation of the
/// run comes first: a run cancelled by its caller is `Cancelled` whatever the launcher returned. Then a launcher that
/// reports itself cancelled gives `Cancelled`; a launch error gives a `Failed` with the launch reason; anything else
/// gives `Failed` with the reason of a run that ended without a result.
fn settle(entry: &Entry, returned: Option<DelegationResult<SubagentOutcome>>) -> DelegationResult<()> {
    if entry.collector.outcome()?.is_some() {
        return Ok(());
    }
    if entry.cancel.is_cancelled() {
        return conclude_cancelled(&entry.collector);
    }
    match returned {
        Some(Ok(outcome)) if outcome.status() == SubagentStatus::Cancelled => {
            conclude_cancelled(&entry.collector)
        }
        Some(Err(_)) => fail(&entry.collector, LAUNCH_FAILED_REASON),
        _ => fail(&entry.collector, NO_RESULT_REASON),
    }
}

/// Concludes a collector as cancelled, with no session.
fn conclude_cancelled(collector: &ResultCollector) -> DelegationResult<()> {
    let _concluded = collector.conclude_as(SubagentStatus::Cancelled, CANCELLED_REASON, None)?;
    Ok(())
}

/// Concludes a collector as failed, with a fixed reason.
fn fail(collector: &ResultCollector, reason: &str) -> DelegationResult<()> {
    let _concluded = collector.fail(reason)?;
    Ok(())
}

/// Drives a batch run (`agent-sessions/delegation#FR-4`).
///
/// The first message of the sub-agent is `task`. After each turn that leaves the collector unconcluded, the fixed reminder
/// asks for the result again, in the same run. The run stops when the collector is concluded, when a turn fails or is
/// cancelled, or after `BATCH_MAX_TURNS` turns without a result. The outcome is the one of the collector: `Valid` needs
/// an accepted result.
///
/// # Errors
///
/// Returns [`DelegationError::Poisoned`] when a lock of the collector is poisoned.
pub async fn run_batch<P, C, S>(
    runner: &mut TurnRunner<P, C, S>,
    task: &str,
    collector: &ResultCollector,
    cancel: &CancelHandle,
) -> DelegationResult<SubagentOutcome>
where
    P: InferenceProvider,
    C: CompactionStrategy,
    S: SessionStore,
{
    let mut message: &str = task;
    for _turn in 0..BATCH_MAX_TURNS {
        match runner
            .run_turn(message, cancel)
            .await
            .map(|output| output.outcome())
        {
            Ok(Outcome::Valid) => {}
            Ok(Outcome::Cancelled) => {
                conclude_cancelled(collector)?;
                break;
            }
            Ok(Outcome::Failed) | Err(_) => {
                fail(collector, TURN_FAILED_REASON)?;
                break;
            }
        }
        if collector.outcome()?.is_some() {
            break;
        }
        message = REMINDER;
    }
    if collector.outcome()?.is_none() {
        fail(collector, NO_RESULT_IN_TURNS_REASON)?;
    }
    collector.outcome()?.ok_or(DelegationError::Launch)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::{
        INTERRUPTED_REASON, LAUNCH_FAILED_REASON, LaunchRequest, NO_RESULT_IN_TURNS_REASON, NO_RESULT_REASON,
        REMINDER, RunProgress, RunRecord, RunRegistry, SubagentLauncher, run_batch,
    };
    use super::{Normalized, resolve};
    use crate::agents::{
        ErrorClass, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse, ModelProfile,
    };
    use crate::clock::{ManualClock, Timestamp};
    use crate::context::{Budget, ContextResult, DefaultStrategy, Digest, ReplayMode, Summarizer};
    use crate::delegation::{
        DEFAULT_CORRECTIONS, DelegationError, EventHub, ResultCollector, SubagentOutcome, SubagentStatus,
        Submission,
    };
    use crate::runtime::{TurnConfig, TurnRunner};
    use crate::scratchpad::{Grants, Scratchpad, ScratchpadEngine, ScratchpadLimits};
    use crate::session::{
        AgentRef, CancelHandle, Event, EventKind, EventSink, InMemoryStore, Message, ModelSnapshot, Outcome,
        RecordingSink, Redactor, RunId, SegmentContext, SessionId, SessionState,
    };

    /// Identifier of the main session of the tests.
    const MAIN: &str = "main-session";
    /// A secret of the task: it must never reach an event, a notice or an error.
    const TASK_SECRET: &str = "tok-live-4f2a9c";
    /// The answer a scripted sub-agent submits.
    const ANSWER: &str = "answer-7c1e";

    // --- doubles -------------------------------------------------------------------------------------------------

    /// What a scripted sub-agent does once launched, chosen by its name.
    #[derive(Clone, Copy)]
    enum Behaviour {
        /// Waits for a release, submits `ANSWER` and returns the outcome of its collector.
        Answer,
        /// Waits for the cancellation of its run, then gives up.
        AwaitCancel,
        /// Gives up at once, without concluding its collector.
        Refuse,
        /// Returns a cancelled outcome of its own collector, which the run's collector does not hold.
        ReportCancelled,
        /// Returns a valid outcome of its own collector, which the run's collector does not hold.
        ReportStray,
        /// Panics, without concluding its collector.
        Panic,
        /// Concludes its collector as interrupted, with a session, as a host could do by mistake.
        Interrupting,
        /// Concludes its collector as cancelled, with a session.
        CancellingWithSession,
        /// Concludes its collector as interrupted with a session, then keeps running until released.
        InterruptingLinger,
    }

    fn behaviour_of(subagent: &str) -> Behaviour {
        match subagent {
            "awaiting" => Behaviour::AwaitCancel,
            "refusing" => Behaviour::Refuse,
            "reporting-cancel" => Behaviour::ReportCancelled,
            "reporting-stray" => Behaviour::ReportStray,
            "panicking" => Behaviour::Panic,
            "interrupting" => Behaviour::Interrupting,
            "interrupting-linger" => Behaviour::InterruptingLinger,
            "cancelling-with-session" => Behaviour::CancellingWithSession,
            _ => Behaviour::Answer,
        }
    }

    /// Launcher double: counts its calls, records the session and the resumption of each request, and follows the
    /// behaviour of its sub-agent.
    struct Scripted {
        release: Arc<Notify>,
        launches: AtomicUsize,
        requests: Mutex<Vec<(SessionId, bool)>>,
        debugs: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new() -> Self {
            Self {
                release: Arc::new(Notify::new()),
                launches: AtomicUsize::new(0),
                requests: Mutex::new(Vec::new()),
                debugs: Mutex::new(Vec::new()),
            }
        }

        /// Lets one answering run finish. A release given before the launch is kept.
        fn release(&self) {
            self.release.notify_one();
        }

        fn launches(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
        }

        fn requests(&self) -> Vec<(SessionId, bool)> {
            lock(&self.requests).clone()
        }

        fn debugs(&self) -> Vec<String> {
            lock(&self.debugs).clone()
        }
    }

    impl SubagentLauncher for Scripted {
        fn launch(
            &self,
            request: LaunchRequest,
        ) -> impl Future<Output = Result<SubagentOutcome, DelegationError>> + Send {
            let _before = self.launches.fetch_add(1, Ordering::SeqCst);
            lock(&self.requests).push((request.session().clone(), request.resumed()));
            lock(&self.debugs).push(format!("{request:?}"));
            perform(request, Arc::clone(&self.release))
        }
    }

    /// Runs the sub-agent of `request` as its behaviour says.
    async fn perform(
        request: LaunchRequest,
        release: Arc<Notify>,
    ) -> Result<SubagentOutcome, DelegationError> {
        match behaviour_of(request.subagent()) {
            Behaviour::Answer => {
                release.notified().await;
                let _submission = request.collector().submit(json!({"answer": ANSWER}))?;
                request.collector().outcome()?.ok_or(DelegationError::Launch)
            }
            Behaviour::AwaitCancel => {
                request.cancel().cancelled().await;
                Err(DelegationError::Launch)
            }
            Behaviour::Refuse => Err(DelegationError::Launch),
            Behaviour::ReportCancelled => {
                let own = ResultCollector::with_default_corrections(request.subagent(), &answer_schema())?;
                let _concluded = own.conclude_as(SubagentStatus::Cancelled, "own", None)?;
                own.outcome()?.ok_or(DelegationError::Launch)
            }
            Behaviour::ReportStray => {
                let own = ResultCollector::with_default_corrections(request.subagent(), &answer_schema())?;
                let _submission = own.submit(json!({"answer": ANSWER}))?;
                own.outcome()?.ok_or(DelegationError::Launch)
            }
            Behaviour::Panic => panic!("the launcher panics on purpose"),
            Behaviour::Interrupting => {
                let session = SessionId::new("other-session").ok_or(DelegationError::Launch)?;
                let _concluded = request.collector().conclude_as(
                    SubagentStatus::Interrupted,
                    "the host reported an interruption",
                    Some(session),
                )?;
                request.collector().outcome()?.ok_or(DelegationError::Launch)
            }
            Behaviour::InterruptingLinger => {
                let session = SessionId::new("other-session").ok_or(DelegationError::Launch)?;
                let _concluded = request.collector().conclude_as(
                    SubagentStatus::Interrupted,
                    "the host reported an interruption",
                    Some(session),
                )?;
                release.notified().await;
                Err(DelegationError::Launch)
            }
            Behaviour::CancellingWithSession => {
                let session = SessionId::new("other-session").ok_or(DelegationError::Launch)?;
                let _concluded = request.collector().conclude_as(
                    SubagentStatus::Cancelled,
                    "the host reported a cancellation",
                    Some(session),
                )?;
                request.collector().outcome()?.ok_or(DelegationError::Launch)
            }
        }
    }

    /// Waits for a run at most five seconds: a registry that never concludes a run fails the test instead of hanging it.
    async fn bounded_wait(registry: &RunRegistry<Scripted>, run: &RunId) -> SubagentOutcome {
        tokio::time::timeout(Duration::from_secs(5), registry.wait(run))
            .await
            .expect("the run is concluded within five seconds")
            .expect("the run is known")
    }

    /// Locks a test mutex, recovering a poisoned one: a test double keeps its record whatever a failing test did.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn answer_schema() -> Value {
        json!({"type": "object", "required": ["answer"], "properties": {"answer": {"type": "string"}}})
    }

    /// Declared sub-agents, each with the answer schema and the default corrections.
    fn declared(names: &[&str]) -> BTreeMap<String, (Value, u32)> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), (answer_schema(), DEFAULT_CORRECTIONS)))
            .collect()
    }

    fn main_session() -> SessionId {
        SessionId::new(MAIN).expect("a non-empty text makes a session identifier")
    }

    fn run_id(text: &str) -> RunId {
        RunId::new(text).expect("a non-empty text makes a run identifier")
    }

    fn sample_record(run: &str, subagent: &str, session: &str) -> RunRecord {
        RunRecord::new(
            run_id(run),
            subagent.to_owned(),
            SessionId::new(session).expect("a non-empty text makes a session identifier"),
        )
    }

    /// A registry with its sinks and launcher, kept by the test.
    struct Rig {
        registry: RunRegistry<Scripted>,
        sink: Arc<RecordingSink>,
        hub: Arc<EventHub>,
        launcher: Arc<Scripted>,
    }

    fn rig(names: &[&str]) -> Rig {
        rig_with(
            names,
            Arc::new(RecordingSink::new()),
            Arc::new(EventHub::new()),
            Arc::new(Scripted::new()),
        )
    }

    fn rig_with(
        names: &[&str],
        sink: Arc<RecordingSink>,
        hub: Arc<EventHub>,
        launcher: Arc<Scripted>,
    ) -> Rig {
        let shared_sink: Arc<dyn EventSink> = Arc::<RecordingSink>::clone(&sink);
        let registry = RunRegistry::new(
            Arc::clone(&launcher),
            declared(names),
            shared_sink,
            Arc::new(ManualClock::new(Timestamp::from_millis(1_000))),
            Arc::clone(&hub),
            main_session(),
        )
        .expect("the answer schema compiles");
        Rig {
            registry,
            sink,
            hub,
            launcher,
        }
    }

    /// Runs of the recorded events that ended, with their outcome.
    fn run_ended(sink: &RecordingSink) -> Vec<(RunId, Outcome)> {
        sink.events()
            .into_iter()
            .filter_map(|event| {
                let EventKind::RunEnded { run, outcome } = event.kind else {
                    return None;
                };
                Some((run, outcome))
            })
            .collect()
    }

    /// Runs of the recorded events that were interrupted, with the session of their sub-agent.
    fn interrupted_runs(sink: &RecordingSink) -> Vec<(RunId, SessionId)> {
        sink.events()
            .into_iter()
            .filter_map(|event| {
                let EventKind::RunInterrupted { run, session } = event.kind else {
                    return None;
                };
                Some((run, session))
            })
            .collect()
    }

    /// Notices `ended` of the event hub, as (run, status). Draining the hub: each call sees only what is new.
    fn ended_notices(hub: &EventHub) -> Vec<(String, String)> {
        let events = hub.poll().expect("the hub polls");
        events
            .into_iter()
            .filter(|event| event.source() == "subagent" && event.name() == "ended")
            .map(|event| {
                let text = |key: &str| {
                    event
                        .data()
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                (text("run"), text("status"))
            })
            .collect()
    }

    /// Yields to the runtime until `condition` holds, for at most a thousand rounds.
    async fn settle_until(condition: impl Fn() -> bool) {
        for _round in 0..1_000 {
            if condition() {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    // --- start, wait, end -----------------------------------------------------------------------------------------

    // Scenario: délégation immédiate et événement de fin
    #[tokio::test]
    async fn start_hands_back_a_handle_before_the_run_ends() {
        let rig = rig(&["answering"]);
        let cancel = CancelHandle::new();
        let handle = rig
            .registry
            .start("answering", "task", &cancel)
            .expect("a declared sub-agent starts");
        assert_eq!(
            rig.registry.status(handle.run()).expect("the run is known"),
            RunProgress::Running,
            "the run must still be running when the handle comes back"
        );
        assert!(
            run_ended(&rig.sink).is_empty(),
            "no RunEnded before the end of the run"
        );
        assert!(
            ended_notices(&rig.hub).is_empty(),
            "no ended notice before the end of the run"
        );
        rig.launcher.release();
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Valid,
            "the answering sub-agent concludes Valid"
        );
        assert_eq!(
            run_ended(&rig.sink),
            vec![(handle.run().clone(), Outcome::Valid)],
            "RunEnded carries the run and its outcome"
        );
        assert_eq!(
            ended_notices(&rig.hub),
            vec![(handle.run().as_str().to_owned(), "valid".to_owned())],
            "the source subagent receives the notice ended with the run and its status"
        );
    }

    // Scenario: sous-agent non autorisé
    #[tokio::test]
    async fn start_refuses_a_subagent_outside_the_declared_list() {
        let rig = rig(&["extractor"]);
        let refused = rig.registry.start("reviewer", "task", &CancelHandle::new());
        let error = refused.err();
        assert!(
            matches!(&error, Some(DelegationError::UnknownSubagent { name }) if name == "reviewer"),
            "the refusal must name the sub-agent that is not allowed"
        );
        for _round in 0..3 {
            tokio::task::yield_now().await;
        }
        assert!(
            rig.registry.records().expect("records").is_empty(),
            "no run must exist after the refusal"
        );
        assert_eq!(
            rig.launcher.launches(),
            0,
            "the launcher must never be called for a refused sub-agent"
        );
    }

    // Scenario: attente synchrone
    #[tokio::test]
    async fn wait_returns_the_final_outcome_of_the_run() {
        let rig = rig(&["answering"]);
        let handle = rig
            .registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        rig.launcher.release();
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Valid,
            "the outcome is the final one"
        );
        assert_eq!(
            outcome.result(),
            Some(&json!({"answer": ANSWER})),
            "the validated result is in the outcome"
        );
    }

    // Scenario: échec de lancement
    #[tokio::test]
    async fn a_launch_error_fails_the_run_with_a_fixed_reason() {
        let rig = rig(&["refusing"]);
        let handle = rig
            .registry
            .start("refusing", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "a run whose launch failed is Failed"
        );
        assert_eq!(
            outcome.reason(),
            Some(LAUNCH_FAILED_REASON),
            "the reason is the fixed launch reason"
        );
        assert_eq!(
            run_ended(&rig.sink),
            vec![(handle.run().clone(), Outcome::Failed)],
            "RunEnded is emitted for the failed run"
        );
    }

    // Scenario: un registre détruit ne tue pas ses runs
    #[tokio::test]
    async fn dropping_the_registry_leaves_its_runs_running() {
        let rig = rig(&["answering"]);
        let handle = rig
            .registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let Rig {
            registry,
            sink,
            hub,
            launcher,
        } = rig;
        drop(registry);
        launcher.release();
        settle_until(|| !run_ended(&sink).is_empty()).await;
        assert_eq!(
            run_ended(&sink),
            vec![(handle.run().clone(), Outcome::Valid)],
            "the run finishes and emits RunEnded after the registry is gone"
        );
        assert_eq!(
            ended_notices(&hub),
            vec![(handle.run().as_str().to_owned(), "valid".to_owned())],
            "the notice is still sent after the registry is gone"
        );
    }

    // --- cancellation ---------------------------------------------------------------------------------------------

    // Scenario: annulation propagée
    #[tokio::test]
    async fn cancelling_the_caller_cancels_every_run() {
        let rig = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let first = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let second = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        caller.cancel();
        for handle in [&first, &second] {
            let outcome = bounded_wait(&rig.registry, handle.run()).await;
            assert_eq!(
                outcome.status(),
                SubagentStatus::Cancelled,
                "each run under the cancelled caller is Cancelled"
            );
        }
    }

    // Scenario: annulation d'un seul run
    #[tokio::test]
    async fn cancelling_one_run_leaves_the_others_running() {
        let rig = rig(&["awaiting", "answering"]);
        let cancelled = rig
            .registry
            .start("awaiting", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let running = rig
            .registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        rig.registry.cancel(cancelled.run()).expect("the run is known");
        let outcome = bounded_wait(&rig.registry, cancelled.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Cancelled,
            "the cancelled run finishes Cancelled"
        );
        assert_eq!(
            run_ended(&rig.sink),
            vec![(cancelled.run().clone(), Outcome::Cancelled)],
            "only the cancelled run has ended, and it is reported Cancelled"
        );
        assert_eq!(
            rig.registry.status(running.run()).expect("the run is known"),
            RunProgress::Running,
            "the other run is not touched by the cancellation"
        );
        rig.launcher.release();
        let other = bounded_wait(&rig.registry, running.run()).await;
        assert_eq!(
            other.status(),
            SubagentStatus::Valid,
            "the other run finishes normally"
        );
    }

    // Scenario: annulation sans session (invariant : une session seulement pour Interrupted)
    #[tokio::test]
    async fn only_an_interrupted_outcome_carries_a_session() {
        let rig = rig(&["awaiting", "refusing", "answering"]);
        let caller = CancelHandle::new();
        let cancelled = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        caller.cancel();
        let failed = rig
            .registry
            .start("refusing", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let valid = rig
            .registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        rig.launcher.release();
        for handle in [&cancelled, &failed, &valid] {
            let outcome = bounded_wait(&rig.registry, handle.run()).await;
            assert!(
                outcome.session().is_none(),
                "a run that is not interrupted carries no session, got status {:?}",
                outcome.status()
            );
        }
    }

    // --- unknown runs ---------------------------------------------------------------------------------------------

    // Scenario: statut d'un run inconnu
    #[tokio::test]
    async fn an_unknown_run_is_refused_by_status_wait_and_cancel() {
        let rig = rig(&["answering"]);
        let unknown = run_id("run-99");
        assert!(
            matches!(
                rig.registry.status(&unknown),
                Err(DelegationError::UnknownRun { .. })
            ),
            "status must refuse an unknown run"
        );
        assert!(
            matches!(
                rig.registry.cancel(&unknown),
                Err(DelegationError::UnknownRun { .. })
            ),
            "cancel must refuse an unknown run"
        );
        assert!(
            matches!(
                rig.registry.wait(&unknown).await,
                Err(DelegationError::UnknownRun { .. })
            ),
            "wait must refuse an unknown run"
        );
    }

    // --- records and restore --------------------------------------------------------------------------------------

    // Scenario: records ne liste que les runs en cours
    #[tokio::test]
    async fn records_list_only_the_runs_in_progress() {
        let rig = rig(&["answering", "awaiting"]);
        let done = rig
            .registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        rig.launcher.release();
        let _finished = bounded_wait(&rig.registry, done.run()).await;
        let caller = CancelHandle::new();
        let running = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let records = rig.registry.records().expect("records");
        let runs: Vec<RunId> = records.iter().map(|record| record.run().clone()).collect();
        assert_eq!(
            runs,
            vec![running.run().clone()],
            "only the run in progress is persisted"
        );
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, running.run()).await;
    }

    // Scenario: run interrompu à la restauration
    #[tokio::test]
    async fn an_interrupted_run_names_the_session_of_its_sub_agent() {
        let first = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let handle = first
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let records = first.registry.records().expect("records");
        let [record] = records.as_slice() else {
            panic!("one run is in progress, so one record is persisted");
        };
        let sub_session = record.session().clone();
        let sink = Arc::new(RecordingSink::new());
        let hub = Arc::new(EventHub::new());
        let second = rig_with(
            &["awaiting"],
            Arc::clone(&sink),
            Arc::clone(&hub),
            Arc::new(Scripted::new()),
        );
        second.registry.restore(records).expect("the record is restored");
        assert_eq!(
            interrupted_runs(&sink),
            vec![(handle.run().clone(), sub_session.clone())],
            "RunInterrupted names the run and the session of its sub-agent"
        );
        assert_eq!(
            ended_notices(&hub),
            vec![(handle.run().as_str().to_owned(), "interrupted".to_owned())],
            "the source subagent receives the status interrupted"
        );
        let progress = second
            .registry
            .status(handle.run())
            .expect("the restored run is known");
        let RunProgress::Ended(outcome) = progress else {
            panic!("a restored run is concluded, it is not resumed by itself");
        };
        assert_eq!(
            outcome.status(),
            SubagentStatus::Interrupted,
            "the restored run is Interrupted"
        );
        assert_eq!(
            outcome.session(),
            Some(&sub_session),
            "the outcome of an interrupted run carries its session"
        );
        assert_eq!(
            outcome.reason(),
            Some(INTERRUPTED_REASON),
            "the reason is the fixed interruption reason"
        );
        caller.cancel();
    }

    // Scenario: relance d'un run interrompu
    #[tokio::test]
    async fn resuming_hands_the_saved_session_to_the_launcher() {
        let first = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let _started = first
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let records = first.registry.records().expect("records");
        let sub_session = records
            .first()
            .map(|record| record.session().clone())
            .expect("one record is persisted");
        let second = rig(&["awaiting"]);
        second.registry.restore(records).expect("the record is restored");
        let resumed_caller = CancelHandle::new();
        let resumed = second
            .registry
            .resume("awaiting", "task", &resumed_caller, &sub_session)
            .expect("a declared sub-agent resumes");
        settle_until(|| second.launcher.requests().len() == 1).await;
        assert_eq!(
            second.launcher.requests(),
            vec![(sub_session.clone(), true)],
            "the launcher receives the saved session, flagged as resumed"
        );
        resumed_caller.cancel();
        let outcome = bounded_wait(&second.registry, resumed.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Cancelled,
            "the resumed run finishes once its caller cancels it"
        );
        caller.cancel();
    }

    // Scenario: restauration refusée sans modification (sous-agent non autorisé)
    #[test]
    fn restore_refuses_an_unknown_subagent_without_any_event() {
        let rig = rig(&["answering"]);
        let records = vec![
            sample_record("run-1", "ghost", "ghost-session"),
            sample_record("run-2", "answering", "answering-session"),
        ];
        let refused = rig.registry.restore(records);
        assert!(
            matches!(&refused, Err(DelegationError::UnknownSubagent { name }) if name == "ghost"),
            "the refusal names the sub-agent that is not allowed"
        );
        assert!(
            interrupted_runs(&rig.sink).is_empty(),
            "no RunInterrupted is emitted by a refused restore"
        );
        assert!(
            ended_notices(&rig.hub).is_empty(),
            "no notice is sent by a refused restore"
        );
        assert!(
            matches!(
                rig.registry.status(&run_id("run-2")),
                Err(DelegationError::UnknownRun { .. })
            ),
            "the valid record of a refused restore is not restored either"
        );
    }

    // Scenario: restauration refusée après un enregistrement valide, sans modification
    #[test]
    fn restore_refuses_an_unknown_subagent_after_a_valid_record_without_change() {
        let rig = rig(&["answering"]);
        let records = vec![
            sample_record("run-2", "answering", "answering-session"),
            sample_record("run-1", "ghost", "ghost-session"),
        ];
        let refused = rig.registry.restore(records);
        assert!(
            matches!(&refused, Err(DelegationError::UnknownSubagent { name }) if name == "ghost"),
            "the refusal names the sub-agent that is not allowed"
        );
        assert!(
            interrupted_runs(&rig.sink).is_empty(),
            "the valid record before the refusal is not reported"
        );
        assert!(
            matches!(
                rig.registry.status(&run_id("run-2")),
                Err(DelegationError::UnknownRun { .. })
            ),
            "the valid record before the refusal is not registered"
        );
    }

    // Scenario: restauration refusée pour un identifiant répété
    #[test]
    fn restore_refuses_a_repeated_run_identifier() {
        let rig = rig(&["answering"]);
        let records = vec![
            sample_record("run-1", "answering", "session-a"),
            sample_record("run-1", "answering", "session-b"),
        ];
        let refused = rig.registry.restore(records);
        assert!(
            matches!(&refused, Err(DelegationError::DuplicateRun { id }) if id == "run-1"),
            "the refusal names the repeated identifier"
        );
        assert!(
            interrupted_runs(&rig.sink).is_empty(),
            "no RunInterrupted is emitted by a refused restore"
        );
        assert!(
            matches!(
                rig.registry.status(&run_id("run-1")),
                Err(DelegationError::UnknownRun { .. })
            ),
            "the refused restore is atomic: run-1 stays unknown, even the first record that was valid"
        );
        assert!(
            rig.registry.records().expect("records").is_empty(),
            "the refused restore registers no run to persist"
        );
    }

    // Scenario: les nouveaux runs ne réutilisent pas un identifiant restauré
    #[tokio::test]
    async fn new_runs_never_reuse_a_restored_identifier() {
        let rig = rig(&["answering"]);
        rig.registry
            .restore(vec![sample_record("run-1", "answering", "session-a")])
            .expect("the record is restored");
        let caller = CancelHandle::new();
        let handle = rig
            .registry
            .start("answering", "task", &caller)
            .expect("a declared sub-agent starts");
        assert_ne!(
            handle.run().as_str(),
            "run-1",
            "a new run must not take the identifier of a restored run"
        );
        caller.cancel();
    }

    // --- construction and secrets ---------------------------------------------------------------------------------

    // Scenario: schéma invalide refusé à la construction
    #[test]
    fn an_invalid_schema_is_refused_when_the_registry_is_built() {
        let names: BTreeMap<String, (Value, u32)> = BTreeMap::from([(
            "broken".to_owned(),
            (json!({"type": "entier"}), DEFAULT_CORRECTIONS),
        )]);
        let built = RunRegistry::new(
            Arc::new(Scripted::new()),
            names,
            Arc::new(RecordingSink::new()),
            Arc::new(ManualClock::new(Timestamp::from_millis(0))),
            Arc::new(EventHub::new()),
            main_session(),
        );
        assert!(
            matches!(&built, Err(DelegationError::InvalidSchema { subagent, .. }) if subagent == "broken"),
            "a schema that does not compile is refused, naming the sub-agent"
        );
    }

    // Scenario: aucun secret dans les événements
    #[tokio::test]
    async fn no_secret_reaches_the_events_or_the_notices() {
        let rig = rig(&["answering"]);
        let handle = rig
            .registry
            .start("answering", TASK_SECRET, &CancelHandle::new())
            .expect("a declared sub-agent starts");
        rig.launcher.release();
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.result(),
            Some(&json!({"answer": ANSWER})),
            "the result is kept in the outcome, read by wait"
        );
        let events = format!("{:?}", rig.sink.events());
        let notices = format!("{:?}", ended_notices(&rig.hub));
        assert!(
            !events.contains(TASK_SECRET) && !notices.contains(TASK_SECRET),
            "the task must not reach the events or the notices"
        );
        assert!(
            !events.contains(ANSWER) && !notices.contains(ANSWER),
            "the result must not reach the events or the notices"
        );
    }

    // Scenario: un lanceur qui rend une annulation
    #[tokio::test]
    async fn a_launcher_that_reports_cancellation_concludes_cancelled() {
        let rig = rig(&["reporting-cancel"]);
        let handle = rig
            .registry
            .start("reporting-cancel", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Cancelled,
            "a cancellation reported by the launcher concludes the run Cancelled"
        );
        assert!(outcome.session().is_none(), "a cancelled run carries no session");
        assert_eq!(
            run_ended(&rig.sink),
            vec![(handle.run().clone(), Outcome::Cancelled)],
            "RunEnded reports the cancellation"
        );
    }

    // Scenario: un lanceur qui rend sans conclure le collecteur du run
    #[tokio::test]
    async fn a_launcher_that_returns_without_concluding_the_run_fails_it() {
        let rig = rig(&["reporting-stray"]);
        let handle = rig
            .registry
            .start("reporting-stray", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "a launcher that returns an outcome the run does not hold leaves the run Failed"
        );
        assert_eq!(
            outcome.reason(),
            Some(NO_RESULT_REASON),
            "the reason is the fixed reason of a run without a result"
        );
        assert_eq!(
            run_ended(&rig.sink),
            vec![(handle.run().clone(), Outcome::Failed)],
            "RunEnded reports the failure"
        );
    }

    // Scenario: un lanceur qui panique ne laisse pas l'attente ouverte
    #[tokio::test]
    async fn a_panicking_launcher_still_concludes_its_run() {
        let rig = rig(&["panicking"]);
        let handle = rig
            .registry
            .start("panicking", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "an abandoned run is concluded Failed"
        );
        assert_eq!(
            outcome.reason(),
            Some(NO_RESULT_REASON),
            "the reason is the fixed reason of a run without a result"
        );
        assert_eq!(
            ended_notices(&rig.hub),
            vec![(handle.run().as_str().to_owned(), "failed".to_owned())],
            "the notice ended still reaches the hub after the panic"
        );
    }

    // --- red-phase tests (lot D6 corrections) ---------------------------------------------------------------------

    /// Builds a registry for the main session `parent`, with its own sink, hub and launcher, and returns the launcher too.
    fn registry_for(
        parent: &str,
        names: &[&str],
        sink: Arc<dyn EventSink>,
        hub: Arc<EventHub>,
    ) -> (RunRegistry<Scripted>, Arc<Scripted>) {
        let launcher = Arc::new(Scripted::new());
        let registry = RunRegistry::new(
            Arc::clone(&launcher),
            declared(names),
            sink,
            Arc::new(ManualClock::new(Timestamp::from_millis(1_000))),
            hub,
            SessionId::new(parent).expect("a non-empty text makes a session identifier"),
        )
        .expect("the answer schema compiles");
        (registry, launcher)
    }

    // Scenario: un identifiant déjà connu est refusé par restore (DuplicateRun)
    #[tokio::test]
    async fn a_restore_refuses_an_identifier_already_known_to_the_registry() {
        let rig = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let live = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let refused = rig.registry.restore(vec![sample_record(
            live.run().as_str(),
            "awaiting",
            "session-of-the-record",
        )]);
        assert!(
            matches!(&refused, Err(DelegationError::DuplicateRun { id }) if id == "run-1"),
            "a run already known is refused with DuplicateRun naming it"
        );
        assert!(
            interrupted_runs(&rig.sink).is_empty(),
            "no RunInterrupted is emitted for a refused restore"
        );
        caller.cancel();
    }

    // Scenario: un identifiant répété dans une restauration est refusé par DuplicateRun
    #[test]
    fn restore_refuses_a_repeated_identifier_with_duplicate_run() {
        let rig = rig(&["answering"]);
        let refused = rig.registry.restore(vec![
            sample_record("run-1", "answering", "session-a"),
            sample_record("run-1", "answering", "session-b"),
        ]);
        assert!(
            matches!(&refused, Err(DelegationError::DuplicateRun { id }) if id == "run-1"),
            "a repeated identifier is refused with DuplicateRun"
        );
    }

    // Scenario: la session du sous-agent porte la session principale et le run
    #[tokio::test]
    async fn the_sub_agent_session_names_its_main_session_and_its_run() {
        let rig = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let handle = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        settle_until(|| rig.launcher.requests().len() == 1).await;
        assert_eq!(
            rig.launcher.requests(),
            vec![(
                SessionId::new("main-session-run-1").expect("a non-empty identifier"),
                false
            )],
            "a fresh sub-agent session is `<main session>-<run>`, here main-session-run-1"
        );
        assert_eq!(handle.run().as_str(), "run-1", "the run is numbered run-1");
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, handle.run()).await;
    }

    // Scenario: deux sessions principales ne produisent jamais la même session de sous-agent
    #[tokio::test]
    async fn two_main_sessions_never_share_a_sub_agent_session() {
        let hub = Arc::new(EventHub::new());
        let sink: Arc<dyn EventSink> = Arc::new(RecordingSink::new());
        let (first, first_launcher) =
            registry_for("main-a", &["awaiting"], Arc::clone(&sink), Arc::clone(&hub));
        let (second, second_launcher) =
            registry_for("main-b", &["awaiting"], Arc::clone(&sink), Arc::clone(&hub));
        let caller = CancelHandle::new();
        let one = first
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let other = second
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        settle_until(|| first_launcher.requests().len() == 1 && second_launcher.requests().len() == 1).await;
        let first_session = first_launcher.requests();
        let second_session = second_launcher.requests();
        assert_ne!(
            first_session, second_session,
            "two main sessions must not give the same sub-agent session"
        );
        assert_eq!(
            one.run().as_str(),
            other.run().as_str(),
            "the run identifiers may repeat across registries: only the session must be unique"
        );
        caller.cancel();
        let _first_done = bounded_wait(&first, one.run()).await;
        let _second_done = bounded_wait(&second, other.run()).await;
    }

    // Scenario: le compteur de runs reste au-dessus des identifiants restaurés
    #[tokio::test]
    async fn a_new_run_numbers_past_a_restored_identifier() {
        let rig = rig(&["awaiting"]);
        rig.registry
            .restore(vec![sample_record("run-7", "awaiting", "session-7")])
            .expect("the record is restored");
        let caller = CancelHandle::new();
        let handle = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        assert_eq!(
            handle.run().as_str(),
            "run-8",
            "the counter continues after the restored run-7"
        );
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, handle.run()).await;
    }

    // Scenario: resume refuse une session qui n'appartient pas à un run restauré de ce sous-agent
    #[tokio::test]
    async fn resume_refuses_a_session_of_no_restored_run_of_that_subagent() {
        let rig = rig(&["awaiting", "answering"]);
        rig.registry
            .restore(vec![sample_record("run-7", "awaiting", "session-7")])
            .expect("the record is restored");
        let unknown = SessionId::new("session-unknown").expect("a non-empty identifier");
        let refused = rig
            .registry
            .resume("awaiting", "task", &CancelHandle::new(), &unknown);
        assert!(
            matches!(&refused, Err(DelegationError::UnknownSession { id }) if id == "session-unknown"),
            "a session of no restored run is refused, and the error names it"
        );
        let other_subagent = SessionId::new("session-7").expect("a non-empty identifier");
        let refused_other = rig
            .registry
            .resume("answering", "task", &CancelHandle::new(), &other_subagent);
        assert!(
            matches!(&refused_other, Err(DelegationError::UnknownSession { .. })),
            "a session restored for another sub-agent is refused too"
        );
        assert!(
            rig.launcher.requests().is_empty(),
            "a refused resume never reaches the launcher"
        );
    }

    // Scenario: un lanceur qui conclut Interrupted hors restauration est ramené à Failed, sans session
    #[tokio::test]
    async fn a_launcher_reporting_an_interruption_is_normalized_to_failed() {
        let rig = rig(&["interrupting"]);
        let handle = rig
            .registry
            .start("interrupting", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "an interruption outside a restore is Failed"
        );
        assert_eq!(
            outcome.reason(),
            Some("the sub-agent reported an interruption outside a restore"),
            "the reason is the fixed normalization reason, not the host text"
        );
        assert!(outcome.session().is_none(), "no session outside Interrupted");
        assert_eq!(
            run_ended(&rig.sink),
            vec![(handle.run().clone(), Outcome::Failed)],
            "RunEnded reports the normalized failure"
        );
    }

    // Scenario: une annulation qui porte une session la perd côté registre
    #[tokio::test]
    async fn a_cancellation_carrying_a_session_loses_it() {
        let rig = rig(&["cancelling-with-session"]);
        let handle = rig
            .registry
            .start("cancelling-with-session", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let outcome = bounded_wait(&rig.registry, handle.run()).await;
        assert_eq!(outcome.status(), SubagentStatus::Cancelled, "the status is kept");
        assert!(
            outcome.session().is_none(),
            "a session is only carried by Interrupted"
        );
    }

    // Scenario: la fin d'un run et l'interruption portent la session principale
    #[tokio::test]
    async fn run_end_events_carry_the_main_session() {
        let rig = rig(&["answering", "awaiting"]);
        let done = rig
            .registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        rig.launcher.release();
        let _finished = bounded_wait(&rig.registry, done.run()).await;
        let caller = CancelHandle::new();
        let running = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let records = rig.registry.records().expect("records");
        let second_sink = Arc::new(RecordingSink::new());
        let second = rig_with(
            &["awaiting"],
            Arc::clone(&second_sink),
            Arc::new(EventHub::new()),
            Arc::new(Scripted::new()),
        );
        second.registry.restore(records).expect("the record is restored");
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, running.run()).await;
        let sessions: Vec<SessionId> = rig
            .sink
            .events()
            .into_iter()
            .chain(second_sink.events())
            .map(|event| event.session)
            .collect();
        assert!(!sessions.is_empty(), "events were emitted");
        assert!(
            sessions.iter().all(|session| session.as_str() == MAIN),
            "every RunEnded and RunInterrupted names the main session"
        );
    }

    /// Records, for each `RunEnded`, how many notices the hub holds at that moment: none is expected, the notice comes after.
    struct OrderingSink {
        hub: Arc<EventHub>,
        pending_at_end: Mutex<Vec<usize>>,
    }

    impl EventSink for OrderingSink {
        fn emit(&self, event: Event) {
            if matches!(event.kind, EventKind::RunEnded { .. }) {
                let pending = self.hub.poll().map_or(usize::MAX, |events| events.len());
                lock(&self.pending_at_end).push(pending);
            }
        }
    }

    // Scenario: RunEnded est émis avant la notice ended
    #[tokio::test]
    async fn run_end_is_reported_before_its_notice() {
        let hub = Arc::new(EventHub::new());
        let sink = Arc::new(OrderingSink {
            hub: Arc::clone(&hub),
            pending_at_end: Mutex::new(Vec::new()),
        });
        let dynamic: Arc<dyn EventSink> = Arc::<OrderingSink>::clone(&sink);
        let launcher = Arc::new(Scripted::new());
        let registry = RunRegistry::new(
            Arc::clone(&launcher),
            declared(&["answering"]),
            dynamic,
            Arc::new(ManualClock::new(Timestamp::from_millis(1_000))),
            Arc::clone(&hub),
            main_session(),
        )
        .expect("the answer schema compiles");
        let handle = registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        launcher.release();
        let _finished = bounded_wait(&registry, handle.run()).await;
        assert_eq!(
            *lock(&sink.pending_at_end),
            vec![0],
            "RunEnded is emitted while no notice is pending yet"
        );
        assert_eq!(
            ended_notices(&hub),
            vec![(handle.run().as_str().to_owned(), "valid".to_owned())],
            "the notice follows RunEnded"
        );
    }

    // Scenario: le Debug d'une requête ne montre ni la tâche ni le collecteur
    #[tokio::test]
    async fn the_debug_of_a_launch_request_shows_neither_task_nor_collector() {
        let rig = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let handle = rig
            .registry
            .start("awaiting", TASK_SECRET, &caller)
            .expect("a declared sub-agent starts");
        settle_until(|| !rig.launcher.debugs().is_empty()).await;
        let debugs = rig.launcher.debugs();
        let [debug] = debugs.as_slice() else {
            panic!("one request was launched");
        };
        assert!(!debug.contains(TASK_SECRET), "the task is not shown: {debug}");
        assert!(
            !debug.contains("collector"),
            "the collector is not shown: {debug}"
        );
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, handle.run()).await;
    }

    // Scenario: un run a son propre enfant d'annulation, distinct de l'appelant
    #[tokio::test]
    async fn cancelling_a_run_leaves_its_caller_alone() {
        let rig = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let handle = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        rig.registry.cancel(handle.run()).expect("the run is known");
        let _stopped = bounded_wait(&rig.registry, handle.run()).await;
        assert!(
            !caller.is_cancelled(),
            "cancelling a run must not cancel the handle of its caller"
        );
    }

    // Scenario: wait sur un run restauré rend immédiatement
    #[tokio::test]
    async fn waiting_a_restored_run_returns_at_once() {
        let rig = rig(&["awaiting"]);
        rig.registry
            .restore(vec![sample_record("run-1", "awaiting", "session-1")])
            .expect("the record is restored");
        let run = run_id("run-1");
        let outcome = tokio::time::timeout(Duration::from_millis(200), rig.registry.wait(&run))
            .await
            .expect("the wait of a restored run ends at once")
            .expect("the restored run is known");
        assert_eq!(outcome.status(), SubagentStatus::Interrupted);
    }

    // Scenario: aucun secret sur les chemins Failed, Cancelled et Interrupted
    #[tokio::test]
    async fn no_secret_on_the_failed_cancelled_or_interrupted_paths() {
        let rig = rig(&["refusing", "awaiting"]);
        let failed = rig
            .registry
            .start("refusing", TASK_SECRET, &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let caller = CancelHandle::new();
        let cancelled = rig
            .registry
            .start("awaiting", TASK_SECRET, &caller)
            .expect("a declared sub-agent starts");
        caller.cancel();
        let _failed = bounded_wait(&rig.registry, failed.run()).await;
        let _cancelled = bounded_wait(&rig.registry, cancelled.run()).await;
        let interrupted_sink = Arc::new(RecordingSink::new());
        let interrupted_hub = Arc::new(EventHub::new());
        let second = rig_with(
            &["awaiting"],
            Arc::clone(&interrupted_sink),
            Arc::clone(&interrupted_hub),
            Arc::new(Scripted::new()),
        );
        let pending = rig.registry.records().expect("records");
        second
            .registry
            .restore(pending)
            .expect("the records are restored");
        let text = format!(
            "{:?}{:?}{:?}{:?}",
            rig.sink.events(),
            ended_notices(&rig.hub),
            interrupted_sink.events(),
            ended_notices(&interrupted_hub)
        );
        assert!(
            !text.contains(TASK_SECRET),
            "the task must not reach any event or notice"
        );
    }

    // Scenario: fenêtre entre la conclusion du lanceur et la fin du run : status et records donnent l'issue normalisée
    #[tokio::test]
    async fn the_window_after_a_launcher_concludes_shows_the_normalized_outcome() {
        let rig = rig(&["interrupting-linger"]);
        let handle = rig
            .registry
            .start("interrupting-linger", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        let run = handle.run().clone();
        settle_until(|| matches!(rig.registry.status(&run), Ok(RunProgress::Ended(_)))).await;
        let RunProgress::Ended(outcome) = rig.registry.status(&run).expect("the run is known") else {
            panic!("the launcher has concluded its collector");
        };
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "status shows the normalized Failed, not the raw Interrupted"
        );
        assert_eq!(
            outcome.reason(),
            Some("the sub-agent reported an interruption outside a restore"),
            "the reason is the fixed normalization reason"
        );
        assert!(outcome.session().is_none(), "no session outside Interrupted");
        assert!(
            rig.registry.records().expect("records").is_empty(),
            "a run its launcher has concluded is not persisted as in progress"
        );
        assert!(
            run_ended(&rig.sink).is_empty(),
            "RunEnded waits for the end of the run"
        );
        // Not normalized yet for `wait`: it returns at the end of the run only, after `RunEnded` (`Must` of run.sdd).
        let early = tokio::time::timeout(Duration::from_millis(20), rig.registry.wait(&run)).await;
        assert!(early.is_err(), "wait does not return before the end of the run");
        rig.launcher.release();
        let finished = bounded_wait(&rig.registry, &run).await;
        assert_eq!(
            finished.status(),
            SubagentStatus::Failed,
            "wait agrees with status"
        );
        assert_eq!(
            run_ended(&rig.sink),
            vec![(run.clone(), Outcome::Failed)],
            "RunEnded reports the same Failed"
        );
    }

    // Scenario: un run est toujours conclu, même si la tâche est arrêtée (`Must` de run.sdd, « même si la tâche est arrêtée »)
    #[test]
    fn a_run_is_concluded_when_its_task_is_stopped_before_it_first_runs() {
        let rig = rig(&["answering"]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime builds");
        let handle = {
            let _entered = runtime.enter();
            rig.registry
                .start("answering", "task", &CancelHandle::new())
                .expect("a declared sub-agent starts")
        };
        // Stopping the runtime drops the task of the run before its first poll.
        drop(runtime);
        let RunProgress::Ended(outcome) = rig.registry.status(handle.run()).expect("the run is known") else {
            panic!("a run whose task was stopped must be concluded");
        };
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "the run is concluded as failed"
        );
        assert_eq!(
            outcome.reason(),
            Some(NO_RESULT_REASON),
            "the failure carries the fixed reason"
        );
        assert_eq!(
            run_ended(&rig.sink),
            vec![(handle.run().clone(), Outcome::Failed)],
            "RunEnded is emitted for the stopped run"
        );
        assert_eq!(
            ended_notices(&rig.hub),
            vec![(handle.run().as_str().to_owned(), "failed".to_owned())],
            "the notice ended is sent for the stopped run"
        );
    }

    // Not a Scenario (correction 2): a start on a thread without runtime is refused with `NoRuntime`, and registers nothing,
    // so that no wait is left on a run that never ran. The next run gets the number the refused one did not take.
    #[test]
    fn start_without_a_runtime_is_refused_and_registers_nothing() {
        let rig = rig(&["answering"]);
        let refused = rig.registry.start("answering", "task", &CancelHandle::new());
        assert!(
            matches!(refused, Err(DelegationError::NoRuntime)),
            "a start without runtime is refused as NoRuntime"
        );
        assert!(
            rig.registry.records().expect("records").is_empty(),
            "the refused run is not registered"
        );
        assert_eq!(rig.launcher.launches(), 0, "the launcher is never called");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime builds");
        let handle = {
            let _entered = runtime.enter();
            rig.registry
                .start("answering", "task", &CancelHandle::new())
                .expect("a declared sub-agent starts once a runtime is entered")
        };
        assert_eq!(
            handle.run().as_str(),
            "run-1",
            "the refused start took no run number"
        );
    }

    // Not a Scenario (correction 2): a resumption on a thread without runtime is refused, and the restored run stays listed
    // and resumable.
    #[test]
    fn resume_without_a_runtime_keeps_the_restored_run() {
        let rig = rig(&["awaiting"]);
        rig.registry
            .restore(vec![sample_record("run-7", "awaiting", "session-7")])
            .expect("the record is restored");
        let session = SessionId::new("session-7").expect("a non-empty identifier");
        let refused = rig
            .registry
            .resume("awaiting", "task", &CancelHandle::new(), &session);
        assert!(
            matches!(refused, Err(DelegationError::NoRuntime)),
            "a resumption without runtime is refused as NoRuntime"
        );
        assert_eq!(
            rig.registry.records().expect("records"),
            vec![sample_record("run-7", "awaiting", "session-7")],
            "the restored run is not consumed by the refused resumption"
        );
    }

    // Scenario: resume refuse un sous-agent non autorisé avant de vérifier la session
    #[tokio::test]
    async fn resume_refuses_an_undeclared_subagent_before_the_session() {
        let rig = rig(&["awaiting"]);
        rig.registry
            .restore(vec![sample_record("run-7", "awaiting", "session-7")])
            .expect("the record is restored");
        let session = SessionId::new("session-7").expect("a non-empty identifier");
        let refused = rig
            .registry
            .resume("ghost", "task", &CancelHandle::new(), &session);
        assert!(
            matches!(&refused, Err(DelegationError::UnknownSubagent { name }) if name == "ghost"),
            "an undeclared sub-agent is refused as UnknownSubagent"
        );
    }

    // Scenario: une session restaurée ne se reprend qu'une fois tant que le run repris vit
    #[tokio::test]
    async fn a_restored_session_is_resumed_once_while_its_run_lives() {
        let rig = rig(&["awaiting"]);
        rig.registry
            .restore(vec![sample_record("run-7", "awaiting", "session-7")])
            .expect("the record is restored");
        let session = SessionId::new("session-7").expect("a non-empty identifier");
        let caller = CancelHandle::new();
        let first = rig
            .registry
            .resume("awaiting", "task", &caller, &session)
            .expect("the first resume starts");
        let second = rig.registry.resume("awaiting", "task", &caller, &session);
        assert!(
            matches!(&second, Err(DelegationError::DuplicateRun { id }) if id == first.run().as_str()),
            "a second resume is refused while the first resumed run lives, and names it"
        );
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, first.run()).await;
    }

    // Scenario: resume refuse la session d'un run qui n'a pas été restauré, même terminé
    #[tokio::test]
    async fn resume_refuses_a_session_of_a_run_that_was_not_restored() {
        let rig = rig(&["answering"]);
        let handle = rig
            .registry
            .start("answering", "task", &CancelHandle::new())
            .expect("a declared sub-agent starts");
        rig.launcher.release();
        let _finished = bounded_wait(&rig.registry, handle.run()).await;
        let session = SessionId::new("main-session-run-1").expect("a non-empty identifier");
        let refused = rig
            .registry
            .resume("answering", "task", &CancelHandle::new(), &session);
        assert!(
            matches!(&refused, Err(DelegationError::UnknownSession { id }) if id == "main-session-run-1"),
            "a session of a run that was not restored is refused as UnknownSession"
        );
    }

    // Scenario: resume refuse la session d'un run neuf en cours comme une session non restaurée (UnknownSession)
    #[tokio::test]
    async fn resume_refuses_the_session_of_a_fresh_run_in_progress_as_unknown() {
        let rig = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let started = rig
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let session = SessionId::new("main-session-run-1").expect("a non-empty identifier");
        let refused = rig
            .registry
            .resume("awaiting", "task", &CancelHandle::new(), &session);
        assert!(
            matches!(&refused, Err(DelegationError::UnknownSession { id }) if id == "main-session-run-1"),
            "the session of a fresh run in progress is not a restored session: UnknownSession"
        );
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, started.run()).await;
    }

    // Scenario: un run restauré reste dans records tant qu'il n'est pas repris : une seconde sauvegarde ne le perd pas
    #[tokio::test]
    async fn a_restored_run_stays_in_records_until_it_is_resumed() {
        let first = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let started = first
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let saved = first.registry.records().expect("records");
        caller.cancel();
        let _stopped = bounded_wait(&first.registry, started.run()).await;
        let second = rig(&["awaiting"]);
        second
            .registry
            .restore(saved.clone())
            .expect("the record is restored");
        let resaved = second.registry.records().expect("records");
        assert_eq!(
            resaved, saved,
            "a second save before the resume keeps the restored run, with its sub-agent and its session"
        );
    }

    // Scenario: restaurer, sauvegarder à nouveau, restaurer encore, puis reprendre
    #[tokio::test]
    async fn a_restored_session_survives_two_saves_before_its_resume() {
        let first = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let started = first
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let saved = first.registry.records().expect("records");
        let session = saved
            .first()
            .map(|record| record.session().clone())
            .expect("one record is persisted");
        caller.cancel();
        let _stopped = bounded_wait(&first.registry, started.run()).await;
        let second = rig(&["awaiting"]);
        second.registry.restore(saved).expect("the record is restored");
        let resaved = second.registry.records().expect("records");
        let third = rig(&["awaiting"]);
        third
            .registry
            .restore(resaved)
            .expect("the record saved again is restored a second time");
        let resumed_caller = CancelHandle::new();
        let resumed = third
            .registry
            .resume("awaiting", "task", &resumed_caller, &session)
            .expect("the session of the twice-restored run resumes");
        settle_until(|| third.launcher.requests().len() == 1).await;
        assert_eq!(
            third.launcher.requests(),
            vec![(session.clone(), true)],
            "the launcher receives the saved session, flagged as resumed"
        );
        resumed_caller.cancel();
        let _resumed_done = bounded_wait(&third.registry, resumed.run()).await;
    }

    // Scenario: resume retire l'entrée restaurée de records, et le run repris n'est listé qu'une fois
    #[tokio::test]
    async fn resuming_takes_the_restored_run_out_of_records() {
        let first = rig(&["awaiting"]);
        let caller = CancelHandle::new();
        let started = first
            .registry
            .start("awaiting", "task", &caller)
            .expect("a declared sub-agent starts");
        let saved = first.registry.records().expect("records");
        let session = saved
            .first()
            .map(|record| record.session().clone())
            .expect("one record is persisted");
        caller.cancel();
        let _stopped = bounded_wait(&first.registry, started.run()).await;
        let second = rig(&["awaiting"]);
        second.registry.restore(saved).expect("the record is restored");
        assert_eq!(
            second.registry.records().expect("records").len(),
            1,
            "the restored run is listed until it is resumed"
        );
        let resumed_caller = CancelHandle::new();
        let resumed = second
            .registry
            .resume("awaiting", "task", &resumed_caller, &session)
            .expect("the session resumes");
        let listed = second.registry.records().expect("records");
        let [only] = listed.as_slice() else {
            panic!("only the resumed run is listed, once");
        };
        assert_eq!(
            only.run(),
            resumed.run(),
            "the restored run is no longer listed: only the resumed run is"
        );
        assert_eq!(
            only.session(),
            &session,
            "the resumed run keeps the saved session of its sub-agent"
        );
        resumed_caller.cancel();
        let _resumed_done = bounded_wait(&second.registry, resumed.run()).await;
    }

    // Scenario: une session reprise ne se reprend plus une fois le run repris terminé
    #[tokio::test]
    async fn a_resumed_session_is_not_resumed_again_once_its_run_ended() {
        let rig = rig(&["awaiting"]);
        rig.registry
            .restore(vec![sample_record("run-7", "awaiting", "session-7")])
            .expect("the record is restored");
        let session = SessionId::new("session-7").expect("a non-empty identifier");
        let caller = CancelHandle::new();
        let first = rig
            .registry
            .resume("awaiting", "task", &caller, &session)
            .expect("the first resume starts");
        caller.cancel();
        let _stopped = bounded_wait(&rig.registry, first.run()).await;
        let again = rig
            .registry
            .resume("awaiting", "task", &CancelHandle::new(), &session);
        assert!(
            matches!(&again, Err(DelegationError::UnknownSession { id }) if id == "session-7"),
            "the restored entry is consumed by the resume: a second resume is refused as UnknownSession"
        );
    }

    // Scenario: numérotation épuisée par un identifiant restauré : refus sans boucle
    #[test]
    fn a_numbering_exhausted_by_a_restored_identifier_is_refused_without_looping() {
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime builds");
            let _entered = runtime.enter();
            let rig = rig(&["answering"]);
            let restored = rig.registry.restore(vec![sample_record(
                "run-18446744073709551615",
                "answering",
                "session-max",
            )]);
            let started = rig.registry.start("answering", "task", &CancelHandle::new());
            let refused = matches!(&started, Err(DelegationError::DuplicateRun { .. }));
            let _sent = sender.send((restored.is_ok(), refused));
        }));
        let (restored, refused) = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the start ends within five seconds, it does not loop");
        assert!(restored, "the record with the largest number is restored");
        assert!(
            refused,
            "a new run finds no free identifier and is refused with DuplicateRun"
        );
    }

    // Scenario: une issue illisible donne un échec à raison fixe, jamais l'issue brute
    #[test]
    fn an_unreadable_collector_gives_a_fixed_failure_never_the_raw_outcome() {
        let normalized = Normalized::new("answering", &answer_schema(), DEFAULT_CORRECTIONS)
            .expect("the fixed outcomes build");
        let raw_collector = ResultCollector::with_default_corrections("answering", &answer_schema())
            .expect("the answer schema compiles");
        let host_session = SessionId::new("host-session").expect("a non-empty identifier");
        let _concluded = raw_collector
            .conclude_as(
                SubagentStatus::Interrupted,
                "the host reported it",
                Some(host_session.clone()),
            )
            .expect("the collector concludes");
        let raw = raw_collector.outcome().expect("the collector is readable");
        let unreadable = resolve(&normalized, false, Err(DelegationError::Poisoned))
            .expect("an unreadable run is reported as failed");
        assert_eq!(
            unreadable.status(),
            SubagentStatus::Failed,
            "a failure, not an error"
        );
        assert_eq!(
            unreadable.reason(),
            Some("the outcome of the run could not be read"),
            "the reason is the fixed unreadable reason"
        );
        assert!(unreadable.session().is_none(), "no session outside Interrupted");
        let live = resolve(&normalized, false, Ok(raw.clone())).expect("the live run is concluded");
        assert_eq!(
            live.status(),
            SubagentStatus::Failed,
            "a live Interrupted reads as Failed"
        );
        assert!(
            live.session().is_none(),
            "the host session is not reported for a live run"
        );
        let restored = resolve(&normalized, true, Ok(raw)).expect("the restored run is concluded");
        assert_eq!(
            restored.status(),
            SubagentStatus::Interrupted,
            "a restored run keeps Interrupted"
        );
        assert_eq!(restored.session(), Some(&host_session), "and its session");
    }

    // --- batch driver ---------------------------------------------------------------------------------------------

    /// What the scripted batch sub-agent does at one model call.
    #[derive(Clone)]
    enum Step {
        /// Submits a value to the collector, as the module `result` does, then answers.
        Submit(Value),
        /// Answers this text.
        Say(&'static str),
        /// Fails permanently.
        Fail,
    }

    /// What a test reads on the batch sub-agent after the run.
    #[derive(Default)]
    struct Probe {
        calls: AtomicUsize,
        submissions: Mutex<Vec<Submission>>,
        prompts: Mutex<Vec<String>>,
        script: Mutex<VecDeque<Step>>,
    }

    /// Batch sub-agent double: each model call takes the next step of the script, or answers `bonjour` once it is exhausted.
    struct Batch {
        collector: Arc<ResultCollector>,
        probe: Arc<Probe>,
    }

    fn reply(text: &str) -> InferenceResponse {
        InferenceResponse::new(Message::assistant(text, None, Vec::new()), None)
    }

    impl Batch {
        /// Submits `value` to the collector, and keeps the answer of the collector for the test.
        fn submit(&self, value: Value) {
            if let Ok(submission) = self.collector.submit(value) {
                lock(&self.probe.submissions).push(submission);
            }
        }
    }

    impl InferenceProvider for Batch {
        fn complete(
            &self,
            request: InferenceRequest,
        ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send {
            let _before = self.probe.calls.fetch_add(1, Ordering::SeqCst);
            lock(&self.probe.prompts).push(format!("{:?}", request.messages()));
            let step = lock(&self.probe.script)
                .pop_front()
                .unwrap_or(Step::Say("bonjour"));
            let answer = match step {
                Step::Submit(value) => {
                    self.submit(value);
                    Ok(reply("submitted"))
                }
                Step::Say(text) => Ok(reply(text)),
                Step::Fail => Err(InferenceError::new(ErrorClass::Permanent, "refused".to_owned())),
            };
            std::future::ready(answer)
        }
    }

    /// Summarizer that is never needed by the batch tests.
    struct NoSummary;

    impl Summarizer for NoSummary {
        fn summarize(
            &self,
            _transcript: &str,
            _digest: Option<&Digest>,
        ) -> impl Future<Output = ContextResult<String>> + Send {
            std::future::ready(Ok(String::new()))
        }
    }

    /// A turn runner of a fresh sub-agent session `session`, driven by `provider`. Each test names its own session: the turn
    /// gate of the process refuses two active turns of one session, and the tests run in parallel.
    fn batch_runner<P: InferenceProvider>(
        session: &str,
        provider: P,
    ) -> TurnRunner<P, DefaultStrategy<NoSummary>, InMemoryStore> {
        let profile = ModelProfile::new("profile-batch".to_owned(), "prov".to_owned(), "model".to_owned())
            .with_context_size(100_000)
            .with_replay(ReplayMode::None)
            .resolve(None)
            .expect("the profile resolves");
        let context = SegmentContext::new("system prompt", Vec::new(), ModelSnapshot::new("model", "none"));
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .build()
            .expect("the engine builds");
        let config = TurnConfig::new(
            Arc::new(ManualClock::new(Timestamp::from_millis(0))),
            Redactor::new(),
            Arc::new(RecordingSink::new()),
            CancelHandle::new(),
            Budget::new(100_000, 100, 1000, 1000),
        );
        let state = SessionState::new(
            SessionId::new(session).expect("a non-empty text makes a session identifier"),
            AgentRef::new("sub-agent", "fp-1"),
            "ctx",
            BTreeMap::new(),
            Timestamp::from_millis(0),
        );
        TurnRunner::new(
            provider,
            DefaultStrategy::new(NoSummary, ReplayMode::None),
            profile,
            context,
            Scratchpad::new(Arc::new(engine)),
            InMemoryStore::new(),
            state,
            config,
        )
    }

    fn batch_collector() -> Arc<ResultCollector> {
        Arc::new(
            ResultCollector::with_default_corrections("answering", &answer_schema())
                .expect("the answer schema compiles"),
        )
    }

    // Scenario: pilote batch avec correction
    #[tokio::test]
    async fn batch_accepts_the_corrected_result_in_the_same_run() {
        let collector = batch_collector();
        let probe = Arc::new(Probe::default());
        *lock(&probe.script) = VecDeque::from(vec![
            Step::Submit(json!({})),
            Step::Submit(json!({"answer": ANSWER})),
        ]);
        let provider = Batch {
            collector: Arc::clone(&collector),
            probe: Arc::clone(&probe),
        };
        let mut runner = batch_runner("sub-batch_accepts_the_corrected_result_in_the_same_run", provider);
        let outcome = run_batch(&mut runner, "the task", &collector, &CancelHandle::new())
            .await
            .expect("the batch runs");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Valid,
            "the corrected result is accepted, so the run is Valid"
        );
        assert_eq!(
            outcome.result(),
            Some(&json!({"answer": ANSWER})),
            "the validated result is the corrected one"
        );
        let submissions = lock(&probe.submissions).clone();
        match submissions.as_slice() {
            [
                Submission::Rejected { errors, remaining },
                Submission::Accepted { .. },
            ] => {
                assert_eq!(
                    *remaining, 3,
                    "three corrections remain after the first rejection"
                );
                assert!(
                    errors.iter().any(|error| error.contains("answer")),
                    "the refusal names the missing answer, got {errors:?}"
                );
            }
            other => panic!("a rejection then an acceptance were expected, got {other:?}"),
        }
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2, "one model call per turn");
        let prompts = lock(&probe.prompts).clone();
        assert!(
            prompts.get(1).is_some_and(|prompt| prompt.contains(REMINDER)),
            "the second turn starts with the fixed reminder"
        );
    }

    // Scenario: pilote batch qui n'aboutit pas
    #[tokio::test]
    async fn batch_fails_after_the_turn_limit_without_result() {
        let collector = batch_collector();
        let probe = Arc::new(Probe::default());
        let provider = Batch {
            collector: Arc::clone(&collector),
            probe: Arc::clone(&probe),
        };
        let mut runner = batch_runner("sub-batch_fails_after_the_turn_limit_without_result", provider);
        let outcome = run_batch(&mut runner, "the task", &collector, &CancelHandle::new())
            .await
            .expect("the batch runs");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "without a result the run is Failed"
        );
        assert_eq!(
            outcome.reason(),
            Some(NO_RESULT_IN_TURNS_REASON),
            "the reason names the turn limit"
        );
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            8,
            "the run stops after the maximal number of turns, one model call each"
        );
    }

    // Scenario: pilote batch, échec d'un tour
    #[tokio::test]
    async fn batch_stops_when_a_turn_fails() {
        let collector = batch_collector();
        let probe = Arc::new(Probe::default());
        lock(&probe.script).push_back(Step::Fail);
        let provider = Batch {
            collector: Arc::clone(&collector),
            probe: Arc::clone(&probe),
        };
        let mut runner = batch_runner("sub-batch_stops_when_a_turn_fails", provider);
        let outcome = run_batch(&mut runner, "the task", &collector, &CancelHandle::new())
            .await
            .expect("the batch runs");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "a failed turn makes the run Failed"
        );
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            1,
            "no further turn is started after a failed one"
        );
    }

    // Scenario: pilote batch annulé avant le premier tour
    #[tokio::test]
    async fn batch_ends_as_cancelled_when_its_run_is_cancelled() {
        let collector = batch_collector();
        let probe = Arc::new(Probe::default());
        let provider = Batch {
            collector: Arc::clone(&collector),
            probe: Arc::clone(&probe),
        };
        let mut runner = batch_runner("sub-batch_ends_as_cancelled_when_its_run_is_cancelled", provider);
        let cancel = CancelHandle::new();
        cancel.cancel();
        let outcome = run_batch(&mut runner, "the task", &collector, &cancel)
            .await
            .expect("the batch runs");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Cancelled,
            "a cancelled run is Cancelled"
        );
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            0,
            "the cancelled run makes no model call"
        );
    }
}
