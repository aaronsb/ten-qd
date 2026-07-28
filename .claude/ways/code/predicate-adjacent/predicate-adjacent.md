---
description: the recurring defect shape here — correct logic whose guarding predicate reads the thing next door
vocabulary: guard predicate condition if branch state check flag stale index identity compare bug review defect
refire: 0.15
scope: agent
---
# Predicate-adjacent defects

The dominant defect in this codebase is **not** wrong logic. It is correct logic
whose guarding predicate reads something *next to* what it actually needs. The
body is right, the comment is right, and the mistake is one clause away — which
is why these read fine in review and keep shipping.

Ten instances, across ten subsystems, in two weeks.

## The catalogue

| Where | Read | Needed |
|---|---|---|
| `m3u()` | the raw string | the sanitised one, three lines down |
| `audio_rec` | panel state zeroed one line earlier | the take's real length |
| `blank()` in `listen.rs` | the entry, discarding `playing` | both |
| the recorder's writer | whether the arm said running | whether there was data in hand |
| `audio_rec`'s `ending` | whether the arm was `Running` | `secs > 0.0`, as the branch six lines above already asked |
| `own_output_is_looping` | one hop | reachability across four |
| `routing()` on failure | `Idle`, meaning "nothing is wrong" | a value meaning "I could not look" |
| `Reseat`'s spent attempt | a sink *description* | the sink index; descriptions are not unique |
| the guard's stale-plug clear | wrote `None` blind | only if it was still the plug just judged |
| `Reseat`'s reset on change of stream | the stream | the stream *and* the output device, one field over |

## The three sub-shapes

**A one-hop answer to a transitive question.** A predicate comparing one
identity, or reading one flag, when the real question spans a chain — a graph, a
state machine that gained a state, a value that moved. If the question is
transitive, a single comparison *is* the bug.

**A failure conflated with a clean result.** `Idle`, `None`, `0`, `false`,
`default()` — when one of these is a legitimate healthy answer *and* what you
return when you could not see, every caller downstream is reading "I don't know"
as "fine." Give the failure its own value; `Option` usually suffices.

**Identity by the wrong attribute.** A name where the system uses an index, a
description where two devices share one, an index the server may recycle. Ask
what the *system* distinguishes by, and key on that; keep the human-readable
thing for display only.

## How to apply

- Ask what the predicate is **literally reading**, and whether that is the same
  object as the thing being decided about — especially across a mutation, a
  sanitisation, or a boundary between live state and buffered data.
- Ask whether the question is transitive. If it is, one comparison is wrong.
- After changing a state machine, **grep for every predicate naming any of its
  states**. Each is now potentially answering about the wrong one, and the
  compiler cannot see it.
- When you fix one instance, look one field over. Twice now the fix has landed
  on one subject while its twin kept the defect.
- Check whether the data source can express the right answer **at all**.
  `own_output_is_looping` could not have been fixed on `pactl`, whose object
  model has no filter nodes, so the offending edge was invisible in principle.

## Why it matters more here than usual

Several of these wrote wrong data into a file that is never rewritten, so
nothing self-corrects. Others govern a background thread that moves *other
applications'* audio around. A predicate that is one clause off does not fail
loudly here; it produces a plausible number, or a confident panel, and sends you
hunting somewhere else entirely.

## See Also

- `.claude/ways/testing/revert-the-fix/revert-the-fix.md` — how to prove the
  corrected predicate is actually guarded
