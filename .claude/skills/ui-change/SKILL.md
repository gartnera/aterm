---
name: ui-change
description: Flow for any aterm change that affects rendered pixels (tab bar, URL preview, fonts, colors, cursor, grid layout, padding). Captures a before/after pair under Xvfb, runs the usual checks, hosts the screenshots on a side branch, and opens a PR that embeds both images via raw GitHub URLs.
---

# UI change flow for aterm

Any change that affects what aterm draws on screen goes through this flow,
so the reviewer can see the visual before/after without running the code
themselves.

## Steps

1. **Snapshot the current state.** Build aterm, drive it to a representative
   scene via `scripts/e2e.sh`, screenshot.
2. **Make the change** (edit `src/gfx.rs` or whichever file).
3. **Snapshot the new state** with the *same* scene, same window size, same
   IPC calls — anything that drifts makes the diff non-comparable.
4. **Run the integration tests.** `DISPLAY=:99 cargo test --release --test
   integration` from `/home/user/aterm`. UI code paths are exercised by
   tests in `tests/integration.rs` (tab create/close/select, font-size
   clamp, snapshot grid, URL hover) — a render-layer regression usually
   surfaces here first, and CI runs the same command on every push so
   green locally before pushing saves a round-trip. The e2e-test skill
   is the source of truth for the harness.
5. **Update or add an integration test** if the change introduces
   observable behavior — see "Updating tests" below.
6. **Lint:** `cargo fmt --check` and `cargo clippy --release --bin aterm`.
7. **Host the screenshots** on a `pr-assets/<topic>` orphan branch — see
   below. Do NOT commit PNGs to the PR branch.
8. **Open the PR**, embedding both screenshots via raw GitHub URLs.

## Updating tests

A UI change that adds something observable through the debug IPC should
grow an assertion in `tests/integration.rs`. Rough guide:

| Change | Test it with |
|---|---|
| New tab marker / title format / truncation | `tabs()` (titles + active flag) |
| Hover, click region, URL detection | `hover_url(row, col, ctrl)` |
| Color / cursor / cell state | `snapshot_text()` and/or specific `wait_for_text` |
| Window/OSC title | `title()` |
| Font-size step or default | `font_size(delta)` / `font_size_reset()` |

Pure restyling that doesn't change anything observable over IPC (a color
swap, a quad offset, a stripe under the active tab) doesn't need a new
test — but the existing tests should still pass unchanged. If you find
yourself needing to *loosen* an assertion to make the test pass, stop
and reconsider whether the visual change broke a contract.

See the e2e-test skill for the `AtermTest` helper API.

## Capturing a scene

```
sudo scripts/e2e.sh setup            # one-time on a fresh container
scripts/e2e.sh start                 # builds aterm if needed
# … drive to the scene via `scripts/e2e.sh ipc …` (see e2e-test skill) …
DISPLAY=:99 import -window root /tmp/aterm-before.png

# edit files, rebuild
cargo build --release
scripts/e2e.sh stop && scripts/e2e.sh start
# … same IPC calls, same window state …
DISPLAY=:99 import -window root /tmp/aterm-after.png
scripts/e2e.sh stop
```

Pick a scene that actually exercises the change:

| Change area | Minimum scene |
|---|---|
| Tab bar | ≥3 tabs, a non-first tab active, distinct titles |
| URL preview | shell prints a URL, then `hover_url row=N col=M ctrl=true` |
| Colors / cursor | run a `printf` that emits SGR for fg/bg/bold/italic/underline |
| Font size | call `font_size delta=…` between snapshots (or capture both sizes) |

Reuse the exact same IPC calls between before and after. If the cursor blink
phase matters, snapshot both within ~100ms of the same step so blink
position lines up.

## Hosting screenshots without polluting the PR

GitHub PR bodies need an HTTP-reachable URL — drag-and-drop upload is
browser-only and isn't exposed by the API or MCP tools. Workaround: push
the PNGs to an orphan branch named `pr-assets/<topic>` in this repo and
reference them by raw URL. The PR branch itself stays code-only.

```
# from the main worktree, with your PR commit already made
git checkout --orphan pr-assets/<topic>
git rm -rf .
cp /tmp/aterm-before.png ./<topic>-before.png
cp /tmp/aterm-after.png  ./<topic>-after.png
git add <topic>-before.png <topic>-after.png
git commit -m "PR assets: <topic> screenshots"
git push -u origin pr-assets/<topic>
git checkout <your-pr-branch>
```

**Run the orphan-branch dance inside `/home/user/aterm`, not in a separate
`git worktree`.** The signing helper at `/tmp/code-sign` errors with
`missing source` when commits are made from an external worktree path; the
main worktree signs fine.

Raw URLs follow this shape (always use `raw/`, not `blob/`):

```
https://github.com/<owner>/<repo>/raw/pr-assets/<topic>/<topic>-before.png
https://github.com/<owner>/<repo>/raw/pr-assets/<topic>/<topic>-after.png
```

## PR body template

```markdown
## Summary

<what changed and why, 2-4 bullets>

## Before / after

Before:

![before](https://github.com/<owner>/<repo>/raw/pr-assets/<topic>/<topic>-before.png)

After:

![after](https://github.com/<owner>/<repo>/raw/pr-assets/<topic>/<topic>-after.png)

(Screenshots are hosted on the `pr-assets/<topic>` branch and not
committed into this PR.)

## Test plan

- [x] `cargo fmt --check`
- [x] `cargo clippy --release --bin aterm`
- [x] `DISPLAY=:99 cargo test --release --test integration`
- [x] Visual check under Xvfb at <dimensions>: <one-line description of scene>
```

## When to skip

- Pure logic changes (PTY handling, IPC, config parsing) with no rendered
  effect — go straight to the e2e-test skill.
- A one-line color or padding tweak the reviewer will eyeball from the
  diff alone. Use judgment.
