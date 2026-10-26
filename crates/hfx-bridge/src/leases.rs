// SPDX-License-Identifier: GPL-2.0-only

//! At most one lease per device. Clients lease for 1 to 60 s. The bridge's
//! own holders (OpenRazer routes, battery probes) follow the burst rule:
//! held while an exchange is in flight, kept 25 ms after a matched reply so
//! the next report of a burst finds it, dropped at once after anything else.

use hfx_proto::ids::{ClientName, lower_hex};
use hfx_proto::messages::Lane;

pub const BURST_IDLE_MS: u64 = 25;
/// More than the devices of 16 receivers could ever need.
pub const MAX_LEASES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    Client { conn: u64, name: ClientName },
    OpenRazer,
    HyperFlux,
}

impl Holder {
    #[must_use]
    pub fn name(&self) -> ClientName {
        match self {
            Self::Client { name, .. } => name.clone(),
            Self::OpenRazer => ClientName::new("OpenRazer").expect("short"),
            Self::HyperFlux => ClientName::new("HyperFlux").expect("short"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Lease {
    pub id: String,
    pub rx: u64,
    pub lane: Lane,
    pub holder: Holder,
    pub expires: u64,
}

#[derive(Default)]
pub struct Leases {
    list: Vec<Lease>,
    counter: u64,
}

pub enum Acquire<'a> {
    Granted(&'a Lease),
    Busy(&'a Lease),
    Full,
}

impl Leases {
    #[must_use]
    pub fn on_device(&self, rx: u64, lane: Lane) -> Option<&Lease> {
        self.list.iter().find(|l| l.rx == rx && l.lane == lane)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Lease> {
        self.list.iter().find(|l| l.id == id)
    }

    pub fn acquire(&mut self, rx: u64, lane: Lane, holder: Holder, expires: u64) -> Acquire<'_> {
        if let Some(i) = self.list.iter().position(|l| l.rx == rx && l.lane == lane) {
            return Acquire::Busy(&self.list[i]);
        }
        if self.list.len() >= MAX_LEASES {
            return Acquire::Full;
        }
        self.counter += 1;
        let mut random = [0_u8; 4];
        let _ = getrandom::fill(&mut random);
        self.list.push(Lease {
            id: format!("lease-{}-{}", lower_hex(&random), self.counter),
            rx,
            lane,
            holder,
            expires,
        });
        Acquire::Granted(self.list.last().expect("just pushed"))
    }

    /// Takes the device for an internal holder, or reuses its own lease.
    /// False when someone else holds it.
    pub fn take_internal(&mut self, rx: u64, lane: Lane, holder: &Holder) -> bool {
        if let Some(lease) = self.list.iter_mut().find(|l| l.rx == rx && l.lane == lane) {
            if &lease.holder != holder {
                return false;
            }
            lease.expires = u64::MAX;
            return true;
        }
        matches!(
            self.acquire(rx, lane, holder.clone(), u64::MAX),
            Acquire::Granted(_)
        )
    }

    /// After an internal exchange: keep for the burst window, or drop.
    pub fn after_internal(
        &mut self,
        rx: u64,
        lane: Lane,
        holder: &Holder,
        matched: bool,
        now: u64,
    ) {
        if matched {
            if let Some(lease) = self
                .list
                .iter_mut()
                .find(|l| l.rx == rx && l.lane == lane && &l.holder == holder)
            {
                lease.expires = now + BURST_IDLE_MS;
            }
        } else {
            self.list
                .retain(|l| !(l.rx == rx && l.lane == lane && &l.holder == holder));
        }
    }

    pub fn renew(&mut self, id: &str, expires: u64) {
        if let Some(lease) = self.list.iter_mut().find(|l| l.id == id) {
            lease.expires = expires;
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.list.retain(|l| l.id != id);
    }

    pub fn end_device(&mut self, rx: u64, lane: Lane) {
        self.list.retain(|l| !(l.rx == rx && l.lane == lane));
    }

    pub fn end_receiver(&mut self, rx: u64) {
        self.list.retain(|l| l.rx != rx);
    }

    pub fn end_connection(&mut self, conn: u64) {
        self.list
            .retain(|l| !matches!(l.holder, Holder::Client { conn: c, .. } if c == conn));
    }

    pub fn expire(&mut self, now: u64) {
        self.list.retain(|l| l.expires > now);
    }

    #[must_use]
    pub fn next_expiry(&self) -> Option<u64> {
        self.list
            .iter()
            .map(|l| l.expires)
            .filter(|e| *e != u64::MAX)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(conn: u64) -> Holder {
        Holder::Client {
            conn,
            name: ClientName::new("Test").unwrap(),
        }
    }

    #[test]
    fn one_lease_per_device() {
        let mut leases = Leases::default();
        let id = match leases.acquire(1, Lane::Pointer, client(1), 1000) {
            Acquire::Granted(l) => l.id.clone(),
            _ => panic!("first acquire"),
        };
        assert!(id.starts_with("lease-") && id.ends_with("-1"));
        assert!(
            matches!(leases.acquire(1, Lane::Pointer, client(2), 1000), Acquire::Busy(l) if l.id == id)
        );
        assert!(matches!(
            leases.acquire(1, Lane::Keyboard, client(2), 1000),
            Acquire::Granted(_)
        ));
        assert!(!leases.take_internal(1, Lane::Pointer, &Holder::OpenRazer));
        leases.end_connection(1);
        assert!(leases.take_internal(1, Lane::Pointer, &Holder::OpenRazer));
    }

    #[test]
    fn burst_rule() {
        let mut leases = Leases::default();
        let holder = Holder::OpenRazer;
        assert!(leases.take_internal(2, Lane::Keyboard, &holder));
        leases.expire(0);
        assert_eq!(leases.next_expiry(), None);
        leases.after_internal(2, Lane::Keyboard, &holder, true, 100);
        leases.expire(110);
        assert_eq!(leases.next_expiry(), Some(125));
        assert!(leases.take_internal(2, Lane::Keyboard, &holder));
        leases.after_internal(2, Lane::Keyboard, &holder, false, 120);
        assert!(leases.on_device(2, Lane::Keyboard).is_none());
        leases.after_internal(2, Lane::Keyboard, &holder, true, 130);
        assert!(leases.on_device(2, Lane::Keyboard).is_none());
        assert!(leases.take_internal(2, Lane::Keyboard, &holder));
        leases.after_internal(2, Lane::Keyboard, &holder, true, 200);
        leases.expire(225);
        assert!(leases.on_device(2, Lane::Keyboard).is_none());
    }
}
