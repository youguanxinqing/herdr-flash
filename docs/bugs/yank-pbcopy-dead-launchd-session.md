# `y` silently copies nothing when the Herdr server outlives its login session

- **Date:** 2026-09-15
- **Status:** Fixed

## Symptom

`prefix+s`, search, select, `y` — the picker closed exactly as it does on success, and the
clipboard still held whatever was in it before. No error anywhere: not on screen, not in the
Herdr server log. Herdr's own copy-mode kept working the whole time, which made the plugin look
like the broken part.

## Conditions to reproduce

macOS, and a `herdr server` process that has outlived a login session:

```
last                                  → guan console  Mon Sep 14 23:53  still logged in
uptime                                → up 13 days              (no reboot)
ps -o lstart= -p <herdr server pid>   → Wed Sep  2 09:47:27     (older than the console login)
```

The trigger is logging out and back in, not restarting the terminal app and not rebooting — a
reboot takes the server with it. One command tells a healthy server from a poisoned one: compare

```
ps eww -p <herdr server pid> | tr ' ' '\n' | grep SSH_AUTH_SOCK
ps eww -p <terminal app pid> | tr ' ' '\n' | grep SSH_AUTH_SOCK
```

Two different `/var/run/com.apple.launchd.*` paths means the server is bound to a session that no
longer exists.

## What actually failed

`y` shelled out to `pbcopy`. Every pane the Herdr server spawns inherits the server's Mach
bootstrap port, and that port referred to the launchd session torn down at logout. `pbcopy` there
cannot reach `pboard`, so it exits 1 with an empty stderr. Two symptoms confirm the namespace
rather than the tool:

```
osascript -e 'set the clipboard to "x"'  → -10006 Can't set clipboard
launchctl managername                    → Could not get manager name.
```

Herdr's copy-mode was unaffected because it never touches `pbcopy`: it writes OSC 52, which the
server relays to the attached client process — and that process lives in the *live* session.

## Why nothing was reported

`copy_selected_text(clipboard, &text)?` propagated the error out of the picker loop. Herdr then
tore down the temporary picker tab, so the message was printed onto a pane that no longer existed.
A failed yank and a successful one looked identical.

## Approaches that do not fix it

- **Retry, or pick a different clipboard tool.** Every candidate is `exec`ed from the same dead
  bootstrap; `wl-copy`/`xclip`/`xsel` would fail the same way if they existed on macOS.
- **Re-enter the live session from the plugin.** `launchctl asuser` and `launchctl bsexec` both
  require root, and `reattach-to-user-namespace` moves a process out of a *subset* namespace — it
  has nothing to reattach to when the session itself is gone.
- **Reporting the error better, alone.** Necessary, but it only renames the failure.

## The invariant the fix establishes

The picker never spawns a clipboard process. `y` writes `ESC ] 52 ; c ; <base64> BEL` to its own
stdout, and Herdr relays it to the terminal it is attached to. The clipboard write now travels the
same path Herdr's own copy-mode uses, so it is bound to the session the human is actually sitting
in, not to whatever session the server was born in. It also means a pane opened through
`herdr --remote` copies to the local machine rather than the remote host.

Second invariant: a copy failure never closes the picker. It lands on the status row
(`copy failed · …`) and leaves the picker open, because this pane is the only screen the message
can ever reach.

## Regression tests

- `clipboard::tests::writes_one_osc52_set_sequence` — the exact byte sequence, so a refactor back
  to a subprocess fails here.
- `clipboard::tests::base64_pads_every_chunk_remainder` — includes non-ASCII, since pane text is
  UTF-8.
- `picker::flash::tests::failed_yank_reports_on_the_status_row_instead_of_closing` — with
  `exit_on_yank` on, so the test fails if a failed copy ever returns `Copied` again.

## End-to-end verification

In any Herdr pane, with no plugin involved:

```
printf '\033]52;c;b3NjNTItd29ya3M=\a'
```

Nothing is drawn. Paste elsewhere; it must produce `osc52-works`. If that works, `prefix+s` →
search → `v`/`V` → motions → `y` must put the selection on the clipboard even while `pbcopy` in
the same pane exits 1.

The same probe, plus the step that says whether to file against this plugin, Herdr, or the
terminal, is in the README under "If `y` stops copying" — that is the copy users are meant to
find.
