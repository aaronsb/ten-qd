---
description: verifying that a test can actually fail, by reverting the fix it guards before trusting it
vocabulary: test guard assert regression coverage mutation revert prove verify passing green suite
refire: 0.15
scope: agent
---
# Revert the fix to check the guard

A test that cannot fail is worse than no test. No test leaves you knowing you
are unguarded; a test that always passes tells you that you are guarded when you
are not, and it will go on telling you that for years.

**So after writing a guard, break the thing it guards and watch it fail.**
Then put it back. This is the only evidence that the assertion is connected to
the behaviour, and it takes about a minute.

## What this catches, repeatedly

Real examples from this repository, all of which passed a full green suite:

| The test | Why it could never fail |
|---|---|
| asserted the panel did not contain `"taken back"` | that phrase had been deleted from the source; the bay could say anything at all |
| asserted the LINK lamp was lit by reading `Modifier::BOLD` | both the alert and the mode helper set BOLD, so drawing the *fault* lamp green passed |
| asserted "still fine at 84 columns" | survived a revert that took eight more cells off the row |
| covered `decide()` and `judge()` separately | three mutations destroying the whole feature — never pushing, pushing always, crossing the two memories — passed 219 tests |

The last one is the shape to watch for: **each half tested, the pairing not.**
When behaviour lives in how two correct pieces are wired together, tests of the
pieces prove nothing about the wire. Extract the wiring so it returns a value —
intents rather than side effects — and assert on that.

Earlier rounds on the recording work found one inert test in **every round the
check was applied** — six in total. `contains("NO LOG")` survived a cell of
overprint, because the label was six characters inside an eight-cell box.
`seconds <= 116` was also true of an entry never held at all. A level-limit test
measured at half scale, where the meter saturates with or without the clamp. An
arming test was really exercising the writer's refusal to open a file, not the
gate it claimed to guard. Every one looked like a real guard.

## How to do it

- Change the production code so the defect is present again. Prefer the *exact*
  prior behaviour over a random mutation: it is the regression you actually care
  about.
- Run the test. It must fail, and the failure message should name the number or
  the string it found, not merely say `assertion failed`.
- Restore, and re-run to green.
- If it did not fail, the test is decorative. Delete it or fix it — do not leave
  it there looking like coverage.

## Two ways to lose work doing this

**Copy the file to the scratchpad first and restore from that copy. Never
`git checkout <file>`** — it silently destroys every other uncommitted change in
that file. Done twice in one session here, costing a writer rewrite and two
tests both times.

**Run the full gate before committing, not after.** A scripted revert/restore
can race cargo's rebuild and leave a stale red result that you then commit on
top of.

## When the window is too small to hook

Microseconds, no observable state, nothing a test could catch — say so in a
comment instead of writing a test that would pass either way. An honest absence
is a better artefact than an inert guard: the comment tells the next reader the
gap is known, where the test tells them it is covered.

## Do not use coverage as the proxy

Line coverage says the line ran. It cannot say an assertion would have noticed
if the line were wrong, which is the entire question. Every decorative test in
the table above was on a covered line.

## See Also

- `docs/the-rack-is-an-output-device.md` — where a wrong predicate survived
  because its data source could not express the right answer
