//! Schema-1 search document, with counters and ordered incompleteness reasons.
use super::value::*;
use crate::search::Results;

pub(crate) fn encode_search(
    platform: &str,
    terms: &[String],
    notes: bool,
    result: &Results,
) -> Vec<u8> {
    encode(&obj([
        ("schema", n(1)),
        ("document", s("search")),
        ("platform", s(platform)),
        ("terms", array(terms, s)),
        ("notes", b(notes)),
        (
            "hits",
            array(&result.hits, |hit| {
                obj([
                    ("problem", s(&hit.problem)),
                    ("origin", s(hit.origin)),
                    ("fields", array(&hit.fields, |field| s(*field))),
                    ("notes_lines", array(&hit.notes_lines, |line| n(*line))),
                    ("notes_lines_dropped", n(hit.notes_lines_dropped)),
                    ("notes_truncated", b(hit.notes_truncated)),
                ])
            }),
        ),
        ("findings", array(&result.findings, super::ls::source)),
        ("incomplete", b(result.reasons.iter().any(|set| *set))),
        ("incomplete_reasons", array(result.reasons(), s)),
        ("entries", n(result.entries)),
        ("bytes_read", u(result.bytes_read)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Hit;

    #[test]
    fn search_document_vectors() {
        let mut result = Results {
            hits: vec![Hit {
                problem: "local:p".into(),
                origin: "active",
                fields: vec!["tags", "notes"],
                notes_lines: vec![1, 3],
                notes_lines_dropped: 0,
                notes_truncated: false,
            }],
            findings: Vec::new(),
            reasons: [false; 5],
            entries: 3,
            bytes_read: 96,
        };
        let terms = vec!["DP".into()];
        assert_eq!(
            encode_search("unix", &terms, true, &result),
            include_bytes!("fixtures/search.json")
        );
        result.hits[0].notes_truncated = true;
        result.findings.push(crate::search::source(
            &std::path::Path::new("problems")
                .join("broken")
                .join("problem.toml"),
            "metadata_malformed",
            "not TOML",
        ));
        result.reasons = [true; 5];
        result.entries = 100_000;
        result.bytes_read = 67_108_864;
        assert_eq!(
            encode_search("unix", &terms, true, &result),
            include_bytes!("fixtures/search_incomplete.json")
        );
    }
}
