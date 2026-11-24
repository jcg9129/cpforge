# Sites and HTTP contract

Status: v1, draft. Normative for the `http` and `sites` modules and the
site-fixture CI job. It covers the HTTP client and its configuration, the
request dispatcher, the cross-process request permit and cooldown, the
response cache, the canonical sample bytes, the AtCoder and CSES adapters,
bulk selections and the fixture corpus with its private delivery. It ends
where storage §5.8 starts: an adapter result (storage §5.8.1) or a
classified failure.

Related: design §2, §4.1, §6.3, §7, §10; `storage.md` §1, §2.1, §3, §4.5,
§5.8 (fetch transaction); `config.md` §1.1; `report.md` §5;
`../compare-spec.md` (what comparators do with the bytes published here).

MUST / MUST NOT / never are requirements. Numbers are *fixed* (not
configurable) unless this contract names a configuration key.

## 1. Scope and principles

- **Courtesy, not permission.** The rules below are a courtesy policy. They
  are not a claim that a site permits automated access.
- **No login.** cpf sends no credentials, no cookies (no cookie store),
  no `Authorization` header, and runs no script from a page. A page that
  needs a login is classified `auth_required` (§8.3).
- **HTTPS only**, `GET` only, to the hosts of an adapter (§8.4, §8.5).
- **Self-contained.** No external process, browser, helper or daemon takes
  part in fetching (design §2).
- **Untrusted input.** Every response byte is untrusted. Scraped data never
  becomes an executable command, a trusted checker or interactor path, a
  comparator, a local limit or any other judging setting. A tolerance or
  "any valid answer" statement is reported as a hint only (§8.2). Published
  judge limits go only into `problem.toml` tool fields (storage §5.8.2).
- **Capabilities per response** (design §7). Availability of each field,
  the statement language and the kind are decided for each response, never
  assumed for a site.
- **No attachments** in this version: an adapter result carries none, and no
  interactor, checker, image or archive is downloaded.

## 2. Client and dependencies

### 2.1 Dependency configuration

| Crate | Requirement | Configuration |
|---|---|---|
| `reqwest` | `=0.13.5` | `default-features = false`, features `rustls-no-provider`, `system-proxy` only |
| `rustls` (direct) | `0.23`, exact version pinned with the client | `default-features = false`, features `std`, `tls12`, `ring` |
| `rustls-platform-verifier` | `0.7`, exact version pinned with the client | default features |
| `scraper` (HTML5 parser beneath) | exact version pinned with the client | — |
| `flate2` | exact version pinned with the client | `default-features = false`, feature `rust_backend` |
| `brotli-decompressor` | exact version pinned with the client | default features |

- **Verified offline** against the local registry copies: `rustls` 0.23.46
  has the features `std`, `tls12`, `ring`; its default set contains
  `aws_lc_rs` and `prefer-post-quantum`, and `prefer-post-quantum` itself
  enables `aws_lc_rs`, so no crate in the graph may enable either.
  `rustls-platform-verifier` 0.7.1 depends on `rustls` with
  `default-features = false, features = ["std"]` and declares
  `rust-version = "1.85"`. `hyper-rustls` 0.27.10 enables `aws-lc-rs` by
  default and declares 1.85.
- **To be verified when the client is added**, against the published `reqwest` 0.13.5 manifest
  (not readable offline when this contract was written): the feature names
  `rustls-no-provider` and `system-proxy`; that `default-features = false`
  removes `http2`, `charset`, the default TLS provider and every
  decompression feature; that `rustls-no-provider` turns off the
  `hyper-rustls` default features; the builder methods named in §2.2. A
  renamed feature or method is corrected in this table, with the same
  intent, before the client is integrated.
- **Graph checks** (CI on the locked graph, every release target):
  `cargo tree -e features -i aws-lc-rs` and `-i aws-lc-sys` find nothing;
  no `reqwest` feature named `gzip`, `brotli`, `zstd`, `deflate`, `http2` or
  `charset` is enabled; MSRV 1.85 builds the graph for Linux, macOS and
  Windows targets.
- Ring avoids the aws-lc build path; it still compiles C and assembly, so a
  C toolchain remains a build requirement.

### 2.2 Client builder

One `reqwest::Client` per process, built with:

- the `rustls::ClientConfig` built by cpf (ring provider installed
  explicitly, TLS 1.2 and 1.3, certificate verification by
  `rustls-platform-verifier`, ALPN `http/1.1` only), passed as a
  preconfigured TLS backend;
- redirects off (`redirect::Policy::none()`); the client's retry policy
  off; no connection reuse (`pool_max_idle_per_host(0)`), so hyper never
  replays a request on a stale pooled connection; HTTP/1.1 only;
- `https_only(true)`; connect timeout 10 s (§3.4); no client-level total
  timeout (the dispatcher bounds each attempt, §3.4);
- proxies from the environment and the system (`system-proxy`); a proxy
  that requires authentication is unsupported (its 407 is `network`,
  §5). The permit (§4) governs requests to the origin host; a proxy's
  `CONNECT` is part of the same wire request;
- no default headers other than those of §3.1.

Consequence: one dispatcher call of the client is exactly one wire request
(one request written to one new connection), or a transport error.

Trust: the operating system store through the platform verifier; no
bundled root set. On Linux a host without CA certificates fails TLS
(`network{tls}`). Tests add a test root only through the `http-test-hooks`
feature (§10.1).

## 3. Dispatcher

### 3.1 Requests

- Every wire request is made by the dispatcher, never by an adapter.
  Adapters ask for a URL and receive a response (§3.5) or a classified
  failure.
- Headers sent, exactly: `Host`; `User-Agent: cpforge/<version>`;
  `Accept: text/html`; `Accept-Encoding: gzip, br`; on a conditional
  request `If-None-Match` and/or `If-Modified-Since` (§6). Nothing else.
- A **logical request** is one adapter request: its redirect hops and
  retries are wire requests of the same logical request.

### 3.2 Redirects

- The dispatcher follows a `301`, `302`, `303`, `307` or `308` response
  with a `Location` header itself, each hop as a new wire request under the
  permit (§4), always as `GET`.
