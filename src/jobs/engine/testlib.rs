//! Bounded APPES decoding and stages 2–4; support and agreement stay in report.
use crate::report::{
    JudgeEncoding, JudgeRead, JudgeReason as R, JudgeText, Score, Scoring, TestlibOutcome,
};

/// The wire score parser requires canonical strings. APPES accepts leading
/// zeroes and trailing fractional zeroes, so validate its grammar separately.
fn points_units(value: &str) -> Option<i128> {
    let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
    if integer.is_empty()
        || !integer.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || value.contains('.') && fraction.is_empty()
        || fraction.len() > 9
        || integer.trim_start_matches('0').len() + fraction.len() > 18
    {
        return None;
    }
    let integer = integer.trim_start_matches('0');
    let whole = if integer.is_empty() {
        0
    } else {
        integer.parse::<i128>().ok()?
    };
    let fractional = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<i128>().ok()? * 10_i128.pow(9 - fraction.len() as u32)
    };
    Some(whole * Score::UNIT + fractional)
}

fn points(value: &str, scoring: Scoring) -> Option<Score> {
    let units = points_units(value)?;
    match scoring {
        Scoring::Points { scale, .. } => Score::new(units).filter(|raw| *raw <= scale),
        // Stage 4 has no range under `pass_fail`: a grammatical value beyond
        // the score type still reaches stage 5 (`points_not_enabled`), where
        // its magnitude is never used.
        Scoring::PassFail => Score::new(units.min(Score::MAX)),
    }
}

// 0x80..0xbf; 0xc0..0xff are the contiguous Cyrillic alphabet. 0x98
// is unassigned and must fail decoding, rather than become a replacement.
const CP1251: [u16; 64] = [
    0x0402, 0x0403, 0x201a, 0x0453, 0x201e, 0x2026, 0x2020, 0x2021, 0x20ac, 0x2030, 0x0409, 0x2039,
    0x040a, 0x040c, 0x040b, 0x040f, 0x0452, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
    0, 0x2122, 0x0459, 0x203a, 0x045a, 0x045c, 0x045b, 0x045f, 0x00a0, 0x040e, 0x045e, 0x0408,
    0x00a4, 0x0490, 0x00a6, 0x00a7, 0x0401, 0x00a9, 0x0404, 0x00ab, 0x00ac, 0x00ad, 0x00ae, 0x0407,
    0x00b0, 0x00b1, 0x0406, 0x0456, 0x0491, 0x00b5, 0x00b6, 0x00b7, 0x0451, 0x2116, 0x0454, 0x00bb,
    0x0458, 0x0405, 0x0455, 0x0457,
];

