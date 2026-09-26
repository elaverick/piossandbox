//! Checks IPC from user space, as two processes. The kernel's self-test
//! starts the parent, with the boot image (so it can start a copy of itself
//! as the child). They then take turns:
//!
//! 1. The child calls the parent through a badged, send-only handle it was
//!    started with; the parent sees the badge and replies (and can't reply
//!    twice).
//! 2. The child makes its own endpoint and sends the parent a handle to it.
//! 3. The parent calls the child through that handle.
//! 4. The child calls again; the parent drops the reply handle unanswered,
//!    so the call fails.
//! 5. Once the child has exited, calling its endpoint fails too.
//!
//! Along the way each checks that rights are enforced and bad arguments
//! refused. Exits 0 if everything behaved, else 1.

#![no_std]
#![no_main]

use libpios::rights::{DUPLICATE, RECEIVE, SEND, TRANSFER};
use libpios::{Error, ExitStatus, Message, endpoint, println, raw_syscall};
use pios_abi::call;
use pios_bootfs::{BootFs, HEADER_SIZE};

libpios::pios_main!(main);

/// The argument that makes this the child.
const CHILD: usize = 1;

/// Some read-only memory, which the kernel mustn't write a message into.
static READ_ONLY: [u8; 128] = [0; 128];

struct Checks {
    who: &'static str,
    ok: bool,
}

impl Checks {
    fn check(&mut self, passed: bool, what: &str) {
        if !passed {
            println!("ipctest {}: FAILED: {}", self.who, what);
            self.ok = false;
        }
    }
}

fn main() -> i32 {
    let ok = if libpios::argument() == CHILD {
        child()
    } else {
        parent(libpios::argument())
    };
    if ok { 0 } else { 1 }
}

