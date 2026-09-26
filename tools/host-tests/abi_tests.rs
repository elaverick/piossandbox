//! Tests for the system call interface (abi/).

use pios_abi::*;

#[test]
fn errors_round_trip() {
    for error in [
        Error::NoSuchCall,
        Error::InvalidArgument,
        Error::BadAddress,
        Error::BadHandle,
        Error::OutOfMemory,
        Error::PeerGone,
        Error::AccessDenied,
    ] {
        assert_eq!(Error::from_result(error.to_raw()), Err(error));
    }
    assert_eq!(Error::from_result(0), Ok(0));
    assert_eq!(
        Error::from_result(isize::MAX as usize),
        Ok(isize::MAX as usize)
    );
}

#[test]
fn exit_statuses_round_trip() {
    for status in [
        ExitStatus::Code(0),
        ExitStatus::Code(1),
        ExitStatus::Code(-1),
        ExitStatus::Code(i32::MIN),
        ExitStatus::Code(i32::MAX),
        ExitStatus::Fault { esr: 0 },
        ExitStatus::Fault { esr: 0x9200_0007 },
        ExitStatus::Fault { esr: u32::MAX },
    ] {
        let raw = status.to_raw();
        // A successful system call result: not negative.
        assert!((raw as isize) >= 0);
        assert_eq!(ExitStatus::from_raw(raw), Some(status));
    }
    assert_eq!(ExitStatus::from_raw(2 << 32), None);
}

#[test]
fn messages_round_trip_through_bytes() {
    let mut message = Message::new(7, &[1, 2, u64::MAX]);
    message.handle = 3;
    message.badge = 0xBAD6E;
    message.reply = 9;
    message.data[MESSAGE_WORDS - 1] = 0x0102_0304_0506_0708;
    let bytes = message.to_bytes();
    assert_eq!(Message::from_bytes(&bytes), message);
    // The layout is the in-memory one: fields in order, little-endian.
    assert_eq!(bytes[0], 7);
    assert_eq!(bytes[8], 3);
    assert_eq!(bytes[32], 1);
    assert_eq!(bytes[MESSAGE_SIZE - 8], 0x08);
    // SAFETY: Message is plain old data, laid out as repr(C).
    let in_memory: [u8; MESSAGE_SIZE] = unsafe { core::mem::transmute(message) };
    if cfg!(target_endian = "little") {
        assert_eq!(in_memory, bytes);
    }
}