- **Same-site**: the target (resolved against the request URL) must have
  scheme `https`, the request's exact host and the default port. Otherwise
  the response is `network{redirect_offsite}` and is not followed.
- The adapter's **login paths** (§8.4, §8.5) are checked before following:
  a target on a login path is `auth_required`, not followed.
- At most **5** hops are followed. A redirect received after 5 followed
  hops is `network{too_many_redirects}`: a chain costs at most 6 wire
  requests.
- A missing, empty, repeated or unparseable `Location` is
  `network{protocol}`. A redirect status without `Location`, or `300`,
  `304` without a conditional request, `305`, is `network{protocol}`.
- Redirect responses are never cached; their bodies are discarded unread.

### 3.3 Bodies and caps

- **Wire bytes** are the bytes of the message body as received (after
  chunked transfer decoding, before content decoding). The dispatcher reads
  them from the body stream and counts them before any decoder sees them.
- **Wire cap 8 MiB** (8 388 608 bytes): a `Content-Length` above the cap is
  refused before the body is read; a stream that delivers byte 8 388 609 is
  stopped there. Both → `network{too_large_wire}`; the connection is
  dropped.
- **Content decoding** by cpf: no `Content-Encoding` or `identity` →
  unchanged; `gzip` or `x-gzip` → multi-member gzip decoder; `br` →
  brotli decoder. Any other coding, or more than one coding, →
  `network{bad_encoding}`. A decoder error (bad header, CRC or size
  mismatch, trailing garbage) on a complete wire body →
  `network{bad_encoding}`.
- **Decoded cap 8 MiB**: the decoder output is bounded; producing byte
  8 388 609 stops decoding → `network{too_large_decoded}`. Peak memory per
  response is ≤ wire buffer + decoded cap + 64 KiB.
- Bodies of non-`200` responses that need classification (§5, §8.3) are
  read and decoded up to 64 KiB of decoded bytes, then the connection is
  dropped; their wire bytes still count against the wire cap.

### 3.4 Timeouts and budget

- **Connect timeout 10 s** (TCP and TLS handshake, through the proxy if
  any). **Attempt timeout 30 s**: from the start of the wire request to the
  last body byte, the connect time included. Either expiry is a
  `timeout` outcome (§5); the permit is released after the cooldown write.
- **Per-invocation network budget**: **200 wire requests** (every hop,
  retry, conditional request and enumeration counted) and **15 min of
  network time**, measured on the monotonic clock from the first permit
  acquisition attempt of the invocation, including lock waits, spacing
  waits, retry delays and transfers. Cache reads in `--offline` mode cost
  nothing.
- Before each wire request: a count of 200 already sent, or a remaining
  time shorter than the wait that request needs, → `network{budget}`
  without waiting and without a request. An attempt's timeout is
  `min(30 s, remaining time)`.
- An exhausted budget fails the logical request in progress and every later
  one of the invocation, each classified `network{budget}`.

### 3.5 Response to the adapter

A successful logical request yields: `final_url` (after hops), `status`
(`200`, or `304` revalidated from the cache), the decoded body (≤ 8 MiB),
`Content-Type`, `from_cache` (bool), `fetched_at` (UTC wall clock at the
last body byte, or at the `304`), `body_sha256` (of the decoded body), and
the counts of wire requests, hops and retries it used.

## 4. Permit and cooldown

### 4.1 Location and files

- **Permit directory** `<cache>/http/`, `<cache>` chosen by the first
  rule that applies: `CPF_CACHE_DIR` (non-empty, absolute); configuration
  key `http.cache_dir` (user layer only, §11.1); Linux `$XDG_CACHE_HOME/cpforge`
  when non-empty and absolute, else `$HOME/.cache/cpforge`; macOS
  `$HOME/Library/Caches/cpforge`; Windows `%LOCALAPPDATA%\cpforge`. A value
  that is relative, empty, or missing where needed → `network{permit}`,
  no request.
- Per host `h` (the adapter host, lowercase ASCII): `<h>.lock` (lock only,
  never read, written, unlinked or replaced) and `<h>.next` (state). The
  directory is created on first use.
- **State** `<h>.next`: TOML, `schema = 1`, `next_allowed_at_ms` (integer,
  Unix epoch milliseconds, wall clock). Written by atomic replace (storage
  §4.2: the temporary file is synced before the rename, and on Unix the
  directory after it).
- **Lock** primitive as the workspace lock (storage §4.5): a process-local
  mutex, then an exclusive OS lock (`flock`; Windows `LockFileEx` on
  `[0, 1)`), non-inheritable, with the identity recheck of storage §4.5.
  Acquisition polls every 50 ms, is cancellable, and gives up after 120 s
  (`network{permit_busy}`). A holder holds it for at most one attempt plus
  two state writes, so 120 s is above any healthy wait.
- **Spacing** (*fixed*): `atcoder.jp` 2 s, `cses.fi` 1 s. Concurrency 1 per
  host across every cpf process of the user that resolves the same permit
  directory.

### 4.2 Protocol of one wire request

1. Budget check (§3.4).
2. Acquire `<h>.lock`. An I/O error, an unsupported lock, or a known
   network filesystem (§4.4) → fail closed: `network{permit}`, no request.
3. Read `<h>.next`: absent → `next = 0`; malformed (bad TOML, missing or
   non-integer field) → `next = now + 60 s`, rewritten at step 6;
   `too_new` → `network{permit_state}`, never replaced; unreadable (I/O)
   → `network{permit}`. Either failure: release, no request.
4. **Clock clamp**: `next > now + spacing + 1 h` → `next = now + spacing`
   (the only decrease of `next`; a wall clock that jumped back).
5. `next − now > 60 s` → release, `rate_limited{retry_at = next}`, no
   request. Otherwise wait until `next` while holding the lock (monotonic
   sleep of the remaining duration; cancellable: cancellation sends nothing
   and ends the command with the cancellation status, report §5.3).
6. **Reserve**: write `next' = max(next, now + spacing)` and sync it
   (§4.1). A failed write → release, `network{permit}`, no request.
7. Send the one wire request (§2.2) and read the response headers and body
   (§3.3) **while holding the permit**.
