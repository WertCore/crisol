//! Reading this process's own memory, for the acceptance measurement.
//!
//! Feature-gated (`measure`) and off by default: an application embedding Crisol has no use
//! for it. It ships anyway because the product's headline claim is a memory number, and a
//! claim nobody can re-run is not evidence — anyone who clones the repository should be able
//! to reproduce the figure rather than take it on trust.
//!
//! # Why in-process
//!
//! The obvious alternative is to sample from outside with `footprint(1)` or `vmmap`, which is
//! how the M8 table was first produced. That method has a defect that took a contradiction to
//! notice: sampling happens at whatever moment the shell gets around to it, and a GUI process
//! idling under `ControlFlow::Wait` has wildly different memory depending on whether a frame
//! has been drawn yet. Measured that way, a one-window build at *four times* the window area
//! reported **less** memory than the same build at one times — which cannot be true, and which
//! means the number was tracking "did a redraw land before the sample" rather than anything
//! about the configuration.
//!
//! Reading from inside removes the race: the program says when it is ready to be measured.
//!
//! # The metric is not the same on every platform
//!
//! There is no portable definition of "how much memory is this process using", so [`current`]
//! reports which one it used ([`Metric`]) rather than pretending the three are interchangeable.
//! A table comparing macOS to Linux is comparing two different quantities and should say so.

use std::fmt;

/// Which operating-system metric a [`Footprint`] came from.
///
/// These are *not* the same quantity, and the differences are not small: `phys_footprint`
/// counts compressed pages that `VmRSS` does not, and `PrivateUsage` counts committed private
/// bytes whether or not they are resident.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Metric {
    /// macOS and iOS `phys_footprint`, from `task_info(TASK_VM_INFO)`.
    ///
    /// What Activity Monitor's "Memory" column and `footprint(1)` report, and what Apple's
    /// jetsam actually kills on. The closest thing to "what this app costs the device".
    MachPhysFootprint,
    /// Linux `VmRSS` from `/proc/self/status`: resident set size.
    ///
    /// Excludes anything swapped or not yet faulted in, so it reads lower than the macOS
    /// figure for the same work.
    LinuxResident,
    /// Windows `PROCESS_MEMORY_COUNTERS_EX::PrivateUsage`: the commit charge.
    ///
    /// Counts committed private bytes whether resident or not, so it reads higher than a
    /// resident-set figure for the same work.
    WindowsPrivateUsage,
}

impl Metric {
    /// The operating system's own name for this metric, for labelling a measurement.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::MachPhysFootprint => "phys_footprint",
            Self::LinuxResident => "VmRSS",
            Self::WindowsPrivateUsage => "PrivateUsage",
        }
    }
}

/// A reading of this process's memory, and the metric it came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Footprint {
    /// Bytes, as the operating system reported them.
    pub bytes: u64,
    /// Which quantity [`Self::bytes`] is. See [`Metric`].
    pub metric: Metric,
}

impl Footprint {
    /// Mebibytes, for printing.
    #[must_use]
    pub fn mib(self) -> f64 {
        // The cast is lossless for any process size this could report: f64 carries 53 bits of
        // integer precision, so it is exact below 8 PiB.
        #[allow(clippy::cast_precision_loss)]
        {
            self.bytes as f64 / (1024.0 * 1024.0)
        }
    }
}

impl fmt::Display for Footprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1} MiB ({})", self.mib(), self.metric.name())
    }
}

