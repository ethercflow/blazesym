use std::convert::Into;
use std::env::current_dir;
use std::env::set_current_dir;
use std::fs;
use std::fs::File;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use libc::setns;
use libc::CLONE_NEWNS;

use crate::Error;
use crate::ErrorExt;
use crate::Pid;

pub(crate) struct NsInfo {
    tgid: Pid,
    nstgid: Pid,
    need_setns: bool,
    mntns_path: Option<PathBuf>,
    oldns: File,
    // From https://github.com/torvalds/linux/commit/b01c1f69c8660eaeab7d365cd570103c5c073a02, we see
    // once finished we setns to old namespace, which also sets the current working directory (cwd) to "/",
    // trashing the cwd we had. So adding the current working directory to be part of `NsInfo` and restoring
    // it in the `Drop` call.
    oldcwd: PathBuf,
}

fn get_nspid(pid: Pid) -> Result<(Pid, Pid), Error> {
    let fname = format!("/proc/{pid}/status");
    let file = File::open(&fname).context("faild to open `{fname}`")?;
    let reader = BufReader::new(file);
    let (mut tgid, mut nstgid) = (pid, pid);
    let mut found = false;

    for line in reader.lines() {
        match line {
            Ok(line) => {
                /* Use tgid if CONFIG_PID_NS is not defined. */
                if line.contains("Tgid:") {
                    if let Some(num) = line
                        .split("Tgid:")
                        .last()
                        .and_then(|s| s.split_whitespace().next_back())
                    {
                        tgid = num.into();
                        nstgid = num.into();
                        found = true;
                    }
                }
                if line.contains("NStgid:") {
                    if let Some(num) = line
                        .split("NStgid:")
                        .last()
                        .and_then(|s| s.split_whitespace().next_back())
                    {
                        nstgid = num.into();
                        break;
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    if !found {
        unreachable!("{}", format!("failed to get Tgid/NStgid from {fname}"));
    }
    Ok((tgid, nstgid))
}

impl NsInfo {
    pub(crate) fn new(pid: Pid) -> Result<Self, Error> {
        let old_stat_path = "/proc/self/ns/mnt";
        let new_stat_path = format!("/proc/{pid}/ns/mnt");
        let old_stat = fs::metadata(old_stat_path).context("failed to stat `/proc/self/ns/mnt`")?;
        let new_stat =
            fs::metadata(&new_stat_path).context("failed to stat `/proc/{pid}/ns/mnt`")?;
        let oldns = File::open(old_stat_path).context("failed to open `/proc/self/ns/mnt`")?;
        let oldcwd = current_dir().context("failed to get current work dir")?;
        let (tgid, nstgid) = get_nspid(pid).context("failed to get nspid for pid {pid}")?;
        let need_setns = old_stat.ino() != new_stat.ino();
        let mntns_path = if need_setns {
            Some(PathBuf::from(new_stat_path))
        } else {
            None
        };
        Ok(Self {
            tgid,
            nstgid,
            need_setns,
            mntns_path,
            oldns,
            oldcwd,
        })
    }

    pub(crate) fn enter_mntns(&self) -> Result<(), Error> {
        if !self.need_setns {
            return Ok(());
        }

        // SAFTEY: when `need_setns` is true, `mntns_path` must contains a new ns mnt's `PathBuf`, so it's always safe to unwrap.
        let mntns_path = self.mntns_path.as_ref().unwrap();
        let newns = File::open(mntns_path).context("failed to open newns: {mntns_path}")?;
        // SAFTEY: `setns` with the legal file descriptor is always safe to call.
        let rc = unsafe { setns(newns.as_fd().as_raw_fd(), CLONE_NEWNS) };
        if rc < 0 {
            return Err(Error::from(io::Error::last_os_error()))
        }
        Ok(())
    }

    pub(crate) fn pid(&self) -> Pid {
        if self.need_setns {
            self.nstgid
        } else {
            self.tgid
        }
    }
}

impl Drop for NsInfo {
    fn drop(&mut self) {
        if !self.need_setns {
            return;
        }
        // SAFTEY: `setns` with the legal file descriptor is always safe to call.
        let rc = unsafe { setns(self.oldns.as_fd().as_raw_fd(), CLONE_NEWNS) };
        if rc < 0 {
            panic!("failed to set mount ns back");
        }
        // TODO: can we safely ignore this or should panic here?
        let _ = set_current_dir(&self.oldcwd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process;

    #[test]
    fn self_status_parsing() {
        let (tgid, nstgid) = get_nspid(Pid::Slf).unwrap();
        let pid = Pid::from(process::id());
        assert_eq!(tgid, pid);
        assert_eq!(nstgid, pid);
    }

    #[test]
    fn invalid_status_parsing() {
        assert!(get_nspid(Pid::from(u32::MAX)).is_err());
    }
}
