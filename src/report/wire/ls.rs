//! Schema-1 active problem and contest listings.
use super::native::{native_bytes, ns};
use super::value::*;
use crate::organisation::{self, Finding};
use crate::organisation_read::{self as read, Contest, Problem, Source};

fn findings(values: &[Finding]) -> Value {
    array(values, |finding| {
        obj([
            ("field", s(finding.field)),
            ("reason", text(&finding.reason)),
        ])
    })
}
fn source(value: &Source) -> Value {
    let mut bytes = Vec::new();
    for component in value.path.components() {
        if !bytes.is_empty() {
            bytes.push(b'/');
        }
        bytes.extend(native_bytes(component.as_os_str()));
    }
    obj([
        ("path", ns(&bytes)),
        ("kind", s(value.kind)),
        ("field", opt(value.field, s)),
        ("message", text(&value.message)),
    ])
}
fn problem(value: &Problem, contests: &[Contest]) -> Value {
    let fields = value.fields();
    let identity = value.selected.identity.to_string();
    obj([
        ("problem", s(&identity)),
        ("origin", s("active")),
        (
            "title",
            opt(
                fields
                    .and_then(|fields| fields.get("title"))
                    .and_then(toml_edit::Item::as_str),
                s,
            ),
        ),
        (
            "tags",
            array(
                organisation::tags(fields.and_then(|fields| fields.get("tags")))
                    .unwrap_or_default(),
                s,
            ),
        ),
        (
            "user_status",
            opt(
                fields
                    .and_then(|fields| fields.get("status"))
                    .and_then(toml_edit::Item::as_str)
                    .filter(|status| crate::cli::status_name(status).is_ok()),
                s,
            ),
        ),
        ("effective_status", opt(value.status(), s)),
        ("created_at", opt(read::timestamp(fields, "created_at"), s)),
        ("updated_at", opt(read::timestamp(fields, "updated_at"), s)),
        ("solved_at", opt(read::timestamp(fields, "solved_at"), s)),
        (
            "archived_at",
            opt(read::timestamp(fields, "archived_at"), s),
        ),
        (
            "contests",
            array(
                contests
                    .iter()
                    .filter(|contest| contest.members().contains(&identity.as_str())),
                |contest| s(&contest.name),
            ),
        ),
        ("findings", findings(&organisation::findings(fields))),
    ])
}
fn contest(value: &Contest, problems: &[Problem]) -> Value {
    let (counts, unresolved, invalid) = read::counts(value, problems);
    let tags = organisation::tags(value.document.get("tags"));
    let invalid_tags = tags
        .as_ref()
        .err()
        .map(|reason| Finding {
            field: "tags",
            reason: reason.clone(),
        })
        .into_iter()
        .collect::<Vec<_>>();
    obj([
        ("contest", s(&value.name)),
        ("identity", opt(value.identity(), s)),
        ("tags", array(tags.unwrap_or_default(), s)),
        ("members", n(value.members().len() as u32)),
        (
            "counts",
            array(counts, |(status, count)| {
                obj([("status", s(status)), ("count", n(count))])
            }),
        ),
        ("unresolved", n(unresolved)),
        ("invalid", n(invalid)),
        ("findings", findings(&invalid_tags)),
    ])
}

pub(crate) fn encode_ls(
    platform: &str,
    contest_mode: bool,
    problems: &[&Problem],
    contests: &[Contest],
    all_problems: &[Problem],
    invalid: &[Source],
) -> Result<Vec<u8>> {
    Ok(encode(&document(
        "ls",
        platform,
        vec![
            (
                "kind",
                s(if contest_mode { "contests" } else { "problems" }),
            ),
            (
                "problems",
                array(problems, |value| problem(value, contests)),
            ),
            (
                "contests",
                array(contests.iter().filter(|_| contest_mode), |value| {
                    contest(value, all_problems)
                }),
            ),
            ("invalid", array(invalid, source)),
        ],
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::ProblemIdentity;
    use crate::selection;

    #[test]
    fn listing_document_vectors() {
        let problem = Problem {
            selected: selection::Problem {
                dir: std::path::PathBuf::new(),
                identity: ProblemIdentity::parse("local:p").unwrap(),
            },
            document:
                "schema = 1\n[problem]\nid = 'local:p'\ntags = ['DP']\ncreated_at = '2026-11-24'\n"
                    .parse()
                    .unwrap(),
        };
        let contest = Contest {
            name: "c".into(),
            document: "schema = 1\ntags = ['rated']\n[[problem]]\nid = 'local:p'\n[[problem]]\nid = 'local:missing'\n".parse().unwrap(),
        };
        assert_eq!(
            encode_ls("unix", false, &[&problem], &[contest], &[], &[]).unwrap(),
            include_bytes!("fixtures/ls.json")
        );
        let contest = Contest {
            name: "c".into(),
            document: "schema = 1\ntags = ['rated']\n[[problem]]\nid = 'local:p'\n[[problem]]\nid = 'local:missing'\n".parse().unwrap(),
        };
        assert_eq!(
            encode_ls("unix", true, &[], &[contest], &[problem], &[]).unwrap(),
            include_bytes!("fixtures/ls_contests.json")
        );
        let invalid = Problem {
            selected: selection::Problem {
                dir: std::path::PathBuf::new(),
                identity: ProblemIdentity::parse("local:p").unwrap(),
            },
            document: "[problem]\nid = 'local:p'\ntags = ['dp', 'DP']\nstatus = 'Working on it'\n"
                .parse()
                .unwrap(),
        };
        let sources = [
            Source {
                path: std::path::Path::new("problems")
                    .join("broken")
                    .join("problem.toml"),
                kind: "metadata_malformed",
                field: None,
                message: "malformed".into(),
            },
            Source {
                path: std::path::Path::new("problems")
                    .join("p")
                    .join("problem.toml"),
                kind: "invalid_field",
                field: Some("status"),
                message: "invalid status".into(),
            },
        ];
        assert_eq!(
            encode_ls("unix", false, &[&invalid], &[], &[], &sources).unwrap(),
            include_bytes!("fixtures/ls_invalid_fields.json")
        );
    }
}