fn xml_char(c: char) -> bool {
    matches!(c as u32, 9 | 10 | 13 | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}
fn space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}
fn name_start(c: char) -> bool {
    matches!(c, ':' | '_' | 'A'..='Z' | 'a'..='z')
        || matches!(c as u32, 0xc0..=0xd6 | 0xd8..=0xf6 | 0xf8..=0x2ff | 0x370..=0x37d | 0x37f..=0x1fff | 0x200c..=0x200d | 0x2070..=0x218f | 0x2c00..=0x2fef | 0x3001..=0xd7ff | 0xf900..=0xfdcf | 0xfdf0..=0xfffd | 0x10000..=0xeffff)
}
fn name(text: &mut &str) -> Result<String, R> {
    let mut chars = text.char_indices();
    if !chars.next().is_some_and(|(_, c)| name_start(c)) {
        return Err(R::ReportMalformed);
    }
    let end = chars
        .find(|(_, c)| {
            !(name_start(*c)
                || matches!(c, '-' | '.' | '0'..='9' | '\u{b7}')
                || matches!(*c as u32, 0x300..=0x36f | 0x203f..=0x2040))
        })
        .map_or(text.len(), |(i, _)| i);
    let result = text[..end].to_owned();
    *text = &text[end..];
    Ok(result)
}
fn entities(mut text: &str) -> Result<String, R> {
    let mut out = String::new();
    while let Some(index) = text.find('&') {
        out.push_str(&text[..index]);
        text = &text[index + 1..];
        let end = text.find(';').ok_or(R::ReportMalformed)?;
        let entity = &text[..end];
        let c = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = if let Some(hex) = entity.strip_prefix("#x") {
                    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(R::ReportMalformed);
                    }
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(decimal) = entity.strip_prefix('#') {
                    if decimal.is_empty() || !decimal.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(R::ReportMalformed);
                    }
                    decimal.parse().ok()
                } else {
                    None
                };
                number
                    .and_then(char::from_u32)
                    .filter(|c| xml_char(*c))
                    .ok_or(R::ReportMalformed)?
            }
        };
        out.push(c);
        text = &text[end + 1..];
    }
    out.push_str(text);
    Ok(out)
}
fn attributes(text: &mut &str) -> Result<Vec<(String, String)>, R> {
    let mut attrs: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        let trimmed = text.trim_start_matches(space);
        let separated = trimmed.len() != text.len();
        *text = trimmed;
        if text.is_empty() || text.starts_with(['>', '/', '?']) {
            return Ok(attrs);
        }
        if !separated {
            return Err(R::ReportMalformed);
        }
        let key = name(text)?;
        *text = text.trim_start_matches(space);
        *text = text
            .strip_prefix('=')
            .ok_or(R::ReportMalformed)?
            .trim_start_matches(space);
        let quote = text
            .chars()
            .next()
            .filter(|c| matches!(c, '\'' | '"'))
            .ok_or(R::ReportMalformed)?;
        *text = &text[1..];
        let end = text.find(quote).ok_or(R::ReportMalformed)?;
        let raw = &text[..end];
        // A set keeps duplicate detection linear in hostile attribute lists.
        if raw.contains('<') || !seen.insert(key.clone()) {
            return Err(R::ReportMalformed);
        }
        attrs.push((key, entities(&raw.replace(['\t', '\n', '\r'], " "))?));
        *text = &text[end + 1..];
    }
}

fn decode(bytes: &[u8]) -> Result<(String, JudgeEncoding), R> {
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let bytes = if bom { &bytes[3..] } else { bytes };
    let declaration = bytes
        .starts_with(b"<?xml")
        .then(|| bytes.windows(2).position(|b| b == b"?>"))
        .flatten();
    let encoding = if let Some(end) = declaration {
        let mut decl = std::str::from_utf8(&bytes[5..end]).map_err(|_| R::ReportUndecodable)?;
        // Malformed declaration syntax belongs to stage 3, after decoding.
        let attrs = attributes(&mut decl).unwrap_or_default();
        attrs
            .into_iter()
            .find(|(k, _)| k == "encoding")
            .map(|(_, v)| v)
    } else {
        None
    };
    let encoding = match encoding.as_deref() {
        None => JudgeEncoding::Utf8,
        Some(e) if e.eq_ignore_ascii_case("UTF-8") => JudgeEncoding::Utf8,
        // A UTF-8 byte order mark contradicts a windows-1251 declaration.
        Some(e) if e.eq_ignore_ascii_case("windows-1251") && !bom => JudgeEncoding::Windows1251,
        _ => return Err(R::ReportUndecodable),
    };
    let text = match encoding {
        JudgeEncoding::Utf8 => std::str::from_utf8(bytes)
            .map_err(|_| R::ReportUndecodable)?
            .to_owned(),
        JudgeEncoding::Windows1251 => bytes
            .iter()
            .map(|&b| {
                let code = match b {
                    0..=0x7f => u32::from(b),
                    0x80..=0xbf => u32::from(CP1251[usize::from(b - 0x80)]),
                    _ => u32::from(b) + 0x350,
                };
                if b == 0x98 {
                    Err(R::ReportUndecodable)
                } else {
                    char::from_u32(code).ok_or(R::ReportUndecodable)
                }
            })
            .collect::<Result<String, _>>()?,
    };
    Ok((text.replace("\r\n", "\n").replace('\r', "\n"), encoding))
}

