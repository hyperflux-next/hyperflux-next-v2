// SPDX-License-Identifier: GPL-2.0-only

//! The virtual OpenRazer devices, with the test playing the kernel's side
//! of each UHID device and a temporary sysfs tree.

mod common;

use common::{Harness, Options};
use hfx_bridge::uhid::{self, Written};
use hfx_proto::client::Client;
use hfx_proto::devices::Command;
use hfx_proto::messages::{
    Charge, DeviceInfo, Lane, OpenRazerState, Slot, Snapshot, TransactOutcome,
};
use hfx_proto::openrazer::{DESCRIPTOR, Uncertainty, envelope, parse_envelope};
use hfx_proto::report::{self, Report};
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::{Duration, Instant};

/// The kernel end of one virtual device.
struct Device {
    socket: UnixDatagram,
    create: Written,
}

impl Device {
    fn send(&self, event: &[u8]) {
        self.socket.send(event).unwrap();
    }

    fn recv(&self, ms: u64) -> Option<Written> {
        self.socket
            .set_read_timeout(Some(Duration::from_millis(ms)))
            .unwrap();
        let mut buffer = vec![0_u8; uhid::EVENT_BYTES + 1];
        let n = self.socket.recv(&mut buffer).ok()?;
        uhid::decode_written(&buffer[..n])
    }

    fn phys(&self) -> String {
        match &self.create {
            Written::Create { phys, .. } => phys.clone(),
            _ => unreachable!(),
        }
    }

    fn product(&self) -> u32 {
        match &self.create {
            Written::Create { product, .. } => *product,
            _ => unreachable!(),
        }
    }

    /// SET then GET, as the patched driver does. Returns the GET's data.
    fn query(&self, id: u32, request: &Report) -> Vec<u8> {
        self.send(&uhid::kernel_set(id, request));
        assert_eq!(self.recv(2000), set_reply(id, 0));
        self.send(&uhid::kernel_get(id + 1));
        self.got(id + 1)
    }

    /// The data of the answer to GET `id`, which has to come next.
    fn got(&self, id: u32) -> Vec<u8> {
        match self.recv(3000) {
            Some(Written::GetReply {
                id: got,
                errno: 0,
                data,
            }) if got == id => data,
            other => panic!("expected the answer to GET {id}, got {other:?}"),
        }
    }
}

struct Rig {
    h: Harness,
    or: common::OpenRazer,
    /// Devices created before anyone asked for them.
    early: std::cell::RefCell<Vec<Device>>,
}

impl Rig {
    fn new() -> Self {
        Self::with(Options::default())
    }

    fn with(mut options: Options) -> Self {
        options.openrazer = true;
        let (h, or) = Harness::with(options);
        Self {
            h,
            or: or.unwrap(),
            early: std::cell::RefCell::default(),
        }
    }

    /// The next device the bridge creates for `product`, started.
    fn device(&self, product: u32) -> Device {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut early = self.early.borrow_mut();
        let device = match early.iter().position(|d| d.product() == product) {
            Some(i) => early.remove(i),
            None => loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let (create, fd) = self
                    .or
                    .devices
                    .recv_timeout(left)
                    .expect("a virtual device");
                let device = Device {
                    socket: UnixDatagram::from(fd),
                    create: uhid::decode_written(&create).unwrap(),
                };
                if device.product() == product {
                    break device;
                }
                early.push(device);
            },
        };
        device.send(&uhid::kernel_start(0));
        device
    }

    fn drivers(&self) -> std::path::PathBuf {
        self.or.sys.path().join("drivers")
    }

    fn uevent(&self, action: &str) {
        let message =
            format!("{action}@/devices/x\0ACTION={action}\0SUBSYSTEM=hid\0DRIVER=razermouse\0");
        self.or.uevents.send(message.as_bytes()).unwrap();
    }

    fn serial_reads(&self, lane: Lane) -> usize {
        self.h
            .sim
            .requests(0, lane)
            .iter()
            .filter(|r| r[6] == 0x00 && r[7] == 0x82)
            .count()
    }
}

