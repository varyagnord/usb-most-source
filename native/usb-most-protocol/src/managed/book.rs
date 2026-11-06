//! Bounded app-service registry. An issued key is never reused after withdrawal.
//! Missing old entries answer Retire; polling can never recreate an operation.
use super::{
    Action, Capabilities, Directive, EJECT, GENERIC, ImportRequest, MAX_IMPORTS, Publisher, Report,
    WAN, decode_key, encode_key,
};
use crate::eject::{ImportKey, State};

pub struct Book {
    session: [u8; 16],
    next: u64,
    slots: [Option<(ImportKey, Publisher)>; MAX_IMPORTS as usize],
}
impl Book {
    pub fn new(session: [u8; 16]) -> Option<Self> {
        if session == [0; 16] {
            return None;
        }
        Some(Self {
            session,
            next: 1,
            slots: [const { None }; MAX_IMPORTS as usize],
        })
    }
    fn issued(&self, key: ImportKey) -> bool {
        key.session == self.session && key.number > 0 && key.number < self.next
    }
    fn publisher(&mut self, key: ImportKey) -> Option<&mut Publisher> {
        self.slots
            .iter_mut()
            .filter_map(Option::as_mut)
            .find(|(k, _)| *k == key)
            .map(|(_, p)| p)
    }
    /// Bounded adapter commands, shared by host tests and the narrow JNI boundary.
    /// All arrays are exact-size wire metadata; no OS call occurs under this API.
    pub fn execute(&mut self, action: u32, input: &[u8]) -> Option<Vec<u8>> {
        match action {
            0 if input.is_empty() => Some(
                Capabilities {
                    session: self.session,
                    features: GENERIC | WAN | EJECT,
                    max_imports: MAX_IMPORTS,
                }
                .encode()
                .ok()?
                .to_vec(),
            ),
            1 => {
                let request = ImportRequest::decode(input).ok()?;
                if request.session != self.session {
                    return None;
                }
                Some(request.encode().ok()?.to_vec())
            }
            2 if input.is_empty() => {
                let slot = self.slots.iter_mut().find(|s| s.is_none())?;
                let key = ImportKey {
                    session: self.session,
                    number: self.next,
                };
                let next = self.next.checked_add(1)?;
                *slot = Some((key, Publisher::new(key).ok()?));
                self.next = next;
                Some(encode_key(key).ok()?.to_vec())
            }
            6 => {
                let report = Report::decode(input).ok()?;
                if !self.issued(report.import) {
                    return None;
                }
                let directive = if let Some(publisher) = self.publisher(report.import) {
                    publisher.poll(report).ok()?
                } else {
                    Directive {
                        import: report.import,
                        sequence: match report.state {
                            State::Idle => 0,
                            State::Running { sequence } | State::Completed { sequence, .. } => {
                                sequence
                            }
                        },
                        action: Action::Retire,
                    }
                };
                Some(directive.encode().ok()?.to_vec())
            }
            3..=5 | 7..=10 => {
                let key = decode_key(input).ok()?;
                if !self.issued(key) {
                    return None;
                }
                if action == 4 {
                    if let Some(slot) = self
                        .slots
                        .iter_mut()
                        .find(|s| s.as_ref().is_some_and(|(k, _)| *k == key))
                    {
                        *slot = None;
                    }
                    return Some(vec![]);
                }
                let publisher = self.publisher(key)?;
                match action {
                    3 => {
                        publisher.released();
                        Some(vec![])
                    }
                    5 => Some(publisher.request().ok()?.to_be_bytes().to_vec()),
                    7 => Some(
                        Report {
                            import: key,
                            state: publisher.state(),
                        }
                        .encode()
                        .ok()?
                        .to_vec(),
                    ),
                    8 => {
                        publisher.acknowledge_safe().ok()?;
                        // The Java adapter has already applied the confirmed
                        // result to its exact lease. Later polls get Retire via
                        // the issued-key watermark without occupying a slot.
                        if let Some(slot) = self
                            .slots
                            .iter_mut()
                            .find(|s| s.as_ref().is_some_and(|(k, _)| *k == key))
                        {
                            *slot = None;
                        }
                        Some(vec![])
                    }
                    9 => {
                        publisher.mark_uncertain().ok()?;
                        Some(vec![])
                    }
                    10 => Some(vec![u8::from(publisher.can_request())]),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eject::Outcome;
    #[test]
    fn capacity_stale_keys_and_repeated_withdrawal_cannot_recreate_import() {
        let mut book = Book::new([5; 16]).unwrap();
        let mut keys = Vec::new();
        for number in 1..=4 {
            let key = book.execute(2, &[]).unwrap();
            assert_eq!(decode_key(&key).unwrap().number, number);
            keys.push(key);
        }
        assert!(book.execute(2, &[]).is_none());
        let old = decode_key(&keys[0]).unwrap();
        assert!(book.execute(4, &keys[0]).is_some());
        assert!(book.execute(4, &keys[0]).is_some());
        let new = book.execute(2, &[]).unwrap();
        assert_eq!(decode_key(&new).unwrap().number, 5);
        for state in [
            State::Idle,
            State::Running { sequence: 1 },
            State::Completed {
                sequence: 1,
                outcome: Outcome::Safe,
            },
        ] {
            let report = Report { import: old, state };
            let reply = book.execute(6, &report.encode().unwrap()).unwrap();
            assert_eq!(Directive::decode(&reply).unwrap().action, Action::Retire);
        }
        assert!(book.execute(5, &keys[0]).is_none());
        assert!(book.execute(7, &keys[0]).is_none());
        let future = ImportKey { number: 6, ..old };
        assert!(book.execute(4, &encode_key(future).unwrap()).is_none());
        let mut replacement = Book::new([6; 16]).unwrap();
        assert!(replacement.execute(4, &new).is_none());
    }
    #[test]
    fn requests_require_receiver_ready_and_only_os_result_can_be_acknowledged() {
        let mut book = Book::new([7; 16]).unwrap();
        let bytes = book.execute(2, &[]).unwrap();
        let key = decode_key(&bytes).unwrap();
        assert_eq!(book.execute(10, &bytes), Some(vec![0]));
        assert!(book.execute(5, &bytes).is_none());
        assert!(book.execute(8, &bytes).is_none());
        let idle = Report {
            import: key,
            state: State::Idle,
        }
        .encode()
        .unwrap();
        book.execute(6, &idle).unwrap();
        assert_eq!(book.execute(10, &bytes), Some(vec![1]));
        assert_eq!(book.execute(5, &bytes), Some(1u64.to_be_bytes().to_vec()));
        book.execute(3, &bytes).unwrap();
        assert_eq!(book.execute(10, &bytes), Some(vec![0]));
        book.execute(9, &bytes).unwrap();
        let safe = Report {
            import: key,
            state: State::Completed {
                sequence: 1,
                outcome: Outcome::Safe,
            },
        }
        .encode()
        .unwrap();
        book.execute(6, &safe).unwrap();
        assert_eq!(book.execute(7, &bytes), Some(safe.to_vec()));
        book.execute(8, &bytes).unwrap();
        let reply = book.execute(6, &safe).unwrap();
        assert_eq!(Directive::decode(&reply).unwrap().action, Action::Retire);
        assert!(book.execute(5, &bytes).is_none());
    }
    #[test]
    fn invalid_dimensions_session_and_unknown_commands_have_no_side_effects() {
        assert!(Book::new([0; 16]).is_none());
        let mut book = Book::new([1; 16]).unwrap();
        for n in 1..=64 {
            assert!(book.execute(2, &vec![0; n]).is_none());
            assert!(book.execute(0, &vec![0; n]).is_none());
        }
        for action in [11, 12, u32::MAX] {
            assert!(book.execute(action, &[]).is_none());
        }
        let mut id = [0; 32];
        id[..3].copy_from_slice(b"1-7");
        for session in [[1; 16], [2; 16]] {
            let request = ImportRequest {
                busid: id,
                wan: false,
                session,
            }
            .encode()
            .unwrap();
            assert_eq!(book.execute(1, &request).is_some(), session == [1; 16]);
        }
        assert_eq!(
            decode_key(&book.execute(2, &[]).unwrap()).unwrap().number,
            1
        );
        book.next = u64::MAX;
        assert!(book.execute(2, &[]).is_none());
    }
}