struct Document {
    root: String,
    attrs: Vec<(String, String)>,
    children: bool,
    message: String,
}
// Iterative XML 1.0 scanner. Storage and work are bounded by the 64 KiB
// document; nesting never consumes the Rust call stack. DTDs and PIs are
// forbidden; predefined/numeric entities are decoded without expansion.
fn document(mut text: &str) -> Result<Document, R> {
    if !text.chars().all(xml_char) {
        return Err(R::ReportMalformed);
    }
    if let Some(decl) = text.strip_prefix("<?xml") {
        let end = decl.find("?>").ok_or(R::ReportMalformed)?;
        let mut body = &decl[..end];
        // XML declarations do not permit entity references in pseudo-attributes.
        if body.contains(['&', '<']) {
            return Err(R::ReportMalformed);
        }
        let attrs = attributes(&mut body)?;
        if !body.is_empty()
            || attrs
                .first()
                .is_none_or(|(k, v)| k != "version" || v != "1.0")
        {
            return Err(R::ReportMalformed);
        }
        let mut index = 1;
        if attrs.get(index).is_some_and(|(k, _)| k == "encoding") {
            index += 1;
        }
        if attrs
            .get(index)
            .is_some_and(|(k, v)| k == "standalone" && matches!(v.as_str(), "yes" | "no"))
        {
            index += 1;
        }
        if index != attrs.len() {
            return Err(R::ReportMalformed);
        }
        text = &decl[end + 2..];
    }
    let mut doc = Document {
        root: String::new(),
        attrs: Vec::new(),
        children: false,
        message: String::new(),
    };
    let mut stack = Vec::new();
    while !text.is_empty() {
        if let Some(rest) = text.strip_prefix("<!--") {
            let end = rest.find("-->").ok_or(R::ReportMalformed)?;
            if rest[..end].contains("--") || rest[..end].ends_with('-') {
                return Err(R::ReportMalformed);
            }
            text = &rest[end + 3..];
        } else if let Some(rest) = text.strip_prefix("<![CDATA[") {
            if stack.is_empty() {
                return Err(R::ReportMalformed);
            }
            let end = rest.find("]]>").ok_or(R::ReportMalformed)?;
            doc.message.push_str(&rest[..end]);
            text = &rest[end + 3..];
        } else if let Some(rest) = text.strip_prefix("</") {
            text = rest;
            let close = name(&mut text)?;
            if stack.pop().as_ref() != Some(&close) {
                return Err(R::ReportMalformed);
            }
            text = text
                .trim_start_matches(space)
                .strip_prefix('>')
                .ok_or(R::ReportMalformed)?;
        } else if let Some(rest) = text.strip_prefix('<') {
            text = rest;
            let tag = name(&mut text)?;
            let attrs = attributes(&mut text)?;
            if stack.is_empty() {
                if !doc.root.is_empty() {
                    return Err(R::ReportMalformed);
                }
                doc.root = tag.clone();
                doc.attrs = attrs;
            } else {
                doc.children = true;
            }
            if let Some(rest) = text.strip_prefix("/>") {
                text = rest;
            } else {
                text = text.strip_prefix('>').ok_or(R::ReportMalformed)?;
                stack.push(tag);
            }
        } else {
            let end = text.find('<').unwrap_or(text.len());
            let raw = &text[..end];
            if raw.contains("]]>") || stack.is_empty() && !raw.chars().all(space) {
                return Err(R::ReportMalformed);
            }
            let decoded = entities(raw)?;
            if !stack.is_empty() {
                doc.message.push_str(&decoded);
            }
            text = &text[end..];
        }
    }
    if !stack.is_empty() || doc.root.is_empty() {
        return Err(R::ReportMalformed);
    }
    Ok(doc)
}