/// A `serial_sha256` attribute value, as the patched driver writes it: the
/// fingerprint of the serial, which equals the bridge's own.
fn serial_hash(serial: &[u8]) -> String {
    let print = hfx_bridge::decode::fingerprint(serial).unwrap();
    format!("{}\n", hfx_proto::ids::lower_hex(&print))
}

fn bind(drivers: &Path, driver: &str, name: &str, phys: &str) {
    let dir = drivers.join(driver).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("uevent"),
        format!("DRIVER={driver}\nHID_PHYS={phys}\n"),
    )
    .unwrap();
}

/// The answer to the kernel's SET `id`.
fn set_reply(id: u32, errno: u16) -> Option<Written> {
    Some(Written::SetReply { id, errno })
}

/// The pointer's battery query, the one OpenRazer sends most.
fn battery() -> Report {
    report::build(0x1f, 0x07, 0x80, &[0, 0])
}

#[test]
fn two_serial_reads_publish() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let Written::Create {
        name,
        phys,
        uniq,
        bus,
        vendor,
        version,
        descriptor,
        ..
    } = &mouse.create
    else {
        panic!()
    };
    assert_eq!(name, "Razer Basilisk V3 Pro 35K");
    let label = phys
        .strip_prefix(hfx_proto::openrazer::PHYS_PREFIX)
        .unwrap();
    assert!(hfx_proto::openrazer::label_ok(label) && label.starts_with("native-"));
    assert_eq!(
        (uniq.as_str(), *bus, *vendor, *version),
        ("", 0x06, 0x1532, 0x0100)
    );
    assert_eq!(descriptor.as_slice(), DESCRIPTOR);
    assert_eq!(rig.serial_reads(Lane::Pointer), 2);
    let keyboard = rig.device(0x0296);
    assert_ne!(keyboard.phys(), mouse.phys());
    // The keyboard's serial read uses OpenRazer's 0xff, and the receiver saw
    // its lane selector.
    let serial = keyboard.query(1, &hfx_bridge::decode::serial_request(0xff));
    assert_eq!((serial[0], serial[1]), (2, 0xff));
    assert!(
        rig.h
            .sim
            .requests(0, Lane::Keyboard)
            .iter()
            .all(|r| r[1] == 0x9f)
    );
}

#[test]
fn set_acked_before_exchange() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let battery = battery();
    let started = Instant::now();
    mouse.send(&uhid::kernel_set(7, &battery));
    assert_eq!(mouse.recv(2000), set_reply(7, 0));
    // The acknowledgement doesn't wait for the 8 ms exchange.
    assert!(started.elapsed() < Duration::from_millis(8));
    mouse.send(&uhid::kernel_get(8));
    let data = mouse.got(8);
    assert_eq!((data[0], data[1], data[9]), (2, 0x1f, 202));
    // Each answer is handed over once.
    mouse.send(&uhid::kernel_get(9));
    assert_eq!(
        mouse.recv(1000),
        Some(Written::GetReply {
            id: 9,
            errno: uhid::EIO,
            data: vec![]
        })
    );
    // The device mode stays local: nothing reaches the receiver.
    let before = rig.h.sim.requests(0, Lane::Pointer).len();
    let mode = mouse.query(10, &report::build(0x1f, 0x00, 0x04, &[0x03, 0x00]));
    assert_eq!(mode[0], 2);
    let read = mouse.query(12, &report::build(0x1f, 0x00, 0x84, &[0, 0]));
    assert_eq!(&read[8..10], &[0x03, 0x00]);
    assert_eq!(rig.h.sim.requests(0, Lane::Pointer).len(), before);
}

