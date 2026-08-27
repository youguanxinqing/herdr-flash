# Bug records

This directory keeps the failures whose fixes depend on behavior outside one function or module.
They are not a changelog. A bug belongs here when a future refactor could pass ordinary unit tests
while reintroducing the same failure.

Each record should preserve:

- the observable symptom and the conditions required to reproduce it;
- the actual event ordering or data invariant that failed;
- approaches that looked plausible but were incomplete;
- the invariant the fix establishes;
- regression tests and an end-to-end verification procedure.

## Index

| Date | Status | Record | Guardrail |
|------|--------|--------|-----------|
| 2026-08-27 | Fixed | [Flash picker entry blink and geometry drift](flash-picker-entry-blink.md) | Paint at final PTY geometry before focus; keep source and picker geometry distinct |
