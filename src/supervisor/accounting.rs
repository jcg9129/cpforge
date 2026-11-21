//! Informative measurements, explicit local mechanisms, and doctor capabilities.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricKind {
    PeakRss,
    PeakWorkingSet,
    PeakJobCommit,
    CpuTime,
    ProcessLifetime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Microseconds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Root,
    RootAndWaitedDescendants,
    Job,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregation {
    MaxOfProcesses,
    Sum,
    JobPeak,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completeness {
    Final,
    Partial,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measurement {
    pub kind: MetricKind,
    pub value: Option<u64>,
    pub unit: Unit,
    pub scope: Scope,
    pub aggregation: Aggregation,
    pub completeness: Completeness,
    pub method: &'static str,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalLimit {
    AddressSpace { bytes: u64 },
    JobCommit { bytes: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    AddressSpace,
    JobCommit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitValidation {
    Zero,
    Duplicate,
    OutOfRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OuterRestriction {
    None,
    Known(u64),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledLimit {
    pub configured: LocalLimit,
    pub inherited: Option<(u64, u64)>,
    pub normalised: u64,
    pub installed: Option<u64>,
    pub mechanism: &'static str,
    pub outer: OuterRestriction,
}

/// A probe is informative: every run still checks installation independently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability<K> {
    pub kind: K,
    pub compiled: bool,
    pub probe: Result<(), String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub metrics: Vec<Capability<MetricKind>>,
    pub limits: Vec<Capability<LimitKind>>,
    pub observers: Vec<Capability<&'static str>>,
    pub connected_runs: Capability<&'static str>,
    pub deferred: Vec<Capability<&'static str>>,
}

/// Cheap, non-spawning probes; no limits or process-wide state are changed.
#[cfg(not(target_os = "macos"))]
pub fn capabilities() -> Capabilities {
    #[cfg(target_os = "linux")]
    let (accounting, address_space, pidfd, procfs) = super::sys::probe_capabilities();
    #[cfg(not(target_os = "linux"))]
    let (accounting, address_space, pidfd, procfs) =
        (unsupported(), unsupported(), unsupported(), unsupported());
    let linux = cfg!(target_os = "linux");
    Capabilities {
        metrics: [
            MetricKind::PeakRss,
            MetricKind::PeakWorkingSet,
            MetricKind::PeakJobCommit,
            MetricKind::CpuTime,
            MetricKind::ProcessLifetime,
        ]
        .into_iter()
        .map(|kind| {
            let compiled = linux && matches!(kind, MetricKind::PeakRss | MetricKind::CpuTime);
            Capability {
                kind,
                compiled,
                probe: if compiled {
                    accounting.clone()
                } else {
                    unsupported()
                },
            }
        })
        .collect(),
        limits: vec![
            Capability {
                kind: LimitKind::AddressSpace,
                compiled: linux,
                probe: address_space,
            },
            Capability {
                kind: LimitKind::JobCommit,
                compiled: false,
                probe: unsupported(),
            },
        ],
        observers: vec![
            Capability {
                kind: "pidfd",
                compiled: linux,
                probe: pidfd,
            },
            Capability {
                kind: "procfs supported view",
                compiled: linux,
                probe: procfs,
            },
        ],
        connected_runs: Capability {
            kind: "connected runs",
            compiled: linux,
            probe: if linux { Ok(()) } else { unsupported() },
        },
        deferred: [
            "cgroup accounting",
            "cgroup containment",
            "--jobs",
            "build cache",
        ]
        .into_iter()
        .map(|kind| Capability {
            kind,
            compiled: false,
            probe: Err("unavailable feature".into()),
        })
        .collect(),
    }
}

#[cfg(target_os = "macos")]
pub fn capabilities() -> Capabilities {
    Capabilities {
        metrics: [
            MetricKind::PeakRss,
            MetricKind::PeakWorkingSet,
            MetricKind::PeakJobCommit,
            MetricKind::CpuTime,
            MetricKind::ProcessLifetime,
        ]
        .into_iter()
        .map(|kind| {
            let compiled = matches!(kind, MetricKind::PeakRss | MetricKind::CpuTime);
            Capability {
                kind,
                compiled,
                probe: if compiled { Ok(()) } else { unsupported() },
            }
        })
        .collect(),
        limits: [LimitKind::AddressSpace, LimitKind::JobCommit]
            .into_iter()
            .map(|kind| Capability {
                kind,
                compiled: false,
                probe: unsupported(),
            })
            .collect(),
        observers: vec![
            Capability {
                kind: "kqueue",
                compiled: true,
                probe: super::sys::macos::probe_observer().map_err(|error| error.to_string()),
            },
            Capability {
                kind: "libproc cleanup verification",
                compiled: true,
                probe: super::sys::macos::probe_cleanup().map_err(|reason| format!("{reason:?}")),
            },
        ],
        connected_runs: Capability {
            kind: "connected runs",
            compiled: true,
            probe: Ok(()),
        },
        deferred: [
            "cgroup accounting",
            "cgroup containment",
            "--jobs",
            "build cache",
        ]
        .into_iter()
        .map(|kind| Capability {
            kind,
            compiled: false,
            probe: Err("unavailable feature".into()),
        })
        .collect(),
    }
}

fn unsupported() -> Result<(), String> {
    Err("unsupported on this platform".into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn measurements(kinds: &[MetricKind]) -> Vec<Measurement> {
    let mut result = Vec::with_capacity(5);
    for &kind in kinds {
        if result.iter().any(|m: &Measurement| m.kind == kind) {
            continue;
        }
        let (unit, scope, aggregation, method) = match kind {
            MetricKind::PeakRss => (
                Unit::Bytes,
                Scope::RootAndWaitedDescendants,
                Aggregation::MaxOfProcesses,
                "wait4 ru_maxrss",
            ),
            MetricKind::CpuTime => (
                Unit::Microseconds,
                Scope::RootAndWaitedDescendants,
                Aggregation::Sum,
                "wait4 ru_utime + ru_stime",
            ),
            MetricKind::PeakWorkingSet => (
                Unit::Bytes,
                Scope::Root,
                Aggregation::MaxOfProcesses,
                "unsupported",
            ),
            MetricKind::PeakJobCommit => {
                (Unit::Bytes, Scope::Job, Aggregation::JobPeak, "unsupported")
            }
            MetricKind::ProcessLifetime => (
                Unit::Microseconds,
                Scope::Root,
                Aggregation::Sum,
                "unsupported",
            ),
        };
        result.push(Measurement {
            kind,
            value: None,
            unit,
            scope,
            aggregation,
            method,
            completeness: Completeness::Partial,
            reason: Some(
                if matches!(kind, MetricKind::PeakRss | MetricKind::CpuTime) {
                    "root was not reaped"
                } else {
                    "unsupported on this platform"
                }
                .into(),
            ),
        });
    }
    result
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn record_usage(outcome: &mut super::RootOutcome, usage: super::sys::RawRusage) {
    let confirmed = outcome.termination.cleanup == super::Cleanup::Confirmed;
    for measurement in &mut outcome.measurements {
        measurement.value = match measurement.kind {
            MetricKind::PeakRss => Some(usage.maxrss_bytes),
            MetricKind::CpuTime => Some(
                (usage.user + usage.system)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64,
            ),
            _ => continue,
        };
        measurement.completeness = if confirmed {
            Completeness::Final
        } else {
            Completeness::Partial
        };
        measurement.reason = (!confirmed).then(|| "cleanup not confirmed".into());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn connected_capability_matches_the_session_platforms() {
        let connected = super::capabilities().connected_runs;
        let supported = cfg!(any(target_os = "linux", target_os = "macos"));
        assert_eq!(connected.kind, "connected runs");
        assert_eq!(connected.compiled, supported);
        assert_eq!(connected.probe.is_ok(), supported);
    }
}