#[test]
fn unknown_command_refused() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let before = rig.h.sim.requests(0, Lane::Pointer).len();
    mouse.send(&uhid::kernel_set(1, &report::build(0x1f, 0x05, 0x01, &[1])));
    assert_eq!(mouse.recv(1000), set_reply(1, uhid::EIO));
    mouse.send(&uhid::kernel_set(2, &[0_u8; 64]));
    assert_eq!(mouse.recv(1000), set_reply(2, uhid::EIO));
    assert_eq!(rig.h.sim.requests(0, Lane::Pointer).len(), before);
    let s = rig.h.client(&[]).snapshot().unwrap();
    let r = common::device(&s, 0, Lane::Pointer).unwrap().refused[0];
    assert_eq!((r.class, r.id, r.count), (0x05, 0x01, 1));
    // The device still works.
    let data = mouse.query(3, &battery());
    assert_eq!(data[0], 2);
}

#[test]
fn busy_query_retries() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let battery = battery();
    let before = rig.h.sim.requests(0, Lane::Pointer).len();
    rig.h.sim.set_busy(0, Lane::Pointer, 2);
    let data = mouse.query(1, &battery);
    assert_eq!((data[0], data[9]), (2, 202), "the third attempt answers");
    rig.h.sim.set_busy(0, Lane::Pointer, 9);
    let data = mouse.query(3, &battery);
    assert_eq!(
        parse_envelope(&data.as_slice().try_into().unwrap()),
        Some(Uncertainty::MayHaveBeenSent)
    );
    assert_eq!(rig.h.sim.requests(0, Lane::Pointer).len(), before + 3 + 5);
    // A write is sent once and comes back as the device answered it.
    rig.h.sim.set_busy(0, Lane::Pointer, 1);
    let data = mouse.query(5, &report::build(0x1f, 0x0f, 0x04, &[1, 0, 200]));
    assert_eq!(data[0], report::STATUS_BUSY);
    assert_eq!(rig.h.sim.requests(0, Lane::Pointer).len(), before + 9);
    rig.h.sim.set_busy(0, Lane::Pointer, 0);
}

#[test]
fn timeout_recycles_device() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    rig.h.sim.set_asleep(0, Lane::Pointer, true);
    let data = mouse.query(1, &battery());
    assert_eq!(
        data,
        envelope(Uncertainty::MayHaveBeenSent).to_vec(),
        "never an old value"
    );
    assert_eq!(mouse.recv(3000), Some(Written::Destroy));
    // Woken up, it comes back within the 2 s re-probe cap.
    std::thread::sleep(Duration::from_millis(500));
    rig.h.sim.set_asleep(0, Lane::Pointer, false);
    let started = Instant::now();
    let again = rig.device(0x00cd);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(again.phys(), mouse.phys());
}

#[test]
fn wake_skips_backoff() {
    let rig = Rig::with(Options::before(|sim| {
        sim.set_asleep(0, Lane::Keyboard, true)
    }));
    // Eight unanswered serial reads in 3.5 s; the next try is 4 s away.
    common::wait_for(10, || rig.serial_reads(Lane::Keyboard) >= 8);
    rig.h.sim.set_asleep(0, Lane::Keyboard, false);
    rig.h.sim.activity(0, Lane::Keyboard);
    let woke = Instant::now();
    let keyboard = rig.device(0x0296);
    assert!(
        woke.elapsed() < Duration::from_millis(1500),
        "created {:?} after the keyboard woke",
        woke.elapsed()
    );
    // Still two reads 250 ms apart before the device.
    let serial = keyboard.query(1, &hfx_bridge::decode::serial_request(0xff));
    assert_eq!(serial[0], 2);
}

#[test]
fn driver_handover() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let reads = rig.serial_reads(Lane::Pointer);
    mouse.send(&uhid::kernel_stop());
    assert_eq!(mouse.recv(3000), Some(Written::Destroy));
    let again = rig.device(0x00cd);
    assert_eq!(
        rig.serial_reads(Lane::Pointer),
        reads + 2,
        "a fresh pair of serial reads"
    );
    assert_eq!(again.query(1, &battery())[0], 2);
}

