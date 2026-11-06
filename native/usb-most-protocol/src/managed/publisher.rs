//! Android-side intent/result state for one exact managed import.
//! The Android adapter must independently retain and check its USB lease,
//! attachment generation, selection revision and computer/service owner. This
//! model never changes USB selection or turns a network result into authority.
use super::{Action, Directive, Report};
use crate::eject::{Decision, Error, Execution, ImportKey, Ledger, Outcome, State};

pub struct Publisher {
    key: ImportKey,
    ledger: Ledger,
    operation: Option<Execution>,
    previous_retry: Option<State>,
    ready: bool,
    released: bool,
    retired: bool,
}
impl Publisher {
    pub fn new(key: ImportKey) -> Result<Self, Error> {
        Ok(Self {
            key,
            ledger: Ledger::new(key)?,
            operation: None,
            previous_retry: None,
            ready: false,
            released: false,
            retired: false,
        })
    }
    pub fn state(&self) -> State {
        self.ledger.state()
    }
    pub fn can_request(&self) -> bool {
        self.ready && !self.released && !self.retired && self.ledger.next_sequence().is_some()
    }
    /// One NEW explicit user action, with Android ownership checks already done.
    /// Retransmission is poll(), never request().
    pub fn request(&mut self) -> Result<u64, Error> {
        if !self.can_request() {
            return Err(Error::Retired);
        }
        let sequence = self.ledger.next_sequence().ok_or(Error::Retired)?;
        let previous = match self.state() {
            state @ State::Completed { outcome, .. } if outcome.permits_explicit_retry() => {
                Some(state)
            }
            _ => None,
        };
        let Decision::Execute(operation) = self.ledger.begin(crate::eject::Operation {
            import: self.key,
            sequence,
        })?
        else {
            return Err(Error::InFlight);
        };
        self.operation = Some(operation);
        self.previous_retry = previous;
        Ok(sequence)
    }
    /// USB EOF is not a safe result. Keep the operation for a late OS report,
    /// but disallow another request on a lease which Android no longer owns.
    pub fn released(&mut self) {
        self.released = true;
    }
    /// Called when the exact selection/attachment/service has been withdrawn.
    /// The receiver may retire only after its still-running OS worker returns.
    pub fn retire(&mut self) {
        self.retired = true;
        self.ready = false;
    }
    /// UI deadline changes knowledge only. The receiver's operation ID remains
    /// unchanged; the eventual OS report can still establish the actual result.
    pub fn mark_uncertain(&mut self) -> Result<bool, Error> {
        let operation = self.operation.as_ref().ok_or(Error::WrongCompletion)?;
        self.ledger.mark_uncertain(operation)
    }
    /// Acknowledge Safe only AFTER the Android adapter has checked/applied it to
    /// its original lease (or established that this lease was already replaced).
    pub fn acknowledge_safe(&mut self) -> Result<(), Error> {
        if !matches!(
            self.state(),
            State::Completed {
                outcome: Outcome::Safe,
                ..
            }
        ) {
            return Err(Error::WrongCompletion);
        }
        self.retire();
        Ok(())
    }
    pub fn poll(&mut self, report: Report) -> Result<Directive, Error> {
        if report.import != self.key {
            return Err(Error::WrongImport);
        }
        if self.retired {
            return Ok(Directive {
                import: self.key,
                sequence: self
                    .operation
                    .as_ref()
                    .map_or(0, |o| o.operation().sequence),
                action: Action::Retire,
            });
        }
        match report.state {
            State::Idle => {
                // A result already recorded on the SAME import cannot be erased
                // by a restarted receiver or a delayed idle snapshot.
                if matches!(self.state(), State::Completed { .. }) {
                    return Err(Error::WrongCompletion);
                }
                self.ready = true;
            }
            State::Running { sequence } => {
                let operation = self.operation.as_ref().ok_or(Error::WrongCompletion)?;
                if sequence != operation.operation().sequence {
                    return Err(Error::WrongCompletion);
                }
                if !matches!(
                    self.state(),
                    State::Running { .. }
                        | State::Completed {
                            outcome: Outcome::Uncertain,
                            ..
                        }
                ) {
                    return Err(Error::ConflictingCompletion);
                }
            }
            State::Completed { sequence, outcome } => {
                let operation = self.operation.as_ref().ok_or(Error::WrongCompletion)?;
                if sequence != operation.operation().sequence {
                    // Until the receiver learns of a new explicit retry it
                    // keeps reporting the previous known Busy/Denied/Unsupported.
                    // Deliver the new command without completing it from that
                    // old report. A UI timeout or a terminal result closes this
                    // delivery window; stale/changed outcomes remain rejected.
                    if self.previous_retry == Some(report.state)
                        && matches!(self.state(), State::Running { .. })
                    {
                        return Ok(Directive {
                            import: self.key,
                            sequence: operation.operation().sequence,
                            action: Action::Eject,
                        });
                    }
                    return Err(Error::WrongCompletion);
                }
                self.ledger.complete(operation, outcome)?;
            }
        }
        let (sequence, action) = match self.state() {
            State::Idle => (0, Action::Wait),
            State::Completed { outcome, .. } if outcome.permits_explicit_retry() => {
                (0, Action::Wait)
            }
            State::Running { sequence } | State::Completed { sequence, .. } => {
                (sequence, Action::Eject)
            }
        };
        Ok(Directive {
            import: self.key,
            sequence,
            action,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: ImportKey = ImportKey {
        session: [9; 16],
        number: 7,
    };
    fn report(state: State) -> Report {
        Report { import: KEY, state }
    }
    #[test]
    fn lost_delivery_duplicate_results_and_safe_ack_never_create_a_new_operation() {
        let mut p = Publisher::new(KEY).unwrap();
        assert!(!p.can_request());
        assert!(p.request().is_err());
        assert_eq!(p.poll(report(State::Idle)).unwrap().action, Action::Wait);
        assert_eq!(p.request(), Ok(1));
        for _ in 0..20 {
            let d = p.poll(report(State::Idle)).unwrap();
            assert_eq!((d.sequence, d.action), (1, Action::Eject));
            assert!(!p.can_request());
        }
        assert!(p.mark_uncertain().unwrap());
        assert!(
            p.poll(report(State::Idle)).is_err(),
            "an uncertain delivery must not start a fresh execution against an idle receiver"
        );
        assert_eq!(
            p.poll(report(State::Running { sequence: 1 }))
                .unwrap()
                .sequence,
            1
        );
        assert_eq!(
            p.state(),
            State::Completed {
                sequence: 1,
                outcome: Outcome::Uncertain
            }
        );
        p.released(); // Kernel/USB EOF may precede the final Windows/UDisks reply.
        let safe = report(State::Completed {
            sequence: 1,
            outcome: Outcome::Safe,
        });
        for _ in 0..10 {
            assert_eq!(p.poll(safe).unwrap().action, Action::Eject);
            assert_eq!(p.state(), safe.state);
        }
        assert!(p.poll(report(State::Idle)).is_err());
        assert!(!p.mark_uncertain().unwrap());
        p.acknowledge_safe().unwrap();
        for _ in 0..10 {
            assert_eq!(p.poll(safe).unwrap().action, Action::Retire);
        }
        assert!(!p.can_request());
        assert!(p.request().is_err());
    }
    #[test]
    fn unsolicited_wrong_import_old_sequence_and_conflicting_result_are_rejected() {
        let mut p = Publisher::new(KEY).unwrap();
        for state in [
            State::Running { sequence: 1 },
            State::Completed {
                sequence: 1,
                outcome: Outcome::Safe,
            },
        ] {
            assert!(p.poll(report(state)).is_err());
            assert_eq!(p.state(), State::Idle);
        }
        let foreign = Report {
            import: ImportKey { number: 8, ..KEY },
            state: State::Idle,
        };
        assert_eq!(p.poll(foreign), Err(Error::WrongImport));
        assert!(p.acknowledge_safe().is_err());
        p.poll(report(State::Idle)).unwrap();
        p.request().unwrap();
        p.poll(report(State::Completed {
            sequence: 1,
            outcome: Outcome::Busy,
        }))
        .unwrap();
        assert!(p.can_request());
        assert_eq!(p.request(), Ok(2));
        assert!(
            p.poll(report(State::Completed {
                sequence: 1,
                outcome: Outcome::Safe
            }))
            .is_err()
        );
        assert_eq!(p.state(), State::Running { sequence: 2 });
        p.poll(report(State::Completed {
            sequence: 2,
            outcome: Outcome::Safe,
        }))
        .unwrap();
        assert!(
            p.poll(report(State::Completed {
                sequence: 2,
                outcome: Outcome::Busy
            }))
            .is_err()
        );
        assert!(!p.can_request());
    }
    #[test]
    fn detached_or_retired_lease_cannot_issue_a_fresh_command() {
        for retired in [false, true] {
            let mut p = Publisher::new(KEY).unwrap();
            p.poll(report(State::Idle)).unwrap();
            p.request().unwrap();
            p.poll(report(State::Completed {
                sequence: 1,
                outcome: Outcome::Denied,
            }))
            .unwrap();
            assert!(p.can_request());
            if retired {
                p.retire()
            } else {
                p.released()
            }
            assert!(!p.can_request());
            assert!(p.request().is_err());
            let d = p
                .poll(report(State::Completed {
                    sequence: 1,
                    outcome: Outcome::Denied,
                }))
                .unwrap();
            assert_eq!(
                d.action,
                if retired {
                    Action::Retire
                } else {
                    Action::Wait
                }
            );
        }
    }

    #[test]
    fn delayed_and_lost_metadata_between_both_models_never_reexecutes_os() {
        use crate::eject::Operation;
        use std::collections::{BTreeSet, VecDeque};
        let mut executions = 0;
        for seed in 1u64..=96 {
            let mut random = seed;
            let mut publisher = Publisher::new(KEY).unwrap();
            let mut receiver = Ledger::new(KEY).unwrap();
            let mut explicit = BTreeSet::new();
            let mut executed = BTreeSet::new();
            let mut worker: Option<Execution> = None;
            let mut delayed = VecDeque::new();
            let mut retired = false;
            publisher.poll(report(State::Idle)).unwrap();
            for _ in 0..512 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                match random % 7 {
                    0 if publisher.can_request() => {
                        explicit.insert(publisher.request().unwrap());
                    }
                    1 => {
                        let _ = publisher.mark_uncertain();
                    }
                    2 if worker.is_some() => {
                        let ticket = worker.take().unwrap();
                        let outcome = [
                            Outcome::Busy,
                            Outcome::Denied,
                            Outcome::Safe,
                            Outcome::Unsupported,
                        ][(random >> 24) as usize % 4];
                        receiver.complete(&ticket, outcome).unwrap();
                    }
                    3 if delayed.len() < 8 => {
                        delayed.push_back(report(receiver.state()));
                    }
                    4 | 5 => {
                        let status = if random & 256 == 0 {
                            delayed.pop_front().unwrap_or(report(receiver.state()))
                        } else {
                            report(receiver.state())
                        };
                        let Ok(command) = publisher.poll(status) else {
                            continue;
                        };
                        if random & 512 == 0 {
                            continue;
                        } // Drop this reply before the receiver sees it.
                        match command.action {
                            Action::Wait => {}
                            Action::Retire => retired = true,
                            Action::Eject if !retired => {
                                if let Ok(Decision::Execute(ticket)) = receiver.begin(Operation {
                                    import: command.import,
                                    sequence: command.sequence,
                                }) {
                                    assert!(explicit.contains(&command.sequence));
                                    assert!(
                                        executed.insert(command.sequence),
                                        "duplicate OS execution"
                                    );
                                    assert!(worker.is_none());
                                    worker = Some(ticket);
                                    executions += 1;
                                }
                            }
                            Action::Eject => {}
                        }
                    }
                    6 if matches!(
                        publisher.state(),
                        State::Completed {
                            outcome: Outcome::Safe,
                            ..
                        }
                    ) =>
                    {
                        publisher.acknowledge_safe().unwrap();
                    }
                    _ => {}
                }
                if let State::Completed {
                    sequence,
                    outcome: Outcome::Safe,
                } = publisher.state()
                {
                    assert_eq!(
                        receiver.state(),
                        State::Completed {
                            sequence,
                            outcome: Outcome::Safe
                        }
                    );
                    assert!(!publisher.can_request());
                }
                assert!(executed.len() <= explicit.len());
            }
        }
        assert!(
            executions > 20,
            "traces must actually exercise OS execution"
        );
    }
    #[test]
    fn explicit_retry_is_delivered_to_previous_result_without_accepting_stale_completion() {
        for previous in [Outcome::Busy, Outcome::Denied, Outcome::Unsupported] {
            let mut p = Publisher::new(KEY).unwrap();
            p.poll(report(State::Idle)).unwrap();
            p.request().unwrap();
            let old = report(State::Completed {
                sequence: 1,
                outcome: previous,
            });
            p.poll(old).unwrap();
            assert_eq!(p.request(), Ok(2));
            for _ in 0..3 {
                let command = p.poll(old).unwrap();
                assert_eq!((command.sequence, command.action), (2, Action::Eject));
                assert_eq!(p.state(), State::Running { sequence: 2 });
            }
            assert!(
                p.poll(report(State::Completed {
                    sequence: 1,
                    outcome: Outcome::Safe
                }))
                .is_err()
            );
            assert!(
                p.poll(report(State::Completed {
                    sequence: 3,
                    outcome: previous
                }))
                .is_err()
            );
            p.mark_uncertain().unwrap();
            assert!(
                p.poll(old).is_err(),
                "no new delivery after uncertain timeout"
            );
            p.poll(report(State::Completed {
                sequence: 2,
                outcome: Outcome::Safe,
            }))
            .unwrap();
            assert!(
                p.poll(old).is_err(),
                "old failure cannot overwrite confirmed Safe"
            );
            p.acknowledge_safe().unwrap();
            assert_eq!(p.poll(old).unwrap().action, Action::Retire);
        }
    }
}