8. When the outcome (§5) sets a cooldown or a retry delay `d`: write
   `max(current next, now + d)` and sync it.
9. Release the lock.

- **Monotonic extension.** Every write is `max(old, new)`; a shorter later
  cooldown never shortens a longer one. Only step 4 lowers `next`.
- **Crash windows.** A kill after step 6 leaves the reservation persisted:
  the next process waits for it even though nothing was sent. A kill
  between the response and step 8 loses that response's cooldown; the next
  process sends one request after the spacing and receives the cooldown
  again. A kill holding the lock releases it with the process (OS lock).
- **Clocks.** `next` is wall-clock time so that processes share it; waits
  use the monotonic clock. A wall clock that jumps forward shortens a wait
  (accepted); one that jumps back is clamped by step 4.

### 4.3 Refusal by a persisted cooldown

A later invocation that finds `next − now > 60 s` refuses before sending
(step 5), classified `rate_limited` with `retry_at`. Waits of at most 60 s
are spent in-process and count against the budget.

### 4.4 Filesystem

- The permit directory must be on a supported local filesystem (storage
  §1 list). A lock that succeeds does not prove that locks work on a
  network filesystem, so network and cloud-synced homes are outside the
  guarantee; the documented remedy is `CPF_CACHE_DIR` or `http.cache_dir`
  on a local disk.
- **Refused** (step 2, `network{permit_fs}`): Linux `statfs` `f_type`
  NFS (`0x6969`), SMB (`0x517B`), CIFS (`0xFF534D42`), SMB2
  (`0xFE534D42`), AFS (`0x5346414F`), Ceph (`0x00C36400`), 9P
  (`0x01021997`); macOS `f_fstypename` `nfs`, `smbfs`, `afpfs`, `webdav`;
  Windows `GetDriveTypeW` `DRIVE_REMOTE` or a UNC path. Any other type is
  used as it is (unverified, no warning).

## 5. Outcome table

Total over every wire result. At most **2 retries per logical request** in
total, across all its hops, so a logical request costs at most 8 wire
requests. `k` is the retry number (1 or 2); `s` the host spacing.

| Wire result | Retry | Cooldown written at step 8 | Class when not retried or retries exhausted |
|---|---|---|---|
| `200` | — | — | success (adapter, §8) |
| `304` to a conditional request | — | — | success from the cache (§6) |
| `301` `302` `303` `307` `308` | hop (§3.2), not a retry | — | §3.2 |
| connect error, connection reset or closed before or during the response, connect or attempt timeout | yes, after `s × 2^k` | `s × 2^k` | `network{transport}` / `network{timeout}` |
| `502`, `504` | yes, after `s × 2^k` | `s × 2^k` | `network{status}` |
| `503` without usable `Retry-After` | as `502` | as `502` | `network{status}` |
| `503` or `429` with usable `Retry-After` `v` ≤ 60 s | yes, after `max(v, s)` | `v` | `rate_limited` |
| `429` without usable `Retry-After` | no | 60 s | `rate_limited` |
| `429` or `503` with usable `Retry-After` `v` > 60 s | no | `min(v, 1 h)` | `rate_limited` |
| `401` | no | — | `auth_required` |
| `403`; any response with `cf-mitigated: challenge`; a challenge document (§8.3) | no | — | `challenge` (or `auth_required` for a recognized login page) |
| `404`, `410` | no | — | `not_found` |
| `407` | no | — | `network{proxy}` |
| TLS handshake or certificate failure, DNS failure | no | — | `network{tls}` / `network{dns}` |
| caps, encoding, redirect and protocol errors of §3.2, §3.3 | no | — | as named there |
| any other `1xx`–`5xx` status (`500`, `501`, other `2xx`, other `4xx`) | no | — | `network{status}` |

- **Usable `Retry-After`**: exactly one header field whose value, after
  optional whitespace is removed, is delta-seconds (1–10 ASCII digits, value
  ≤ 2 147 483 647) or an HTTP-date in one of the three RFC 9110 §5.6.7
  formats, year ≤ 9999, strictly later than now. A malformed, past, equal,
  overflowing or repeated value is unusable. `v` for a date is the date
  minus now, rounded up to whole seconds.
- "Retries exhausted" classifies by the **last** wire result's row.
- A retry is a new wire request of the same URL through §4.2 (its own
  permit, budget check and spacing).
- A response that cannot be read to its end because of a cap is never
  retried.

## 6. Cache and offline

- **Location**: `.cpforge/cache/http/` of the workspace (tool, disposable,
  storage §3; `cpf clean` removes it). One file per entry, named by the
  key: `<64 hex>.entry`.
- **Key**: SHA-256 of `GET <absolute URL>`, the URL after normalization by
  the adapter (§8.4, §8.5). Only `200` responses are stored, under the URL
  that returned them (a redirect target has its own entry).
- **Entry**: a header block (TOML, `schema = 1`: `url`, `stored_at`,
  `etag`, `last_modified`, `content_type`, the `Vary` field names with the
  request values cpf sent for them, `body_sha256`, `body_len`) followed by
  the decoded body. Written by temporary file and rename without sync; two
  writers race harmlessly (last rename wins, each complete).
- **Not stored**: `Cache-Control: no-store`, `Vary: *`, a response over
  the decoded cap.
- **Online use**: an entry is never used without revalidation. When the
  entry has an `ETag` or `Last-Modified`, the wire request is conditional;
  `304` → the stored body is the response (`status = 304`,
  `from_cache = true`); `200` → replaces the entry. An entry whose `Vary`
  values differ from the current request headers is not used.
- **Corrupt entry** (unparseable header, too new, body length or SHA-256
  mismatch, unreadable): ignored, the request is unconditional, and its
  `200` replaces the entry. Cache errors never fail a fetch.
- **`--offline`** (or `http.offline = true`): no permit, no wire request.
  Every logical request reads its entry; a missing or corrupt entry →
  `offline_miss`. An offline result may create a problem (the parsed bytes
  are the same as online); its provenance has `from_cache = true`.
- The cache holds page bodies locally only (design §7). Durable test bytes
  never depend on it (storage §5.8).