#[test]
fn second_unit_on_the_cable() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let twin = rig.drivers().join("razermouse/0003:1532:00CC.0009");
    std::fs::create_dir_all(&twin).unwrap();
    // OpenRazer binds the mouse's other interfaces too, without a serial.
    let other = rig.drivers().join("razermouse/0003:1532:00CC.0008");
    std::fs::create_dir_all(&other).unwrap();
    // Same model, and a hash that can be read and differs: another unit.
    std::fs::write(twin.join("serial_sha256"), serial_hash(b"PM2342H199999999")).unwrap();
    rig.uevent("bind");
    assert_eq!(mouse.recv(300), None);
    // The paired one on its cable, by its serial.
    std::fs::write(twin.join("serial_sha256"), serial_hash(b"PM2342H100000001")).unwrap();
    rig.uevent("bind");
    assert_eq!(mouse.recv(3000), Some(Written::Destroy));
    for dir in [twin, other] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    rig.uevent("remove");
    let again = rig.device(0x00cd);
    assert_eq!(again.phys(), mouse.phys());
}

/// A checked mouse's wired id, unbound: the kernel has announced it, and
/// its driver has not bound it yet.
const ANNOUNCED: &str = "devices/0003:1532:00AA.0011";

#[test]
fn cable_before_its_driver_binds() {
    let rig = mouse_rig(0x00ab, |_| {});
    let mouse = rig.device(0x00ab);
    std::fs::create_dir_all(rig.or.sys.path().join(ANNOUNCED)).unwrap();
    rig.uevent("add");
    // OpenRazer sets up a wired device about 2 s after it appears. The copy
    // has to be gone by then.
    assert_eq!(mouse.recv(2000), Some(Written::Destroy));
    // Bound now, and still connected through the receiver as far as its
    // slot says: it stays away, and nothing asks for its serial.
    let reads = rig.serial_reads(Lane::Pointer);
    std::fs::create_dir(rig.or.sys.path().join(ANNOUNCED).join("driver")).unwrap();
    std::fs::create_dir_all(rig.drivers().join("razermouse/0003:1532:00AA.0011")).unwrap();
    rig.uevent("bind");
    rig.h.sim.activity(0, Lane::Pointer);
    std::thread::sleep(Duration::from_secs(1));
    let s = rig.h.client(&[]).snapshot().unwrap();
    let d = common::device(&s, 0, Lane::Pointer).unwrap();
    let state = d.openrazer.as_ref().map(|o| o.state);
    let cabled = (Some(OpenRazerState::Cable), Some(Slot::Connected));
    assert_eq!(
        ((state, d.slot), rig.serial_reads(Lane::Pointer)),
        (cabled, reads)
    );
}

#[test]
fn cable_in_at_start() {
    let rig = Rig::with(Options {
        sysfs: Some(|sys: &Path| {
            let wired = sys.join("drivers/razerkbd/0003:1532:0292.0021");
            std::fs::create_dir_all(&wired).unwrap();
            // A second unit's hash, so the receiver copy would stay once the
            // keyboard's own serial is known. Before that, with no serial
            // read yet, the cable still hides it.
            std::fs::write(
                wired.join("serial_sha256"),
                serial_hash(b"IO2318K299999999"),
            )
            .unwrap();
        }),
        ..Options::before(|sim| sim.identity(0, Lane::Keyboard, 0x0290, 1))
    });
    let mut c = rig.h.client(&[]);
    let state = |s: &Snapshot| common::device(s, 0, Lane::Keyboard)?.openrazer.clone();
    let cable = |s: &Snapshot| state(s).is_some_and(|o| o.state == OpenRazerState::Cable);
    common::until(&mut c, 5, cable);
    assert_eq!(rig.serial_reads(Lane::Keyboard), 0);
    // The cable goes: the serial reads start, and the device comes.
    std::fs::remove_dir_all(rig.drivers().join("razerkbd/0003:1532:0292.0021")).unwrap();
    rig.uevent("remove");
    rig.device(0x0290);
    assert_eq!(rig.serial_reads(Lane::Keyboard), 2);
}

