# Restaging the binary in place kills the linked plugin with SIGKILL

- **Date:** 2026-09-02
- **Status:** Fixed

## Symptom

Right after `just build` on a machine where the plugin is `herdr plugin link`ed, the flash action
stopped working entirely: pressing the keybinding did nothing, repeatedly, for a couple of
minutes — then recovered on its own. Nothing in the Herdr server log (the process died before its
first RPC).

## Why it happened

`scripts/build.sh` staged the binary with a plain `cp`, which overwrites `bin/herdr-flash`
**in place, keeping the inode**. macOS caches code-signature validity per vnode; rewriting an
executable's content invalidates it, and until the kernel re-evaluates the vnode, fresh execs
from that path are killed outright. Six crash reports told the story:

```
EXC_CRASH / EXC_BAD_ACCESS — SIGKILL (Code Signature Invalid)
termination: namespace CODESIGNING, indicator "Invalid Page"
```

The failure is invisible from inside Herdr (the action process dies pre-socket), transient
(the cache recovers within minutes), and looks exactly like a logic bug in whatever was changed
last — this instance was initially chased as a false positive in the picker re-entry lock.

## Diagnostic that settles it

`ls ~/Library/Logs/DiagnosticReports/herdr-flash-*.ips` — crash timestamps clustering right
after a rebuild, with `Code Signature Invalid`, are this bug and not the plugin's logic.

## Invariant the fix establishes

*The staged binary is only ever replaced by rename onto a fresh inode.*

`build.sh` now copies to `bin/herdr-flash.tmp` and `mv -f`s it into place: a new inode gets a
fresh signature evaluation, and the rename is atomic so there is no window where a keypress
execs a half-written file.

## Regression tests

None runnable in-repo (the failure lives in the macOS kernel's vnode cache). The guardrail is
the `build.sh` comment pointing here; treat any "simplify build.sh back to plain cp" as
reintroducing this bug.

## End-to-end verification

1. Open a picker so the binary is warm, exit it.
2. `just build`, then immediately spam the flash keybinding.
3. Every press must open a picker; no `herdr-flash-*.ips` may appear in
   `~/Library/Logs/DiagnosticReports/`.
