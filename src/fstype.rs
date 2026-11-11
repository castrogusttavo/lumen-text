//! Which file systems inotify cannot be trusted on.
//!
//! inotify only sees changes that go through the local kernel's VFS. On a network or userspace file
//! system (NFS, SMB/CIFS, sshfs and other FUSE mounts, 9p as used by WSL2 and VMs, VirtualBox shared
//! folders) a change made by another machine, by the FUSE daemon itself or by the host never produces
//! an event, so the explorer, open buffers and git status silently go stale. FAT / exFAT drives are
//! often written by another OS (a removable drive, a dual-boot partition) and have 2 s timestamps.
//!
//! [`classify`] maps a `statfs` magic number to a [`Untrusted`] reason; [`untrusted`] asks the kernel
//! for a path (Linux only: the other platforms already use the polling watcher).

/// Why inotify is not trusted for a path. `label` is what the status bar shows: "Polling (NFS)".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Untrusted {
    Nfs,
    Smb,
    Fuse,
    NineP,
    Fat,
    ExFat,
    VBox,
    OtherNet,
    /// `fs.inotify.max_user_watches` is exhausted: the folder could not be watched at all.
    WatchLimit,
}

impl Untrusted {
    pub fn label(self) -> &'static str {
        match self {
            Untrusted::Nfs => "NFS",
            Untrusted::Smb => "SMB",
            Untrusted::Fuse => "FUSE",
            Untrusted::NineP => "9p",
            Untrusted::Fat => "FAT",
            Untrusted::ExFat => "exFAT",
            Untrusted::VBox => "VirtualBox",
            Untrusted::OtherNet => "network",
            Untrusted::WatchLimit => "watch limit",
        }
    }

    /// One sentence for the tooltip: why the folder is polled.
    pub fn why(self) -> &'static str {
        match self {
            Untrusted::Nfs | Untrusted::Smb | Untrusted::OtherNet => {
                "This folder is on a network file system: the kernel does not report changes made by other machines."
            }
            Untrusted::Fuse => {
                "This folder is on a FUSE mount (sshfs, rclone...): changes made behind the mount are not reported."
            }
            Untrusted::NineP => {
                "This folder is shared through 9p (WSL, a VM): changes made by the host are not reported."
            }
            Untrusted::VBox => "This folder is a VirtualBox shared folder: changes made by the host are not reported.",
            Untrusted::Fat | Untrusted::ExFat => {
                "This folder is on a FAT / exFAT drive, often written by another system, with coarse timestamps."
            }
            Untrusted::WatchLimit => {
                "The inotify watch limit (fs.inotify.max_user_watches) is exhausted. Raise it with: sudo sysctl fs.inotify.max_user_watches=524288"
            }
        }
    }
}

// Magic numbers from <linux/magic.h> and the file systems' own headers.
const NFS_SUPER_MAGIC: u32 = 0x6969;
const SMB_SUPER_MAGIC: u32 = 0x517b;
const CIFS_MAGIC_NUMBER: u32 = 0xff53_4d42;
const SMB2_MAGIC_NUMBER: u32 = 0xfe53_4d42;
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;
const V9FS_MAGIC: u32 = 0x0102_1997;
const MSDOS_SUPER_MAGIC: u32 = 0x4d44;
const EXFAT_SUPER_MAGIC: u32 = 0x2011_bab0;
const VBOXSF_MAGIC: u32 = 0x786f_4256;
const CODA_SUPER_MAGIC: u32 = 0x7375_7245;
const AFS_SUPER_MAGIC: u32 = 0x5346_414f;
const AFS_FS_MAGIC: u32 = 0x6b41_4653;
const CEPH_SUPER_MAGIC: u32 = 0x00c3_6400;
const NCP_SUPER_MAGIC: u32 = 0x564c;
const LUSTRE_SUPER_MAGIC: u32 = 0x0bd0_0bd0;
const GFS2_MAGIC: u32 = 0x0116_1970;
const OCFS2_SUPER_MAGIC: u32 = 0x7461_636f;