#[test]
fn checked_keyboard_on_cable() {
    let rig = Rig::with(Options::before(|sim| {
        sim.identity(0, Lane::Keyboard, 0x0290, 1)
    }));
    let keyboard = rig.device(0x0290);
    let mut c = rig.h.client(&[]);
    common::until(&mut c, 5, |s| common::level(s, 0, Lane::Keyboard).is_some());
    // On its cable it charges, and the bridge has to ask to find out.
    rig.h.sim.status(0, Lane::Keyboard, 202, 1, 1);
    let twin = rig.drivers().join("razerkbd/0003:1532:0292.0009");
    std::fs::create_dir_all(&twin).unwrap();
    std::fs::write(twin.join("serial_sha256"), serial_hash(b"IO2318K200000001")).unwrap();
    rig.uevent("bind");
    assert_eq!(keyboard.recv(3000), Some(Written::Destroy));
    let charge = |s: &Snapshot| common::device(s, 0, Lane::Keyboard)?.battery.charge;
    common::until(&mut c, 5, |s| charge(s) == Some(Charge::Charging));
}

#[test]
fn teardown_unbinds_our_device() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let drivers = rig.drivers();
    bind(&drivers, "razermouse", "0006:1532:00CD.0003", &mouse.phys());
    bind(
        &drivers,
        "razermouse",
        "0006:1532:00CD.0004",
        "hyperflux-next/native/v1/native-someoneelse",
    );
    // A SET whose exchange is still running when the receiver goes.
    rig.h.sim.set_silent(0, Lane::Pointer, 1);
    mouse.send(&uhid::kernel_set(1, &battery()));
    assert_eq!(mouse.recv(1000), set_reply(1, 0));
    mouse.send(&uhid::kernel_get(2));
    rig.h.sim.unplug(0);
    // The GET is answered first.
    let data = mouse.got(2);
    assert!(parse_envelope(&data.as_slice().try_into().unwrap()).is_some());
    assert_eq!(mouse.recv(6000), Some(Written::Destroy));
    let unbound = std::fs::read_to_string(drivers.join("razermouse/unbind")).unwrap();
    assert_eq!(unbound, "0006:1532:00CD.0003");
}

#[test]
fn teardown_without_unbind_access() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let drivers = rig.drivers();
    // A directory cannot be opened for writing.
    std::fs::remove_file(drivers.join("razermouse/unbind")).unwrap();
    std::fs::create_dir(drivers.join("razermouse/unbind")).unwrap();
    bind(&drivers, "razermouse", "0006:1532:00CD.0003", &mouse.phys());
    mouse.send(&uhid::kernel_set(1, &battery()));
    assert_eq!(mouse.recv(1000), set_reply(1, 0));
    rig.h.sim.unplug(0);
    mouse.send(&uhid::kernel_get(2));
    mouse.got(2);
    assert_eq!(mouse.recv(3000), Some(Written::Destroy));
}

#[test]
fn client_lease_blocks_openrazer() {
    let rig = Rig::new();
    let mouse = rig.device(0x00cd);
    let (mut c, _, lease) = common::leased(&rig.h, &["native-routes"], Lane::Pointer);
    let data = mouse.query(1, &battery());
    assert_eq!(data, envelope(Uncertainty::NotSent).to_vec());
    c.release_lease(&lease).unwrap();
    assert_eq!(mouse.query(3, &battery())[0], 2);
}

/// The rig with `pid` on the pointer lane, set up by `with` first.
fn mouse_rig(pid: u16, with: fn(&hfx_bridge::sim::Sim)) -> Rig {
    Rig::with(Options::before(move |sim| {
        sim.identity(0, Lane::Pointer, pid, 1);
        with(sim);
    }))
}

