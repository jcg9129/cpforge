//! Problem-owned stress roles and budgets; no filesystem or process work.
use super::check::Validator;
use super::judging::{
    clean, entry_default, error, insert_default, millis, required, role, role_defaults, sourced,
};
use super::*;
use crate::report::{AnswerPolicy, CheckMode, Score, Scoring};

#[derive(Clone, Debug, PartialEq)]
pub struct StressConfig {
    pub generator: Box<RoleSpec>,
    pub brute: Option<Box<RoleSpec>>,
    pub iterations: Sourced<u64>,
    pub time_ms: Sourced<u64>,
    pub min_score: Option<Sourced<Score>>,
}

pub(super) fn defaults(root: &mut EffectiveNode) {
    let EffectiveValue::Table(root) = &mut root.value else {
        return;
    };
    let Some(n) = root.get_mut("stress") else {
        return;
    };
    insert_default(
        n,
        "iterations",
        RawValue::Integer(10_000),
        Some(TypedValue::Count(10_000)),
    );
    insert_default(
        n,
        "time_s",
        RawValue::Integer(60),
        Some(TypedValue::Milliseconds(60_000)),
    );
    if let EffectiveValue::Table(t) = &mut n.value {
        for key in ["generator", "brute"] {
            if let Some(r) = t.get_mut(key) {
                role_defaults(r, false);
            }
        }
    }
    let scale = root
        .get("check")
        .and_then(|c| c.get("scoring"))
        .and_then(|s| s.get("scale"))
        .cloned();
    if let Some(scale) = scale {
        if let Some(n) = root.get_mut("stress") {
            if let Some(TypedValue::Count(value)) = scale.typed {
                if let EffectiveValue::Table(t) = &mut n.value {
                    t.entry("min_score".into()).or_insert_with(|| {
                        let mut n = scale.clone();
                        n.name = "min_score".into();
                        n.origin.key_path = "stress.min_score".into();
                        n.value = EffectiveValue::Scalar(RawValue::String(value.to_string()));
                        n.typed = None;
                        n.provenance = Provenance::Derived {
                            inputs: vec![scale.provenance.clone()],
                        };
                        n
                    });
                }
            }
        }
    }
}

pub(super) fn inherit_entries(root: &mut EffectiveNode) {
    let entries: BTreeMap<_, _> = root
        .get("languages")
        .and_then(EffectiveNode::table)
        .into_iter()
        .flat_map(|t| t.iter())
        .filter_map(|(l, n)| n.get("entry").map(|e| (l.clone(), e.clone())))
        .collect();
    let EffectiveValue::Table(t) = &mut root.value else {
        return;
    };
    let Some(EffectiveNode {
        value: EffectiveValue::Table(t),
        ..
    }) = t.get_mut("stress")
    else {
        return;
    };
    for key in ["generator", "brute"] {
        if let Some(n) = t.get_mut(key).filter(|n| n.get("program").is_none()) {
            entry_default(n, &entries);
        }
    }
}

fn stress_role(v: &mut Validator<'_>, n: &EffectiveNode) -> Option<Box<RoleSpec>> {
    let mut internal = n.clone();
    insert_default(
        &mut internal,
        "profile",
        RawValue::String("simple".into()),
        None,
    );
    let mut role = role(v, &internal, false)?;
    match &mut role.input {
        RoleInput::Built(b) => b.run.stdout_policy = CapPolicy::Kill,
        RoleInput::Prebuilt(p) => p.run.stdout_policy = CapPolicy::Kill,
    }
    Some(Box::new(role))
}

pub(super) fn validate(
    v: &mut Validator<'_>,
    selection: &Selection,
    check: Option<&CheckConfig>,
) -> Option<Box<StressConfig>> {
    let Some(n) = v.merged.at(&["stress"]) else {
        // A missing table is reported only for a selected problem's record:
        // without one there is no problem.toml that could declare it.
        if selection.operation == Operation::Stress && selection.problem {
            v.error(
                stages::origin("stress.generator", "<selection>"),
                "stress.generator is required",
                None,
            );
        }
        return None;
    };
    let before = v.diagnostics.len();
    required(v, n, "generator");
    let generator = n.get("generator").and_then(|r| stress_role(v, r));
    let brute = n.get("brute").and_then(|r| stress_role(v, r));
    if let Some(check) = check {
        if n.get("brute").is_none()
            && !(check.kind.value == CheckMode::Checker && check.answer.value == AnswerPolicy::None)
        {
            required(v, n, "brute");
        }
        if check.kind.value == CheckMode::Interactive && check.answer.value != AnswerPolicy::None {
            error(
                v,
                n,
                "interactive stress needs answer = \"none\": a generator writes no answer",
                None,
            );
        }
        if let Some(score) = n.get("min_score").filter(|n| clean(n)) {
            let value = score.string().and_then(points);
            match check.report_config().scoring {
                Scoring::PassFail => error(v, score, "stress.min_score needs points scoring", None),
                Scoring::Points { scale, .. }
                    if value.is_none_or(|value| value.nanounits() == 0 || value > scale) =>
                {
                    error(
                        v,
                        score,
                        "stress.min_score must be positive and at most the scale",
                        None,
                    )
                }
                _ => {}
            }
        }
    }
    if let Some(kind) = v.merged.problem_kind.as_deref().filter(|k| clean(k)) {
        if kind
            .string()
            .is_some_and(|k| k != "batch" && k != "interactive")
        {
            error(
                v,
                n,
                "stress: problem kind unsupported in this version",
                Some(kind),
            );
        }
    }
    if before != v.diagnostics.len() || !clean(n) {
        return None;
    }
    Some(Box::new(StressConfig {
        generator: generator?,
        brute,
        iterations: sourced(
            n.get("iterations")?,
            match n.get("iterations")?.typed {
                Some(TypedValue::Count(n)) => n,
                _ => return None,
            },
        ),
        time_ms: millis(n.get("time_s")?),
        min_score: n.get("min_score").map(|n| {
            sourced(
                n,
                points(n.string().expect("validated score")).expect("validated score"),
            )
        }),
    }))
}

// Configuration accepts the points grammar, then stores canonical nanounits.
fn points(text: &str) -> Option<Score> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<i128>().ok()?
    };
    let fractional = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<i128>().ok()? * 10_i128.pow(9u32.checked_sub(fraction.len() as u32)?)
    };
    Score::new(whole.checked_mul(Score::UNIT)?.checked_add(fractional)?)
}
