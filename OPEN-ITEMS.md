# Open items — branch `claude/build-status-decisions-zqsnau`

Working notes for review. This file is scratch for the branch, not documentation;
delete it before the branch merges anywhere.

Last updated against `0ddf285`, on top of `master` at `de2e014` (#63).

## Blocked on you

### 1. Flip `experimental.kitty_graphics`?

`0ddf285` turned on `sidebar_card_shapes` and `row_motion = "slide"`, but both sit
behind `kitty_graphics`, which is still `false` and was outside what was asked. Until
it flips, **those two settings change nothing on a stock config** — the gate chain is
`AppState::sidebar_rows_move()` / `image_card::is_available`:

```
sidebar_card_shapes -> kitty_graphics + known host cell size + a proportional face
row_motion = slide  -> all of the above AND sidebar_card_shapes
```

Only the signal bar (`[ui.sidebar.notifications]`) is live for everyone right now,
because it has no graphics gate.

One line in `ExperimentalConfig::default` if you want it. Worth deciding deliberately:
it enables the whole experimental graphics path, not just the sidebar.

### 2. Open a PR to get this built?

**Nothing in `0ddf285` has been compiled or tested.** The vendored libghostty-vt build
fetches from `deps.files.ghostty.org`, which this environment's egress policy blocks
with a 403. (Zig itself was solvable — `ziglang.org` is blocked too, but the pinned
0.15.2 comes down from the PyPI `ziglang` wheel and runs fine. The dependency fetch is
the wall.)

Pushing the branch does not cover it either: `ci.yml` triggers on `pull_request` and on
pushes to `master`/`windows` only. A PR is what would build it.

**One test is expected to fail**, and needs a real run to fix:
`desktop_full_app_semantic_frame_is_characterized` (`src/ui/tab_surface.rs:322`). The
signal bar now draws in the sidebar's reserved header row of every fixture built from
`AppState::test_new()`, which moves the frame's SHA-256. The digest has to be recomputed
from an actual run — it cannot be guessed. The two mobile digests below it should hold;
per the project's own note, their staying put is the check that only the desktop sidebar
moved.

I deliberately did **not** pin notifications off in `test_new()` to dodge this. The
digest moving is true information about the change.

### 3. What did "merge 10 backlogged changes" mean?

Never resolved. There is no backlog to merge:

- 0 open PRs, 0 open issues.
- Of 100 remote branches, every `fm/*`, `fix/*` and `issue/*` one maps to a PR closed
  with a `merged_at` — #33 through #63 all landed. 52 branches have zero content diff
  against master; the rest are stale pre-rebase leftovers whose content reached master
  via squash merge, which is why their commits look "ahead" while their content is in.
- Nothing is ahead of `origin/master` in content.

The only "10" anywhere in the pipeline is `docs/next/CHANGELOG.md`, whose Unreleased
section holds exactly 10 Added, 10 Fixed and 1 Changed against `[0.8.0] - 2026-08-03`.
Candidate readings were: cut a release of that backlog; pull from upstream `herdrdev`
(not in session scope — needs adding); delete the ~99 stale branches; or something
outside this repo.

## Funded, not started

### 4. Smooth row motion

Approved. What `row_motion = "slide"` ships today is whole-cell travel — roughly four
18px steps on a 9x18 cell — which reads as stepping, not as a glide.

Three things are needed **together**; none works alone:

1. sub-cell Kitty placement,
2. a frame tier finer than `anim::behaviour`'s 50ms step (~18ms; `MIN_RENDER_INTERVAL`
   is 16ms, so the loop can serve it),
3. the tree's trunk and branches drawn as pixel artwork — a glyph cannot sit at half a
   cell row, so a character connector cannot follow a card that does.

Known cost attached: every card image needs one extra cell row of transparent padding
for the sub-cell clip to eat.

Measured in `data/herdr-row-slide-reflow/subcell-test/RESULT.md`.

Per the project's own rules this is refactor-risk — it touches the animation engine, the
graphics placement path, and the sidebar's character/pixel split at once — so
characterization tests should be named before any code moves.

### 5. Dissolve tuning

Direction given: high pixel count and more animation frames. Half of that is
unambiguously supported, half conflicts with the measurement in
`data/herdr-dematerialize-density/report.md`:

- **More frames: right.** 220ms is four frames per half, and "no amount of grain makes
  four frames read as motion."
- **High density: bounded.** Both ends are ruled out — 1 particle/cell reads as
  corruption (14px blocks), but **at 21/cell the cards fray**. "High" as stated lands in
  the fraying zone. There is a usable value between; nobody has measured it.

Cost interaction: rasterising ten cards is ~16ms against ~1.4ms to encode, so raising
density *and* frame count multiplies the expensive half. The sheet has to reuse
`SidebarCardLayer::undissolved` or the card path lands on the frame-time tail.

Recommendation: run the existing capture harness at a ladder between 1 and 21 rather
than picking a number.

```bash
HERDR_DISSOLVE_CAPTURE_DIR=/some/dir \
  cargo test --release --bin herdr dissolve_capture -- --ignored --nocapture
```

## Context worth keeping

- All three flipped flags shipped off **in the commit that introduced them** and were
  never flipped since. The history contains **zero reverts**. None was a rollback after
  a bug — the staleness worry is unfounded.
- They are actively load-bearing, not abandoned: `image_card.rs` is modified in 7 of the
  last 10 commits, including both of the two most recent (#62, #63).
- The signal bar's old guard test read *"it must never arrive switched on"*, for cost:
  the `dirty`, `push` and `pr` slots arm a `git status` scan and a forge request whenever
  the bar is drawn. That cost is unchanged; the test now guards the escape hatch
  (`enabled = false`) instead, and the reasoning is recorded beside the default rather
  than deleted.
