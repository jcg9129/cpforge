use super::{judging, run, value::*};
use crate::jobs::engine::{self as e, stress::*};

pub(super) fn stress(v: &StressReport, report: &e::ReportData) -> Result<Value> {
    let (mode, start, budget) = match v.schedule {
        Schedule::Search {
            start_seed, budget, ..
        } => (
            "search",
            start_seed,
            obj([
                ("iterations", u(budget.iterations)),
                ("time_us", dur(budget.time)?),
            ]),
        ),
        Schedule::Replay { seed, .. } => ("replay", seed, Value::Null),
    };
    let last = match v.schedule {
        Schedule::Search { start_seed, .. } => v
            .counters
            .admitted
            .checked_sub(1)
            .and_then(|n| start_seed.checked_add(n)),
        Schedule::Replay { seed, .. } => (v.counters.admitted != 0).then_some(seed),
    };
    let mut classes = Vec::new();
    for (index, class) in IterationClass::ALL.into_iter().enumerate() {
        let Some(aggregate) = &v.classes[index] else {
            continue;
        };
        classes.push(obj([
            ("class", s(class.token())),
            ("count", u(v.counters.classes[index])),
            (
                "first",
                obj([
                    ("iteration", u(aggregate.first.0)),
                    ("seed", u(aggregate.first.1)),
                ]),
            ),
            (
                "diagnostics",
                checked_array(&aggregate.diagnostics, 16, |diag| {
                    Ok(obj([
                        ("iteration", u(diag.iteration)),
                        ("seed", u(diag.seed)),
                        ("role", s(diag.decision.role.map_or("none", |r| r.token()))),
                        ("reason", s(diag.decision.reason)),
                        ("source", s(diag.source)),
                        ("text", judging::text(&diag.text, 1024, false)?),
                    ]))
                })?,
            ),
            ("diagnostics_dropped", n(aggregate.diagnostics_dropped)),
        ]));
    }
    let mut timing = Vec::new();
    for (role, timing_row) in BatchRole::ALL.into_iter().zip(v.timing) {
        if timing_row.runs == 0 {
            continue;
        }
        timing.push(obj([
            ("role", s(role.token())),
            ("runs", u(timing_row.runs)),
            (
                "wall_us",
                timing_row.wall.map_or(Value::Null, |[min, max, total]| {
                    obj([("min", u(min)), ("max", u(max)), ("total", u(total))])
                }),
            ),
        ]));
    }
    let failures = checked_array(&v.failures, 20, |failure| {
        let saved = match failure.saved {
            SaveOutcome::NotSaved(reason) => obj([
                ("kind", s("not_saved")),
                (
                    "reason",
                    s(match reason {
                        NotSavedReason::Io => "io",
                        NotSavedReason::ResourcesHeld => "resources_held",
                    }),
                ),
            ]),
            SaveOutcome::Staged => obj([
                ("kind", s("not_promoted")),
                ("reason", s("io")),
                ("path", s(format!("failures/{}", failure.id))),
                ("message", Value::Null),
            ]),
            SaveOutcome::Promoted => {
                let problem = report
                    .problem
                    .as_ref()
                    .ok_or(Error("promoted failure needs problem"))?;
                let root = problem
                    .dir
                    .strip_prefix(&report.workspace)
                    .map_err(|_| Error("failure problem outside workspace"))?;
                let path = root
                    .join("stress")
                    .join("failures")
                    .join(failure.id.to_string());
                obj([
                    ("kind", s("promoted")),
                    ("path", rel(&path, 166)?),
                    ("binaries", s("none")),
                ])
            }
        };
        Ok(obj([
            ("id", s(failure.id.to_string())),
            ("iteration", u(failure.id.iteration)),
            ("seed", u(failure.seed)),
            ("class", s(failure.decision.class.token())),
            (
                "role",
                s(failure.decision.role.map_or("none", |r| r.token())),
            ),
            ("reason", s(failure.decision.reason)),
            ("saved", saved),
            (
                "runs",
                checked_array(&failure.runs, 8, |(role, class)| {
                    Ok(obj([
                        ("role", s(role.token())),
                        ("class", run::class(class)?),
                        (
                            "record",
                            failure
                                .records
                                .iter()
                                .find(|r| r.id.role == *role)
                                .map(|r| run::reference(&r.record))
                                .transpose()?
                                .unwrap_or(Value::Null),
                        ),
                    ]))
                })?,
            ),
        ]))
    })?;
    Ok(obj([
        ("mode", s(mode)),
        ("shape", s("batch")),
        ("seeds", obj([("start", u(start)), ("last", opt(last, u))])),
        ("budget", budget),
        (
            "stop",
            obj([
                ("reason", s(v.stop.reason.token())),
                ("iteration", opt(v.stop.iteration, u)),
            ]),
        ),
        (
            "iterations",
            obj([
                ("admitted", u(v.counters.admitted)),
                ("retired", u(v.counters.retired)),
                (
                    "saved",
                    n(u32::try_from(v.counters.saved)
                        .map_err(|_| Error("saved count exceeds cap"))?),
                ),
                (
                    "promoted",
                    n(u32::try_from(v.counters.promoted)
                        .map_err(|_| Error("promoted count exceeds cap"))?),
                ),
                ("not_saved", u(v.counters.not_saved)),
            ]),
        ),
        ("runs", u(v.counters.runs_started)),
        ("captured_bytes", u(v.counters.captured_bytes)),
        ("binaries_bytes", u(0)),
        ("classes", Value::Array(classes)),
        ("failures", failures),
        ("failures_dropped", u(v.counters.failures_dropped)),
        ("timing", Value::Array(timing)),
        ("replay", Value::Null),
        (
            "cleanup_failed",
            if v.cleanup_failed.paths.is_empty() && v.cleanup_failed.paths_dropped == 0 {
                Value::Null
            } else {
                obj([
                    (
                        "paths",
                        checked_array(&v.cleanup_failed.paths, 16, |path| {
                            Ok(super::native::nsum(
                                &super::native_bytes(path.as_os_str()),
                                256,
                            ))
                        })?,
                    ),
                    ("paths_dropped", n(v.cleanup_failed.paths_dropped)),
                ])
            },
        ),
    ]))
}
