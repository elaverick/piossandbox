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
