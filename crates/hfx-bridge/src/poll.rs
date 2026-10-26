// SPDX-License-Identifier: GPL-2.0-only

//! epoll and one deadline, for the bridge's loop and the keeper's. Neither
//! wakes up on its own: each round sets its earliest timer with
//! [`Poller::wake_at`], and the next wait ends when an fd is ready or that
//! time comes. The wait clears it. No old deadline is left behind.

use rustix::buffer::spare_capacity;
use rustix::event::epoll::{self, EventData, EventFlags};
use rustix::fd::{AsFd, OwnedFd};
use rustix::time::Timespec;
use std::io;

pub struct Ready {
    pub token: u64,
    pub readable: bool,
    pub writable: bool,
    pub hangup: bool,
}

pub struct Poller {
    epoll: OwnedFd,
    deadline: Option<u64>,
    events: Vec<epoll::Event>,
}

/// Longest single sleep. It keeps the millisecond timeout inside an i32.
const MAX_SLEEP_MS: u64 = 3_600_000;

impl Poller {
    /// # Errors
    /// Fails when epoll_create fails.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            epoll: epoll::create(epoll::CreateFlags::CLOEXEC)?,
            deadline: None,
            events: Vec::with_capacity(64),
        })
    }

    /// # Errors
    /// Fails when epoll_ctl fails.
    pub fn add(&self, fd: impl AsFd, token: u64) -> io::Result<()> {
        epoll::add(&self.epoll, fd, EventData::new_u64(token), EventFlags::IN)?;
        Ok(())
    }

    /// Watches `fd` for one readable event. [`Poller::rearm`] waits for the
    /// next one.
    ///
    /// # Errors
    /// Fails when epoll_ctl fails.
    pub fn add_once(&self, fd: impl AsFd, token: u64) -> io::Result<()> {
        let flags = EventFlags::IN | EventFlags::ONESHOT;
        epoll::add(&self.epoll, fd, EventData::new_u64(token), flags)?;
        Ok(())
    }

    /// # Errors
    /// Fails when epoll_ctl fails.
    pub fn rearm(&self, fd: impl AsFd, token: u64) -> io::Result<()> {
        let flags = EventFlags::IN | EventFlags::ONESHOT;
        epoll::modify(&self.epoll, fd, EventData::new_u64(token), flags)?;
        Ok(())
    }

    /// Switches write interest on or off for an fd already added.
    ///
    /// # Errors
    /// Fails when epoll_ctl fails.
    pub fn want_write(&self, fd: impl AsFd, token: u64, on: bool) -> io::Result<()> {
        let flags = if on {
            EventFlags::IN | EventFlags::OUT
        } else {
            EventFlags::IN
        };
        epoll::modify(&self.epoll, fd, EventData::new_u64(token), flags)?;
        Ok(())
    }

    pub fn remove(&self, fd: impl AsFd) {
        // Closing the fd removes it anyway. This matters for fds that stay
        // open (a receiver file being replaced, say).
        let _ = epoll::delete(&self.epoll, fd);
    }

    /// The next wait ends at `at_ms` at the latest.
    pub fn wake_at(&mut self, at_ms: u64) {
        self.deadline = Some(at_ms);
    }

    /// Waits for readiness or the deadline set since the last wait.
    ///
    /// # Errors
    /// Fails when epoll_wait fails with anything but EINTR.
    pub fn wait(&mut self, now_ms: u64) -> io::Result<Vec<Ready>> {
        let timeout = self.deadline.take().map(|d| {
            let ms = d.saturating_sub(now_ms).min(MAX_SLEEP_MS);
            Timespec {
                tv_sec: i64::try_from(ms / 1000).unwrap_or(0),
                tv_nsec: i64::try_from((ms % 1000) * 1_000_000).unwrap_or(0),
            }
        });
        self.events.clear();
        match epoll::wait(
            &self.epoll,
            spare_capacity(&mut self.events),
            timeout.as_ref(),
        ) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        Ok(self
            .events
            .iter()
            .map(|event| {
                let flags = event.flags;
                Ready {
                    token: event.data.u64(),
                    readable: flags.intersects(EventFlags::IN | EventFlags::RDNORM),
                    writable: flags.contains(EventFlags::OUT),
                    hangup: flags.intersects(EventFlags::HUP | EventFlags::ERR),
                }
            })
            .collect())
    }
}