/// The reason inotify cannot be trusted on a file system with this `statfs` `f_type`, if any.
pub fn classify(magic: u32) -> Option<Untrusted> {
    Some(match magic {
        NFS_SUPER_MAGIC => Untrusted::Nfs,
        SMB_SUPER_MAGIC | CIFS_MAGIC_NUMBER | SMB2_MAGIC_NUMBER => Untrusted::Smb,
        FUSE_SUPER_MAGIC => Untrusted::Fuse,
        V9FS_MAGIC => Untrusted::NineP,
        MSDOS_SUPER_MAGIC => Untrusted::Fat,
        EXFAT_SUPER_MAGIC => Untrusted::ExFat,
        VBOXSF_MAGIC => Untrusted::VBox,
        CODA_SUPER_MAGIC | AFS_SUPER_MAGIC | AFS_FS_MAGIC | CEPH_SUPER_MAGIC | NCP_SUPER_MAGIC | LUSTRE_SUPER_MAGIC
        | GFS2_MAGIC | OCFS2_SUPER_MAGIC => Untrusted::OtherNet,
        _ => return None,
    })
}

/// `statfs(path).f_type`, as the 32-bit magic the kernel headers use.
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_cast)]
pub fn magic(path: &std::path::Path) -> Option<u32> {
    let c = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str())).ok()?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    // `f_type` is i64 on x86_64, u32/i32 elsewhere; the magics are 32-bit (CIFS's is "negative").
    Some((s.f_type as u64 & 0xffff_ffff) as u32)
}

#[cfg(not(target_os = "linux"))]
pub fn magic(_path: &std::path::Path) -> Option<u32> {
    None
}

/// Why inotify cannot be trusted for `path` (None: a local disk, or not Linux, or unknown).
/// `LUMEN_FORCE_POLL=1` treats every path as NFS (to try the fallback on a local disk).
pub fn untrusted(path: &std::path::Path) -> Option<Untrusted> {
    if std::env::var_os("LUMEN_FORCE_POLL").is_some_and(|v| v == "1") {
        return Some(Untrusted::Nfs);
    }
    classify(magic(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_and_userspace_file_systems_are_untrusted() {
        assert_eq!(classify(0x6969), Some(Untrusted::Nfs));
        assert_eq!(classify(0xff53_4d42), Some(Untrusted::Smb), "cifs");
        assert_eq!(classify(0xfe53_4d42), Some(Untrusted::Smb), "smb2");
        assert_eq!(classify(0x517b), Some(Untrusted::Smb), "smbfs");
        assert_eq!(classify(0x6573_5546), Some(Untrusted::Fuse), "sshfs, rclone, ...");
        assert_eq!(classify(0x0102_1997), Some(Untrusted::NineP), "WSL2 drvfs / 9p");
        assert_eq!(classify(0x2011_bab0), Some(Untrusted::ExFat));
        assert_eq!(classify(0x4d44), Some(Untrusted::Fat));
        assert_eq!(classify(0x786f_4256), Some(Untrusted::VBox));
        assert_eq!(classify(0x00c3_6400), Some(Untrusted::OtherNet), "ceph");
    }

    #[test]
    fn local_file_systems_are_trusted() {
        for m in [
            0xef53u32,   // ext2/3/4
            0x9123_683e, // btrfs
            0x5846_5342, // xfs
            0x0102_1994, // tmpfs
            0x794c_7630, // overlayfs
            0x2fc1_2fc1, // zfs
            0xf2f5_2010, // f2fs
        ] {
            assert_eq!(classify(m), None, "{m:#x}");
        }
    }

    #[test]
    fn labels_and_reasons_are_short_and_distinct() {
        let all = [
            Untrusted::Nfs,
            Untrusted::Smb,
            Untrusted::Fuse,
            Untrusted::NineP,
            Untrusted::Fat,
            Untrusted::ExFat,
            Untrusted::VBox,
            Untrusted::OtherNet,
            Untrusted::WatchLimit,
        ];
        let labels: std::collections::HashSet<_> = all.iter().map(|u| u.label()).collect();
        assert_eq!(labels.len(), all.len());
        assert!(all.iter().all(|u| u.label().len() <= 12 && !u.why().is_empty()));
        assert!(Untrusted::WatchLimit.why().contains("max_user_watches"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn statfs_reads_a_real_magic() {
        // Whatever the temp dir is mounted on, statfs answers; a missing path does not.
        assert!(magic(&std::env::temp_dir()).is_some());
        assert!(magic(std::path::Path::new("/definitely/not/here/lumen")).is_none());
        // /proc is procfs (0x9fa0): local, trusted.
        if let Some(m) = magic(std::path::Path::new("/proc")) {
            assert_eq!(m, 0x9fa0);
            assert_eq!(classify(m), None);
        }
    }
}
