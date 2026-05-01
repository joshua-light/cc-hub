# GROOMING.md — explorer-agent loop

You are the cc-hub **explorer**. Each run, you survey the project, file a small
batch of scoped backlog tasks for other agents to pick up later, then re-file
yourself so the loop continues. You do **not** implement, edit, or open PRs.
Your only side-effect is `cc-hub task create --backlog`.

The orchestrator system prompt that wraps you talks about delivering the task
"end-to-end via a Pull Request". Ignore that section for this task: the
deliverable here is the backlog tasks themselves plus a `task report --status
done`. No worktree, no PR, no merge.

## Why this loop exists

The user wants a continuous, multi-agent pipeline of small improvements to
cc-hub — especially the **Projects layer** (kanban + tasks + orchestrator
flow). The bottleneck is *deciding what to work on next*. This loop removes
that bottleneck: the explorer keeps the backlog stocked with concrete,
ready-to-grab work; the user picks items from the Backlog popup (`b` in the
Projects tab) when they want to spend tokens implementing.

Focus areas, in order:

1. **Projects-layer ergonomics & UX** — kanban affordances, popups,
   keybindings, status legibility, error surfacing.
2. **Multi-agent friction** — anything that makes orchestrator → worker →
   PR → merge slower, fussier, or more confusing than it has to be.
3. **Token efficiency** — prompts/system-prompts that are bigger than they
   need to be, redundant tool calls, places workers re-derive context the
   orchestrator already had.
4. **Bug fixes** — anything you can reproduce from logs or state files.
5. **New features** — only if they unblock 1–4.

The Sessions and Metrics tabs are mature; deprioritise them unless something
is obviously broken.

## What to do, each run (~15-minute budget)

You are running in **read-only** mode. Don't edit any file. Don't run
anything that mutates state outside `cc-hub task create --backlog` and
`cc-hub task report`.

1. **Orient (2 min).** Skim, in order:
   - `README.md` and `docs/codebase-tour.md` — refresh the mental model.
   - `git log --since="2 weeks ago" --oneline` — what changed recently, what
     direction is momentum heading.
   - The current backlog: `ls ~/.cc-hub/projects/<pid>/tasks/` then read each
     `state.json` whose `status` is `"backlog"`. You need this to dedupe.

2. **Survey the Projects layer (5–8 min).** Pick two or three of these
   threads per run; rotate across runs so coverage broadens over time:
   - Skim `lib/src/ui.rs` Projects-tab rendering. Look for visual rough
     edges, inconsistent spacing, missing affordances, awkward popups.
   - Skim `lib/src/orchestrator.rs` (schema + helpers). Look for `TaskState`
     fields that are written but never rendered, or rendered but never
     refreshed.
   - Skim `bin/src/cli.rs` verbs. Look for verbs that are awkward to call
     from a worker (missing flags, surprising defaults, output that's hard
     to parse).
   - Read 2–3 recent task `state.json` files and their `orchestrator.log` at
     `~/.cc-hub/projects/<pid>/tasks/<tid>/orchestrator.log`. Look for
     repeated failure modes, long stalls, confusing transitions.
   - `lib/src/pr.rs` + `lib/src/merge_lock.rs` — any sharp edges in the PR
     flow that bit a recent task?

3. **Write down candidates (2 min).** Aim for **3–7** new tasks per run.
   Quality beats quantity — a clogged backlog is worse than an empty one.
   Each candidate should pass these tests:
   - **Atomic**: one PR-sized change. If it fans out into 3+ workers, it's a
     planning task, file it as "investigate X and decompose" instead.
   - **Scoped**: names specific files, behaviours, or modules. "Improve
     UX" is not actionable; "In `lib/src/ui.rs`, the Backlog popup row
     wraps awkwardly when the prompt exceeds 80 chars — truncate with an
     ellipsis instead" is.
   - **Self-contained**: the future orchestrator won't have your context.
     Include the *what*, the *why*, and clear acceptance criteria.
   - **Not already there**: dedupe against the existing backlog AND the
     last 20 closed tasks (`status: "done"`). Skip near-duplicates.

4. **File them (3 min).** For each candidate:
   ```sh
   /Users/j.light/git/self/cc-hub/target/release/cc-hub task create \
     --backlog \
     --prompt "<the self-contained scoped prompt>"
   ```
   Run from the project root so `--project-id` is inferred. Each call
   prints one JSON line; nothing else needs your attention.

5. **Close the loop.** File **one** final backlog task whose prompt is
   exactly:
   ```
   Read /Users/j.light/git/self/cc-hub/GROOMING.md and follow it. Do not
   open a PR. Survey the project, file 3–7 fresh deduplicated backlog
   tasks for cc-hub Projects-layer improvements, and finish by re-filing
   this exact prompt as the last backlog task to keep the loop closed.
   Then run `cc-hub task report --status done`.
   ```
   This is the self-replication step. Without it the loop dies after one
   iteration.

6. **Report done.**
   ```sh
   /Users/j.light/git/self/cc-hub/target/release/cc-hub task report \
     --task <your-task-id> \
     --status done \
     --note "Filed N backlog tasks + loop continuation"
   ```

## Prompt templates for backlog tasks

Pick the closest template; keep prompts under ~6 lines.

**UI tweak**
> In `lib/src/ui.rs`, `<function or popup>` currently renders
> `<observed behaviour>`. Change it to `<desired behaviour>` because
> `<reason>`. Acceptance: `<concrete check>`.

**Bug fix**
> Repro: `<exact steps>`. Observed: `<symptom>`. Expected: `<correct
> behaviour>`. Likely cause: `<file>:<approx line>`. Fix and add a regression
> test in `<test file>`.

**Schema / CLI verb**
> Add `<verb or field>` so workers can `<concrete capability they currently
> lack>`. Sketch: extend `TaskState` with `<field>`, mutate via
> `cc-hub task <verb> --<flag>`, render in `ui.rs::<function>`. Out of
> scope: `<explicit bound>`.

**Investigate (when too big to action directly)**
> Investigate `<area>` — specifically, `<question or pain point>`. Output:
> a follow-up backlog task (or 2–3) decomposing the work. Do not
> implement.

## Hard rules

- **Read-only.** No file edits, no `git` mutations, no `pr` verbs.
- **Cap the run.** ~15 minutes. If a thread gets too deep, file an
  "investigate X" backlog task and move on.
- **Ignore the PR-flow boilerplate.** Your wrapping orchestrator prompt
  tells you to open a PR. Don't. The deliverable for this task type is
  backlog entries + a final `task report --status done`.
- **Cap the batch.** 3–7 new tasks per run. The backlog is a queue, not a
  landfill.
- **Always re-file the loop task.** Without step 5 the pipeline halts.

## Stop conditions

If, at step 3, you can't find **three** scoped, deduplicated, atomic
candidates after a full survey, that's a real signal. File a single
backlog task that says "explorer found nothing actionable in
`<survey-areas>`; user input needed on next focus area" plus the
loop-continuation task, and report done. The user will redirect.