## 7. Canonical sample bytes

The bytes the adapter hands to storage §5.8.1 for each sample input and
answer, published unchanged by fetch and read unchanged by comparators
(compare spec §1.2). Applied in this order:

1. **Charset.** `Content-Type` `charset` parameter; else a `<meta
   charset>` or `<meta http-equiv="Content-Type">` within the first 1024
   bytes; else UTF-8. A UTF-8 BOM at the start overrides both and is
   removed. Accepted labels: the WHATWG labels of UTF-8 (`utf-8`, `utf8`,
   `unicode-1-1-utf-8`, `unicode11utf8`, `unicode20utf8`,
   `x-unicode20utf8`, ASCII case-insensitive). Any other charset →
   `unsupported{charset}`. **Any** malformed UTF-8 in the document →
   `parser_failure{utf8}`; nothing is decoded lossily.
2. **HTML5 parsing** (WHATWG tokenizer and tree construction, through
   `scraper`): input-stream CRLF and lone CR become LF; character
   references (named, decimal, hex) are decoded; a NUL character reference
   becomes U+FFFD; the single LF immediately after a `<pre>` start tag is
   dropped (only one).
3. **Text of a sample element**: its descendant text nodes in document
   order; each `<br>` contributes one LF; comments contribute nothing; other
   elements contribute their text.
4. **Line endings and spaces**: each CR LF pair becomes LF, then each
   remaining CR becomes LF (CRs from character references); each U+00A0
   becomes U+0020. All other characters, including tabs, trailing spaces
   and non-ASCII, are kept and encoded as UTF-8.
5. **Final LF**: a non-empty result that does not end with
   LF gets one LF appended. Nothing is removed: leading and trailing blank
   lines, trailing LFs and trailing spaces stay. An empty result stays empty
   (zero bytes).

- Design §6.3 "nothing is trimmed afterwards" holds: steps 1–5 are the
  documented extraction algorithm, and no later stage changes the bytes.
  The plan's "exactly one LF" is narrowed to step 5, because removing a
  trailing LF can change an input whose last line is empty; the appended LF
  is invisible to `tokens` and `lines` (compare spec §3, §4) and makes the
  final line a complete line for `exact`.
- Each canonical sample is ≤ the decoded cap; at most 64 pairs per page
  (storage §5.8.1; more → `parser_failure{too_many_samples}`).

## 8. Adapters

### 8.1 Interface

- `resolve(url) → ProblemRef | ContestRef | SectionRef | unsupported`:
  pure, no request; it normalizes the URL and derives the identity
  (storage §2.1) from the URL only, never from page text.
- `fetch_problem(ProblemRef) → Problem(parsed problem) | failure`;
  `fetch_contest(ContestRef) → Contest{identity, url, title?, members} |
  failure` (AtCoder); `fetch_section(SectionRef) → ProblemSet{site,
  section, members} | failure` (CSES). The shapes are those of storage
  §5.8.1.
- Every result carries **provenance** (`url`, `final_url`, `fetched_at`,
  `status`, `body_sha256`, `from_cache`), **warnings** (§8.2) and
  **availability** of `title`, `kind`, `limits.time_ms`,
  `limits.memory_bytes`. A field that cannot be parsed is unavailable and
  gives a warning; it never fails the fetch. A `title` over 1024 bytes is
  unavailable.
- `fetch_problem` issues exactly one logical request (the task page); an
  enumeration exactly one (the list page).

### 8.2 Recognition and warnings

- **Positive recognition.** Each adapter has a **marker table** in its
  module: required markers (statement container, title element, limit
  line) and, separately, login markers, challenge markers and
  not-available markers. A `200` document is a task page only when every
  required marker is present exactly once (the limit line at least once).
  Otherwise it is classified (§8.3) and nothing is returned: no parsed
  problem, so storage writes only the attempt status (storage §5.8.1,
  §5.8.9), and nothing is published.
- **Zero samples** are a result only when the page is recognized **and** no
  element of the document matches a sample heading pattern of either
  statement language. A recognized page with sample headings that do not
  pair (§8.4, §8.5) is `parser_failure`, never zero samples. A zero-sample
  result carries the warning `no_samples` (storage §5.8.4 creation).
- **Hints**, reported as warnings and never applied: `tolerance_hint`
  (absolute/relative error statement), `multiple_answers_hint`,
  `language_fallback` (§8.4), `limits_unparsed{field}`, `title_unparsed`.
- The marker tables are code, not configuration. Each entry has a
  synthetic fixture that removes it (→ `unrecognized`) and the private
  corpus (§9.3) holds the real structural variants.

### 8.3 Failure classes

| Class | Meaning | Retried |
|---|---|---|
| `auth_required` | login page (login markers), redirect to a login path, `401` | no |
| `challenge` | `403`, `cf-mitigated: challenge`, or challenge markers | no |
| `rate_limited` | §5 rows, or a persisted cooldown over 60 s (§4.3); carries `retry_at` | §5 |
| `not_found` | `404`, `410`, or the adapter's unknown-task document | no |
| `not_available` | recognized not-available markers (contest not started, task not public) | no |
| `unsupported` | URL form or task kind not supported, CSES contest, charset other than UTF-8 | no |
| `unrecognized` | a `200` document that is not a recognized page and matches no other row | no |
| `parser_failure` | recognized page with inconsistent content (pairing, duplicates, > 64 pairs, malformed UTF-8) | no |
| `network` | transport, TLS, timeout, caps, encoding, redirect, protocol, status, permit, budget; carries `reason` | §5 |
| `offline_miss` | `--offline` and no usable cache entry | no |

- Design §7's "challenge/rate limit" is two classes here; its "valid problem
  without samples" is the warning `no_samples`, not a failure.
