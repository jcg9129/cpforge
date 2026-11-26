"""Independent stdlib oracle for the checked-in JSON and §3.7 arithmetic.

Run with python3 -B src/report/wire/fixtures/verify.py. Rust conformance tests
exercise the real encoders; this oracle can run without compiling the crate.
"""
import base64
import json
import hashlib
import re
import unicodedata
from pathlib import Path

ROOT = Path(__file__).parent
TOKEN = "x" * 32
U64 = str(2**64 - 1)
U32 = 2**32 - 1
I32 = -(2**31)
HEX = "f" * 64
TEXT = '"' * 129
SCORE = "9" * 14 + "." + "9" * 9


def encode(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def unique(pairs):
    value = {}
    for key, member in pairs:
        assert key not in value, key
        value[key] = member
    return value


def native(cap, binary=False):
    head = {"bytes_b64": base64.b64encode(b"\xff" * cap).decode()} if binary else {"utf8": '"' * cap}
    return dict(head=head, len=U64, truncated=True, sha256=HEX)


def cause():
    return dict(kind="capture_loss", stream=TOKEN, loss=TOKEN)


def cls():
    return dict(kind="infrastructure", causes=[cause()] * 8, causes_dropped=U32)


def signal():
    return dict(kind="signal", signal=I32, name=TOKEN, core_dumped=False)


def execution():
    return dict(kind="started", status=dict(kind="exited", code=I32, ntstatus=TOKEN), stop_trigger=dict(kind="output_budget", stream=TOKEN), enforced_at_us=U64, violations=[dict(kind="output_budget", stream=TOKEN, offset=U64)] * 3, cleanup=dict(kind="unverified", reason=TOKEN), kill_attempts=U32)


def stream():
    return dict(observed=U64, stored=U64, forwarded=U64, eof=False, complete=False, exceeded_at=U64, losses=[dict(kind="write_error", os_error=I32)] * 6, file="r99999/out/stdout")


def measurement():
    return dict(kind=TOKEN, value=U64, unit=TOKEN, scope=TOKEN, aggregation=TOKEN, completeness=TOKEN, method=TOKEN, reason=TOKEN)


def limit():
    return dict(kind=TOKEN, configured=U64, installed=U64, mechanism=TOKEN, outer=dict(kind="known", bytes=U64), normalised=U64, inherited_soft=U64, inherited_hard=U64)


def summary():
    return dict([("class", cls()), ("execution", execution()), ("timing", dict(setup_us=U64, wall_us=U64, observer=TOKEN)), ("deadline_us", U64), ("stdout", stream()), ("stderr", stream()), ("measurements", [measurement()] * 5), ("limits", [limit()] * 2), ("supervisor_errors", [TOKEN] * 8), ("notes", [TOKEN] * 16), ("notes_dropped", U32), ("cwd_overflow_bytes", U64)])


def persist():
    return dict(kind="io", os_error=I32, message=TEXT)


def reference():
    return dict(digest=HEX, path="r99999/record.json", error=persist())


def entry(binary=False):
    return dict([("index", U32), ("stage", TOKEN), ("role", dict(kind="hook", name="x" * 64, trigger=TOKEN)), ("coverage", dict(kind="not_started", reason=dict(kind="create_failed", api=TOKEN, os_error=I32))), ("run", "r99999"), ("program", native(256, binary)), ("spec", reference()), ("record", reference()), ("summary", summary()), ("class", None), ("invariant", TOKEN)])


def check():
    return dict(kind="failed", fail="wrong_answer", reason=TOKEN, diag=dict(reserved="x" * 2033))


def test(class_value, check_value):
    return dict([("id", "x" * 64), ("origin", TOKEN), ("answer", TOKEN), ("input_sha256", HEX), ("answer_sha256", HEX), ("executions", dict(first=U32, count=U32)), ("class", class_value), ("check", check_value), ("diagnostic", False), ("verdict", dict(kind="internal_error", invariant=TOKEN)), ("score", None), ("validation", None), ("judges", []), ("session", None), ("output_file", None)])


def aggregate():
    return dict(test="x" * 64, iterations=U32, succeeded=U32, wall_us=dict(min=U64, median=U64, max=U64))


def warning(binary=False):
    return dict(kind="recovery", paths=U32, first=native(128, binary), message=TEXT)


def hook():
    return dict(name="x" * 64, trigger=TOKEN, execution=U32, outcome=dict(kind="internal_error", invariant=TOKEN))


def capture(binary=False):
    return dict(check=dict(kind="unchecked", reasons=[TOKEN] * 4), sources=[native(128, binary)] * 16, sources_dropped=U32, support_files=[native(128, binary)] * 16, support_files_dropped=U32, counts=dict(sources=U32, support_files=U32), bytes=U64, lists_sha256=HEX, manifest="inputs/capture.json")


def build(binary=False):
    return dict(execution=U32, build_id="r99999", output_root="builds/r99999", result=dict(kind="failed", cause=dict(kind="missing_output", names=[native(128, binary)] * 16, names_dropped=U32)), artifacts=[], artifacts_dropped=U32)


def diagnostic(binary=False):
    return dict(file=native(256, binary), key=TEXT, line=U32, col=U32, message=TEXT)


def header(binary=False):
    value = json.loads((ROOT / "passing.json").read_bytes())
    value["persistence"] = dict(report=dict(kind="failed", error=persist()), durability=None)
    value["workspace"] = native(1024, binary)
    value["problem"] = dict(identity="local:" + "x" * 64, name="x" * 64, kind=TEXT, dir="x" * 75)
    compare = dict(kind="float")
    for key in ["abs", "rel", "rel_base", "combine", "ulps", "int_exact", "reserved_a", "reserved_b"]:
        compare[key] = dict(text="x" * 32, bits="f" * 16)
    value["target"] = dict(name="x" * 64, language="x" * 64, profile="x" * 64, build_free=False, compare=compare, check=dict(kind=TOKEN, answer=TOKEN, scoring=dict(kind=TOKEN, scale=SCORE, aggregation=TOKEN), display=TOKEN, interactive=dict(checking=TOKEN, mode=TOKEN, idle_us=U64, early_reader_close=False, interactor_allowance_us=U64, peer_allowance_us=U64, solution_seed=TOKEN, interactor_seed=TOKEN)), io=None, run_deadline=dict(us=U64, source=TOKEN), published_limits=dict(time_ms=U32, memory_bytes=U64), local_limits=[limit()] * 2)
    value["capture"] = capture(binary)
    value["fingerprints"] = dict(source=HEX, tests=HEX, judge=HEX)
    value["build"] = build(binary)
    role = json.loads((ROOT / "role_size_vectors.json").read_bytes())[2 if binary else 0]
    value["roles"] = [dict(role, role=name) for name in ["checker", "validator", "interactor"]]
    for key in ["executions", "tests", "aggregates"]:
        value[key] = []
    result = value["result"]
    result["verdict"] = result["primary_verdict"] = TOKEN
    result["partial"] = False
    result["cancellation"] = dict(requested_at_us=U64, source=TOKEN, phase=TOKEN)
    result["pre_delivery_exit"] = dict(code=I32, reraise=TOKEN)
    def fill(value):
        return {key: fill(member) for key, member in value.items()} if isinstance(value, dict) else U32
    result["counts"] = fill(result["counts"])
    result["internal"] = dict(invariant=TOKEN, locus=dict(kind="test", test="x" * 64), message=TEXT)
    result["score"] = dict(kind=TOKEN, aggregation=TOKEN, value=SCORE, max=SCORE, full=False)
    value["fact"] = dict(outcome=dict(kind="not_published", reason=TOKEN, message=TEXT), warnings=[TOKEN] * 2)
    value["hooks"] = [hook()] * 32
    value["hooks_dropped"] = U32
    value["warnings"] = [warning(binary)] * 24
    value["warnings_dropped"] = U32
    value["cpf_version"] = TOKEN
    value["command"] = TOKEN
    value["events"] = dict(dropped=U64)
    value["timing"] = dict(started_at="9999-12-31T23:59:59.999Z", finished_at="9999-12-31T23:59:59.999Z", duration_us=U64)
    return value


def decimal(value):
    assert isinstance(value, str) and re.fullmatch(r"0|[1-9][0-9]*", value)
    assert int(value) <= 2**64 - 1
    return int(value)


def i32(value):
    assert type(value) is int and -(2**31) <= value < 2**31


def portable_name(name):
    assert isinstance(name, str) and 1 <= len(name.encode()) <= 255
    assert name not in [".", ".."] and not name.endswith((".", " "))
    assert not any(c in '/\\:<>"|?*' or ord(c) < 32 or ord(c) == 127 for c in name)
    assert not name[:5].lower() == ".cpf-"
    stem = name.split(".")[0].rstrip(" ").upper()
    assert stem not in {"CON", "PRN", "AUX", "NUL"}
    assert name.upper() not in {"CONIN$", "CONOUT$"}
    assert not (stem[:3] in {"COM", "LPT"} and stem[3:] in set("123456789¹²³"))


def file_io_config(value):
    assert list(value) == ["input", "output", "stdin"]
    assert value["input"] is not None or value["output"] is not None
    for name in [value["input"], value["output"]]:
        if name is not None:
            portable_name(name)
    assert value["stdin"] == ("null_device" if value["input"] is not None else "test_input")
    if value["input"] is not None and value["output"] is not None:
        def key(name):
            return unicodedata.normalize("NFD", unicodedata.normalize("NFD", name).casefold())
        assert key(value["input"]) != key(value["output"])


REJECTIONS = dict(missing="output_missing", misnamed="output_misnamed",
                  not_regular="output_not_regular", unreadable="output_unreadable",
                  too_large="output_too_large", too_many_entries="output_dir_too_large")


def output_file(value):
    kind = value["kind"]
    members = {
        "collected": ["bytes", "sha256", "path"], "not_regular": ["type"],
        "unreadable": ["os_error"], "too_large": ["bytes", "budget"],
        "not_collected": ["reason", "os_error"], "missing": [], "misnamed": [],
        "too_many_entries": [], "not_attempted": [],
    }
    assert kind in members and list(value) == ["kind"] + members[kind]
    if kind == "collected":
        decimal(value["bytes"])
        assert re.fullmatch("[0-9a-f]{64}", value["sha256"])
        assert re.fullmatch(r"r[0-9]{5}/out/output", value["path"])
    elif kind == "not_regular":
        assert value["type"] in ["link", "directory", "other"]
    elif kind == "unreadable":
        i32(value["os_error"])
    elif kind == "too_large":
        assert decimal(value["bytes"]) > decimal(value["budget"])
    elif kind == "not_collected":
        assert value["reason"] in ["io", "timeout", "unstable"]
        if value["os_error"] is not None:
            assert value["reason"] == "io"
            i32(value["os_error"])


def budget_streams(value, is_test):
    assert list(value) == ["kind", "stream", "streams"] and value["kind"] == "output_budget"
    streams = value["streams"]
    order = ["stdout", "stderr"] + (["output_file"] if is_test else [])
    assert streams and streams == [s for s in order if s in streams]
    assert value["stream"] == streams[0]


def shaped_bound(planned, tests, aggregates, validator=False, judges=0, interactive=False, output=False):
    assert 0 <= planned <= 10_000 and 0 <= tests <= planned and 0 <= aggregates <= planned
    assert 0 <= judges <= 2 and not (interactive and output)
    if validator or judges or interactive:
        assert tests <= 1000
    per_test = 3072 + validator * 2560 + judges * 11264 + interactive * 19456 + output * 192
    return 131072 + planned * 9216 + per_test * tests + aggregates * 256


def verify_file_io():
    def fixture(name):
        return json.loads((ROOT / name).read_bytes(), object_pairs_hook=unique)
    configs = fixture("file_io_configs.json")
    for config in configs:
        file_io_config(config)
    outputs = fixture("file_io_variants.json")
    for output in outputs:
        output_file(output)
        assert len(encode(output)) + 15 <= 169
    checks = fixture("file_io_checks.json")
    for check_value in checks:
        assert list(check_value) == ["kind", "reason"] and check_value["kind"] == "output_rejected"
        assert check_value["reason"] in REJECTIONS.values()
    assert {v["reason"] for v in checks} == set(REJECTIONS.values())
    warnings = fixture("file_io_warnings.json")
    for warning in warnings:
        assert list(warning) == ["kind", "run", "detail", "message"]
        assert warning["kind"] == "io_scratch" and re.fullmatch(r"r[0-9]{5}", warning["run"])
        assert warning["detail"] in ["staged_input_kept", "output_kept", "partial_copy_kept", "not_measured"]
        assert len(encode(warning["message"])) == 260
        assert len(encode(warning)) <= 351
    report = fixture("file_io_report.json")
    assert list(report["target"]) == ["name", "language", "profile", "build_free", "compare", "check", "io", "run_deadline", "published_limits", "local_limits"]
    file_io_config(report["target"]["io"])
    output_file(report["tests"][0]["output_file"])
    assert list(report["tests"][0])[-1] == "output_file"
    for execution in report["executions"]:
        assert execution["summary"]["class"] == {"kind": "exited_zero"}
    assert len(encode(report)) <= shaped_bound(1, 1, 0, output=True)
    vectors = fixture("file_io_size_vectors.json")
    assert vectors["io"] == configs[2]
    test_value = test(signal(), check())
    test_value["check"]["diag"] = dict(reserved='"' * 1016 + "x")
    assert len(encode(test_value["check"]["diag"])) == 2048
    test_value["output_file"] = outputs[0]
    assert vectors["test"] == test_value
    budget_streams(vectors["class"], True)
    assert vectors["class"]["streams"] == ["stdout", "stderr", "output_file"]
    for name, value, bound in [("target.io (member)", vectors["io"], 562),
                               ("output_file (member)", outputs[0], 169),
                               ("test class (3 streams)", vectors["class"], 204),
                               ("test entry with output file", test_value, 3264)]:
        size = len(encode(value)) + (6 if name.startswith("target") else 15 if name.startswith("output_file") else 0)
        assert size <= bound
        print(f"file I/O {name}: {size} <= bound {bound}")
    for binary in [False, True]:
        head = header(binary)
        head["target"]["io"] = vectors["io"]
        actual = len(encode(head)) + 10_000 * sum(len(encode(v)) for v in [entry(binary), test_value, aggregate()]) + 3 * 9999
        bound = shaped_bound(10_000, 10_000, 10_000, output=True)
        assert actual <= bound == 127_491_072
        assert 2 * bound == 254_982_144
        print(f"file I/O {'base64' if binary else 'utf8'} 10000 full entries: {actual} <= bound {bound}; reservation {2 * bound}")
    assert shaped_bound(4004, 1000, 0, validator=True, judges=2, interactive=True) == 84_647_936
    assert 2 * shaped_bound(9382, 1000, 0, validator=True, judges=2, interactive=True) == 268_423_168
    assert 2 * shaped_bound(9383, 1000, 0, validator=True, judges=2, interactive=True) > 256 * 1024**2


def score(value):
    assert isinstance(value, str) and re.fullmatch(r"(0|[1-9][0-9]{0,13})(\.[0-9]{0,8}[1-9])?", value)
    whole, _, fraction = value.partition(".")
    return int(whole) * 10**9 + int((fraction + "0" * 9)[:9])


def verify_judging():
    def fixture(name):
        return json.loads((ROOT / name).read_bytes(), object_pairs_hook=unique)
    def summary(value, cap):
        assert list(value) == ["head", "len", "truncated", "sha256"]
        head = value["head"]
        data = head["utf8"].encode() if "utf8" in head else base64.b64decode(head["bytes_b64"], validate=True)
        assert len(data) <= cap and decimal(value["len"]) >= len(data)
        assert value["truncated"] == (decimal(value["len"]) > len(data))
        if not value["truncated"]:
            assert value["sha256"] == hashlib.sha256(data).hexdigest()
    configs = fixture("batch_check_configs.json")
    assert [c["display"] for c in configs] == ["cpf", "judge_cf", "judge_cf"]
    for config in configs:
        assert list(config) == ["kind", "answer", "scoring", "display", "interactive"]
        assert config["interactive"] is None
    checks = fixture("batch_checks.json")
    for check_value in checks:
        members = dict(rejected=["fail", "role"], scored=["raw", "scale", "role"], judge_failed=["class", "role", "reason"], unchecked=["reason"])
        assert list(check_value) == ["kind"] + members[check_value["kind"]]
    assert checks[-2:] == [dict(kind="unchecked", reason=r) for r in ["checking_unverified", "checking_interrupted"]]
    judges = fixture("batch_judges.json")
    for judge in judges:
        assert list(judge) == ["role", "execution", "profile", "result", "message", "stderr", "report"]
        assert judge["role"] in ["checker", "validator", "interactor"]
        assert judge["profile"] in ["simple", "testlib"]
        if judge["message"] is not None:
            summary(judge["message"], 4096)
        if judge["stderr"] is not None:
            summary(judge["stderr"], 1024)
        result = judge["result"]
        if result is not None:
            members = dict(passed=[], rejected=["fail"], scored=["raw", "scale"], judge_failed=["class", "reason"], unverified=[], not_read=["reason"])
            assert list(result) == ["kind"] + members[result["kind"]]
            if result["kind"] == "scored":
                assert score(result["raw"]) <= score(result["scale"])
        report = judge["report"]
        if report is not None:
            assert list(report) == ["path", "bytes", "encoding"]
            assert re.fullmatch(r"r[0-9]{5}/judge/report", report["path"])
            assert decimal(report["bytes"]) <= 65537
            assert report["encoding"] in ["utf_8", "windows_1251", None]
        assert len(encode(judge)) <= 11264
    validations = fixture("batch_validation.json")
    assert [v["outcome"] for v in validations] == ["valid", "invalid", "validation_failed", "validator_failed", "not_run"]
    for value in validations:
        assert list(value) == ["execution", "profile", "outcome", "stderr"]
        assert len(encode(value)) <= 2560
    scores = fixture("batch_scores.json")
    assert scores == [dict(kind="points", aggregation="sum", value="12.5", max="100", full=False), dict(kind="incomplete", aggregation="min", missing=1), dict(kind="no_tests")]
    report = fixture("batch_scored_report.json")
    assert report["result"]["score"] == scores[0]
    assert report["tests"][0]["score"] == "12.5"
    assert report["tests"][0]["judges"] == [judges[4]]
    assert report["tests"][0]["check"] == dict(kind="scored", raw="12.5", scale="100", role="checker")
    # Both caps are full simultaneously; quotes and backslashes force 2x escaping,
    # NUL forces base64. Every other member takes its longest value.
    classes = ["declared_fail", "protocol_unsupported", "report_invalid", "artefact_invalid", "crashed", "test_data_error"]
    reasons = ["declared_fail", "exit_unsupported", "outcome_unsupported", "points_not_enabled", "outside_authority", "report_missing", "report_not_regular", "report_unreadable", "report_too_large", "report_undecodable", "report_malformed", "report_schema", "points_invalid", "disagreement", "tout_missing", "tout_not_regular", "tout_unreadable", "tout_too_large", "signal", "exit_code", "wall_deadline", "output_budget", "dialogue_output", "peer_allowance_expired", "invalid_input", "validation_failed"]
    longest = lambda values: max(values, key=len)
    vectors = fixture("batch_size_vectors.json")
    assert len(vectors) == 6
    for index, char in enumerate(['"', "\\", "\0"]):
        def full_text(n):
            data = char.encode() * n
            head = dict(bytes_b64=base64.b64encode(data).decode()) if char == "\0" else dict(utf8=data.decode())
            return dict(head=head, len=U64, truncated=True, sha256=HEX)
        evidence = dict(role=longest(["checker", "validator", "interactor"]), execution=U32, profile=longest(["testlib", "simple"]), result=dict(kind="judge_failed", **{"class": longest(classes)}, reason=longest(reasons)), message=full_text(4096), stderr=full_text(1024), report=dict(path="r99999/judge/report", bytes="65536", encoding="windows_1251"))
        validation = dict(execution=U32, profile="testlib", outcome=longest(["valid", "invalid", "validation_failed", "validator_failed", "not_run"]), stderr=full_text(1024))
        assert [list(v) for v in vectors[2 * index:2 * index + 2]] == [list(evidence), list(validation)]
        assert [evidence, validation] == vectors[2 * index:2 * index + 2]
        assert len(encode(evidence)) <= 10928 and len(encode(validation)) <= 2321
        test_value = test(signal(), check())
        test_value.update(validation=validation, judges=[evidence])
        assert len(encode(test_value)) <= 3072 + 2560 + 11264
    assert fixture("judge_cf_display.json") == ["PE  presentation error", "WA  presentation error"]
    print("batch judging: result/validation/score vectors; full 4 KiB message + 1 KiB diagnostic fit T")


REPORT_MEMBERS = ["schema", "document", "platform", "outcome", "cpf_version", "command", "job", "workspace", "problem", "target", "capture", "fingerprints", "build", "roles", "executions", "tests", "aggregates", "result", "fact", "submission", "hooks", "hooks_dropped", "warnings", "warnings_dropped", "events", "timing", "refusal", "stress"]
TEST_MEMBERS = ["id", "origin", "answer", "input_sha256", "answer_sha256", "executions", "class", "check", "diagnostic", "verdict", "score", "validation", "judges", "session", "output_file"]
BY_VERDICT = ["passed", "wrong_answer", "unchecked", "checker_failed", "wall_deadline", "output_budget", "signal", "nonzero_exit", "cancelled", "infrastructure", "not_run", "internal_error", "presentation_error", "scored", "session_limit", "idle"]


def verify_roles():
    def fixture(name):
        return json.loads((ROOT / name).read_bytes(), object_pairs_hook=unique)
    def preview(v):
        assert list(v) == ["head", "len", "truncated", "sha256"]
        assert list(v["head"]) in [["utf8"], ["bytes_b64"]]
        if "utf8" in v["head"]:
            data = v["head"]["utf8"].encode()
            assert not any(ord(c) < 32 or ord(c) == 127 for c in v["head"]["utf8"])
        else:
            data = base64.b64decode(v["head"]["bytes_b64"], validate=True)
            assert base64.b64encode(data).decode() == v["head"]["bytes_b64"]
        assert len(data) <= 128 and decimal(v["len"]) >= len(data)
        assert v["truncated"] == (decimal(v["len"]) > len(data))
        assert re.fullmatch("[0-9a-f]{64}", v["sha256"])
        if not v["truncated"]:
            assert hashlib.sha256(data).hexdigest() == v["sha256"]
    def role(v):
        assert list(v) == ["role", "profile", "origin", "build", "capture", "provenance"]
        assert v["role"] in ["checker", "validator", "interactor", "generator", "brute"]
        assert (v["profile"] is not None) == (v["role"] not in ["generator", "brute"])
        origin = v["origin"]
        assert list(origin) == (["kind", "language", "entry"] if origin["kind"] == "built" else ["kind", "program"])
        for key in ["entry", "program"]:
            if origin.get(key) is not None:
                preview(origin[key])
        c = v["capture"]
        assert list(c) == ["check", "sources", "sources_dropped", "support_files", "support_files_dropped", "counts", "bytes", "lists_sha256", "content_sha256"]
        assert c["check"] in [dict(kind="checked"), dict(kind="unchecked", reasons=["shell_stage"])]
        for key in ["sources", "support_files"]:
            assert len(c[key]) <= 16 and c["counts"][key] == len(c[key]) + c[key + "_dropped"]
            for value in c[key]:
                preview(value)
        decimal(c["bytes"])
        for key in ["lists_sha256", "content_sha256"]:
            assert re.fullmatch("[0-9a-f]{64}", c[key])
        p = v["provenance"]
        if origin["kind"] == "prebuilt":
            assert p == dict(kind="declared") and v["build"] is None
        else:
            assert list(p) == ["kind", "header"] and p["kind"] == "built"
            h = p["header"]
            if h["kind"] == "candidate":
                assert list(h) == ["kind", "path", "sha256", "pinned"]
                preview(h["path"])
                assert type(h["pinned"]) is bool and re.fullmatch("[0-9a-f]{64}", h["sha256"])
            elif h["kind"] == "not_identified":
                assert list(h) == ["kind", "reason"] and h["reason"] in ["none", "ambiguous", "not_beside_sources", "angle_include"]
            else:
                assert h == dict(kind="not_applicable")
            assert (h["kind"] != "not_applicable") == (v["profile"] == "testlib")
        build_value = v["build"]
        if build_value is not None:
            assert list(build_value) == ["execution", "build_id", "output_root", "result", "artifacts", "artifacts_dropped"]
            assert build_value["output_root"] == "builds/" + build_value["build_id"]
            for a in build_value["artifacts"]:
                preview(a)
            if build_value["result"]["kind"] == "failed":
                assert build_value["artifacts"] == [] and build_value["artifacts_dropped"] == 0
                for name in build_value["result"]["cause"].get("names", []):
                    preview(name)
            assert len(encode(build_value)) <= 6664
        assert len(encode(v)) <= 21454
    provenance = fixture("role_provenance.json")
    assert [v["provenance"]["header"].get("reason", v["provenance"]["header"].get("pinned")) for v in provenance] == [True, False, "ambiguous", "not_beside_sources", "none", "angle_include"]
    vectors = fixture("role_size_vectors.json")
    for v in provenance + vectors:
        role(v)
    for v, char in zip(vectors, ['"', "\\", "\0"]):
        head = v["capture"]["sources"][0]["head"]
        data = head["utf8"].encode() if "utf8" in head else base64.b64decode(head["bytes_b64"])
        assert data == char.encode() * 128 and len(v["capture"]["sources"]) == 16
        assert len(v["capture"]["support_files"]) == 16
    warnings = fixture("judging_warnings.json")
    assert [v["detail"] for v in warnings] == ["staged_copy_kept", "transcript_kept", "not_identified", "not_pinned", "declared"]
    for v in warnings:
        assert list(v) == (["kind", "run", "detail", "message"] if v["kind"] == "judge_scratch" else ["kind", "role", "detail", "message"])
        assert len(encode(v["message"])) == 260 and len(encode(v)) <= 768
    configs = fixture("interactive_configs.json")
    assert len(configs) == 6
    for config in configs:
        assert list(config) == ["kind", "answer", "scoring", "display", "interactive"] and config["kind"] == "interactive"
        i = config["interactive"]
        assert list(i) == ["checking", "mode", "idle_us", "early_reader_close", "interactor_allowance_us", "peer_allowance_us", "solution_seed", "interactor_seed"]
        assert i["checking"] in ["interactor", "checker", "interactor_checker"] and i["mode"] in ["relay", "direct"]
        for key in ["idle_us", "interactor_allowance_us", "peer_allowance_us"]:
            if i[key] is not None:
                decimal(i[key])
        assert i["solution_seed"] == "env" and i["interactor_seed"] == "argv"
        assert len(encode(config)) <= 630
    events = fixture("judging_run_events.json")
    assert [e["stage"] for e in events] == ["build", "checker", "validator", "interactor"]
    for event in events:
        assert list(event) == ["event", "seq", "run", "execution", "stage", "role", "digest"]
        assert event["event"] == "run_started" and re.fullmatch("r[0-9]{5}", event["run"])
        decimal(event["seq"])
        assert re.fullmatch("[0-9a-f]{64}", event["digest"]) and len(encode(event)) < 400
        # A role build has stage build; each judge role has its own stage.
        expected = dict(role_build="build").get(event["role"]["kind"], event["role"]["kind"])
        assert event["stage"] == expected
    print("roles: provenance, builds, capture lists, warnings, interactive configuration and run events fit their bounds")


def job_report_members(report, path):
    """Appendix A order with every always-present member (report §3.6)."""
    members = list(report)
    assert members in [REPORT_MEMBERS, REPORT_MEMBERS + ["persistence"]], path
    if report["target"] is not None:
        assert list(report["target"]) == ["name", "language", "profile", "build_free", "compare", "check", "io", "run_deadline", "published_limits", "local_limits"], path
    if report["fingerprints"] is not None:
        assert list(report["fingerprints"]) == ["source", "tests", "judge"], path
    for value in report["tests"]:
        assert list(value) == TEST_MEMBERS, path
    assert list(report["result"])[-1] == "score", path
    assert list(report["result"]["counts"]["by_verdict"]) == BY_VERDICT, path


def verify():
    files = sorted(ROOT.glob("*.json"))
    for path in files:
        raw = path.read_bytes()
        value = json.loads(raw, object_pairs_hook=unique)
        assert encode(value) == raw, path
        if "schema" in value:
            assert raw.startswith(b'{"schema":1,"document":'), path
        if isinstance(value, dict) and value.get("document") == "status":
            for problem in value["problems"]:
                assert "findings" in problem, path
                for target in problem["targets"]:
                    assert list(target) == ["target", "fact_state", "comparison", "last_verdict", "finished_at", "capture_check", "judge"]
                    assert target["judge"] in ["none", "unchanged", "changed", "unknown"]
        for report in [value, value.get("report") if isinstance(value, dict) else None]:
            if isinstance(report, dict) and report.get("document") == "job_report":
                job_report_members(report, path)
    org = json.loads((ROOT / "status_invalid_fields.json").read_bytes())["problems"][0]
    assert org["tags"] == ["dp", "DP"] and org["user_status"] == "Working on it"
    assert [f["field"] for f in org["findings"]] == ["tags", "status"]
    notes = json.loads((ROOT / "notes.json").read_bytes())
    assert [n["state"] for n in notes] == ["created", "existing", "absent"]
    assert all(list(n) == ["schema", "document", "platform", "problem", "path", "state"] for n in notes)
    print(f"{len(files)} golden fixtures: compact UTF-8, unique members, canonical order/prefix")
    for binary in [False, True]:
        refusal = json.loads((ROOT / "refusal.json").read_bytes())
        head = header(binary)
        for key in ["job", "workspace", "problem", "result", "warnings", "warnings_dropped", "events", "timing", "persistence"]:
            refusal[key] = head[key]
        refusal["refusal"] = dict([("class", TOKEN), ("error", dict(kind=TOKEN, message=TEXT, path=native(256, binary))), ("diagnostics", [diagnostic(binary)] * 64), ("diagnostics_dropped", U32)])
        rows = [("Cause", cause(), 129, 129), ("class", cls(), 1122, 1122), ("non-infrastructure class", signal(), 169, 169), ("execution summary", execution(), 804, 804), ("stream", stream(), 608, 608), ("Measurement", measurement(), 346, 346), ("InstalledLimit", limit(), 360, 360), ("RunSummary", summary(), 6774, 7168), ("Ref", reference(), 451, 451), ("executions entry", entry(binary), 8851, 9216), ("tests entry with diagnostic", test(signal(), check()), 3002, 3072), ("tests entry infrastructure", test(cls(), dict(kind="passed")), 3002, 3072), ("aggregate", aggregate(), 224, 256), ("warning", warning(binary), 741, 768), ("hook", hook(), 244, 244), ("capture", capture(binary), 13311, 13311), ("build", build(binary), 6664, 6664), ("report header", head, 119638, 131072), ("Diagnostic", diagnostic(binary), 1237, 1237), ("refusal", refusal, 103966, 106496), ("run_finished event", dict(event="run_finished", seq=U64, run="r99999", summary=summary()), 6873, 7168), ("output_chunk event", dict(event="output_chunk", seq=U64, run="r99999", stream=TOKEN, bytes=dict(bytes_b64=base64.b64encode(b"\xff" * 4096).decode())), 5621, 7168)]
        # Every shape uses the same header allowance.
        assert len(encode(head)) <= 131072
        for name, value, total, bound in rows:
            actual = len(encode(value))
            assert actual <= total and actual <= bound, (name, binary, actual, total, bound)
            print(f"{'base64' if binary else 'utf8'} {name}: {actual} <= table {total} <= bound {bound}")
    errors = json.loads((ROOT / "persist_errors.json").read_bytes())
    assert errors == [dict(kind=kind) for kind in ["timeout", "queue_full", "cancelled", "worker_unavailable"]] + [dict(kind="io", os_error=None, message="publication failed")]
    assert all(len(encode(error)) <= 337 for error in errors)
    cwd = json.loads((ROOT / "cwd_unmeasured_summaries.json").read_bytes())
    base = json.loads((ROOT / "run_finished.json").read_bytes())["summary"]
    assert cwd == [dict(base, notes=["cwd_unmeasured_" + reason])
                   for reason in ["timeout", "io", "cancelled"]]
    assert all(len(value["notes"][0]) <= 32 and
               value["cwd_overflow_bytes"] is None for value in cwd)
    # Exact length of a complete serialization, counting all list commas.
    actual = len(encode(header())) + 10_000 * sum(len(encode(v)) for v in [entry(), test(signal(), check()), aggregate()]) + 3 * 9999
    table = 119638 + 12080 * 10_000
    bound = shaped_bound(10_000, 10_000, 10_000)
    assert actual <= table <= bound
    print(f"10000 full entries: {actual} <= table {table} <= reservation {bound}")
    verify_file_io()
    verify_judging()
    verify_roles()
    verify_judge_fingerprints()
    verify_judged_summaries()


def judge_fingerprint_vectors():
    """Config §9.1.3, independently of the Rust encoder and SHA writer."""
    def integer(v):
        return v.to_bytes(8, "little")

    def string(v):
        v = v.encode() if isinstance(v, str) else v
        return integer(len(v)) + v

    def strings(v):
        return integer(len(v)) + b"".join(map(string, v))

    def stage(argv):
        # argv tag, command, cwd=run, empty env_remove and env.
        return b"\0" + strings(argv) + b"\0\0" + integer(0) * 2

    def role(token, prebuilt, shell=False):
        result = b"\1" + string(token) + string("simple" if prebuilt else "testlib")
        result += bytes([int(prebuilt)]) + integer(2)
        result += string("a") + string(b"X") + string("z") + string(b"\0\xff")
        result += strings(["a"])
        if not prebuilt:
            result += strings([])
        result += strings(["z"])
        if not prebuilt:
            result += string("python") + b"\1" + string("E")
            if shell:
                settings = b"\1" + strings(["sh", "-c"]) + string("echo {}") + strings(["{entry}", "x"])
                settings += b"\0\0" + strings(["A", "Z"]) + integer(2)
                settings += string("OFF") + b"\0" + string("X") + b"\1" + string("v")
                result += b"\1" + settings * 2
            else:
                result += b"\1" + stage(["cc", "{sources}"]) + stage(["run", "{entry}"])
        if shell:
            return result + b"\0\0\1" + integer(12) + string("argv")
        result += b"\1" + integer(1_234_000) + b"\1" + integer(10) + b"\0"
        return result + string("none")

    check = dict(kind="checker", answer="file", scoring=dict(kind="pass_fail"), interactive=None)
    prefix = string("cpf-fp-judge-v1") + string(encode(check))
    cases = [
        ("absent", b"\0\0\0"),
        ("built", role("checker", False) + b"\0\0"),
        ("prebuilt", role("checker", True) + b"\0\0"),
        ("all_roles", role("checker", True) + role("validator", False) + role("interactor", False)),
    ]
    inputs = [(name, prefix + data) for name, data in cases]
    interactive = dict(kind="interactive", answer="none", scoring=dict(kind="points", scale="100", aggregation="min"),
                       interactive=dict(checking="interactor_checker", mode="relay", idle_us="1000000", early_reader_close=True,
                                        interactor_allowance_us="2000000", peer_allowance_us="3000000", solution_seed="env", interactor_seed="argv"))
    data = string("cpf-fp-judge-v1") + string(encode(interactive)) + b"\0\0" + role("interactor", False, shell=True)
    inputs.append(("interactive_shell", data))
    return [dict(name=name, input_hex=data.hex(), sha256=hashlib.sha256(data).hexdigest()) for name, data in inputs]


def verify_judge_fingerprints():
    vectors = json.loads((ROOT / "judge_fingerprints.json").read_bytes())
    assert vectors == judge_fingerprint_vectors()
    print("judge fingerprint: 5 independent full byte/SHA-256 vectors")


def judged_summary_vectors():
    templates = [f"2/3 passed ({p} checker)" for p in ["simple", "testlib"]]
    templates += [f"2/3 passed (interactive, checking {c})"
                  for c in ["interactor", "checker", "interactor_checker"]]
    templates += [f"12.5/100 points ({a}) over 3 tests" for a in ["sum", "min"]]
    templates += ["incomplete: 2 tests without a score"]
    return [t + " (inputs validated) (interrupted after completion)" for t in templates]


def verify_judged_summaries():
    vectors = json.loads((ROOT / "judged_summaries.json").read_bytes())
    assert vectors == judged_summary_vectors()
    assert all("accepted" not in v and "correct" not in v for v in vectors)
    print("judged summaries: checker, interactive, points, incomplete and validated-input suffix")


if __name__ == "__main__":
    verify()
