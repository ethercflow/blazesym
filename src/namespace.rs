#[cfg(target_os = "linux")]
use nix::sched::setns;
#[cfg(target_os = "linux")]
use nix::sched::CloneFlags;

use std::convert::Into;
use std::env::current_dir;
use std::env::set_current_dir;
use std::fs::File;
use std::fs::{self};
use std::io::BufRead;
use std::io::BufReader;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use crate::Error;
use crate::Pid;

pub(crate) struct NsCookie {
    oldns: File,
    oldcwd: PathBuf,
}

pub(crate) struct NsInfo {
    tgid: Pid,
    nstgid: Pid,
    need_setns: bool,
    mntns_path: PathBuf,
}

#[cfg(target_os = "linux")]
pub(crate) fn create_nsinfo(pid: Pid) -> Result<NsInfo, Error> {
    let old_stat = fs::metadata("/proc/self/ns/mnt")?;
    let new_stat = fs::metadata(format!("/proc/{pid}/ns/mnt"))?;
    let (tgid, nstgid) = get_nspid(pid)?;
    let need_setns = old_stat.ino() != new_stat.ino();
    let mntns_path = if need_setns {
        "/proc/self/ns/mnt".into()
    } else {
        format!("/proc/{pid}/ns/mnt")
    }
    .into();

    Ok(NsInfo {
        tgid,
        nstgid,
        need_setns,
        mntns_path,
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn create_nsinfo(pid: Pid) -> Result<NsInfo, Error> {
    Ok(NsInfo {
        tgid: pid,
        nstgid: pid,
        need_setns: false,
        mntns_path: "".into(),
    })
}

#[cfg(target_os = "linux")]
fn get_nspid(pid: Pid) -> Result<(Pid, Pid), Error> {
    let file = File::open(format!("/proc/{pid}/status"))?;
    let reader = BufReader::new(file);
    let (mut tgid, mut nstgid) = (pid, pid);
    let mut found = false;

    for line in reader.lines() {
        match line {
            Ok(line) => {
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
        return Err(Error::with_not_found(
            "failed to get Tgid/NStgid from {file}",
        ));
    }
    Ok((tgid, nstgid))
}

#[cfg(not(target_os = "linux"))]
fn get_nspid(pid: Pid) -> Result<(Pid, Pid), Error> {
    Ok((pid, pid))
}

impl NsInfo {
    #[cfg(target_os = "linux")]
    pub(crate) fn enter_mntns(&self) -> Result<Option<NsCookie>, Error> {
        if !self.need_setns {
            return Ok(None);
        }

        let oldcwd = current_dir()?;
        let oldns = File::open(&oldcwd)?;
        let newns = File::open(&self.mntns_path)?;
        setns(newns, CloneFlags::CLONE_NEWNS).map_err(Into::<std::io::Error>::into)?;

        Ok(Some(NsCookie { oldcwd, oldns }))
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn enter_mntns(&self) -> Result<Option<NsCookie>, Error> {
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn exit_mntns(&self, nc: Option<NsCookie>) -> Result<(), Error> {
        if let Some(nc) = nc {
            setns(nc.oldns, CloneFlags::CLONE_NEWNS).map_err(Into::<std::io::Error>::into)?;
            set_current_dir(nc.oldcwd)?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn exit_mntns(&self, _nc: Option<NsCookie>) -> Result<(), Error> {
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
