# Testlib profile

The tested header is MikeMirzayanov/testlib commit
`7679fcb53a0b08b94739dad4a2c036f73cc5498d` (2026-11-09),
`VERSION "0.9.51"`.

Header SHA-256:
`d92ca8a6b3690d34d5c56c9394d99798130e21cb0096db817d91e265f8d958c3`.
The unmodified CI fixture and its MIT notice live in
`tests/fixtures/testlib/`, excluded from crate packages. License SHA-256:
`edf4e4b144f24cc4d8070b7a18d700f9429e850f3353736dd628a0a3cacff6c9`.

The licence is MIT (`LICENSE`: `Copyright (c) 2015 Mike Mirzayanov`). The
header carries its own notices (`Copyright (c) 2005-2026`, Mike Mirzayanov,
provided "as is" with no warranty). The fixture copies keep every notice
unchanged.

Outcome list of the tested header, as written to the APPES `outcome`
attribute (default build, without `PCMS2`): `accepted`, `wrong-answer`,
`presentation-error`, `fail`, `points`, `reserved`, `unexpected-eof`,
`partially-correct`. Default exit codes: `accepted` 0, `wrong-answer` 1,
`presentation-error` 2 (also unexpected end of file and extra output),
`fail` 3, `points` 7, `partially-correct` `PC_BASE_EXIT_CODE` (0) plus
`pctype`. Points are written with `%.10f` and trailing zeroes removed, so
values needing ten fractional digits fail the points grammar. cpf supports
the first five outcomes; the others are unsupported.

Users supply their own header. A captured candidate beside all sources is
marked pinned exactly when its hash matches this pin. This describes the
candidate, not proof of which header a compiler used. Prebuilt programs
declare the default exit-code profile; custom remaps are not detected.

Checkers receive input, output, answer, report, and `-appes`; validators
receive input on stdin with no appended arguments. The protocol and its
validation order are specified in `design-checkers.md` §2.1 and
`contracts/execution.md` §2.1. Interactors are not enabled.
