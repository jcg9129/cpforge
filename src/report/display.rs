//! Presentation data from report §2.8. Rendering/escaping belongs to callers.

use super::types::*;
use crate::compare::Diag;
use crate::supervisor::{LocalLimit, Measurement};
use std::time::Duration;

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
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Qualifier {
    Comparator(&'static str),
    CleanupNotVerified,
    FirstDifference(Box<Diag>),
    NoAnswer,
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
    OutputDiffers(Box<Diag>),
    Measurement(Measurement),
}
impl Qualifier {
    /// Fixed wording, without terminal escaping or formatting of evidence.
    pub const fn fixed_text(&self) -> Option<&'static str> {
        match self {
            Self::CleanupNotVerified => Some("cleanup not verified"),
            Self::NoAnswer => Some("no answer"),
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
            Self::OutputDiffers(_) => "output_differs",
            Self::Measurement(_) => "measurement",
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
    Run(&'a ExecutionResult),
    Hook(&'a HookOutcome),
    Job(JobVerdict),
}
impl LabelSubject<'_> {
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Test { .. } => "test",
            Self::Build(_) => "build",
            Self::Run(_) => "run",
            Self::Hook(_) => "hook",
            Self::Job(_) => "job",
        }
    }
}
/// Label tokens and typed qualifier fields only. A diagnostic match never
/// replaces the run label with OK, and a diagnostic mismatch never becomes WA.
pub fn label(subject: LabelSubject<'_>, context: &DisplayContext) -> DisplayLabel {
    let show_comparator = match subject {
        LabelSubject::Test { verdict, .. } => matches!(verdict, TestVerdict::Passed),
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
    if token == Some(LabelToken::Unchecked) {
        qualifiers.push(Qualifier::NoAnswer);
    }
    if token == Some(LabelToken::CheckerFailed) {
        qualifiers.push(Qualifier::InvalidAnswer);
    }
    if token == Some(LabelToken::TimeLimit) {
        qualifiers.push(Qualifier::WallDeadline {
            deadline: context.wall_deadline,
            measured_wall: context.measured_wall,
        });
    }
    // Qualify only the chosen label: e.g. cancellation at check admission must
    // not accidentally acquire a runtime-error qualifier from the run class.
    if let Some(class) = class {
        match class {
            RunClass::OutputBudget { streams } if token == Some(LabelToken::OutputLimit) => {
                qualifiers.push(Qualifier::OutputBudget {
                    streams: *streams,
                    budgets: context.budgets.clone(),
                })
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
}
impl SummaryTemplate {
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Passed { .. } => "passed",
            Self::Unchecked { .. } => "unchecked",
            Self::NoTests => "no_tests",
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
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub template: SummaryTemplate,
    pub interrupted_after_completion: bool,
}
impl Summary {
    pub fn render(&self) -> String {
        let mut text = self.template.render();
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
        interrupted_after_completion: completed && recorded_intent,
    }
}
