//! Shared resolved execution description for persistence and the supervisor.
use super::ExecutionSpec;
use crate::{report::wire, supervisor as p};
use std::time::Duration;

impl ExecutionSpec {
    pub fn wire_spec(&self) -> wire::Spec {
        let capture = |name: &str, cap: &crate::config::StreamCap| p::StreamPolicy {
            sink: if matches!(self.role, super::Role::Raw) {
                p::Sink::Forward(if name == "stdout" {
                    p::Stream::Stdout
                } else {
                    p::Stream::Stderr
                })
            } else {
                p::Sink::File(self.directory.join("out").join(name))
            },
            budget: cap.bytes.value,
            on_exceed: super::exceed(cap.policy),
            events: self.output_events,
        };
        let mut limits = Vec::new();
        if let Some(limit) = &self.resolved.limits.address_space {
            limits.push(p::LocalLimit::AddressSpace { bytes: limit.value });
        }
        if let Some(limit) = &self.resolved.limits.job_commit {
            limits.push(p::LocalLimit::JobCommit { bytes: limit.value });
        }
        wire::Spec {
            platform: super::super::persist::platform(),
            stage: self.stage,
            program: self.command.program.clone(),
            args: self.command.args.clone(),
            cwd: self.resolved.cwd.clone(),
            env: self.resolved.environment.clone(),
            stdin: if matches!(self.role, super::Role::Raw) {
                wire::SpecStdin::Forward
            } else {
                self.stdin
                    .clone()
                    .map_or(wire::SpecStdin::Null, wire::SpecStdin::File)
            },
            stdout: capture("stdout", &self.resolved.stdout),
            stderr: capture("stderr", &self.resolved.stderr),
            wall_deadline: Duration::from_millis(self.resolved.deadline.milliseconds.value),
            limits,
            measure: vec![p::MetricKind::PeakRss, p::MetricKind::CpuTime],
        }
    }

    /// Length-prefixed binary fields in E §2 declaration order. Integers and
    /// list lengths are little-endian u64; enum tags are length-prefixed tokens.
    /// Native Windows strings use little-endian WTF-16 code units.
    pub fn spec_digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        fn bytes(hash: &mut Sha256, value: &[u8]) {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        fn native(hash: &mut Sha256, value: &std::ffi::OsStr) {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                let value: Vec<_> = value.encode_wide().flat_map(u16::to_le_bytes).collect();
                bytes(hash, &value);
            }
            #[cfg(not(windows))]
            {
                bytes(hash, &wire::native_bytes(value));
            }
        }
        let spec = self.wire_spec();
        let mut hash = Sha256::new();
        bytes(
            &mut hash,
            match spec.stage {
                super::Stage::Build => b"build",
                super::Stage::Run => b"run",
                super::Stage::Command => b"command",
                super::Stage::Hook => b"hook",
            },
        );
        native(&mut hash, spec.program.as_os_str());
        hash.update((spec.args.len() as u64).to_le_bytes());
        for arg in &spec.args {
            native(&mut hash, arg);
        }
        native(&mut hash, spec.cwd.as_os_str());
        hash.update((spec.env.len() as u64).to_le_bytes());
        for (name, value) in &spec.env {
            native(&mut hash, name);
            native(&mut hash, value);
        }
        match &spec.stdin {
            wire::SpecStdin::Null => bytes(&mut hash, b"null"),
            wire::SpecStdin::Forward => bytes(&mut hash, b"forward"),
            wire::SpecStdin::File(path) => {
                bytes(&mut hash, b"file");
                native(&mut hash, path.as_os_str());
            }
        }
        for stream in [&spec.stdout, &spec.stderr] {
            match &stream.sink {
                p::Sink::File(path) => {
                    bytes(&mut hash, b"file");
                    native(&mut hash, path.as_os_str());
                }
                p::Sink::Forward(stream) => {
                    bytes(&mut hash, b"forward");
                    bytes(
                        &mut hash,
                        match stream {
                            p::Stream::Stdout => b"stdout",
                            p::Stream::Stderr => b"stderr",
                        },
                    );
                }
                p::Sink::Discard => bytes(&mut hash, b"discard"),
                p::Sink::Memory => bytes(&mut hash, b"memory"),
            }
            hash.update(stream.budget.to_le_bytes());
            bytes(
                &mut hash,
                match stream.on_exceed {
                    p::Exceed::Kill => b"kill",
                    p::Exceed::Discard => b"discard",
                },
            );
            hash.update([u8::from(stream.events)]);
        }
        hash.update(
            self.resolved
                .deadline
                .milliseconds
                .value
                .saturating_mul(1000)
                .to_le_bytes(),
        );
        bytes(&mut hash, b"none"); // deadline_group
        hash.update((spec.limits.len() as u64).to_le_bytes());
        for limit in spec.limits {
            let (tag, value): (&[u8], _) = match limit {
                p::LocalLimit::AddressSpace { bytes } => (b"address_space", bytes),
                p::LocalLimit::JobCommit { bytes } => (b"job_commit", bytes),
            };
            bytes(&mut hash, tag);
            hash.update(value.to_le_bytes());
        }
        hash.update((spec.measure.len() as u64).to_le_bytes());
        for metric in spec.measure {
            bytes(
                &mut hash,
                match metric {
                    p::MetricKind::PeakRss => b"peak_rss",
                    p::MetricKind::CpuTime => b"cpu_time",
                    p::MetricKind::PeakWorkingSet => b"peak_working_set",
                    p::MetricKind::PeakJobCommit => b"peak_job_commit",
                    p::MetricKind::ProcessLifetime => b"process_lifetime",
                },
            );
        }
        hash.finalize().into()
    }
}
