//! At-most-once OS ejection for ONE owned import. No device paths or OS calls.
//!
//! The integration must bind `ImportKey` to the exact authenticated IMPORT,
//! never reuse its number within a session, and keep the OS attachment/worker
//! alive until the system call actually retires. A timeout changes knowledge,
//! not ownership. This module alone neither authorizes nor performs ejection.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportKey {
    pub session: [u8; 16],
    pub number: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operation {
    pub import: ImportKey,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Only a positive response from the owned device's OS removal operation.
    Safe,
    Busy,
    Denied,
    Unsupported,
    /// The import disappeared; this does NOT establish safe removal.
    Unavailable,
    /// The operation may still be running, or its result cannot be established.
    Uncertain,
}

impl Outcome {
    pub fn permits_explicit_retry(self) -> bool {
        matches!(self, Self::Busy | Self::Denied | Self::Unsupported)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Running { sequence: u64 },
    Completed { sequence: u64, outcome: Outcome },
}

impl State {
    fn sequence(self) -> u64 {
        match self {
            Self::Idle => 0,
            Self::Running { sequence } | Self::Completed { sequence, .. } => sequence,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    WrongImport,
    OlderOperation,
    InFlight,
    Retired,
    WrongCompletion,
    ConflictingCompletion,
}

/// Created only by `begin`. The worker retains it through actual OS return.
/// Deliberately not Clone: transport retransmission does not create a new worker.
#[derive(Debug, PartialEq, Eq)]
pub struct Execution {
    operation: Operation,
}

impl Execution {
    pub fn operation(&self) -> Operation {
        self.operation
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Execute(Execution),
    Pending,
    Result(Outcome),
}

/// Constant-space state for one import, retained after successful OS removal
/// until the peer acknowledges/withdraws that exact export. Never evict and
/// recreate this ledger for the same key just to make room for a new request.
pub struct Ledger {
    import: ImportKey,
    state: State,
}

impl Ledger {
    pub fn new(import: ImportKey) -> Result<Self, Error> {
        if import.number == 0 || import.session == [0; 16] {
            return Err(Error::Invalid);
        }
        Ok(Self {
            import,
            state: State::Idle,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Allocate only in response to a NEW explicit user action, never a timer.
    pub fn next_sequence(&self) -> Option<u64> {
        match self.state {
            State::Idle => Some(1),
            State::Completed { sequence, outcome } if outcome.permits_explicit_retry() => {
                sequence.checked_add(1)
            }
            _ => None,
        }
    }

    pub fn suppress_reimport(&self) -> bool {
        match self.state {
            State::Idle => false,
            State::Running { .. } => true,
            State::Completed { outcome, .. } => !outcome.permits_explicit_retry(),
        }
    }

    pub fn begin(&mut self, operation: Operation) -> Result<Decision, Error> {
        if operation.import != self.import {
            return Err(Error::WrongImport);
        }
        if operation.sequence == 0 {
            return Err(Error::Invalid);
        }
        if operation.sequence < self.state.sequence() {
            return Err(Error::OlderOperation);
        }
        if operation.sequence == self.state.sequence() {
            return Ok(match self.state {
                State::Running { .. } => Decision::Pending,
                State::Completed { outcome, .. } => Decision::Result(outcome),
                State::Idle => unreachable!("zero sequence was rejected"),
            });
        }
        match self.state {
            State::Running { .. } => return Err(Error::InFlight),
            State::Completed { outcome, .. } if !outcome.permits_explicit_retry() => {
                return Err(Error::Retired);
            }
            _ => {}
        }
        self.state = State::Running {
            sequence: operation.sequence,
        };
        Ok(Decision::Execute(Execution { operation }))
    }

    fn matches(&self, execution: &Execution) -> Result<(), Error> {
        if execution.operation.import != self.import {
            return Err(Error::WrongImport);
        }
        if execution.operation.sequence != self.state.sequence() {
            return Err(Error::WrongCompletion);
        }
        Ok(())
    }

    /// Retain the execution ticket AND OS worker/attachment after this call.
    /// A deadline/cancel request does not mean that the OS released anything.
    pub fn mark_uncertain(&mut self, execution: &Execution) -> Result<bool, Error> {
        self.matches(execution)?;
        if let State::Running { sequence } = self.state {
            self.state = State::Completed {
                sequence,
                outcome: Outcome::Uncertain,
            };
            return Ok(true);
        }
        Ok(false) // A late timer cannot downgrade an already confirmed result.
    }

    /// A late OS return may settle Uncertain for the SAME operation. Conflicting
    /// or stale completions never overwrite an already confirmed outcome.
    pub fn complete(&mut self, execution: &Execution, outcome: Outcome) -> Result<bool, Error> {
        self.matches(execution)?;
        if let State::Completed { outcome: old, .. } = self.state {
            if old == outcome {
                return Ok(false);
            }
            if old != Outcome::Uncertain {
                return Err(Error::ConflictingCompletion);
            }
        }
        self.state = State::Completed {
            sequence: execution.operation.sequence,
            outcome,
        };
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: ImportKey = ImportKey {
        session: [7; 16],
        number: 1,
    };

    fn op(sequence: u64) -> Operation {
        Operation {
            import: KEY,
            sequence,
        }
    }

    fn start(ledger: &mut Ledger, sequence: u64) -> Execution {
        match ledger.begin(op(sequence)).unwrap() {
            Decision::Execute(ticket) => ticket,
            other => panic!("expected single OS execution, got {other:?}"),
        }
    }

    #[test]
    fn zero_identity_and_operation_are_rejected_without_mutation() {
        for key in [
            ImportKey { number: 0, ..KEY },
            ImportKey {
                session: [0; 16],
                ..KEY
            },
        ] {
            assert!(matches!(Ledger::new(key), Err(Error::Invalid)));
        }
        let mut ledger = Ledger::new(KEY).unwrap();
        assert_eq!(ledger.begin(op(0)), Err(Error::Invalid));
        assert_eq!(ledger.state(), State::Idle);
        assert_eq!(ledger.next_sequence(), Some(1));
        assert!(!ledger.suppress_reimport());
    }

    #[test]
    fn duplicate_delivery_never_executes_os_twice() {
        let mut ledger = Ledger::new(KEY).unwrap();
        let ticket = start(&mut ledger, 1);
        for _ in 0..32 {
            assert_eq!(ledger.begin(op(1)), Ok(Decision::Pending));
        }
        assert_eq!(ledger.begin(op(2)), Err(Error::InFlight));
        assert_eq!(ledger.next_sequence(), None);
        assert!(ledger.suppress_reimport());
        assert_eq!(ticket.operation(), op(1));
        ledger.complete(&ticket, Outcome::Safe).unwrap();
        for _ in 0..32 {
            assert_eq!(ledger.begin(op(1)), Ok(Decision::Result(Outcome::Safe)));
        }
        assert_eq!(ledger.begin(op(2)), Err(Error::Retired));
        assert!(ledger.suppress_reimport());
        assert_eq!(ledger.next_sequence(), None);
    }

    #[test]
    fn known_rejection_requires_a_new_explicit_sequence() {
        for reason in [Outcome::Busy, Outcome::Denied, Outcome::Unsupported] {
            let mut ledger = Ledger::new(KEY).unwrap();
            let first = start(&mut ledger, 4);
            ledger.complete(&first, reason).unwrap();
            assert_eq!(ledger.begin(op(4)), Ok(Decision::Result(reason)));
            assert_eq!(ledger.next_sequence(), Some(5));
            assert!(!ledger.suppress_reimport());
            let second = start(&mut ledger, 5);
            assert_eq!(ledger.begin(op(4)), Err(Error::OlderOperation));
            assert_eq!(
                ledger.complete(&first, Outcome::Safe),
                Err(Error::WrongCompletion)
            );
            assert_eq!(ledger.state(), State::Running { sequence: 5 });
            ledger.complete(&second, Outcome::Safe).unwrap();
        }
    }

    #[test]
    fn uncertain_result_retains_execution_and_accepts_only_its_late_return() {
        for late in [
            Outcome::Safe,
            Outcome::Busy,
            Outcome::Denied,
            Outcome::Unavailable,
        ] {
            let mut ledger = Ledger::new(KEY).unwrap();
            let ticket = start(&mut ledger, 1);
            assert!(ledger.mark_uncertain(&ticket).unwrap());
            assert!(!ledger.mark_uncertain(&ticket).unwrap());
            assert_eq!(
                ledger.begin(op(1)),
                Ok(Decision::Result(Outcome::Uncertain))
            );
            assert_eq!(ledger.begin(op(2)), Err(Error::Retired));
            assert!(ledger.suppress_reimport());
            assert!(ledger.complete(&ticket, late).unwrap());
            assert_eq!(ledger.begin(op(1)), Ok(Decision::Result(late)));
            assert_eq!(ledger.suppress_reimport(), !late.permits_explicit_retry());
        }
    }

    #[test]
    fn late_timer_and_conflicting_completion_cannot_downgrade_success() {
        let mut ledger = Ledger::new(KEY).unwrap();
        let ticket = start(&mut ledger, 1);
        assert!(ledger.complete(&ticket, Outcome::Safe).unwrap());
        assert!(!ledger.complete(&ticket, Outcome::Safe).unwrap());
        assert!(!ledger.mark_uncertain(&ticket).unwrap());
        assert_eq!(
            ledger.complete(&ticket, Outcome::Busy),
            Err(Error::ConflictingCompletion)
        );
        assert_eq!(ledger.begin(op(1)), Ok(Decision::Result(Outcome::Safe)));
    }

    #[test]
    fn replacement_import_and_process_reject_old_request_and_completion() {
        let mut old = Ledger::new(KEY).unwrap();
        let ticket = start(&mut old, 1);
        for replacement in [
            ImportKey { number: 2, ..KEY },
            ImportKey {
                session: [8; 16],
                ..KEY
            },
        ] {
            let mut ledger = Ledger::new(replacement).unwrap();
            assert_eq!(ledger.begin(op(1)), Err(Error::WrongImport));
            assert_eq!(
                ledger.complete(&ticket, Outcome::Safe),
                Err(Error::WrongImport)
            );
            assert_eq!(ledger.mark_uncertain(&ticket), Err(Error::WrongImport));
            assert_eq!(ledger.state(), State::Idle);
        }
    }

    #[test]
    fn missing_import_is_not_safe_and_does_not_restart() {
        let mut ledger = Ledger::new(KEY).unwrap();
        let ticket = start(&mut ledger, 1);
        ledger.complete(&ticket, Outcome::Unavailable).unwrap();
        assert_eq!(
            ledger.begin(op(1)),
            Ok(Decision::Result(Outcome::Unavailable))
        );
        assert_eq!(ledger.begin(op(2)), Err(Error::Retired));
        assert!(ledger.suppress_reimport());
    }

    #[test]
    fn sequence_exhaustion_cannot_wrap_into_an_old_operation() {
        let mut ledger = Ledger::new(KEY).unwrap();
        let ticket = start(&mut ledger, u64::MAX);
        ledger.complete(&ticket, Outcome::Busy).unwrap();
        assert_eq!(ledger.next_sequence(), None);
        assert_eq!(ledger.begin(op(0)), Err(Error::Invalid));
        assert_eq!(ledger.begin(op(u64::MAX - 1)), Err(Error::OlderOperation));
        assert_eq!(
            ledger.begin(op(u64::MAX)),
            Ok(Decision::Result(Outcome::Busy))
        );
    }

    #[test]
    fn bounded_random_traces_execute_each_sequence_at_most_once() {
        for seed in 1u64..=64 {
            let mut random = seed;
            let mut ledger = Ledger::new(KEY).unwrap();
            let mut seen = std::collections::BTreeSet::new();
            let mut ticket = None;
            for _ in 0..512 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let sequence = (random >> 32) % 48;
                match random % 4 {
                    0 | 1 => {
                        if let Ok(Decision::Execute(next)) = ledger.begin(op(sequence)) {
                            assert!(seen.insert(sequence), "duplicate OS execution");
                            ticket = Some(next);
                        }
                    }
                    2 => {
                        if let Some(ticket) = &ticket {
                            let _ = ledger.mark_uncertain(ticket);
                        }
                    }
                    _ => {
                        if let Some(ticket) = &ticket {
                            let outcome = [
                                Outcome::Busy,
                                Outcome::Denied,
                                Outcome::Safe,
                                Outcome::Unavailable,
                                Outcome::Uncertain,
                            ][(random >> 16) as usize % 5];
                            let before = ledger.state();
                            if ledger.complete(ticket, outcome).is_err() {
                                assert_eq!(before, ledger.state(), "rejection mutated state");
                            }
                        }
                    }
                }
            }
        }
    }
}
