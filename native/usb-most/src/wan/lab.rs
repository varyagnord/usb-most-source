//! Virtual-time WAN fault model. Uses the real bounded pump and fake kernel, no USB access.
use super::{Session, sim::Fake};
use std::{collections::VecDeque, sync::Arc};
#[derive(Debug)]
pub struct ResultStats {
    pub produced: u64,
    pub delivered: u64,
    pub virtual_ms: u64,
    pub highwater: u64,
    pub max_transit: u64,
}
fn random(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}
pub fn scenario(rtt: u32, jitter: u32, loss: u32, count: u32) -> Result<ResultStats, i32> {
    if rtt > 2000 || jitter > 500 || loss > 50 || count == 0 || count > 10000 {
        return Err(libc::EINVAL);
    }
    let f = Arc::new(Fake::default());
    let s = Session::new(f.clone());
    s.command(1, 1)?;
    s.command(5, 0)?;
    let result = (|| {
        let mut produced = 0u64;
        let mut delivered = 0u64;
        let mut now = 0u64;
        let mut seed = 0x716553u64;
        let mut network = VecDeque::<(u64, u32, Vec<u8>)>::new();
        let mut acknowledgments = VecDeque::<(u64, u32)>::new();
        let mut last_rx = 0;
        let mut last_ack = 0;
        let mut highwater = 0;
        let mut max_transit = 0;
        while delivered < u64::from(count) {
            if now > u64::from(count) * 8 + 30000 {
                return Err(libc::ETIMEDOUT);
            }
            if produced < u64::from(count) && now == produced * 8 {
                let mut bytes = [0u8; 15];
                bytes[..4].copy_from_slice(&(produced as u32).to_be_bytes());
                bytes[4] = u8::from(produced * 8 < 60000);
                if !f.emit(&bytes) {
                    return Err(libc::ENODATA);
                }
                produced += 1;
            }
            while acknowledgments.front().is_some_and(|a| a.0 <= now) {
                let (_, ack) = acknowledgments.pop_front().unwrap();
                s.command(2, ack as i32)?;
            }
            for _ in 0..4 {
                if let Some(c) = s.reap(0)? {
                    let delay = u64::from(rtt) / 2
                        + if jitter == 0 {
                            0
                        } else {
                            random(&mut seed) % (u64::from(jitter) + 1)
                        };
                    // TCP retransmission model: delay bytes, never silently delete/reorder the stream.
                    let retry = if random(&mut seed) % 1000 < u64::from(loss) {
                        u64::from(rtt).max(100) * 2
                    } else {
                        0
                    };
                    let due = (now + delay + retry).max(last_rx);
                    last_rx = due;
                    network.push_back((due, c.wan_sequence, c.data().unwrap_or(&[]).to_vec()));
                } else {
                    break;
                }
            }
            while network.front().is_some_and(|p| p.0 <= now) {
                let (due, seq, bytes) = network.pop_front().unwrap();
                if seq as u64 != delivered + 1
                    || bytes.len() != 15
                    || u32::from_be_bytes(bytes[..4].try_into().unwrap()) as u64 != delivered
                    || bytes[4] != u8::from(delivered * 8 < 60000)
                {
                    return Err(libc::EPROTO);
                }
                max_transit = max_transit.max(due - delivered * 8);
                delivered += 1;
                let ack_at = (now + u64::from(rtt) / 2 + u64::from(jitter)).max(last_ack);
                last_ack = ack_at;
                acknowledgments.push_back((ack_at, seq));
            }
            highwater = highwater.max(s.command(4, 0)?[8] as u64);
            if delivered == u64::from(count) {
                break;
            }
            let next_source = if produced < u64::from(count) {
                produced * 8
            } else {
                u64::MAX
            };
            now = next_source
                .min(network.front().map_or(u64::MAX, |p| p.0))
                .min(acknowledgments.front().map_or(u64::MAX, |p| p.0));
            if now == u64::MAX {
                return Err(libc::EDEADLK);
            }
        }
        Ok(ResultStats {
            produced,
            delivered,
            virtual_ms: now,
            highwater,
            max_transit,
        })
    })();
    if s.close() != 0 {
        return Err(libc::EIO);
    }
    result
}
pub fn sample(
    rtt: u32,
    jitter: u32,
    loss: u32,
    count: u32,
) -> Result<crate::engine::Completion, i32> {
    let stats = scenario(rtt, jitter, loss, count)?;
    if stats.max_transit > stats.virtual_ms {
        return Err(libc::EPROTO);
    }
    let mut data = Vec::new();
    for n in [
        stats.produced,
        stats.delivered,
        stats.virtual_ms,
        stats.highwater,
    ] {
        data.extend(n.to_be_bytes());
    }
    let f = Arc::new(Fake::default());
    let s = Session::new(f.clone());
    s.command(1, 1)?;
    s.command(5, 0)?;
    if !f.emit(&data) {
        return Err(libc::EIO);
    }
    let c = s.reap(0)?.ok_or(libc::EIO)?;
    if s.close() != 0 {
        return Err(libc::EIO);
    }
    Ok(c)
}