- **Challenge detection** (resolves the plan's open question): only `403`,
  the `cf-mitigated: challenge` header, or a challenge marker of the
  adapter. Any other unrecognized `200` document is `unrecognized`. Both
  publish nothing; the difference is the message.
- Every class is a fetch failure of the problem (storage status `outcome =
  "failed"`, `reason = "<class>[: <reason>]"`) and the exit status `site`
  of the commands contract (proposed 12). Cancellation is not a class: report §5.3.

### 8.4 AtCoder

- **Host** `atcoder.jp`. Login path prefix `/login`.
- **URLs** (`http` accepted and normalized to `https`; trailing `/`, query
  and fragment dropped; lowercase only):
  `/contests/<c>/tasks/<t>` → task; `/contests/<c>` and
  `/contests/<c>/tasks` → contest. Anything else → `unsupported`.
- **Identity** `atcoder:<t>`, directory `atcoder-<t>`; contest
  `atcoder:<c>`, directory `contests/atcoder-<c>`. Native grammar for `<t>`
  and `<c>`: `[a-z0-9][a-z0-9_-]{0,55}`.
- **Canonical URL** of a task: `https://atcoder.jp/contests/<c>/tasks/<t>`
  with the contest of the request. A task fetched through another contest
  changes `url` by storage §5.8.4 step 6.
- **Language** (`sites.atcoder.language`, `en` or `ja`, default `en`). A
  page has an English section, a Japanese section, or (older pages) one
  unmarked section, which counts as Japanese. The configured section is
  used when present, else the other one with the warning
  `language_fallback`. Samples are taken from the chosen section only. The
  chosen section without sample headings while the other has some →
  `parser_failure{language_mismatch}`. Two sections of one language →
  `parser_failure{duplicate_section}`.
- **Pairing** within the chosen section: sample input headings and sample
  output headings carry numbers `n`; inputs and outputs must each be
  exactly `1..N` (no gap, no repeat) with the same `N`, and the sample
  element is the first `<pre>` after its heading within its section. Input
  `n` without output `n`, a repeat or a gap → `parser_failure{pairing}`.
  Pair `n` has ordinal `n`.
- **Title**: the title element's text, ASCII whitespace runs collapsed and
  trimmed, a leading `<label> - ` prefix removed.
- **Limits**: the limit line's time (`<number> sec`, `<number> ms`) and
  memory (`<number> KB|KiB|MB|MiB|GB|GiB`; K = 2^10, M = 2^20, G = 2^30,
  the site's MB convention). A time not a whole number of milliseconds, or
  an unparseable value → that field unavailable, `limits_unparsed`.
- **Kind**: `batch`; `interactive` when an interactive marker is present
  (then `samples = []`, no interactor fetched, warning
  `interactive_samples_skipped`); a task of a heuristic contest (contest id
  starting `ahc`) or any other kind marker → `unsupported{kind}`.
- **Contest enumeration**: one logical request to `/contests/<c>/tasks`.
  Recognized by the task table marker; each row gives a label (storage
  §5.2 label grammar, else `parser_failure{label}`) and a task link of the
  task URL form (else `parser_failure{member}`). Duplicate labels or tasks
  → `parser_failure`. More than 10 000 rows → `parser_failure{too_many}`.
  Not-started or private contests are `not_available` (markers or a
  redirect to the contest top or login).

### 8.5 CSES

- **Host** `cses.fi`. Login path prefix `/login`.
- **URLs** (normalized as in §8.4): `/problemset/task/<n>` and
  `/problemset/view/<n>` → task; `/problemset` and `/problemset/list` →
  the problem list. Any other path, including every contest path, →
  `unsupported` (gate 2 supports the problemset only).
- **Identity** `cses:<n>`, directory `cses-<n>`; native grammar
  `[1-9][0-9]{0,6}`. Canonical URL `https://cses.fi/problemset/task/<n>`.
- **Samples**: each example block (one or several numbered examples)
  holds an input element followed by its output element; pairs are taken
  in document order, ordinal from 1. An input without its output →
  `parser_failure{pairing}`.
- **Title**, **limits** (`<number> s`, memory as in §8.4), **kind**
  (`interactive` by marker, as in §8.4) as for AtCoder.
- **Unknown id**: the site's not-found document or `404` → `not_found`.
- **Section enumeration** (`--all-in-section` with a task URL): one logical
  request to `/problemset/`; the section is the one whose task list holds
  the task; members are that list in order, each label its native id, the
  `section` name the section heading's text (whitespace collapsed). The
  task is not in any section → `parser_failure{section}`.

### 8.6 Bulk selections

- A contest or section selection costs exactly one enumeration logical
  request. Storage §5.8.1 then applies `--only <labels>` and the 50-member
  bound **before any member request**: a refusal after the enumeration
  leaves exactly the enumeration's wire requests sent.
- Each member costs one logical request; all count against one
  invocation budget (§3.4). A budget exhausted during the member loop
  classifies the remaining members `network{budget}` (the command is
  partial).
- The enumeration page is not fetched again for its own task (a section
  task named on the command line is fetched once, as a member).

## 9. Fixtures

### 9.1 Tiers

| Tier | Where | Content | Runs |
|---|---|---|---|
| synthetic | public repo, `tests/fixtures/sites/<adapter>/<case>/` | written from scratch: minimal markup with the marker structure, no site text | every CI job |
| private excerpts | private notes repo, delivered as an encrypted bundle (§9.3) | reviewed minimal excerpts of real pages: markup skeleton and sample blocks, prose replaced by a placeholder, ≤ 64 KiB each, one provenance file per case (source URL, retrieval date, what was stripped, review date) | site-fixture job (§9.4) |
| live smoke | the sites, scheduled | real requests | advisory workflow (§10.10) |

### 9.2 Case format and replay transport

- A case directory holds `case.toml` and its body files:
  - `adapter`, `input` (the URL given to `resolve`), `selection`
    (`problem` | `contest` | `section`);
  - `[[exchange]]` in expected order: `url`, `conditional` (bool),
    `status`, `headers` (array of `[name, value]`), `body` (file name),
    `body_encoding` (`identity` | `gzip` | `br`, applied by the harness);
  - `[expect]`: either `failure = "<class>"` with optional `reason`, or the
    parsed result (`identity`, `url`, `title`, `kind`, `limits`, the
    availability of each, `warnings`, and per pair `input_sha256`,
    `answer_sha256`, `input_len`, `answer_len`; synthetic cases may give
    the bytes inline instead).
- The **replay transport** (feature `http-test-hooks`) replaces only the
  client call of §2.2: the dispatcher, permit (in a temporary permit
  directory with the test time scale), caps and decoders run as in
  production. A request that is not the next expected exchange fails the
  test; at the end the number of wire requests equals the number of
  exchanges.

### 9.3 Private bundle

- **Plaintext**: a deterministic tar (entries sorted, mtime 0, uid and
  gid 0, mode 0644, regular files only) of `manifest.toml` and the case
  directories.
- **Manifest** `manifest.toml`: `schema = 1`, `version` (`YYYY-MM-DD.N`),
  `cases` (number of `case.toml` files), `[[file]]` `path` and `sha256`
  for every other file of the tar.
- **Encryption**: `age` to one X25519 recipient; the identity (private
  key) is the Actions secret `CPF_SITE_FIXTURES_KEY`. The ciphertext is a
  release asset of the public repository named
  `site-fixtures-<version>.tar.age` (no third-party bytes in git history;
  deletable).
- **Version binding**: the tracked lock file
  `tests/fixtures/site-fixtures.lock` (TOML: `schema = 1`, `version`,
  `bundle_sha256` of the ciphertext, `plaintext_sha256` of the tar,
  `cases`) pins the reviewed version. Updating the corpus is a reviewed
  public commit that changes only this file and the tests that need it.

### 9.4 Delivery in CI

The `site-fixtures` job of `ci.yml` (Linux) runs on every push and pull
request, from the change that introduces it on:

1. **Fork PR** (`pull_request` whose head repository is not this
   repository): print exactly `private fixtures skipped: fork PR` and
   succeed. This is the only skip; every other event and branch requires
   the corpus.
2. Secret empty or absent → fail (`private fixture key missing`).
3. Download the ciphertext named by the lock; its SHA-256 ≠
   `bundle_sha256` → fail (`fixture bundle does not match lock`).
4. Decrypt into a fresh directory under `$RUNNER_TEMP` (mode 0700); error
   → fail. Tar SHA-256 ≠ `plaintext_sha256` → fail.
5. Extract; the extracted regular files must equal exactly the manifest's
   files plus `manifest.toml` and the `case.toml` files (no link, no
   absolute or `..` path, no extra file), each with its manifest hash.
   `version` and `cases` must equal the lock's. Any difference → fail.
6. Run the ignored test `sites_private` (`--include-ignored`, feature
   `http-test-hooks`) with `CPF_PRIVATE_SITE_FIXTURES` (the directory) and
   `CPF_PRIVATE_SITE_COUNT` (a count file). Without the directory variable
   the test fails. It runs every case through §9.2 and, after the last case
   passed, writes `executed = <n>`.
7. Count file missing or `n ≠ cases` → fail (`executed <n> of <cases>
   private cases`).
8. Always (also after a failure): delete the plaintext directory. No
   artifact upload of anything under it.

- **No plaintext in logs**: the harness prints case ids, classes, lengths,
  SHA-256 values and the first differing offset, never page or sample
  bytes; assertions on bytes compare hashes.

## 10. Tests

Integration tests drive the dispatcher or the real `cpf` binary built with
`http-test-hooks`, against the in-tree local server (a tokio listener with
TLS from a test CA, named site hosts mapped to it by the client's resolve
override, ALPN `http/1.1`) or the replay transport. No test except §10.10
touches the network. **Time scale**: integration tests run with the hook's
scale 1/10 (spacing 200 ms and 100 ms, connect 1 s, attempt 3 s; every
other duration of §3.4, §4 and §5, thresholds, caps and `Retry-After`
values included, scaled alike; the request count budget is not scaled)
unless a row says *production*; unit tests assert the
unscaled constants. **Allowances**: arrival gaps ≥ scaled spacing − 20 ms;
time bounds + 200 ms (×2 on macOS and Windows runners). Platforms: L M W
unless stated.

### 10.1 Client

- The `cargo tree` checks of §2.1 pass on the locked graph; MSRV 1.85
  builds for Linux, macOS and Windows targets.
- HTTPS to the local server with the test root: 1 request, `200`, body
  intact. Without the test root: `network{tls}`, 1 TLS attempt, 0 requests
  at the HTTP layer, no retry.
- An `http://` URL handed to the dispatcher directly: refused, 0
  connections.
- An h2-only server (ALPN `h2` only): `network{tls}`, exactly 1 connection
  attempt observed by the server.
- A sent request has exactly the headers of §3.1 (server records them).

### 10.2 Dispatcher

| Server script | Expected (server-counted wire requests, result) |
|---|---|
| 5 same-host redirects, then `200` | 6, success, `final_url` = last target |
| 6 same-host redirects | 6, `network{too_many_redirects}` |
| redirect to another host | 1, `network{redirect_offsite}`; 0 at the other host |
| redirect to `http://` same host | 1, `network{redirect_offsite}` |
| redirect to the login path | 1, `auth_required` |
| redirect without `Location` | 1, `network{protocol}` |
| connection closed after reading the request, 3 times | 3 (no hidden client replay), `network{transport}` |
| `Content-Length` 8 MiB + 1 | 1, `network{too_large_wire}`, ≤ 64 KiB of body read |
| chunked identity body of 8 MiB + 1 | 1, `network{too_large_wire}` |
| exactly 8 MiB identity body | 1, success, `body_len` = 8 388 608 |
| gzip of empty members, 8 MiB + 1 on the wire, decodes to 0 bytes | 1, `network{too_large_wire}` |
| gzip bomb: ≤ 64 KiB wire, decodes to 8 MiB + 1 | 1, `network{too_large_decoded}`; decoder output stops at 8 388 609 bytes |
| gzip decoding to exactly 8 MiB; `br` page | 1 each, success, bodies equal the originals |
| `Content-Encoding: deflate`; `gzip, br`; corrupt gzip CRC | 1 each, `network{bad_encoding}` |
| body dripped below the attempt timeout | 3, `network{timeout}`, elapsed ≈ 3 attempts + 2 delays |
| accept, read the request, never answer | 3, `network{timeout}` |
| TCP handshake never completes (blackhole hook) | 3 connection attempts, 0 requests, `network{timeout}` after 3 connect timeouts + 2 delays |
| budget: 50 section members, each answered by 1 redirect then `502` | exactly 200 requests (enumeration included), the remaining members `network{budget}` |
| time budget (hook: 3 s) against `502` with long delays | stops before 3 s + allowance, `network{budget}`, no request started after the budget |

### 10.3 Permit and cooldown

- **Two processes**, each issuing 10 logical requests to the same host
  (some answered by a redirect, some by one `502`): the server counts
  exactly the expected wire requests (hops and retries included); every
  pair of consecutive arrivals across both processes is ≥ spacing apart;
  no two requests overlap (each response finished before the next
  arrival). One run at *production* spacing for each host (2 requests per
  process; gaps ≥ 2 s and ≥ 1 s).
- **Reservation before sending**: process A killed at barrier
  `http.after_reserve` (state written, nothing sent): the server counts 0
  from A; process B's first arrival is ≥ A's reserved `next`.
- **Monotonic**: a `429` with 60 s (scaled) persisted, then a later
  response asking 5 s: the state file still holds the 60 s value.
- **Lock failure**: `<h>.lock` is a directory, or the permit directory is
  read-only: `network{permit}`, 0 requests. Unreadable state: same.
  Malformed state: one request after 60 s (scaled), state rewritten.
  `too_new` state: `network{permit_state}`, file unchanged.
- **Filesystem**: each refused type of §4.4 through the filesystem-type
  injection hook → `network{permit_fs}`, 0 requests; the CI filesystem
  (ext4 or tmpfs, APFS, NTFS) is accepted. (Closes the unowned "network
  cache dir" row.)
- **Clock clamp**: state `now + spacing + 1 h + 1 s` → clamped, the request
  is sent within spacing + allowance; state `now + spacing + 1 h − 1 s`
  → `rate_limited`, 0 requests, state unchanged.
- **Persisted cooldown refusal** (unscaled values, run scaled): state
  `now + 61 s` → `rate_limited` with `retry_at`, 0 requests; `now + 59 s`
  → waits, then exactly 1 request, sent no earlier than the state value.
- **Cancellation while waiting** (state `now + 30 s` scaled, cancel after
  1 s): 0 requests; the command ends within 1 s + allowance with the
  cancellation status (6 on Windows and for API cancellation; the
  re-raised signal on Unix, report §5.3).
- **Lock busy**: another process holds the lock past 120 s (scaled 12 s):
  `network{permit_busy}`, 0 requests from the waiter.

### 10.4 Outcome table

One test per row of §5, each with the server-counted wire requests, the
class and the state value read back after the command:

| Script | Requests | Class | Persisted `next − send time` |
|---|---|---|---|
| `502`, `502`, `200` | 3 | success | ≥ `s × 4` after the second |
| `502` × 3; `504` × 3 | 3 | `network{status}` | `s × 4` |
| `503` no `Retry-After`, then `200` | 2 | success | `s × 2` |
| `503` `Retry-After: 5`, then `200` | 2 | success, second arrival ≥ 5 s (scaled) later | 5 s |
| `429` `Retry-After: 5` × 3 | 3 | `rate_limited` | 5 s |
| `429` without `Retry-After` | 1 | `rate_limited` | 60 s |
| `429` `Retry-After: 120`; `503` `Retry-After: 120` | 1 | `rate_limited` | 120 s |
| `429` `Retry-After: 99999` | 1 | `rate_limited` | 1 h |
| `429` with an HTTP-date 30 s ahead (each of the 3 date formats) | then retried: 2 with a `200` | success | 30 s |
| `429` with a malformed value, a past date, `Retry-After: 99999999999` (11 digits), two fields | 1 each | `rate_limited` | 60 s |
| `503` with each unusable value above, then `200` | 2 | success | `s × 2` |
| `401`; `403`; `200` with `cf-mitigated: challenge`; `404`; `410`; `407`; `500`; `501`; `204` | 1 each | per §5 | `s` only |
| connect refused × 3 | 3 connection attempts | `network{transport}` | `s × 4` |

### 10.5 Cache and offline

- `200` with `ETag`: stored; the next online fetch sends `If-None-Match`,
  gets `304`: 1 request, body equal to the stored one, `from_cache =
  true`, `status = 304`.
- `Last-Modified` only → `If-Modified-Since`; no validators → an
  unconditional request.
- Corrupt entries (body byte flipped, truncated header, `schema = 99`):
  unconditional request, entry replaced, fetch succeeds.
- `no-store`, `Vary: *`: no entry file. `Vary: Accept-Encoding` with a
  stored value different from the current one: unconditional request.
- `--offline` with an entry: 0 requests, 0 permit files touched, result
  equal to the online result; without an entry: `offline_miss`, 0
  requests. `--offline` creating a new problem from a cache entry: the
  published samples equal the online run's bytes.

### 10.6 Canonical bytes

Golden table, each row a synthetic recognized page with one sample `<pre>`
(shown as its source) and the expected bytes:

| Source of the sample element | Bytes |
|---|---|
| `<pre>1 2\n</pre>` | `1 2\n` |
| `<pre>\n1 2\n</pre>` | `1 2\n` |
| `<pre>\n\n1\n</pre>` | `\n1\n` |
| `<pre>1 2</pre>` | `1 2\n` (appended) |
| `<pre>1\n\n</pre>` | `1\n\n` (kept) |
| `<pre>a&lt;b&amp;c&#x41;&#66;&nbsp;d</pre>` | `a<b&cAB d\n` |
| `<pre>1<br>2<br/>3</pre>` | `1\n2\n3\n` |
| source bytes `<pre>1\r\n2\r3</pre>` | `1\n2\n3\n` |
| `<pre>1&#13;&#10;2&#13;3</pre>` | `1\n2\n3\n` |
| `<pre>1 \n2\t </pre>` | `1 \n2\t \n` (trailing spaces kept) |
| `<pre>é</pre>` | `c3 a9 0a` |
| raw U+00A0 (`c2 a0`) between `1` and `2` | `1 2\n` |
| `<pre></pre>`; `<pre>\n</pre>` | empty (0 bytes) |
| `<pre> </pre>` | ` \n` |
| `<pre><var>N</var> <span>3</span></pre>` | `N 3\n` |
| `<pre>1<!-- c -->2</pre>` | `12\n` |
| `<pre>&#0;</pre>` | `ef bf bd 0a` |

- Malformed UTF-8 (byte `ff`) outside every sample → `parser_failure{utf8}`,
  nothing published. A UTF-8 BOM → removed, samples unchanged.
- `charset=iso-8859-1` header; no header and `<meta charset="windows-1252">`
  → `unsupported{charset}`. Header `utf-8` with that meta → accepted.
- Through `cpf fetch`: the published files' bytes equal the table's
  bytes exactly.

### 10.7 AtCoder

Synthetic fixtures (public) and private excerpts (§9.3) for: modern task
with English and Japanese sections (samples from the English section
only, N pairs, not 2N); `ja` configured → Japanese section; Japanese-only
old page → `language_fallback` and its samples; ARC and AGC tasks; zero
samples on a recognized page → 0 pairs and `no_samples`; interactive
marker → `kind = "interactive"`, 0 pairs, `interactive_samples_skipped`;
"any valid answer" → `multiple_answers_hint`, comparator unchanged in
`problem.toml`; tolerance statement → `tolerance_hint`, comparator
unchanged. Failures: input 3 without output 3 → `parser_failure{pairing}`;
duplicate section → `parser_failure{duplicate_section}`; chosen language
without samples while the other has them →
`parser_failure{language_mismatch}`; login page → `auth_required`; `404` →
`not_found`; challenge → `challenge`; contest not started →
`not_available`; each required marker removed in turn → `unrecognized`;
limit values unparseable → limits unavailable, `limits_unparsed`, samples
kept. Contest enumeration: labels and identities in order; a contest of
51 tasks → `selection_too_large{51}` with exactly 1 enumeration request and
0 member requests counted; a URL of an unsupported form →
`unsupported`, 0 requests; `ahc` contest task → `unsupported{kind}`.
For each failing case run through `cpf fetch`: the tree under `problems/`
and `contests/` is unchanged (hash of the listing and bytes), no entry is
left in `.cpforge/downloads/`, `status.toml` records `failed` with the
class, and the exit status is the `site` status.

### 10.8 CSES

Ordinary task (1 pair); several examples (pairs in order); recognized
task without example → 0 pairs, `no_samples`; interactive → `kind =
"interactive"`, 0 pairs; login redirect → `auth_required`, 1 request;
unknown id → `not_found`; section enumeration → members in list order,
labels equal native ids, `section` named; task in no section →
`parser_failure{section}`; any contest URL → `unsupported`, 0 requests;
`/problemset/view/<n>` and `/problemset/task/<n>/` normalize to the
canonical URL. Bulk: a 51-member section → refused after exactly 1 request.

### 10.9 Private delivery

- A same-repo branch run: the job decrypts the bundle, every check of §9.4
  passes, and `executed` equals `cases` of the lock and the manifest.
- Test workflow without the secret → the job fails at step 2, it does not
  skip.
- A ciphertext of another version → fails at step 3; a lock with the
  right ciphertext hash but another `version` → fails at step 5.
- One case disabled in the harness → fails at step 7 (`executed n − 1 of
  n`).
- A fork PR → the exact skip line, the job succeeds, every other job runs.
- A scan of the job log for marker strings planted in the private cases
  finds none; the workflow uploads no artifact from `$RUNNER_TEMP`.

### 10.10 Live smoke (advisory)

- Workflow `live-smoke.yml`: weekly schedule and manual dispatch,
  concurrency group `live-smoke` (no parallel runs), never a required check.
- It fetches one AtCoder task and one CSES task (each with ≥ 1 sample)
  into a fresh workspace and permit directory, with the transport log
  (`CPF_HTTP_LOG`, one JSON line per wire request: time, host, path,
  status, wire bytes, hop or retry; never bodies).
- Pass: ≥ 1 wire request per adapter host, each page positively recognized
  and its samples parsed (≥ 1 pair each), and the log's line count (hops
  and retries included) ≤ 20.
- A classified failure opens one issue labelled `live-smoke`, or comments
  on the open one (deduplicated by title).

## 11. Amendments to other documents

### 11.1 Configuration (config §1.1)

| Key | Type | Layers | Notes |
|---|---|---|---|
| `sites.atcoder.language` | `"en"` or `"ja"` | 1, 2 | default `"en"`; §8.4 |
| `http.offline` | boolean | 1, 2 | default `false`; `--offline` sets it for one invocation; §6 |
| `http.cache_dir` | absolute path string | 1 | permit directory base (§4.1); `CPF_CACHE_DIR` wins. Not layer 2: a workspace value would split the per-host permit between workspaces |

### 11.2 Report and exit statuses

- Exit: every class of §8.3 → the `site` status (proposed 12, report §5.1
  reserved range); cancellation → report §5.3.
- **`fetch` document** (schema 1, draft, completed with the report and commands amendments): `schema`,
  `kind = "fetch"`, `selection` (`kind`, `identity`, `url`, member count),
  `problems[]` (`identity`, storage status `outcome`, `failure` with
  `class`, `reason`, `http_status`, `retry_at` when present, `provenance`
  of §8.1, `warnings`, the storage findings, `txn`), `network`
  (`wire_requests`, `redirects`, `retries`, `revalidated`, `offline`,
  `elapsed_ms`, `budget_requests = 200`, `budget_seconds = 900`).

### 11.3 Storage and design

- Storage §3: `.cpforge/cache/http/` is the disposable HTTP cache of §6
  (within the reserved `cache/` tree).
- Storage §2.1: the native-id grammars of §8.4 and §8.5.
- Storage §5.8.10 (optional, proposed): a `page_sha256` field in
  `[history]` would make the raw-response hash durable (design §6.3
  "raw response provenance kept separately"); without it the hash is in
  the `fetch` document and the disposable cache only.
- Design §7: the failure classes of §8.3, `no_samples` as a warning,
  the fixture tiers of §9.1; design §6.3: the extraction algorithm
  of §7 including the final-LF rule; design §10: private excerpts through
  the bundle.
