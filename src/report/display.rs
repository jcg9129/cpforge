//! Typed presentation and human labels from report §2.8.
//! Terminal line escaping and length limits belong to callers.

use super::types::*;
use super::{
    AggregateScore, Aggregation, CheckingUnchecked, InteractiveChecking, Judge, JudgeFailClass,
    JudgeProfile, JudgeReason, JudgeResult, JudgeRole, JudgeText, JudgedCheck, OutputFile,
    OutputFileType, RoleBuildCause, RoleBuildResult, RoleName, Score,
};
use crate::compare::Diag;
use crate::supervisor::{LocalLimit, Measurement};
use std::time::Duration;

pub const fn comparator_token(comparator: &crate::config::Comparator) -> &'static str {
    match comparator {
        crate::config::Comparator::Exact => "exact",
        crate::config::Comparator::Tokens => "tokens",
        crate::config::Comparator::Lines => "lines",
        crate::config::Comparator::Float(_) => "float",
    }
}

/// Render typed labels using bounded, escaped comparator diagnostics.
pub fn render_label(display: &DisplayLabel) -> String {
    let mut parts = vec![display.token.map_or("", LabelToken::wire_token).to_owned()];
    for qualifier in &display.qualifiers {
        let text = match qualifier {
            Qualifier::Comparator(mode) => format!("{mode} comparison"),
            Qualifier::FirstDifference(diag) => diag.to_string(),
            Qualifier::OutputDiffers(diag) => format!("(output differs: {diag})"),
            Qualifier::OutputMatched => "(output matched)".into(),
            Qualifier::OutputFile {
                name,
                outcome,
                diagnostic,
            } => {
                let text = match outcome.as_ref() {
                    // Report §2.8 spells the diagnostic form without the name.
                    OutputFile::Missing if *diagnostic => "no output file".to_owned(),
                    OutputFile::Missing => format!("no output file {name}"),
                    OutputFile::Misnamed => {
                        format!("output file {name} not found under that exact spelling")
                    }
                    OutputFile::NotRegular { file_type } => format!(
                        "output file {name} {}",
                        match file_type {
                            OutputFileType::Link => "is a link",
                            OutputFileType::Directory => "is a directory",
                            OutputFileType::Other => "is not a regular file",
                        }
                    ),
                    OutputFile::Unreadable { .. } => format!("output file {name} not readable"),
                    OutputFile::TooManyEntries => {
                        format!("too many files in the working directory to find {name}")
                    }
                    OutputFile::TooLarge { budget, .. } => {
                        format!("output file {name}: more than {budget} bytes")
                    }
                    _ => format!("output file {name}"),
                };
                if *diagnostic {
                    format!("({text})")
                } else {
                    text
                }
            }
            Qualifier::WallDeadline {
                deadline,
                measured_wall,
            } => {
                let mut text = deadline.map_or_else(
                    || "wall deadline".into(),
                    |d| format!("wall deadline {}s", d.as_secs_f64()),
                );
                if let Some(wall) = measured_wall {
                    text.push_str(&format!(
                        "; measured wall {}s (root, observed)",
                        wall.as_secs_f64()
                    ));
                }
                text
            }
            Qualifier::Signal {
                signal,
                name,
                core_dumped,
            } => format!(
                "signal {}{}",
                name.clone().unwrap_or_else(|| signal.to_string()),
                if *core_dumped { " (core dumped)" } else { "" }
            ),
            Qualifier::Exit { code, .. } => format!("exit code {code}"),
            Qualifier::NotRun(reason) => reason.wire_token().replace('_', " "),
            Qualifier::PrimaryCause(cause) => match cause {
                InfrastructureCause::NotStarted { reason } => {
                    format!("not started: {}", reason.wire_token().replace('_', " "))
                }
                InfrastructureCause::CaptureLoss { stream, loss } => format!(
                    "{} capture: {}",
                    stream.wire_token(),
                    loss.wire_token().replace('_', " ")
                ),
                InfrastructureCause::SupervisorError { error } => {
                    format!("supervisor: {}", error.wire_token().replace('_', " "))
                }
                other => other.wire_token().replace('_', " "),
            },
            Qualifier::Invariant(invariant) => invariant.wire_token().replace('_', " "),
            Qualifier::BuildCause(cause) => cause.wire_token().replace('_', " "),
            Qualifier::OutputBudget { streams, budgets } => streams
                .streams()
                .map(|stream| {
                    budgets.iter().find(|(s, _)| *s == stream).map_or_else(
                        || format!("{} output budget", stream.wire_token()),
                        |(_, bytes)| format!("{} output budget {bytes} bytes", stream.wire_token()),
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
            Qualifier::CleanupNotVerified => "(cleanup not verified)".into(),
            Qualifier::Judged(evidence) => evidence.render(),
            Qualifier::TestPoints { raw, scale } => format!("{raw}/{scale}"),
            Qualifier::JobScore(score) => match score {
                AggregateScore::Points {
                    value,
                    max,
                    aggregation,
                    ..
                } => {
                    format!("{value}/{max} ({})", aggregation_text(*aggregation))
                }
                AggregateScore::Incomplete { missing, .. } => incomplete_score(*missing),
                AggregateScore::NoTests => "no tests: nothing verified".into(),
            },
            Qualifier::RoleBuild { role, cause } => {
                format!("{} build: {}", role.wire_token(), role_build_cause(cause))
            }
            Qualifier::PeerAllowanceExpired => "did not exit after the interactor finished".into(),
            Qualifier::SessionDeadline(deadline) => deadline.map_or_else(
                || "session deadline".into(),
                |d| format!("session deadline {}s", d.as_secs_f64()),
            ),
            Qualifier::Idle(interval) => interval.map_or_else(
                || "no successful forwarding observed".into(),
                |d| {
                    format!(
                        "no successful forwarding observed for {} s",
                        d.as_secs_f64()
                    )
                },
            ),
            Qualifier::InstalledLimit(LocalLimit::AddressSpace { bytes }) => {
                format!("local address-space limit {bytes} bytes")
            }
            Qualifier::InstalledLimit(LocalLimit::JobCommit { bytes }) => {
                format!("local job-commit limit {bytes} bytes")
            }
            other => other
                .fixed_text()
                .map_or_else(|| format!("{other:?}"), str::to_owned),
        };
        parts.push(text);
    }
    parts.join("  ")
}

fn aggregation_text(aggregation: Aggregation) -> &'static str {
    match aggregation {
        Aggregation::Sum => "sum",
        Aggregation::Min => "min",
    }
}
fn incomplete_score(missing: u32) -> String {
    format!("incomplete: {missing} tests without a score")
}
fn judge_role_text(role: JudgeRole) -> &'static str {
    match role {
        JudgeRole::Checker => "checker",
        JudgeRole::Validator => "validator",
        JudgeRole::Interactor => "interactor",
    }
}
fn judge_reason_text(reason: JudgeReason) -> String {
    // Enum serialization supplies the authoritative snake_case token.
    serde_json::to_value(reason)
        .expect("judge reason token")
        .as_str()
        .expect("judge reason string")
        .replace('_', " ")
}
fn run_cause(class: &RunClass) -> String {
    match class {
        RunClass::Signal {
            signal,
            core_dumped,
        } => format!(
            "signal {signal}{}",
            if *core_dumped { " (core dumped)" } else { "" }
        ),
        RunClass::NonzeroExit { code } => format!("exit code {code}"),
        _ => class.wire_token().replace('_', " "),
    }
}
fn role_build_cause(cause: &RoleBuildCause) -> String {
    match cause {
        RoleBuildCause::Run(class) => run_cause(class),
        RoleBuildCause::MissingOutput { .. } => "missing output".into(),
        RoleBuildCause::GeneratedProgramInvalid { .. } => "generated program invalid".into(),
        RoleBuildCause::OutputTooLarge { bytes, cap } => {
            format!("output too large: {bytes} bytes, cap {cap}")
        }
    }
}

/// Finalized deciding evidence, separate from the contestant's run class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JudgedDisplayEvidence {
    Deciding {
        role: JudgeRole,
        message: Option<JudgeText>,
    },
    Failure {
        role: JudgeRole,
        class: JudgeFailClass,
        reason: JudgeReason,
        run_class: Option<RunClass>,
    },
    CheckingUnverified,
    CheckingInterrupted,
}
impl JudgedDisplayEvidence {
    pub fn from_judge(judge: &Judge, run_class: Option<RunClass>) -> Option<Self> {
        match judge.result.as_ref()? {
            JudgeResult::Passed | JudgeResult::Rejected { .. } | JudgeResult::Scored { .. } => {
                Some(Self::Deciding {
                    role: judge.role,
                    message: if judge.profile == JudgeProfile::Simple {
                        judge.stderr.clone()
                    } else {
                        judge.message.clone()
                    },
                })
            }
            JudgeResult::JudgeFailed { class, reason } => Some(Self::Failure {
                role: judge.role,
                class: *class,
                reason: *reason,
                run_class,
            }),
            JudgeResult::Unverified
            | JudgeResult::NotRead {
                reason: super::JudgeNotRead::CleanupIncomplete,
            } => Some(Self::CheckingUnverified),
            JudgeResult::NotRead { .. } => None,
        }
    }
    fn render(&self) -> String {
        match self {
            Self::Deciding { role, message } => {
                let mut text = judge_role_text(*role).to_owned();
                if let Some(message) = message {
                    // Match the shared line sanitization before quoting. The
                    // progress sink applies the shared line-size cap afterward.
                    let head: String = String::from_utf8_lossy(&message.head)
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();
                    text.push_str(&format!(": \"{head}\""));
                }
                text
            }
            Self::Failure {
                role,
                class,
                reason,
                run_class,
            } => {
                let role_name = judge_role_text(*role);
                if *class == JudgeFailClass::TestDataError && *role == JudgeRole::Validator {
                    return format!("{role_name}: {}", judge_reason_text(*reason));
                }
                if *class == JudgeFailClass::Crashed {
                    return format!(
                        "{role_name} crashed: {}",
                        run_class
                            .as_ref()
                            .map_or_else(|| judge_reason_text(*reason), run_cause)
                    );
                }
                let class = match class {
                    JudgeFailClass::DeclaredFail => "declared fail",
                    JudgeFailClass::ProtocolUnsupported => "protocol unsupported",
                    JudgeFailClass::ReportInvalid => "report invalid",
                    JudgeFailClass::ArtefactInvalid => "artefact invalid",
                    JudgeFailClass::TestDataError => "test data error",
                    JudgeFailClass::Crashed => unreachable!(),
                };
                format!("{role_name}: {class} ({})", judge_reason_text(*reason))
            }
            Self::CheckingUnverified => "cleanup not verified: report not read".into(),
            Self::CheckingInterrupted => "checking interrupted".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelToken {
    Ok,
    WrongAnswer,
    Unchecked,
    CheckerFailed,
    TimeLimit,
    OutputLimit,
    RuntimeError,
    Cancelled,
    Infrastructure,
    Internal,
    NotRun,
    CompileError,
    PresentationError,
    Points,
    SessionLimit,
    Idle,
}
impl LabelToken {
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::WrongAnswer => "WA",
            Self::Unchecked => "UNCHECKED",
            Self::CheckerFailed => "CHECKER-FAILED",
            Self::TimeLimit => "TLE",
            Self::OutputLimit => "OLE",
            Self::RuntimeError => "RE",
            Self::Cancelled => "CANCELLED",
            Self::Infrastructure => "INFRA",
            Self::Internal => "INTERNAL",
            Self::NotRun => "NOT RUN",
            Self::CompileError => "CE",
            Self::PresentationError => "PE",
            Self::Points => "SCORE",
            Self::SessionLimit => "TLE",
            Self::Idle => "IDLE",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Qualifier {
    Comparator(&'static str),
    CleanupNotVerified,
    FirstDifference(Box<Diag>),
    NoAnswer,
    PresentationError,
    InvalidAnswer,
    WallDeadline {
        deadline: Option<Duration>,
        measured_wall: Option<Duration>,
    },
    OutputBudget {
        streams: BudgetStreams,
        budgets: Vec<(Stream, u64)>,
    },
    Signal {
        signal: i32,
        name: Option<String>,
        core_dumped: bool,
    },
    Exit {
        code: i64,
        ntstatus: Option<String>,
    },
    PrimaryCause(InfrastructureCause),
    Invariant(Invariant),
    NotRun(SkipReason),
    BuildCause(BuildCause),
    InstalledLimit(LocalLimit),
    OutputMatched,
    OutputFile {
        name: String,
        outcome: Box<OutputFile>,
        diagnostic: bool,
    },
    OutputDiffers(Box<Diag>),
    Measurement(Measurement),
    Judged(Box<JudgedDisplayEvidence>),
    TestPoints {
        raw: Score,
        scale: Score,
    },
    JobScore(AggregateScore),
    RoleBuild {
        role: RoleName,
        cause: Box<RoleBuildCause>,
    },
    PeerAllowanceExpired,
    SessionDeadline(Option<Duration>),
    Idle(Option<Duration>),
}
impl Qualifier {
    /// Fixed wording, without terminal escaping or formatting of evidence.
    pub const fn fixed_text(&self) -> Option<&'static str> {
        match self {
            Self::CleanupNotVerified => Some("cleanup not verified"),
            Self::NoAnswer => Some("no answer"),
            Self::PresentationError => Some("presentation error"),
            Self::InvalidAnswer => Some("invalid answer"),
            Self::OutputMatched => Some("output matched"),
            _ => None,
        }
    }
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Comparator(_) => "comparator",
            Self::CleanupNotVerified => "cleanup_not_verified",
            Self::FirstDifference(_) => "first_difference",
            Self::NoAnswer => "no_answer",
            Self::PresentationError => "presentation_error",
            Self::InvalidAnswer => "invalid_answer",
            Self::WallDeadline { .. } => "wall_deadline",
            Self::OutputBudget { .. } => "output_budget",
            Self::Signal { .. } => "signal",
            Self::Exit { .. } => "exit",
            Self::PrimaryCause(_) => "primary_cause",
            Self::Invariant(_) => "invariant",
            Self::NotRun(_) => "not_run",
            Self::BuildCause(_) => "build_cause",
            Self::InstalledLimit(_) => "installed_limit",
            Self::OutputMatched => "output_matched",
            Self::OutputFile { .. } => "output_file",
            Self::OutputDiffers(_) => "output_differs",
            Self::Measurement(_) => "measurement",
            Self::Judged(_) => "judged",
            Self::TestPoints { .. } => "test_points",
            Self::JobScore(_) => "job_score",
            Self::RoleBuild { .. } => "role_build",
            Self::PeerAllowanceExpired => "peer_allowance_expired",
            Self::SessionDeadline(_) => "session_deadline",
            Self::Idle(_) => "idle",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayLabel {
    /// `no_tests` has only the specified summary, since §2.8 gives it no label.
    pub token: Option<LabelToken>,
    pub qualifiers: Vec<Qualifier>,
}
/// References let display preserve exact diagnostics and execution evidence.
#[derive(Clone, Copy, Debug)]
pub enum LabelSubject<'a> {
    Test {
        verdict: &'a TestVerdict,
        class: Option<&'a RunClass>,
        check: Option<&'a CheckOutcome>,
    },
    Build(&'a BuildResult),
    RoleBuild {
        role: RoleName,
        result: &'a RoleBuildResult,
    },
    Run(&'a ExecutionResult),
    Hook(&'a HookOutcome),
    Job(JobVerdict),
}
impl LabelSubject<'_> {
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Test { .. } => "test",
            Self::Build(_) => "build",
            Self::RoleBuild { .. } => "role_build",
            Self::Run(_) => "run",
            Self::Hook(_) => "hook",
            Self::Job(_) => "job",
        }
    }
}
/// Label tokens and typed qualifier fields only. A diagnostic match never
/// replaces the run label with OK, and a diagnostic mismatch never becomes WA.
pub fn label(subject: LabelSubject<'_>, context: &DisplayContext) -> DisplayLabel {
    let show_comparator = context.judged.is_none()
        && match subject {
            LabelSubject::Test { verdict, check, .. } => {
                matches!(verdict, TestVerdict::Passed)
                    && !check.is_some_and(|check| matches!(check.kind, CheckKind::Judged { .. }))
            }
            LabelSubject::Job(JobVerdict::Passed) => true,
            _ => false,
        };
    let mut primary_cause = None;
    let (token, class, check, invariant, reason, cause) = match subject {
        LabelSubject::Test {
            verdict,
            class,
            check,
        } => {
            let (token, invariant, reason) = match verdict {
                TestVerdict::PresentationError => (LabelToken::PresentationError, None, None),
                TestVerdict::Scored => (LabelToken::Points, None, None),
                TestVerdict::SessionLimit => (LabelToken::SessionLimit, None, None),
                TestVerdict::Idle => (LabelToken::Idle, None, None),
                TestVerdict::Passed => (LabelToken::Ok, None, None),
                TestVerdict::WrongAnswer => (LabelToken::WrongAnswer, None, None),
                TestVerdict::Unchecked => (LabelToken::Unchecked, None, None),
                TestVerdict::CheckerFailed => (LabelToken::CheckerFailed, None, None),
                TestVerdict::WallDeadline => (LabelToken::TimeLimit, None, None),
                TestVerdict::OutputBudget => (LabelToken::OutputLimit, None, None),
                TestVerdict::Signal | TestVerdict::NonzeroExit => {
                    (LabelToken::RuntimeError, None, None)
                }
                TestVerdict::Cancelled => (LabelToken::Cancelled, None, None),
                TestVerdict::Infrastructure => (LabelToken::Infrastructure, None, None),
                TestVerdict::NotRun { reason } => (LabelToken::NotRun, None, Some(*reason)),
                TestVerdict::InternalError { invariant } => {
                    (LabelToken::Internal, Some(*invariant), None)
                }
            };
            (Some(token), class, check, invariant, reason, None)
        }
        LabelSubject::Build(result) => match result {
            BuildResult::Succeeded => (Some(LabelToken::Ok), None, None, None, None, None),
            BuildResult::Cancelled => (Some(LabelToken::Cancelled), None, None, None, None, None),
            BuildResult::Infrastructure { causes } => {
                primary_cause = causes.primary().copied();
                (
                    Some(LabelToken::Infrastructure),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            }
            BuildResult::Failed { cause } => (
                Some(LabelToken::CompileError),
                None,
                None,
                None,
                None,
                Some(cause),
            ),
            BuildResult::InternalError { invariant } => (
                Some(LabelToken::Internal),
                None,
                None,
                Some(*invariant),
                None,
                None,
            ),
        },
        LabelSubject::RoleBuild { role, result } => return role_build_label(role, result, context),
        LabelSubject::Run(execution) => {
            if let Some(invariant) = super::execution_invariant(execution) {
                (
                    Some(LabelToken::Internal),
                    None,
                    None,
                    Some(invariant),
                    None,
                    None,
                )
            } else if let Coverage::Skipped { reason } = execution.coverage {
                (
                    Some(if reason == SkipReason::Cancelled {
                        LabelToken::Cancelled
                    } else {
                        LabelToken::NotRun
                    }),
                    None,
                    None,
                    None,
                    (reason != SkipReason::Cancelled).then_some(reason),
                    None,
                )
            } else {
                (
                    execution.class.as_ref().map(class_label),
                    execution.class.as_ref(),
                    None,
                    None,
                    None,
                    None,
                )
            }
        }
        LabelSubject::Hook(outcome) => match outcome {
            HookOutcome::Succeeded => (Some(LabelToken::Ok), None, None, None, None, None),
            HookOutcome::Failed { class } => (
                Some(class_label(class)),
                Some(class),
                None,
                None,
                None,
                None,
            ),
            HookOutcome::Cancelled => (Some(LabelToken::Cancelled), None, None, None, None, None),
            // As for any skipped execution, cancellation is labelled CANCELLED.
            HookOutcome::Skipped { reason } => (
                Some(if *reason == SkipReason::Cancelled {
                    LabelToken::Cancelled
                } else {
                    LabelToken::NotRun
                }),
                None,
                None,
                None,
                (*reason != SkipReason::Cancelled).then_some(*reason),
                None,
            ),
            HookOutcome::InternalError { invariant } => (
                Some(LabelToken::Internal),
                None,
                None,
                Some(*invariant),
                None,
                None,
            ),
        },
        LabelSubject::Job(verdict) => (
            match verdict {
                JobVerdict::InternalError => Some(LabelToken::Internal),
                JobVerdict::Infrastructure => Some(LabelToken::Infrastructure),
                JobVerdict::Cancelled => Some(LabelToken::Cancelled),
                JobVerdict::BuildFailed => Some(LabelToken::CompileError),
                JobVerdict::WallDeadline => Some(LabelToken::TimeLimit),
                JobVerdict::OutputBudget => Some(LabelToken::OutputLimit),
                JobVerdict::Signal | JobVerdict::NonzeroExit => Some(LabelToken::RuntimeError),
                JobVerdict::CheckerFailed => Some(LabelToken::CheckerFailed),
                JobVerdict::WrongAnswer => Some(LabelToken::WrongAnswer),
                JobVerdict::Unchecked => Some(LabelToken::Unchecked),
                JobVerdict::NoTests => None,
                JobVerdict::PresentationError => Some(LabelToken::PresentationError),
                JobVerdict::Scored => Some(LabelToken::Points),
                JobVerdict::SessionLimit => Some(LabelToken::SessionLimit),
                JobVerdict::Idle => Some(LabelToken::Idle),
                JobVerdict::Passed | JobVerdict::Succeeded => Some(LabelToken::Ok),
            },
            None,
            None,
            context.invariant,
            None,
            None,
        ),
    };
    let mut qualifiers = Vec::new();
    if token == Some(LabelToken::Ok) {
        if show_comparator {
            if let Some(comparator) = context.comparator {
                qualifiers.push(Qualifier::Comparator(comparator));
            }
        }
        if context.cleanup_unverified {
            qualifiers.push(Qualifier::CleanupNotVerified);
        }
    }
    if token == Some(LabelToken::PresentationError) {
        qualifiers.push(Qualifier::PresentationError);
    }
    let judged = context.judged.clone().or_else(|| {
        check.and_then(|check| {
            let CheckKind::Judged { outcome } = &check.kind else {
                return None;
            };
            match outcome.as_ref() {
                JudgedCheck::Rejected { role, .. } | JudgedCheck::Scored { role, .. } => {
                    Some(Box::new(JudgedDisplayEvidence::Deciding {
                        role: *role,
                        message: None,
                    }))
                }
                JudgedCheck::JudgeFailed {
                    role,
                    class,
                    reason,
                } => Some(Box::new(JudgedDisplayEvidence::Failure {
                    role: *role,
                    class: *class,
                    reason: *reason,
                    run_class: None,
                })),
                JudgedCheck::Unchecked {
                    reason: CheckingUnchecked::CheckingUnverified,
                } => Some(Box::new(JudgedDisplayEvidence::CheckingUnverified)),
                JudgedCheck::Unchecked {
                    reason: CheckingUnchecked::CheckingInterrupted,
                } => Some(Box::new(JudgedDisplayEvidence::CheckingInterrupted)),
            }
        })
    });
    if token == Some(LabelToken::Unchecked) && judged.is_none() {
        qualifiers.push(Qualifier::NoAnswer);
    }
    if token == Some(LabelToken::CheckerFailed) && judged.is_none() {
        qualifiers.push(Qualifier::InvalidAnswer);
    }
    if token == Some(LabelToken::TimeLimit) {
        qualifiers.push(if context.peer_allowance_expired {
            Qualifier::PeerAllowanceExpired
        } else {
            Qualifier::WallDeadline {
                deadline: context.wall_deadline,
                measured_wall: context.measured_wall,
            }
        });
    }
    if token == Some(LabelToken::SessionLimit) {
        qualifiers.push(Qualifier::SessionDeadline(context.session_deadline));
    }
    if token == Some(LabelToken::Idle) {
        qualifiers.push(Qualifier::Idle(context.idle_interval));
    }
    if matches!(
        token,
        Some(
            LabelToken::Ok
                | LabelToken::WrongAnswer
                | LabelToken::PresentationError
                | LabelToken::CheckerFailed
                | LabelToken::Unchecked
        )
    ) {
        if let Some(judged) = judged {
            qualifiers.push(Qualifier::Judged(judged));
        }
    }
    if matches!(subject, LabelSubject::Test { .. }) {
        let points = context.test_points.or_else(|| {
            check.and_then(|check| match &check.kind {
                CheckKind::Judged { outcome } => match outcome.as_ref() {
                    JudgedCheck::Scored { raw, scale, .. } => Some((*raw, *scale)),
                    _ => None,
                },
                _ => None,
            })
        });
        if let Some((raw, scale)) = points {
            qualifiers.push(Qualifier::TestPoints { raw, scale });
        }
    } else if matches!(subject, LabelSubject::Job(_)) {
        if let Some(score) = &context.job_score {
            qualifiers.push(Qualifier::JobScore(score.clone()));
        }
    }
    // Qualify only the chosen label: e.g. cancellation at check admission must
    // not accidentally acquire a runtime-error qualifier from the run class.
    if let Some(class) = class {
        match class {
            RunClass::OutputBudget { streams } if token == Some(LabelToken::OutputLimit) => {
                let mut streams = *streams;
                if context.output_file.is_some() {
                    streams.output_file = false;
                }
                if streams.primary().is_some() {
                    qualifiers.push(Qualifier::OutputBudget {
                        streams,
                        budgets: context.budgets.clone(),
                    });
                }
            }
            RunClass::Signal {
                signal,
                core_dumped,
            } if token == Some(LabelToken::RuntimeError) => qualifiers.push(Qualifier::Signal {
                signal: *signal,
                name: context.signal_name.clone(),
                core_dumped: *core_dumped,
            }),
            RunClass::NonzeroExit { code } if token == Some(LabelToken::RuntimeError) => qualifiers
                .push(Qualifier::Exit {
                    code: *code,
                    ntstatus: context.ntstatus.clone(),
                }),
            RunClass::Infrastructure { causes } if token == Some(LabelToken::Infrastructure) => {
                if let Some(cause) = causes.primary() {
                    qualifiers.push(Qualifier::PrimaryCause(*cause));
                }
            }
            _ => {}
        }
    }
    if let Some(cause) = primary_cause {
        qualifiers.push(Qualifier::PrimaryCause(cause));
    }
    if let Some(check) = check {
        match &check.kind {
            CheckKind::Passed if check.diagnostic => qualifiers.push(Qualifier::OutputMatched),
            CheckKind::Failed { diag } if check.diagnostic => {
                qualifiers.push(Qualifier::OutputDiffers(diag.clone()))
            }
            CheckKind::Failed { diag } if token == Some(LabelToken::WrongAnswer) => {
                qualifiers.push(Qualifier::FirstDifference(diag.clone()))
            }
            _ => {}
        }
        if let Some((name, outcome)) = &context.output_file {
            if matches!(check.kind, CheckKind::OutputRejected { .. })
                || (matches!(check.kind, CheckKind::Passed | CheckKind::Failed { .. })
                    && !check.diagnostic)
            {
                qualifiers.push(Qualifier::OutputFile {
                    name: name.clone(),
                    outcome: Box::new(outcome.clone()),
                    diagnostic: check.diagnostic
                        && !(token == Some(LabelToken::OutputLimit)
                            && matches!(outcome, OutputFile::TooLarge { .. })),
                });
            }
        }
    }
    if let Some(invariant) = invariant {
        qualifiers.push(Qualifier::Invariant(invariant));
    }
    if let Some(reason) = reason {
        qualifiers.push(Qualifier::NotRun(reason));
    }
    if let Some(cause) = cause {
        qualifiers.push(Qualifier::BuildCause(cause.clone()));
    }
    qualifiers.extend(
        context
            .installed_limits
            .iter()
            .copied()
            .map(Qualifier::InstalledLimit),
    );
    qualifiers.extend(
        context
            .measurements
            .iter()
            .cloned()
            .map(Qualifier::Measurement),
    );
    DisplayLabel { token, qualifiers }
}

fn role_build_label(
    role: RoleName,
    result: &RoleBuildResult,
    context: &DisplayContext,
) -> DisplayLabel {
    let result = match result {
        RoleBuildResult::Failed { cause } => {
            return DisplayLabel {
                token: Some(LabelToken::CompileError),
                qualifiers: vec![Qualifier::RoleBuild {
                    role,
                    cause: cause.clone(),
                }],
            };
        }
        RoleBuildResult::Skipped { reason } => {
            return label(
                LabelSubject::Hook(&HookOutcome::Skipped { reason: *reason }),
                context,
            );
        }
        RoleBuildResult::Succeeded => BuildResult::Succeeded,
        RoleBuildResult::Cancelled => BuildResult::Cancelled,
        RoleBuildResult::Infrastructure { causes } => BuildResult::Infrastructure {
            causes: causes.clone(),
        },
        RoleBuildResult::InternalError { invariant } => BuildResult::InternalError {
            invariant: *invariant,
        },
    };
    label(LabelSubject::Build(&result), context)
}
/// A profile changes only the human label; raw verdicts remain unchanged.
pub fn label_for_profile(
    subject: LabelSubject<'_>,
    context: &DisplayContext,
    profile: super::DisplayProfile,
) -> DisplayLabel {
    let mut display = label(subject, context);
    if profile == super::DisplayProfile::JudgeCf
        && display.token == Some(LabelToken::PresentationError)
    {
        display.token = Some(LabelToken::WrongAnswer);
    }
    display
}
fn class_label(class: &RunClass) -> LabelToken {
    match class {
        RunClass::ExitedZero => LabelToken::Ok,
        RunClass::NonzeroExit { .. } | RunClass::Signal { .. } => LabelToken::RuntimeError,
        RunClass::WallDeadline => LabelToken::TimeLimit,
        RunClass::OutputBudget { .. } => LabelToken::OutputLimit,
        RunClass::Cancelled => LabelToken::Cancelled,
        RunClass::Infrastructure { .. } => LabelToken::Infrastructure,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummaryTemplate {
    Passed {
        passed: u32,
        selected: u32,
        comparator: &'static str,
    },
    Unchecked {
        count: u32,
    },
    NoTests,
    Checker {
        passed: u32,
        selected: u32,
        profile: JudgeProfile,
    },
    Interactive {
        passed: u32,
        selected: u32,
        checking: InteractiveChecking,
    },
    Points {
        value: Score,
        max: Score,
        aggregation: Aggregation,
        selected: u32,
    },
    IncompleteScore {
        missing: u32,
    },
}
impl SummaryTemplate {
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Passed { .. } => "passed",
            Self::Unchecked { .. } => "unchecked",
            Self::NoTests => "no_tests",
            Self::Checker { .. } => "checker",
            Self::Interactive { .. } => "interactive",
            Self::Points { .. } => "points",
            Self::IncompleteScore { .. } => "incomplete_score",
        }
    }
    pub fn render(&self) -> String {
        match self {
            Self::Passed {
                passed,
                selected,
                comparator,
            } => format!("{passed}/{selected} passed ({comparator} comparison)"),
            Self::Unchecked { count } => format!("{count} unchecked (no answer): not verified"),
            Self::NoTests => "no tests: nothing verified".into(),
            Self::Checker {
                passed,
                selected,
                profile,
            } => format!(
                "{passed}/{selected} passed ({} checker)",
                match profile {
                    JudgeProfile::Testlib => "testlib",
                    JudgeProfile::Simple => "simple",
                }
            ),
            Self::Interactive {
                passed,
                selected,
                checking,
            } => format!(
                "{passed}/{selected} passed (interactive, checking {})",
                match checking {
                    InteractiveChecking::Interactor => "interactor",
                    InteractiveChecking::Checker => "checker",
                    InteractiveChecking::InteractorChecker => "interactor_checker",
                }
            ),
            Self::Points {
                value,
                max,
                aggregation,
                selected,
            } => format!(
                "{value}/{max} points ({}) over {selected} tests",
                aggregation_text(*aggregation)
            ),
            Self::IncompleteScore { missing } => incomplete_score(*missing),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub template: SummaryTemplate,
    pub file_io: bool,
    pub inputs_validated: bool,
    pub interrupted_after_completion: bool,
}
impl Summary {
    pub fn with_validated_inputs(mut self) -> Self {
        self.inputs_validated = true;
        self
    }
    pub fn with_file_io(mut self) -> Self {
        self.file_io = true;
        self
    }
    pub fn render(&self) -> String {
        let mut text = self.template.render();
        if self.file_io {
            text.push_str(" (file I/O)");
        }
        if self.inputs_validated {
            text.push_str(" (inputs validated)");
        }
        if self.interrupted_after_completion {
            text.push_str(" (interrupted after completion)");
        }
        text
    }
}
/// A template plus a qualifier; completion is a finalized observation, not an
/// inference from intent. Call once for each summary line to be displayed.
pub fn summary(template: SummaryTemplate, completed: bool, recorded_intent: bool) -> Summary {
    Summary {
        template,
        file_io: false,
        inputs_validated: false,
        interrupted_after_completion: completed && recorded_intent,
    }
}