fn schema(doc: &Document, scoring: Scoring) -> Result<JudgeRead, R> {
    if doc.root != "result" || doc.children {
        return Err(R::ReportSchema);
    }
    let attr = |key| {
        doc.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let outcome = match attr("outcome") {
        Some("accepted") => TestlibOutcome::Accepted,
        Some("wrong-answer") => TestlibOutcome::WrongAnswer,
        Some("presentation-error") => TestlibOutcome::PresentationError,
        Some("fail") => TestlibOutcome::Fail,
        Some("points") => TestlibOutcome::Points,
        Some("partially-correct" | "reserved" | "unexpected-eof") => TestlibOutcome::Unsupported,
        _ => return Err(R::ReportSchema),
    };
    let is_points = outcome == TestlibOutcome::Points;
    let partial = attr("outcome") == Some("partially-correct");
    if attr("points").is_some() != is_points
        || attr("pctype").is_some() != partial
        || doc.attrs.iter().any(|(k, _)| {
            k != "outcome" && !(is_points && k == "points" || partial && k == "pctype")
        })
    {
        return Err(R::ReportSchema);
    }
    let raw = if is_points {
        Some(points(attr("points").expect("schema points"), scoring).ok_or(R::PointsInvalid)?)
    } else {
        None
    };
    Ok(JudgeRead::Parsed { outcome, raw })
}

pub(super) fn parse(
    bytes: &[u8],
    scoring: Scoring,
) -> (JudgeRead, Option<JudgeEncoding>, Option<JudgeText>) {
    let (text, encoding) = match decode(bytes) {
        Ok(v) => v,
        Err(reason) => return (JudgeRead::Invalid(reason), None, None),
    };
    let doc = match document(&text) {
        Ok(v) => v,
        Err(reason) => return (JudgeRead::Invalid(reason), Some(encoding), None),
    };
    match schema(&doc, scoring) {
        Ok(read) => (read, Some(encoding), Some(JudgeText::message(&doc.message))),
        Err(reason) => (JudgeRead::Invalid(reason), Some(encoding), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scoring() -> Scoring {
        Scoring::Points {
            scale: Score::parse("1000000000").unwrap(),
            aggregation: crate::report::Aggregation::Sum,
        }
    }
    #[test]
    fn appes_points_grammar() {
        for value in [
            "0",
            "0000",
            "1000000000",
            "000000001.230000000",
            "999999999.999999999",
            "0.000000001",
        ] {
            assert!(points(value, scoring()).is_some(), "{value}");
        }
        for value in [
            "",
            "1.",
            ".1",
            "1e3",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1.0000000001",
            "1234567890123456789",
            "1000000000.000000001",
            "1.2.3",
        ] {
            assert!(points(value, scoring()).is_none(), "{value}");
        }
        assert!(points_units("123456789012345678").is_some());
        assert!(points_units("1234567890123456789").is_none());
        assert!(points_units("999999999.999999999").is_some());
        // Eighteen integer digits pass grammar but exceed the configured scale.
        assert!(points("123456789012345678", scoring()).is_none());
        assert_eq!(points("00012.500", scoring()).unwrap().to_string(), "12.5");
        // Under pass_fail stage 4 checks the grammar only; stage 5 decides.
        assert!(points("123456789012345678", Scoring::PassFail).is_some());
        assert!(points("1234567890123456789", Scoring::PassFail).is_none());
        assert!(points("1e3", Scoring::PassFail).is_none());
    }
    #[test]
    fn header_output_and_hostile_documents() {
        // The tested header's exact layout: spaces around `=`, trailing newline.
        let (read, encoding, message) = parse(
            b"<?xml version=\"1.0\" encoding=\"windows-1251\"?><result outcome = \"points\" points = \"12.5\">ok &lt;&amp;&gt; &quot;x&quot;</result>\n",
            scoring(),
        );
        assert_eq!(
            read,
            JudgeRead::Parsed {
                outcome: TestlibOutcome::Points,
                raw: Score::parse("12.5"),
            }
        );
        assert_eq!(encoding, Some(JudgeEncoding::Windows1251));
        assert_eq!(message.unwrap().head, b"ok <&> \"x\"");
        // Attribute order is free; a UTF-8 byte order mark is accepted.
        assert!(matches!(
            parse(
                b"\xef\xbb\xbf<result points='1' outcome='points'/>",
                scoring()
            ),
            (JudgeRead::Parsed { .. }, Some(JudgeEncoding::Utf8), Some(_))
        ));
        assert_eq!(
            parse(
                b"\xef\xbb\xbf<?xml version='1.0' encoding='windows-1251'?><result outcome='accepted'/>",
                scoring()
            )
            .0,
            JudgeRead::Invalid(R::ReportUndecodable)
        );
        // Truncation at any point never yields a parsed document.
        let whole = b"<?xml version='1.0' encoding='UTF-8'?><result outcome='accepted'>m</result>";
        for end in 0..whole.len() {
            let (read, _, message) = parse(&whole[..end], scoring());
            assert!(matches!(read, JudgeRead::Invalid(_)), "{end}");
            assert!(message.is_none());
        }
        // Thousands of attributes and deep nesting stay well-formedness errors
        // or schema errors, decided in linear work without recursion.
        let mut many = String::from("<result");
        for i in 0..7_000 {
            many.push_str(&format!(" a{i}=''"));
        }
        many.push_str(" a0=''/>");
        assert_eq!(
            parse(many.as_bytes(), scoring()).0,
            JudgeRead::Invalid(R::ReportMalformed)
        );
        let deep = format!(
            "<result outcome='accepted'>{}{}</result>",
            "<a>".repeat(9_000),
            "</a>".repeat(9_000)
        );
        assert_eq!(
            parse(deep.as_bytes(), scoring()).0,
            JudgeRead::Invalid(R::ReportSchema)
        );
        for text in [
            "<!DOCTYPE result [<!ENTITY e 'x'>]><result outcome='accepted'>&e;</result>",
            "<result outcome='accepted'><?pi?></result>",
            "<result outcome='accepted' points='1>",
            "<result outcome='accepted'>&#xD800;</result>",
            "<result outcome='accepted'>&amp</result>",
            "<result outcome='accepted'>]]></result>",
            " <?xml version='1.0'?><result outcome='accepted'/>",
            "<?xml version='1.0' standalone='yes' encoding='UTF-8'?><result outcome='accepted'/>",
        ] {
            assert_eq!(
                parse(text.as_bytes(), scoring()).0,
                JudgeRead::Invalid(R::ReportMalformed),
                "{text}"
            );
        }
    }
    #[test]
    fn xml_stages_and_entities() {
        for text in [
            "<result>",
            "<!DOCTYPE result><result/>",
            "<?other?><result/>",
            "<result>&nbsp;</result>",
            "<result a='1' a='2'/>",
            "<result>\0</result>",
            "<result>&#0;</result>",
            "<result/>extra",
        ] {
            assert_eq!(
                parse(text.as_bytes(), scoring()).0,
                JudgeRead::Invalid(R::ReportMalformed),
                "{text}"
            );
        }
        for text in [
            "<verdict/>",
            "<result outcome='unknown'/>",
            "<result outcome='accepted' points='1'/>",
            "<result outcome='points' points='1' pctype='2'/>",
            "<result outcome='accepted'><child/></result>",
        ] {
            assert_eq!(
                parse(text.as_bytes(), scoring()).0,
                JudgeRead::Invalid(R::ReportSchema),
                "{text}"
            );
        }
        let (_, _, message) = parse(
            b"<result outcome='accepted'>a&amp;&#x410;<![CDATA[<>]]></result>",
            scoring(),
        );
        assert_eq!(message.unwrap().head, "a&А<>".as_bytes());
        for bytes in [
            b"<?xml version='1.0' encoding='koi8-r'?><result/>".as_slice(),
            b"<result>\xff</result>",
            b"<?xml version='1.0' encoding='windows-1251'?><result>\x98</result>",
        ] {
            assert_eq!(
                parse(bytes, scoring()).0,
                JudgeRead::Invalid(R::ReportUndecodable)
            );
        }
    }
    #[test]
    fn cp1251_message_bound_and_hash() {
        use sha2::{Digest, Sha256};
        let mut bytes =
            b"<?xml version='1.0' encoding='WINDOWS-1251'?><result outcome='wrong-answer'>"
                .to_vec();
        bytes.extend(vec![0xdf; 3000]);
        bytes.extend_from_slice(b"</result>");
        let (_, encoding, message) = parse(&bytes, scoring());
        let message = message.unwrap();
        assert_eq!(encoding, Some(JudgeEncoding::Windows1251));
        assert_eq!(message.head, "Я".repeat(2048).as_bytes());
        assert_eq!(message.len, 6000);
        assert_eq!(
            message.sha256,
            <[u8; 32]>::from(Sha256::digest("Я".repeat(3000).as_bytes()))
        );
    }
}
