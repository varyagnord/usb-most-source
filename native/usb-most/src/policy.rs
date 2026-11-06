//! Pure, bounded application policy. No Android objects, I/O or global state.

pub const MAX_DEVICES: usize = 128;

/// Shell commands contain only validated literals and Android-derived numeric bus IDs.
/// This generates text; it never starts a process or imports a USB device.
pub fn connection_commands(host: &str, mode: i32, ids: &[i32]) -> Option<String> {
    if !valid_host(host, mode) || ids.len() > MAX_DEVICES {
        return None;
    }
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    if sorted.windows(2).any(|p| p[0] == p[1])
        || sorted
            .iter()
            .any(|&id| !(1..=65535).contains(&(id / 1000)) || id % 1000 == 0)
    {
        return None;
    }
    use std::fmt::Write;
    let mut out = format!("sudo modprobe vhci_hcd\nusbip list -r {host}");
    for id in sorted {
        write!(
            out,
            "\nsudo usbip attach -r {host} -b {}-{}",
            id / 1000,
            id % 1000
        )
        .ok()?;
    }
    Some(out)
}
pub fn valid_host(host: &str, mode: i32) -> bool {
    usb_most_connection::config::valid_legacy_host(host, mode)
}

/// 0 supported; 1 hub; 2 ISO; 3 no usable interfaces; 4 unsupported bulk streams.
pub fn capability(class: i32, configurations: i32, interfaces: i32, flags: i32) -> i32 {
    if class == 9 || flags & 1 != 0 {
        1
    } else if flags & 2 != 0 {
        2
    } else if configurations <= 0 || interfaces <= 0 {
        3
    } else if flags & 4 != 0 {
        4
    } else {
        0
    }
}

#[derive(Clone, Copy, Default)]
struct Entry {
    id: i32,
    eligible: bool,
    selected: bool,
}
pub struct Selection {
    entries: [Entry; MAX_DEVICES],
    len: usize,
    all: bool,
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            entries: [Entry::default(); MAX_DEVICES],
            len: 0,
            all: false,
        }
    }
}
impl Selection {
    /// Validate the whole snapshot before mutation; disappeared IDs lose selection.
    /// Android detach explicitly removes an ID, even if an identical ID is reused.
    pub fn sync(&mut self, pairs: &[i32]) -> bool {
        if !pairs.len().is_multiple_of(2) || pairs.len() / 2 > MAX_DEVICES {
            return false;
        }
        for (i, p) in pairs.as_chunks::<2>().0.iter().enumerate() {
            if p[0] < 0
                || !matches!(p[1], 0 | 1)
                || pairs[..i * 2]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .any(|q| q[0] == p[0])
            {
                return false;
            }
        }
        let mut next = [Entry::default(); MAX_DEVICES];
        for (slot, p) in next.iter_mut().zip(pairs.as_chunks::<2>().0.iter()) {
            let eligible = p[1] == 1;
            *slot = Entry {
                id: p[0],
                eligible,
                selected: eligible
                    && (self.all
                        || self.entries[..self.len]
                            .iter()
                            .any(|e| e.id == p[0] && e.selected)),
            };
        }
        self.entries = next;
        self.len = pairs.len() / 2;
        true
    }
    pub fn select(&mut self, id: i32, selected: bool) -> bool {
        let Some(e) = self.entries[..self.len].iter_mut().find(|e| e.id == id) else {
            return false;
        };
        if selected && !e.eligible {
            return false;
        }
        e.selected = selected;
        if !selected {
            self.all = false;
        }
        true
    }
    pub fn select_all(&mut self, selected: bool) {
        self.all = selected;
        for e in &mut self.entries[..self.len] {
            e.selected = selected && e.eligible;
        }
    }
    pub fn snapshot(&self) -> Vec<i32> {
        let mut out = Vec::with_capacity(self.len + 1);
        out.push(i32::from(self.all));
        out.extend(
            self.entries[..self.len]
                .iter()
                .filter(|e| e.selected)
                .map(|e| e.id),
        );
        out
    }
}

/// Validate actual descriptor framing and return [bcdDevice, configs, firstConfig,
/// firstInterfaceCount, class, subclass, protocol, capability]. No untrusted offsets.
pub fn describe(raw: &[u8]) -> Option<[i32; 8]> {
    if raw.len() < 18 || raw.len() > 65536 || raw[0] != 18 || raw[1] != 1 || raw[17] == 0 {
        return None;
    }
    let mut at = 18;
    let mut configurations = 0;
    let mut first = (0, 0);
    let mut flags = 0;
    while at < raw.len() {
        let len = *raw.get(at)? as usize;
        let ty = *raw.get(at + 1)?;
        if len < 2 || at + len > raw.len() {
            return None;
        }
        if ty != 2 {
            at += len;
            continue;
        }
        if len < 9 {
            return None;
        }
        let total = u16::from_le_bytes([raw[at + 2], raw[at + 3]]) as usize;
        let end = at.checked_add(total)?;
        if total < len || end > raw.len() || raw[at + 5] == 0 {
            return None;
        }
        let count = raw[at + 4];
        if configurations == 0 {
            first = (raw[at + 5], count);
        }
        configurations += 1;
        let mut interfaces = [false; 256];
        let mut zero = [false; 256];
        let mut alternatives = Vec::new();
        let mut cursor = at + len;
        let mut interface_seen = false;
        let mut addresses = 0u32;
        while cursor < end {
            let n = *raw.get(cursor)? as usize;
            let kind = *raw.get(cursor + 1)?;
            if n < 2 || cursor + n > end {
                return None;
            }
            if kind == 4 {
                if n < 9 {
                    return None;
                }
                let id = raw[cursor + 2];
                let alt = raw[cursor + 3];
                if alternatives.contains(&(id, alt)) {
                    return None;
                }
                alternatives.push((id, alt));
                interfaces[id as usize] = true;
                if alt == 0 {
                    zero[id as usize] = true;
                }
                if raw[cursor + 5] == 9 {
                    flags |= 1;
                }
                // Real devices can misreport bNumEndpoints (including Rutoken Lite).
                // Like Linux, use the bounded, framed endpoint descriptors themselves.
                interface_seen = true;
                addresses = 0;
            } else if kind == 5 {
                if n < 7 || !interface_seen {
                    return None;
                }
                let ep = raw[cursor + 2];
                if ep & 0x70 != 0 || ep & 15 == 0 {
                    return None;
                }
                let bit = 1u32 << ((ep & 15) + (if ep & 128 != 0 { 16 } else { 0 }));
                if addresses & bit != 0 {
                    return None;
                }
                addresses |= bit;
                if raw[cursor + 3] & 3 == 1 {
                    flags |= 2;
                }
            }
            cursor += n;
        }
        if interfaces.iter().filter(|&&x| x).count() != count as usize
            || interfaces.iter().zip(zero).any(|(&i, z)| i && !z)
        {
            return None;
        }
        at = end;
    }
    if configurations != raw[17] as usize {
        return None;
    }
    Some([
        u16::from_le_bytes([raw[12], raw[13]]) as i32,
        configurations as i32,
        first.0 as i32,
        first.1 as i32,
        raw[4] as i32,
        raw[5] as i32,
        raw[6] as i32,
        capability(raw[4] as i32, configurations as i32, first.1 as i32, flags),
    ])
}

#[cfg(test)]
mod tests;