#[test]
fn recycle_needs_confirmation() {
    let rig = mouse_rig(0x00ab, |_| {});
    let mouse = rig.device(0x00ab);
    let battery = battery();
    let asleep = |on| rig.h.sim.set_asleep(0, Lane::Pointer, on);
    // A 0x04 from a device that turns out to be awake keeps it...
    asleep(true);
    mouse.query(1, &battery);
    asleep(false);
    assert_eq!(mouse.recv(1_500), None);
    // ...and one the bridge's own probe gets too doesn't.
    asleep(true);
    mouse.query(3, &battery);
    assert_eq!(mouse.recv(5_000), Some(Written::Destroy));
}

#[test]
fn no_usable_serial_still_published() {
    let rig = mouse_rig(0x00ab, |sim| sim.set_serial(0, Lane::Pointer, ""));
    let mouse = rig.device(0x00ab);
    assert_eq!(rig.serial_reads(Lane::Pointer), 2);
    let serial = mouse.query(1, &hfx_bridge::decode::serial_request(0x1f));
    assert_eq!(serial[8..30], [0; 22]);
}

/// The device on the lane, once it is `pid` and passes `check`.
fn paired(c: &mut Client, lane: Lane, pid: u16, check: fn(&DeviceInfo) -> bool) -> DeviceInfo {
    let ours = |d: &&DeviceInfo| d.product_id == pid && check(d);
    let s = common::until(c, 5, |s| common::device(s, 0, lane).filter(ours).is_some());
    common::device(&s, 0, lane).unwrap().clone()
}

/// Every device in the catalog paired on its lane in turn: a tested or
/// checked one gets a route, its battery, an OpenRazer device exactly when
/// it has an OpenRazer part (OpenRGB goes by the plugin's table, which
/// tests/test_device_tables.py checks) and its listed commands, no others. A
/// listed device is never controllable.
#[test]
fn every_catalog_device() {
    let rig = Rig::new();
    // The simulator's own two go when their lanes change, and so does each
    // OpenRazer device below: the bridge destroys it, as on a real machine.
    let _first = [rig.device(0x00cd), rig.device(0x0296)];
    let mut live;
    let mut c = rig.h.client(&["native-routes"]);
    let timeout = hfx_proto::ids::ExchangeTimeoutMs::new(1000).unwrap();
    for entry in hfx_proto::devices::DEVICES {
        let (lane, pid) = (entry.kind.lane(), entry.product_id);
        rig.h.sim.identity(0, lane, pid, 1);
        rig.h.sim.status(0, lane, 128, 0, 0);
        let d = paired(&mut c, lane, pid, |d| d.battery.level.is_some());
        if !entry.qualified() {
            assert!(!d.controllable, "{pid:04x}");
            continue;
        }
        let shown = || rig.h.shown(0, lane).is_some_and(|b| b.name == entry.name);
        common::wait_for(10, shown);
        live = entry
            .openrazer
            .is_some()
            .then(|| rig.device(u32::from(pid)));
        if live.is_none() {
            std::thread::sleep(Duration::from_millis(400));
            let reads = rig.serial_reads(lane);
            assert_eq!(reads, 0, "{pid:04x} is on its way to OpenRazer");
        }
        // Once OpenRazer's serial reads are done with it.
        paired(&mut c, lane, pid, |d| {
            d.controllable && d.controlled_by.is_none()
        });
        let token = common::route(&mut c, lane).route_token.unwrap();
        let lease = common::lease(&mut c, &token);
        let sizes = |m: &'static Command| m.sizes.iter().map(|n| (m.class, m.id, *n));
        let listed = entry.commands.iter().flat_map(sizes);
        for (class, id, size) in listed.chain([(0x05, 0x01, 1)]) {
            let request = report::build(0xff, class, id, &[0; 80][..usize::from(size)]);
            let result = c.transact(&lease, &token, &request, timeout).unwrap();
            let sent = result.outcome != TransactOutcome::NotSent;
            assert_eq!(sent, class != 0x05, "{pid:04x} {class:02x}/{id:02x}");
        }
        c.release_lease(&lease).unwrap();
    }
}
