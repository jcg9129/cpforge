"""Independent stdlib oracle for the checked-in JSON and §3.7 arithmetic.

Run with python3 -B src/report/wire/fixtures/verify.py. Rust conformance tests
exercise the real encoders; this oracle can run without compiling the crate.
"""
import base64
import json
from pathlib import Path

ROOT = Path(__file__).parent
TOKEN = "x" * 32
U64 = str(2**64 - 1)
U32 = 2**32 - 1
I32 = -(2**31)
HEX = "f" * 64
TEXT = '"' * 129


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
    return dict([("id", "x" * 64), ("origin", TOKEN), ("answer", TOKEN), ("input_sha256", HEX), ("answer_sha256", HEX), ("executions", dict(first=U32, count=U32)), ("class", class_value), ("check", check_value), ("diagnostic", False), ("verdict", dict(kind="internal_error", invariant=TOKEN))])


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
    value["target"] = dict(name="x" * 64, language="x" * 64, profile="x" * 64, build_free=False, compare=compare, run_deadline=dict(us=U64, source=TOKEN), published_limits=dict(time_ms=U32, memory_bytes=U64), local_limits=[limit()] * 2)
    value["capture"] = capture(binary)
    value["fingerprints"] = dict(source=HEX, tests=HEX)
    value["build"] = build(binary)
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


def verify():
    files = sorted(ROOT.glob("*.json"))
    for path in files:
        raw = path.read_bytes()
        value = json.loads(raw, object_pairs_hook=unique)
        assert encode(value) == raw, path
        if "schema" in value:
            assert raw.startswith(b'{"schema":1,"document":'), path
    print(f"{len(files)} golden fixtures: compact UTF-8, unique members, canonical order/prefix")
    for binary in [False, True]:
        refusal = json.loads((ROOT / "refusal.json").read_bytes())
        head = header(binary)
        for key in ["job", "workspace", "problem", "result", "warnings", "warnings_dropped", "events", "timing", "persistence"]:
            refusal[key] = head[key]
        refusal["refusal"] = dict([("class", TOKEN), ("error", dict(kind=TOKEN, message=TEXT, path=native(256, binary))), ("diagnostics", [diagnostic(binary)] * 64), ("diagnostics_dropped", U32)])
        rows = [("Cause", cause(), 129, 129), ("class", cls(), 1122, 1122), ("non-infrastructure class", signal(), 169, 169), ("execution summary", execution(), 804, 804), ("stream", stream(), 608, 608), ("Measurement", measurement(), 346, 346), ("InstalledLimit", limit(), 360, 360), ("RunSummary", summary(), 6774, 7168), ("Ref", reference(), 451, 451), ("executions entry", entry(binary), 8851, 9216), ("tests entry with diagnostic", test(signal(), check()), 2868, 3072), ("tests entry infrastructure", test(cls(), dict(kind="passed")), 1723, 3072), ("aggregate", aggregate(), 224, 256), ("warning", warning(binary), 741, 768), ("hook", hook(), 244, 244), ("capture", capture(binary), 13311, 13311), ("build", build(binary), 6664, 6664), ("report header", head, 53645, 65536), ("Diagnostic", diagnostic(binary), 1237, 1237), ("refusal", refusal, 103966, 106496), ("run_finished event", dict(event="run_finished", seq=U64, run="r99999", summary=summary()), 6873, 7168), ("output_chunk event", dict(event="output_chunk", seq=U64, run="r99999", stream=TOKEN, bytes=dict(bytes_b64=base64.b64encode(b"\xff" * 4096).decode())), 5621, 7168)]
        for name, value, total, bound in rows:
            actual = len(encode(value))
            assert actual <= total and actual <= bound, (name, binary, actual, total, bound)
            print(f"{'base64' if binary else 'utf8'} {name}: {actual} <= table {total} <= bound {bound}")
    # Exact length of a complete serialization, counting all list commas.
    actual = len(encode(header())) + 10_000 * sum(len(encode(v)) for v in [entry(), test(signal(), check()), aggregate()]) + 3 * 9999
    table = 53645 + 11946 * 10_000
    bound = 65536 + 12288 * 10_000
    assert actual <= table <= bound
    print(f"10000 full entries: {actual} <= table {table} <= reservation {bound}")


if __name__ == "__main__":
    verify()