fn parent(image_addr: usize) -> bool {
    let mut c = Checks {
        who: "parent",
        ok: true,
    };
    // SAFETY: the kernel maps the boot image read-only at our argument, for
    // as long as we run.
    let header = unsafe { core::slice::from_raw_parts(image_addr as *const u8, HEADER_SIZE) };
    let size = BootFs::total_size(header).expect("a boot image");
    // SAFETY: as above.
    let image = unsafe { core::slice::from_raw_parts(image_addr as *const u8, size) };
    let me = BootFs::new(image).unwrap().find("ipctest").unwrap().data;

    let e = endpoint().unwrap();

    // Copies can lose rights but not gain them, and a badge is set once.
    let d = e.duplicate(SEND | DUPLICATE, 7).unwrap();
    c.check(
        d.duplicate(SEND | RECEIVE, 0) == Err(Error::AccessDenied),
        "a copy gained a right",
    );
    c.check(
        d.duplicate(SEND, 8) == Err(Error::InvalidArgument),
        "a badge was changed",
    );
    c.check(
        d.duplicate(SEND, 0).is_ok(),
        "a copy with fewer rights was refused",
    );
    drop(d);

    let to_parent = e.duplicate(SEND | TRANSFER, 42).unwrap();
    let child = libpios::spawn(me, CHILD, Some(to_parent), "").unwrap();

    // 1. A call, with the badge; a reply handle works once.
    let r = e.receive().unwrap();
    c.check(r.badge == 42, "a call didn't carry its badge");
    c.check(
        r.message.label == 1 && r.message.data[..2] == [2, 3],
        "a call's data changed",
    );
    c.check(
        r.message.handle.is_none(),
        "a call without a handle brought one",
    );
    let reply = r
        .reply
        .expect("a call comes with a reply handle")
        .into_handle()
        .into_raw();
    let answer = pios_abi::Message::new(0, &[5]);
    let answer = &answer as *const _ as usize;
    c.check(
        raw_syscall(call::REPLY, reply, answer, 0) == Ok(0),
        "a reply failed",
    );
    c.check(
        raw_syscall(call::REPLY, reply, answer, 0) == Err(Error::BadHandle),
        "a reply handle worked twice",
    );

    // 2. A send carrying a handle.
    let r = e.receive().unwrap();
    c.check(
        r.message.label == 2 && r.badge == 42,
        "a send arrived wrong",
    );
    c.check(r.reply.is_none(), "a send came with a reply handle");
    let to_child = r
        .message
        .handle
        .expect("the child's message carries a handle");

    // 3. Calling the child through it.
    let answer = to_child.call(Message::new(3, &[10]));
    c.check(
        answer.map(|m| m.data[0]) == Ok(11),
        "a call through a passed handle wasn't answered",
    );

    // 4. A call whose reply handle is dropped.
    let r = e.receive().unwrap();
    c.check(
        r.message.label == 4 && r.reply.is_some(),
        "the last call arrived wrong",
    );
    drop(r);

    c.check(
        child.wait() == Ok(ExitStatus::Code(0)),
        "the child's checks failed",
    );

    // 5. Nobody left to receive, or to send.
    c.check(
        to_child.call(Message::new(5, &[])).err() == Some(Error::PeerGone),
        "calling a gone process worked",
    );
    let g = endpoint().unwrap();
    let receive_only = g.duplicate(RECEIVE, 0).unwrap();
    drop(g);
    c.check(
        receive_only.receive().err() == Some(Error::PeerGone),
        "receiving with no senders left didn't fail",
    );
    c.check(
        receive_only.send(Message::new(0, &[])) == Err(Error::AccessDenied),
        "sending needs the send right",
    );

    // Handles only move with the transfer right.
    let no_transfer = e.duplicate(SEND, 0).unwrap();
    c.check(
        to_child.send(Message::new(0, &[]).with_handle(no_transfer)) == Err(Error::AccessDenied),
        "a handle without the transfer right was sent",
    );

    // Bad arguments.
    c.check(
        raw_syscall(call::SEND, e.raw(), 0, 0) == Err(Error::BadAddress),
        "a null message was accepted",
    );
    c.check(
        raw_syscall(call::RECEIVE, e.raw(), READ_ONLY.as_ptr() as usize, 0)
            == Err(Error::BadAddress),
        "a message was received into read-only memory",
    );
    c.check(
        raw_syscall(call::RECEIVE, e.raw(), 0xFFFF_FF80_0008_0000, 0) == Err(Error::BadAddress),
        "a message was received into kernel memory",
    );
    let mut buffer = pios_abi::Message::default();
    let buffer = &mut buffer as *mut _ as usize;
    c.check(
        raw_syscall(call::RECEIVE, 9999, buffer, 0) == Err(Error::BadHandle),
        "a made-up handle worked",
    );
    c.check(
        raw_syscall(call::WAIT, e.raw(), 0, 0) == Err(Error::BadHandle),
        "waited on an endpoint",
    );
    c.check(
        raw_syscall(call::REPLY, e.raw(), buffer, 0) == Err(Error::BadHandle),
        "replied through an endpoint",
    );

    // Timers notify: pending notifications collect into one, marked by
    // their badges, ahead of any waiting message.
    const TICK: u64 = 1 << 40;
    const TOCK: u64 = 1 << 41;
    let tick = libpios::timer(&e, TICK, 10).unwrap();
    let tock = libpios::timer(&e, TOCK, 10).unwrap();
    let start = libpios::counter();
    while libpios::counter() - start < libpios::counter_frequency() / 20 {
        libpios::yield_now(); // 50 ms: several ticks
    }
    let r = e.receive().unwrap();
    c.check(r.message.label == pios_abi::NOTIFY, "a timer's notification had the wrong label");
    c.check(r.badge == TICK | TOCK, "pending notifications didn't combine");
    c.check(r.reply.is_none() && r.message.handle.is_none(), "a notification carried handles");
    drop(tock);
    // (Collect anything `tock` sent before it stopped; after that, only
    // `tick` may notify.)
    let _ = e.receive().unwrap();
    let r = e.receive().unwrap();
    c.check(r.badge == TICK, "a stopped timer still notified");
    drop(tick);
    c.check(libpios::timer(&e, 0, 10).err() == Some(Error::InvalidArgument), "a timer without a badge was allowed");
    c.check(libpios::timer(&receive_only, TICK, 10).err() == Some(Error::AccessDenied), "a timer needs the send right");

    // Device calls need the right kind of handle.
    c.check(e.map(0x50_0000_0000) == Err(Error::BadHandle), "mapped an endpoint");
    c.check(e.acknowledge_interrupt() == Err(Error::BadHandle), "acknowledged an endpoint as an interrupt");
    c.check(e.bind_interrupt(&e, 1) == Err(Error::BadHandle), "bound an endpoint as an interrupt");
    c.ok
}

fn child() -> bool {
    let mut c = Checks {
        who: "child",
        ok: true,
    };
    let to_parent = libpios::start_handle().expect("started with a handle");

    // 1.
    let answer = to_parent.call(Message::new(1, &[2, 3]));
    c.check(
        answer.map(|m| (m.label, m.data[0])) == Ok((0, 5)),
        "the first call's reply was wrong",
    );
    c.check(
        to_parent.receive().err() == Some(Error::AccessDenied),
        "receiving needs the receive right",
    );
    c.check(
        to_parent.duplicate(SEND, 0) == Err(Error::AccessDenied),
        "copying needs the duplicate right",
    );

    // 2.
    let mine = endpoint().unwrap();
    let to_me = mine.duplicate(SEND | TRANSFER, 0).unwrap();
    let sent = to_me.raw();
    c.check(
        to_parent
            .send(Message::new(2, &[]).with_handle(to_me))
            .is_ok(),
        "sending a handle failed",
    );
    c.check(
        raw_syscall(call::CLOSE, sent, 0, 0) == Err(Error::BadHandle),
        "a sent handle stayed behind",
    );

    // 3.
    let r = mine.receive().unwrap();
    c.check(
        r.message.label == 3 && r.badge == 0,
        "the parent's call arrived wrong",
    );
    let reply = r.reply.expect("a call comes with a reply handle");
    c.check(
        reply
            .reply(Message::new(0, &[r.message.data[0] + 1]))
            .is_ok(),
        "replying failed",
    );

    // 4.
    c.check(
        to_parent.call(Message::new(4, &[])).err() == Some(Error::PeerGone),
        "a call whose reply handle was dropped didn't fail",
    );
    c.ok
}
