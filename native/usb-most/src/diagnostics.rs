//! Internal diagnostic records. Raw bytes and digests must never be exported.
//! The Java adapter compares the transient digest, then exports only safe fields.
use std::{collections::VecDeque, time::Instant};
use zeroize::Zeroize;

pub const CAPACITY: usize = 256;
pub const STRIDE: usize = 9;

/// This is an accidental-copy-corruption check, not a security authenticator.
pub fn digest(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

/// [kind, receiver slot, value]. No coordinates, keys, strings or raw payload.
pub fn classify(data: &[u8]) -> [i64; 3] {
    if data.len() < 2 || !(1..=7).contains(&data[1]) {
        return [0; 3];
    }
    let slot = i64::from(data[1]);
    match (data.len(), data[0]) {
        (15, 0x20) if data[2] == 2 => [1, slot, i64::from(data[3] & 31)],
        (15, 0x20) if data[2] == 0x42 => [2, slot, i64::from(data[3] == 1)],
        (7, 0x10) if data[2] == 0x41 => [3, slot, i64::from(data[4] & 64 != 0)],
        (7, 0x10) if data[2] == 0x8f => [4, slot, i64::from(data[5])],
        (20, 0x11)
            if data[2] == 0x0a && data[6] == 0 && matches!(data[5], 0 | 0xae | 0xaf | 0xb0) =>
        {
            [5, slot, i64::from(data[5])]
        }
        _ => [0; 3],
    }
}

pub struct Trace {
    started: Instant,
    records: VecDeque<[i64; STRIDE]>,
    total: i64,
    typed: i64,
    dropped: i64,
}
impl Trace {
    pub fn new() -> Result<Self, i32> {
        let mut records = VecDeque::new();
        records
            .try_reserve_exact(CAPACITY)
            .map_err(|_| libc::ENOMEM)?;
        Ok(Self {
            started: Instant::now(),
            records,
            total: 0,
            typed: 0,
            dropped: 0,
        })
    }
    pub fn record(&mut self, id: i32, endpoint: u8, status: i32, actual: i32, data: &[u8]) {
        // Enabled only for the verified receiver; never read other endpoints.
        if endpoint != 0x83 {
            return;
        }
        self.total = self.total.saturating_add(1);
        let meta = if status == 0 { classify(data) } else { [0; 3] };
        self.typed = self.typed.saturating_add(i64::from(meta[0] != 0));
        if self.records.len() == CAPACITY {
            if let Some(mut discarded) = self.records.pop_front() {
                discarded.zeroize();
            }
            self.dropped = self.dropped.saturating_add(1);
        }
        // A transient digest can include keyboard bytes, but is compared and
        // destroyed inside the process. It is NOT a diagnostic export field.
        self.records.push_back([
            i64::from(id),
            i64::from(status),
            i64::from(actual),
            self.started.elapsed().as_millis().min(i64::MAX as u128) as i64,
            meta[0],
            meta[1],
            meta[2],
            digest(data) as i64,
            i64::from(endpoint),
        ]);
    }
    pub fn drain(&mut self) -> Result<Vec<i64>, i32> {
        let mut out = Vec::new();
        out.try_reserve_exact(6 + self.records.len() * STRIDE)
            .map_err(|_| libc::ENOMEM)?;
        out.extend([
            1,
            STRIDE as i64,
            self.records.len() as i64,
            self.total,
            self.typed,
            self.dropped,
        ]);
        for mut record in self.records.drain(..) {
            out.extend_from_slice(&record);
            record.zeroize();
        }
        Ok(out)
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        for record in &mut self.records {
            record.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_drops_keyboard_and_coordinates() {
        let mut packet = [0xff; 15];
        packet[..4].copy_from_slice(&[0x20, 1, 2, 1]);
        assert_eq!(classify(&packet), [1, 1, 1]);
        packet[4..].fill(0);
        assert_eq!(classify(&packet), [1, 1, 1]);
        packet[2] = 1;
        assert_eq!(classify(&packet), [0; 3]);
        for size in 0..32 {
            let bytes = vec![0; size];
            assert_eq!(classify(&bytes), [0; 3]);
        }
        assert_eq!(classify(&[0x10, 1, 0x41, 4, 0x42, 0x2d, 0x40]), [3, 1, 1]);
        assert_eq!(
            classify(&[0x20, 1, 0x42, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            [2, 1, 1]
        );
    }
    #[test]
    fn corruption_check_and_bounded_retention() {
        assert_eq!(digest(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(digest(b"hello"), 0xa430_d846_80aa_bd0b);
        for byte in 0..=255u8 {
            assert_ne!(digest(&[byte]), digest(&[byte ^ 1]));
        }
        let mut trace = Trace::new().unwrap();
        trace.record(99, 0x82, 0, 15, &[0; 15]);
        for id in 0..300 {
            trace.record(
                id,
                0x83,
                0,
                15,
                &[0x20, 1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
        }
        let records = trace.drain().unwrap();
        assert_eq!(&records[..6], &[1, 9, 256, 300, 300, 44]);
        assert_eq!(records[6], 44);
        assert_eq!(&trace.drain().unwrap()[..6], &[1, 9, 0, 300, 300, 44]);
    }
}
