# macOS supervisor

Ordinary roots use the Unix lifecycle core on Linux and macOS. Darwin native
operations live in `src/supervisor/sys/macos.rs`; `root_macos.rs` supplies the
observer integration. Linux's mechanisms and lifecycle ordering are unchanged.
No new dependency or Tokio feature is required.

## Creation and inheritance

Creation uses `std::process::Command::spawn` and `process_group(0)`, with an
absolute executable, explicit cwd, cleared/replaced environment, and prepared
standard streams. Only the supervisor consumes the returned child's status.
The temporary `Child` is decomposed and forgotten, as on Linux.

The signal policy matches Linux: inherit the creator's signal mask and ignored
dispositions, except for Rust's normal SIGPIPE reset to default. Startup restores
SIGCHLD to default and clears SA_NOCLDWAIT. An empty child signal mask or a reset
of every ignored signal is **not** promised. The source audit covered
[Rust 1.85's Unix Command implementation](https://github.com/rust-lang/rust/blob/1.85.0/library/std/src/sys/pal/unix/process/process_unix.rs)
and the [stable implementation](https://github.com/rust-lang/rust/blob/stable/library/std/src/sys/process/unix/unix.rs):
the eligible posix_spawn path sets the process group in spawn attributes, resets
SIGPIPE through spawn attributes, and inherits the mask; the fork/exec fallback
sets the process group and SIGPIPE before exec and also inherits the mask.
Re-audit these implementation details when changing toolchains. Unstable
`-Zon-broken-pipe` overrides are outside this build policy.

There is no application `pre_exec` callback on Darwin. Thus application code
never allocates, logs, enumerates descriptors, accesses the environment or takes
a lock after fork. Any future callback must prepare its inputs beforehand and
use only async-signal-safe calls and allocation-free error handling. It must
preserve Rust's private spawn-error channel.

At startup, before workers, `/dev/fd` enumeration marks inherited descriptors
above 2 CLOEXEC, excluding the directory stream's own descriptor. It never
closes them. Rust file opens use O_CLOEXEC, and stdio stays CLOEXEC until std's
duplication onto 0–2. Darwin lacks pipe2 and kqueue1: a common creation mutex
serializes pipe/kqueue creation plus F_SETFD with every supervisor spawn. The
mutex is acquired only in the parent. Startup sanitization alone would not
cover these concurrent creation windows.

## Observation, reaping and cleanup

Each run owns a CLOEXEC kqueue registered with Tokio AsyncFd. Registration uses
one EVFILT_PROC/NOTE_EXIT change, EV_ADD | EV_RECEIPT, one receipt slot, and a
zero timeout. Count, pid, filter and EV_ERROR are validated before inspecting
receipt data. Zero acknowledges registration; ESRCH latches exit; other errors
record Register and select bounded polling. EINTR retries both call failures
and error receipts. These receipt semantics follow the
[Apple kqueue manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kqueue.2.html).

Successful registration is followed by waitid(P_PID, WEXITED | WNOHANG |
WNOWAIT). Every waitid attempt starts with a fresh zeroed siginfo_t, including
EINTR retries. Queue exit events require the matching pid, EVFILT_PROC and
NOTE_EXIT; EV_ERROR is handled separately. A consumed exit event remains
latched, including across cancellation of an observer future. Reactor/queue
failures during the async wait record Register and switch to polling with the
same 1–10 ms backoff as Linux. A synchronous exit check whose queue read fails
takes one non-consuming polling step instead, so a queue failure never abandons
the still-owned root. A real-kernel test explicitly awaits the queue fd
through Tokio, beginning while the root is live, and verifies the latch after
clearing readiness.

The shared core performs group termination under its signalling capability,
then returns Unsupported cleanup verification without scanning. It revokes
signalling before wait4(WNOHANG). Zero is a pending reap, records Reap, and
retries with backoff until the existing two-second deadline. Timeout transfers
the still-owned root; the cleanup service continues polling. No path restores
numeric signalling authority after a consuming-wait attempt. killpg and the
native root-only kill primitive both accept ESRCH. Connected root coordination
is still Linux-only.

All native calls (kqueue, kevent, waitid, wait4, killpg, kill and the /dev/fd
enumeration APIs) and their flags and types come from the `libc` crate; there is
no hand-declared production FFI.

## Accounting, limits and validation scope

wait4 ru_maxrss is converted directly to bytes on Darwin, without Linux's
multiplication by 1024. CPU uses ru_utime plus ru_stime. Both measurements retain
the shared RootAndWaitedDescendants scope, MaxOfProcesses/Sum aggregation and
method strings. Signed values are clamped at zero. Cleanup is unverified, so
these populated measurements remain Partial under execution contract §7.1;
Unverified does not block a pass. Both LocalLimit kinds return UnsupportedLimit
before capture setup, workers or process creation. This is a support policy,
not a claim that Darwin has no traditional resource limits.

The five existing lifecycle/execution/build/job suites also run on macOS. Tests
use libproc only to observe fixture identities, including zombies (pidinfo
arg=1); these helpers are not the production cleanup verifier. Linux pidfd,
procfs inspection, thread enumeration, slow-scan injections and
local-limit assertions stay Linux-only. Ordinary group termination, deadlines,
cancellation/drop, transfer, capture, resource counters, RSS tolerance and CPU
assertions are shared. Darwin also checks cwd and the complete environment by
executing pwd/env; file-stdin assertions run on both platforms. Scheduling allowances scale on macOS; policy deadlines
do not. Real C++/Python/Java build/run tests are enabled, with test configuration
selecting Homebrew g++-14 on macOS.

Darwin-specific tests cover ESRCH registration after exit, receipt validation,
registration-error fallback, forced polling, Tokio queue readiness, fresh
no-event probes, finite zero-return reap retries, CLOEXEC startup, Send transfer
types, capabilities, unsupported limits and connected refusal before admission.
Direct/relay sessions and transcripts stay Linux-only and return Unsupported
on macOS before admission or endpoint setup.

The compatibility target remains macOS 13+. Validation covers the macOS
releases and architectures exercised in CI (currently arm64 macOS 26); older
releases and Intel are not separately validated. Cleanup verification is
Unsupported on all Darwin versions until libproc scanning is implemented and
validated. The child signal state is the same as on Linux, as described under
Creation and inheritance.
