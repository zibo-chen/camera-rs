//! Retain a borrowed USB descriptor independently of a Java connection/caller.
use crate::error::{CameraError, Result as CameraResult};
use std::os::fd::{FromRawFd, OwnedFd};
pub(crate) fn duplicate(fd: i32) -> CameraResult<OwnedFd> {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return Err(CameraError::Io(std::io::Error::last_os_error()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::fd::AsRawFd};
    #[test]
    fn duplicate_outlives_original_and_closes_on_drop() {
        let original = std::fs::File::open("/dev/zero").unwrap();
        let copy = duplicate(original.as_raw_fd()).unwrap();
        let fd = copy.as_raw_fd();
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        drop(original);
        let mut file = std::fs::File::from(copy);
        let mut bytes = [1; 4];
        file.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [0; 4]);
        drop(file);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    }
    #[test]
    fn invalid_descriptor_fails_without_an_owner() {
        assert!(duplicate(-1).is_err());
    }
}