/// This process's memory right now, or `None` if the platform has no implementation here.
///
/// `None` is a real possibility rather than a formality — it is what every platform other
/// than macOS, iOS, Linux, Android and Windows returns — and callers that print a measurement
/// should say so rather than substituting a zero.
#[must_use]
pub fn current() -> Option<Footprint> {
    platform::current()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod platform {
    use super::{Footprint, Metric};
    use mach2::task_info::{TASK_VM_INFO, task_vm_info};
    use mach2::vm_types::natural_t;

    pub(super) fn current() -> Option<Footprint> {
        let mut info = task_vm_info::default();
        // `task_info` counts in `natural_t` words, not bytes, and it is an in/out parameter:
        // going in it says how much room there is, coming out how much was written.
        let mut count = u32::try_from(size_of::<task_vm_info>() / size_of::<natural_t>()).ok()?;

        // SAFETY: `task_info` writes at most `count` words, and `count` is `task_vm_info`'s
        // own size, so it cannot write past the struct. Reading our own task's info needs no
        // port rights beyond `mach_task_self`.
        let status = unsafe {
            mach2::task::task_info(
                mach2::traps::mach_task_self(),
                TASK_VM_INFO,
                std::ptr::from_mut(&mut info).cast(),
                &raw mut count,
            )
        };

        // KERN_SUCCESS, and enough words to have reached `phys_footprint`. A short read is a
        // failure rather than a small number: an older kernel that stopped before the field
        // would leave `Default`'s zero there, and a zero-byte process is not a measurement.
        let reached_footprint =
            (count as usize) * size_of::<natural_t>() >= footprint_offset() + size_of::<u64>();
        if status != mach2::kern_return::KERN_SUCCESS || !reached_footprint {
            return None;
        }

        Some(Footprint {
            bytes: info.phys_footprint,
            metric: Metric::MachPhysFootprint,
        })
    }

    /// Byte offset of `phys_footprint` within `task_vm_info`.
    ///
    /// Computed rather than written down. The struct is `repr(C, packed(4))` and this crate
    /// does not own it, so a hand-counted offset would be a number that silently stops being
    /// true when Apple adds a field — which is how a memory probe ends up confidently
    /// reporting the wrong quantity.
    fn footprint_offset() -> usize {
        let info = task_vm_info::default();
        // `&raw const` rather than a reference: the struct is packed, so a reference to a
        // field that wants 8-byte alignment would be undefined behaviour even unread.
        let base = (&raw const info).addr();
        let field = (&raw const info.phys_footprint).addr();
        field - base
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The offset is the whole binding: everything else is a call that would fail loudly.
        ///
        /// Nineteen members precede `phys_footprint`: seventeen of 8 bytes, plus the two
        /// adjacent 32-bit ones that share a slot. 17 * 8 + 8 = **144**.
        ///
        /// This assertion has already earned itself. The first version of this test asserted
        /// 152, from miscounting those slots as nineteen rather than eighteen. Had the probe
        /// hardcoded that offset instead of computing it, it would have read
        /// `compressed_lifetime` and reported it as a memory footprint — a wrong number that
        /// looks entirely plausible, which is the worst kind for an instrument whose only job
        /// is to be trusted. If Apple inserts a field ahead of it, this fails here rather than
        /// in a measurement somebody is quoting.
        #[test]
        fn phys_footprint_is_where_the_abi_says() {
            assert_eq!(footprint_offset(), 144);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use super::{Footprint, Metric};

    pub(super) fn current() -> Option<Footprint> {
        // `/proc/self/status` rather than `statm`: `statm` is in pages and would need the page
        // size to interpret, while `status` states its unit, and the unit is the thing most
        // likely to be got wrong silently.
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        parse_vm_rss(&status).map(|bytes| Footprint {
            bytes,
            metric: Metric::LinuxResident,
        })
    }

    /// Pulls `VmRSS` out of `/proc/self/status`, in bytes.
    ///
    /// Split out from the read so it can be tested against a real sample on any platform —
    /// the parsing is the part with a bug in it, and it is unreachable on a developer's Mac.
    pub(super) fn parse_vm_rss(status: &str) -> Option<u64> {
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        let mut fields = line.split_ascii_whitespace().skip(1);
        let value: u64 = fields.next()?.parse().ok()?;
        // The kernel writes "kB" and means KiB. Refuse anything else rather than assume: a
        // unit change would otherwise turn into a number wrong by three orders of magnitude.
        match fields.next()? {
            "kB" => Some(value * 1024),
            _ => None,
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{Footprint, Metric};
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub(super) fn current() -> Option<Footprint> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?,
            ..unsafe { std::mem::zeroed() }
        };

        // SAFETY: `cb` tells the call how much room it has, and it is this struct's own size.
        // The cast to the base type is what the API expects for the extended struct; the call
        // distinguishes them by `cb`.
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };

        (ok != 0).then(|| Footprint {
            bytes: counters.PrivateUsage as u64,
            metric: Metric::WindowsPrivateUsage,
        })
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
)))]
mod platform {
    use super::Footprint;

    pub(super) fn current() -> Option<Footprint> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instrument has to move when memory does, or it is measuring nothing.
    ///
    /// This is the portable version of the check: no external tool, no platform-specific
    /// expected value, just the requirement that allocating and *touching* 64 MiB shows up.
    /// Touching matters — untouched pages are not resident on Linux and not charged on macOS,
    /// so a test that only allocated would pass against a function that returned a constant.
    #[test]
    fn a_large_allocation_is_visible() {
        let Some(before) = current() else {
            // A platform with no implementation is not a failing platform, but it is also not
            // one this test can say anything about.
            return;
        };

        const SIZE: usize = 64 * 1024 * 1024;
        let mut block = vec![0_u8; SIZE];
        // Write one byte per 4 KiB page so every page is faulted in. `black_box` stops the
        // optimiser deleting a write nobody reads.
        for page in block.chunks_mut(4096) {
            page[0] = 1;
        }
        let block = std::hint::black_box(block);

        let after = current().expect("the same platform still reports");
        assert_eq!(after.metric, before.metric);

        let grew = after.bytes.saturating_sub(before.bytes);
        assert!(
            grew >= (SIZE as u64) / 2,
            "{} should have grown by about {} MiB, went from {before} to {after}",
            after.metric.name(),
            SIZE / (1024 * 1024),
        );

        drop(block);
    }

    /// A reading on a supported platform must be a plausible process size.
    #[test]
    fn a_reading_is_not_absurd() {
        let Some(now) = current() else { return };
        assert!(now.bytes > 256 * 1024, "a live process is not {now}");
        assert!(now.bytes < 64 * 1024 * 1024 * 1024, "a test is not {now}");
    }

    #[test]
    fn mib_and_display_agree() {
        let reading = Footprint {
            bytes: 25 * 1024 * 1024,
            metric: Metric::MachPhysFootprint,
        };
        assert!((reading.mib() - 25.0).abs() < f64::EPSILON);
        assert_eq!(reading.to_string(), "25.0 MiB (phys_footprint)");
    }

    /// The Linux parse is the piece most likely to be wrong and least likely to be run, since
    /// it never executes on the machine this is developed on.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn vm_rss_is_parsed_from_a_real_status_block() {
        let sample = "Name:\tcrisol\nVmPeak:\t 2097152 kB\nVmSize:\t 1048576 kB\n\
                      VmRSS:\t   26180 kB\nVmData:\t  524288 kB\n";
        assert_eq!(super::platform::parse_vm_rss(sample), Some(26180 * 1024));

        // VmRSS must not be matched by prefix against its neighbours.
        assert_eq!(super::platform::parse_vm_rss("VmRSSAnon:\t 100 kB\n"), None);
        // An unexpected unit is refused rather than assumed.
        assert_eq!(super::platform::parse_vm_rss("VmRSS:\t 26180 MB\n"), None);
        assert_eq!(super::platform::parse_vm_rss("VmRSS:\t kB\n"), None);
        assert_eq!(super::platform::parse_vm_rss("Name:\tcrisol\n"), None);
    }
}
