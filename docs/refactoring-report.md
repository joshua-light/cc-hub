# cc-hub refactoring report

Modules ranked by how much a refactor would pay back, with the concrete split
proposed for each. Line references point at `main` as of this survey.

## Bugs found in passing

Nine live defects the survey hit while reading files for other reasons. They are
listed first because they outlive the refactor: each costs less to fix than the
section it came out of, and a reader who refactors nothing should still leave
with this list. Ordered by what a user loses. Confidence is stated where a
finding was reasoned off the source rather than executed.

1. **`lib/src/ops/task.rs:119–189` — `orchestrate start` binds a live
   orchestrator to an unclaimed task.** `orchestrate_start` writes
   `orchestrator_tmux` and the agent identity (`:161`) and never touches
   `status`, so `cc-hub orchestrate start --task ID` against a Backlog task
   leaves an orchestrator working a card the kanban still shows as Backlog —
   exactly the zombie the sibling guard at `ops/task.rs:240–251` refuses to let
   through `task report`. It also has no rollback if the post-spawn
   `update_task_state` fails, where the shared path rolls back at
   `prompts.rs:369–373`. Both omissions come from hand-rolling a sequence
   `launch_orchestrator_session` (`prompts.rs:264`) already factors out and the
   two other spawn paths call. §9.

2. **`lib/src/orchestrator/prompts.rs:371` vs `:478–484` — the claim-first
   rollback is written twice and the copies disagree.** `start_backlog_task`
   restores `Backlog` unconditionally; `restart_task` restores a captured
   `claimed_from` plus the old tmux. One of the two is wrong, and this is the
   concurrency protocol — a failed spawn strands the task in whichever status
   the losing copy picked. §4.

3. **`lib/src/config.rs:431–443` — one misspelled key discards the entire config
   file.** Every struct carries `deny_unknown_fields`, so `toml::from_str`
   rejects the whole document and `load` returns `Config::default()` behind a
   `log::warn` the TUI never displays. All ten sections silently revert, not the
   one bad field, and the module doc (`:1–4`) promises the opposite. The
   `unknown_field_rejected` test (`:498`) pins the rejection; nothing tests its
   cost. §13.

4. **`lib/src/projects_scan.rs:374` — a task whose `state.json` will not parse
   disappears from the kanban.** The repo already rejected this policy
   elsewhere: `orchestrator/mod.rs:1550–1556` exists because "a genuinely live
   task with a momentarily/persistently corrupt state would otherwise vanish",
   and `orchestrator/gc.rs:68` consumes it to fail safe. Net effect: gc protects
   the worktree of a task the board has stopped showing. Secondary leak at
   `:350` — `visited.insert` runs before the parse, so the stale cache entry is
   never served (mtime guard) and never evicted. §13.

5. **`lib/src/platform/window.rs:534` — one operator inverts a success into a
   permissions error.** macOS `focus` returns `raised && activated`,
   contradicting its own comment at `:503–508` ("raise is a no-op and we still
   get correct app-level focus from osascript below"). When AX raise no-ops — the
   documented GLFW case — the app *is* activated, but `Chain::focus` answers
   false and `focus.rs:51` tells the user to grant Accessibility access for a
   window that just came forward. Should be `||`. macOS only. §13.

6. **`lib/src/platform/process.rs:557–561` — on Windows every `codex exec`
   one-shot reports as a live interactive session.** `command_line` is the
   `not(any(linux, macos))` stub returning `""`, which makes the Codex
   interactivity filter a no-op: `is_agent_process` (`:426–435`) accepts on the
   exe name, then `codex_is_interactive("")` finds no positional token and
   returns `true` at `:378–379`. `codex_model_arg` and `codex_resume_session_arg`
   always answer `None`. Linux and macOS are unaffected; Windows is a supported
   target (`README.md:316–330`). *Reasoned, not executed* — nothing here builds
   for Windows and no test covers the stub. Partial mitigation not stated in §11:
   `codex_scanner.rs:344` requires `current_dir(pid)`, also a Windows stub
   returning `None`, so the Codex *scanner* drops those processes; the reachable
   path is `scanner.rs:1147`'s direct liveness call. §11.

7. **`lib/src/metrics.rs:480` — Codex sessions can never be reported as
   interrupted.** The Codex parser passes `in_flight_tool_use_ids:
   HashSet::new()` while the Claude (`:696`) and Pi (`:735`) parsers compute it,
   and `metrics.rs:1091` gates interruption and orphan-cost counting on that set.
   The file documents its other Codex caveat carefully (`:348–355`, token
   accounting) and this one not at all — the asymmetry is the tell that it was
   copied, not decided. **§8 refines the diagnosis: this is a duplication
   casualty, not an oversight.** `codex_conversation.rs:279–305` builds exactly
   that set, pairing `call_id` on `is_tool_call` against `is_tool_call_output`.
   The repo knows how; the copy that needed it could not see it. Fixing the field
   is a five-line change; the class it belongs to is ranking entry 3. §6, §8.

8. **`lib/src/pi_scanner.rs:80–93` and `lib/src/codex_scanner.rs:230–256` — scan
   cost is bounded for one agent and linear in transcript size for the other
   two.** Pi and Codex re-read and re-parse a head plus a tail window (up to
   4 MiB) per session per scan tick; `conversation/cache.rs:106` memoizes the
   equivalent Claude work by mtime and neither other agent has an equivalent.
   `scanner.rs:1119` states the asymmetry as a fact without treating it as a gap,
   and `dir_cache.rs:4` shows the same cost was noticed and fixed for Pi's
   *directory* walk but not its parse. §8.

9. **`lib/src/conversation/classify.rs:196–321` — the anti-drift net has no
   Codex cases.** The parity matrix the module header sells as the guarantee
   that backends agree carries `claude` and `pi` fields only (`:187–192`); the
   doc comments were never updated either (`:3`, `:50`). Not a live
   misbehaviour, but the newest and least-understood backend is the one outside
   the net, and this matrix is the safety net entry 3's refactor needs. §8.

Three further findings are mechanism arguments nobody executed, and are
deliberately **not** filed as bugs: `tmux_pane.rs:206`'s per-chunk DSR scan
cannot see a `\x1b[6n` split across two 8 KiB reads, but no one showed psmux ever
splits it; `tmux_pane.rs:389–444`'s `encode_key` drops ALT on `Backspace`, so
Alt+Backspace reaches tmux as a bare `0x7f`, but nothing records whether that was
intended; and `ui/mod.rs:252–326`'s 25 keybinding hints are structurally
disconnected from `keys.rs`, with only one hint spot-checked. Auditing all 25
hints against their arms is the cheapest way to turn the third into a fact.

## Ranking — what is most worth refactoring

Four rules, because the order is not obvious from the sections:

- **This ranks named files, not sections.** §8 grades a *relationship* among
  seven files; §6 and §8 are one finding seen from two ends; §7 and §12 share one
  card-renderer family; §9, §10 and §11 propose one directory convention; §1 and
  §2 propose one destination module. Merged accordingly, so nothing is counted
  twice or dissolved.
- **Sorted by payback, not by how bad the file is** — roughly (lines a refactor
  deletes, or invariants it makes structural) over (risk of getting it wrong).
  `app/mod.rs` is the worst file in the repo and is fourth.
- **Read the `code` column, never `total`.** `bin/src/cli/pr.rs` is 1506 lines
  and 495 of code; `app/command.rs` is 1889 and 733. Size alone never earned a
  rank here.
- **Do not sum the `≈lines` tables across sections.** They overlap: §1 and §2
  both move code into `app/task_board.rs`, and §8's `agent/discover.rs` row
  supersedes §6's `metrics/discover.rs` row rather than adding to it.

**1. `lib/src/ui/popups.rs` (2071 code) — High.** The best ratio in the survey:
roughly 1130 of its 2071 lines are two copy-paste families — six input popups
(~490 lines) and five list pickers (~640) — that one widget each collapses to
~150 and ~300. The code admits the copies itself (`:1155–1157`, `:651–653`).
*Costs:* one directory split with zero visibility changes, then two widget
extractions; the only real constraint is sequencing (below). *Buys:* ~700 lines
deleted, one definition of the sizing formula instead of five, and 21 renderers
that stop being 21 places to change chrome.
*Sequence:* design it against §1's fold of modal payloads into `View` variants,
or the seven `as_ref() else return` guards get baked into the new abstraction and
the later fold gets harder.

**2. `bin/src/keys.rs` (1055 code) — High.** One 951-line function is 90% of the
file and holds 87 match arms behind an 11-argument signature and an
`#[allow(clippy::too_many_arguments)]`. Every input feature is edited in the same
body; 33 `cc_hub_lib::` calls (19 `orchestrator::`) sit inside it, and the file
has zero tests.
*Costs:* mechanical, with an in-tree precedent — `bin/src/keys/tasks.rs` already
splits one view group into a sibling with a `pub(super) fn` pair, and the `View`
sets are disjoint. Two prerequisites, both stated in §5: introduce the `Runtime`
context struct first (the 11-parameter list is what makes moving arms expensive),
and re-derive arm order before moving anything — `(View::TmuxPane, _)` at
`keys.rs:580` is a catch-all whose position relative to the picker arms was never
verified.
*Buys:* five ~100–360-line sibling modules, a residual `keys.rs` of ~140, and the
end of one merge-conflict surface for every keyboard change.

**3. Transcript parsing, as a system — `lib/src/metrics.rs` (1400) +
`scanner.rs` (1206) + `codex_conversation.rs` (584) + `pi_conversation.rs` (540)
+ `pi_scanner.rs` (535) + `codex_scanner.rs` (504) + `conversation/state.rs`
(365) — High.** The largest single finding in the survey, and the only one no
individual file's section could have produced: **the repo parses every agent's
transcript twice, in two families that share one function.** Key-set overlap is
11/11 for Codex, 16/19 for Claude, 16/19 for Pi; both families discover the same
three roots independently; the only shared code is `parse_timestamp_ms`. Three
scanner pipelines (`pi_scanner.rs:45–122`, `codex_scanner.rs:218–283`,
`scanner.rs:658–713`) differ only in which `*_conversation` module is named, and
`read_jsonl_tail_for_state` is written three times differing in one predicate.
The grade belongs to the fan, not to any file: `conversation/state.rs` alone
would be Medium.
*Costs:* the highest design cost here — one `Transcript` trait, and §8's
load-bearing `agent/transcript.rs` estimate (130 lines) is a number nobody has
written. Do §8's follow-up 3 first (extend the parity matrix to Codex); it is the
only test that would catch this refactor changing what a state means.
*Buys:* the whole class where a capability lands for one agent and silently not
the others — bugs 7, 8 and 9 above are three live instances, plus
`explain.rs`'s 240 lines of Claude state explanation against ~60 for the others,
and `tool_display` truncating at 60 bytes for Claude and 60 characters for Pi.
Afterwards a fourth agent is one impl instead of edits in seven files.
*Caveat to carry:* the two-families charge rests on key-set overlap, which proves
the families read the same record vocabulary, not that they compute the same
values. Neither `metrics.rs` parser body was read in full, and some of what they
do is aggregation the scanners genuinely do not need.

**4. `lib/src/app/mod.rs` (3883 code) — High.** The worst file in the repo, and
deliberately not first. 189 methods, 38 fields over nine concerns, 97 `View::`
references, 65 `self.view =` assignments, 39 `crate::orchestrator::` call sites
and 35 store/filesystem sites. Nearly none of it is duplication — this is ~3900
lines of mostly distinct logic that is entangled rather than repeated, so the
split moves lines without deleting them.
*Costs:* the highest, and partly unknown. Moving `impl App` blocks into `app/*.rs`
siblings costs zero visibility changes (`command.rs:413`, `:448` already reach
private fields), but **§1's decomposition table is the weakest thing in this
report**: its ranges were grouped from names in an outline, probably cut through
methods, and were never checked for cross-range private-helper calls. Re-derive
the groups before moving anything.
*Buys:* the merge surface, and — via §1's follow-up 2, pushing task/PR mutations
down into `ops::` — the single largest step toward the missing application layer
that theme (a) below describes.

**5. `lib/src/ops/pr.rs` (983 code) — High.** `pr_merge` is 342 lines that
release a project-wide lock at 21 hand-written sites, and it has one test,
reachable only from another crate. The rubric hit is "inlined I/O blocks
testing", with the blocking mechanism named precisely.
*Costs:* small first step, large second. Follow-up 1 — move the git harness
(`init_repo`, `seed_task_with_worktree`, `git_available`) from
`bin/src/cli/test_util.rs` into `lib`'s `test_util` — is a ~40-line relocation and
is the safety net for everything else.
*Buys:* an RAII merge-lock guard collapses 21 release sites to one `Drop` plus
one hand-off and kills the wedged-project class outright ("wedges every
subsequent `pr merge` for the project" until `STALE_TTL`). *Read `:540–584` and
`:650–686` before starting:* §10's "no leak today" verdict is an arithmetic
argument over 342 lines, not a reading of them.

**6. The four-renderer card family — `lib/src/ui/sessions.rs` (857) +
`ui/projects/task_cards.rs` (449) + `ui/tasks.rs` (730) — Medium.** §7 found
three copies of one card envelope and predicted a fourth; §12 confirmed it at
`ui/tasks.rs:473`. In `task_cards.rs` the two renderers are 383 of 449 code
lines, and their badge footers have already drifted in four of six spans
(`:125`/`:387`, `:126`/`:388`, `:131`/`:401`, `:137`/`:407`).
*Costs:* do §7's follow-up 2 before follow-up 1 — a `TaskCardCtx<'a>` first, so
the shared `task_meta_row` is written once against one argument instead of twice
against two 9- and 10-parameter lists.
*Buys:* one footer builder for three call sites, one envelope for four
renderers, both `#[allow(clippy::too_many_arguments)]` gone, and two structurally
identical `board.rs:211`/`:223` call sites that stop being transposable. Decide
the unfocused border grey while there: `Rgb(60, 60, 80)` is written four times,
ten blue points from `SEP_GRAY`.

**7. `lib/src/platform/process.rs` (761 code) — Medium.** Two competing platform
abstractions in one file, and the documented one has no users: the `ProcessInfo`
trait is implemented three times and named nowhere outside the file, while five
free functions are cfg-forked by hand across 24 `#[cfg]` attributes.
*Costs:* cheap — the cfg forks already draw the module lines. The macos row
(≈230 lines) is the softest number in §11 and assumes the libproc FFI decls move
with their callers.
*Buys:* regrouping by OS is what exposes bug 6 — a missing capability becomes a
missing function instead of an invisible stub returning `""`.

**8. `bin/src/main.rs` (1415 code) — Medium alone.** `run()` is 646 lines, of
which the first 310 are pure wiring (six channels, ten `tokio::spawn` workers)
before the loop starts, and the loop body owns render policy, telemetry, a CPR
terminal probe and OSC 52 replay. `queue_missing_titles` and
`queue_missing_task_titles` are the same 113/135-line protocol twice, and the
comments explaining why it is shaped that way survive only in the first copy.
*Costs:* the `bin/src/runtime.rs` row cannot land until §5's follow-up 1
(`Runtime` struct) exists — the wiring closes over locals the loop uses. §5
sequences that only for `keys.rs`.
*Buys:* `bin/src/titles.rs` collapses the two title queues onto one protocol
(independent, landable today); the rest makes an untestable 646-line function
into a ~420-line loop that should be long.

**9. `lib/src/orchestrator/mod.rs` (1382) + `orchestrator/prompts.rs` (511) —
Medium.** Well-tested domain code with real locking discipline; two local
duplications and a wide API surface, not entanglement.
*Costs:* the split into `types.rs` / `store.rs` / `registry.rs` / `paths.rs` is
straightforward layering. The valuable change is smaller than the split.
*Buys:* extracting the claim-first protocol into one function parameterized by
precondition, claim and rollback fixes bug 2 structurally — and the same change
absorbs §9's third site (`ops/task.rs:119–189`, bug 1). One finding, three
sites; do it once. Also collapses `TaskState::new`/`new_backlog`, 34 lines
duplicated to vary one field, where `new_personal` (`:541–548`) already shows the
right shape.

**10. `lib/src/ops/task.rs` (739) + `lib/src/ops/worker.rs` (605) — Medium.**
Ranked together because they land in one change with entry 5. `task_report` is
154 lines of which 100 are one closure that threads results out through two
captured `Option`s because it can only return `bool`. `ops/worker.rs` is two
things in one file: `:19–98` is the shared primitive surface every sibling
imports, `:99–605` is three independent verb bodies.
*Costs:* both are function-level moves behind `pub use`, no shared state, no
`impl` blocks, and `ops/mod.rs` already declares the modules.
*Buys:* **one `ops/<verb>/` convention set once.** §9, §10 and §11 each propose
this directory shape independently; doing them separately sets the convention
three times. Extract `ops/prompt.rs` in the same pass — that is the exact surface
`ops/task.rs:11` and `cli/mod.rs:572` import.

**11. `bin/src/cli/mod.rs` (573 code) — Medium.** A 40-field `Flags` struct and a
36-arm parser serve all 27 `parse_flags` call sites, and no verb declares which
flags it accepts: `task delete --skip-build` and `pr create --backlog` parse
without complaint, and the CLI exits 0. The tokenizer exists twice and the flag
taxonomy lives in two hand-maintained const lists that nothing keeps in sync.
*Costs:* three seams that share no state; ~290 lines to `cli/flags.rs`.
*Buys:* a single `(name, arity, free_text)` table kills the second tokenizer, and
a per-verb allowlist kills a failure mode an orchestrator agent cannot see. Do
the table before the allowlist — the allowlist indexes into it.

**12. `lib/src/ui/mod.rs` (388) + `lib/src/ui/metrics.rs` (609) — Medium /
Low.** `ui/mod.rs` carries three parallel matches on `View` (overlay dispatch 21
arms, hint table 25, a `(View, Tab)` space-verb match) plus a 160-line status bar
in a 388-line file; with the two matches in `keys.rs`, adding one overlay is five
edits across two files and the compiler catches the first. `ui/metrics.rs` is
Low alone and rides along with entry 3's `metrics.rs` work.
*Costs:* small. Moving the 25 hints onto a method on `View` is the cheapest
compiler-enforced win in the report.
*Buys:* a variant that cannot be added without describing itself.

**13. `lib/src/app/command.rs` (733 code) — Medium, and explicitly not a size
problem.** Do not split it for length. Its one structural move is to stop §2's
layer cut and §1's feature cut from crossing: after §1's split, a Tasks-board
change touches `command.rs`, `app/task_board.rs` and `bin/src/keys.rs`.
Co-locating each feature's arms with its methods brings that to two — so this
lands *with* entry 4, not separately. Its own follow-up 1 (a uniform outcome
type replacing `bool` / `Option<String>` plus the `tasks.persistence_error` side
channel) collapses ~15 arms to one line each and moves failure text next to the
code that knows the reason.

**14. `lib/src/tasks.rs` (682) + `bin/src/cli/task.rs` (614) +
`lib/src/platform/window.rs` (577) + `lib/src/projects_scan.rs` (381) +
`lib/src/ui/sessions_list.rs` (426) — the tail.** Real work, none of it
structural. `tasks.rs` wants a deletion: 169 lines of one-shot legacy board
migration, 25% of its code, in the file the TUI reads on every board load.
`cli/task.rs` wants two delegations, not seams — delete `task_list`'s body in
favour of `orchestrator::list_task_states` and move `task_show`'s read into
`ops::task`, after which it is 21 uniform wrappers. `window.rs`'s split into
three per-OS modules is mechanically free and buys the first testable unit in the
file (`detect()`, whose chain order puts xdotool ahead of the native macOS
backend on any mac with xdotool on `PATH`). `projects_scan.rs` and
`sessions_list.rs` want type-level fixes only.

**Do not refactor.** Seven files earned a Low on evidence *for* the file, not on
an absence of findings, and three more should be extended rather than cut:
`merge_lock.rs` (flock over a stable sidecar with the inode rationale written
down, tempfile+rename, a liveness proxy beyond the TTL, 20 tests including three
concurrency ones), `bin/src/cli/pr.rs` (ten verbs demonstrably keeping a written
contract), `title.rs` and `spawn.rs` (documented invariants honored on the other
side, outcome-split cache TTLs, adversarial parser tests, `prefix_env`
parameterized on `windows: bool` for testability), `tmux_pane.rs` (`Osc52Scanner`
is a documented state machine with a stated memory bound and six adversarial
tests), `config.rs` (`deny_unknown_fields` across ten sections, one `Default`
per section, the legacy shim pinned by a test), `ui/common.rs` (the survey's
successful anti-duplication module — 11 of 16 UI files import it), and
`conversation/classify.rs` (the best-designed file this survey read).
`task_cards.rs` gets no split rows either: splitting two twins into two files
freezes the duplication instead of removing it.

## Cross-cutting themes

Ten findings no single section could see. Each is assembled from evidence in
sections that were written blind to one another.

**(a) There is no application layer.** Not "the UI calls the store" — there is no
layer between them at all, and the store is reached from render, from state, and
from input. `App` makes 39 `crate::orchestrator::` and 35 store/filesystem calls
(§1), and `update_sessions` opens with a disk read. The command layer's tests are
unix-gated and `$HOME`-redirecting because `App::new()` touches the on-disk store
(§2, `command.rs:730–733`), after a board migration once ran against a
developer's real `~/.cc-hub`. `keys.rs` makes 33 `cc_hub_lib::` calls, 19 of them
`orchestrator::`, **inside `handle_key`** — pressing a key writes the store and
spawns processes (§5). §4 *is* the store being called. `result_popup.rs:329`
decodes an image from disk mid-draw and mutates the cache at `:337` (§7).
`ui/popups.rs:431` calls `Config::resolved_agents` inside a renderer (§13). Every
"this file is untestable" verdict in this report is a restatement of this one
fact, and §1's follow-up 2 — `App` methods return an intent, `ops::` performs the
write — is the only proposal that addresses it directly.

**(b) Every large duplicate this survey found had already drifted.** This is the
argument for acting, and no section states it, because each saw only its own
instance: the claim rollback diverged on what it restores (§4); the Codex parser
lost the in-flight field its twins compute (§6, §8); four of six badge-footer
spans disagree (§7); the readiness ladder's timeout is configurable on one path
and not the other, and one copy rescans synchronously every 500ms where the other
reads a cache (§11); the fourth card renderer picked a raw border colour instead
of `SEP_GRAY` (§12); `wrap_text` exists twice with forked zero-width semantics
(§12 vs §3); `tool_display` truncates at 60 bytes on one side and 60 characters
on the other (§8); `orchestrate_start` dropped the claim and the rollback (§9).
Duplication here is not a latent risk. It is realized, and five entries in the
bug list above are its casualties.

**(c) Draw is not a pure function of state.** Nine renderers take `&mut App`;
eight of them clamp scroll during layout because the viewport only exists then
(§3 three, §6 one, §7 four, §12 one at `ui/tasks.rs:127`), and one
(`result_popup.rs:329`) performs filesystem I/O. The consequence is a hidden
ordering rule: any nav method reading a scroll value depends on a draw having
already happened. §7's follow-up 3 is the shared fix — a `RenderState::clamp(viewport)`
called once per frame — and §12 found its hook already exists and already has a
caller: `ui/mod.rs:63` calls `app.update_grid_cols` once per frame from the
top-level `render`. One ~30-line change in `app/render_state.rs` retires nine
`&mut` signatures. Adjacent but **distinct**, do not merge them: §13's
"immutable input, derived collection rebuilt per call" shape
(`Config::resolved_agents` at 12 call sites including three per scan tick,
`ProjectsSnapshot::roles_by_tmux` twice per frame) allocates during draw but
mutates nothing.

**(d) The palette was never finished — and §3, §6 and §7 diagnose this wrong.**
Those sections count raw `Color::Rgb` literals "against" palette constants,
implying the literals bypass a palette they should be using. Measured across
`lib/src/ui` excluding `palette.rs`: **150 raw literal sites, 100 distinct
values, against a 14-constant palette — and exactly one site uses a value the
palette already defines.** The literals do not bypass the palette in any
meaningful sense, and "use the existing palette" is not the fix. The real finding
is adjacent and larger: **32 distinct values are each repeated across 2+ sites,
are in no constant, and cover 80 of the 150 sites.** `palette.rs:1–4` documents
itself as consolidating "only *exact* duplicate values" — a one-time
de-duplication that has since drifted, accumulating 32 more exact duplicates.
§12 found the mechanism, which is better than the count: because the policy
admits only exact duplicates, a near-miss can never enter, so the palette cannot
converge by construction. Proof: `Color::Rgb(60, 60, 80)` appears four times
(`ui/tasks.rs:399`, `:488`, `projects/board.rs:153`, `projects/backlog.rs:148`),
ten blue points from `SEP_GRAY`, the constant documented for exactly that use.
And there are **three colour authorities that do not reference each other**:
`palette.rs` organizes by value, `ui/common.rs` decides by role
(`task_status_meta`, `priority_color`, `TASK_COLORS`, `ctx_color`), and
`projects/diff.rs:20` defines `TASK_META_DIM` outside both. Finishing the palette
collapses 80 sites into 32 names; deciding which authority owns a colour is the
harder half.
*Do not read the raw-vs-palette ratio as one repo-wide number.* The running total
is 288 raw against 103 palette uses (§3, §6, §7, §12), but §12's four files
invert the trend at 29/48, and 8 of those raw literals are a deliberate identity
table. §13's four files contain none at all. It is a UI-layer shape, not a repo
shape.

**(e) "Present but unparseable" has five answers and the repo already picked the
right one.** Five walkers enumerate the tasks directory:
`orchestrator/mod.rs:1515` (skip + log), `projects_scan.rs:298` (skip + warn,
mtime cache), `bin/src/cli/task.rs:223–247` (skip + eprintln, plus a `t-`/`tk-`
prefix filter nobody else applies), `tasks.rs:151–178` (hard-fail the whole
board), and `orchestrator/mod.rs:1550` — which is not a sixth policy but the
**fix**, a second walker inside an already-counted file whose whole job is to
name what the others dropped. Exactly one of five consumers applies it:
`orchestrator/gc.rs:68`, to keep a corrupt task's worktree alive. So gc protects
the worktree of a task the board has stopped showing (bug 4).
`bin/src/cli/mod.rs:532–559` adds the projects-dir sibling with a sixth policy:
silent skip, no log at all, surfacing as "task not found under any registered
project". Spans §9, §11 and §13.

**(f) User-facing status text has no owner.** 30 `set_status` calls in
`app/mod.rs` (§1), 46 in `command.rs` — one every 16 lines (§2) — and 49 in
`keys.rs`, the largest concentration in the binary (§5). The split is not by
layer but arbitrary: `move_selected_task` formats the *success* message
(`app/mod.rs:1379`) while `MoveTaskRight` formats the *failure*
(`command.rs:486`). Five arms fall back to the same `"no task focused"` because
the caller chooses failure text without knowing why the call failed, and where a
real reason exists it travels through `tasks.take_persistence_error()` — an
out-param implemented as a mutable field on another struct. §2's follow-up 1 (a
uniform outcome type) is the fix, and it wants doing across all three files at
once.

**(g) A crate boundary hides missing tests.** `lib`'s `test_util`
(`lib/src/lib.rs:32`) offers only `with_temp_home`; the git harness (`init_repo`,
`seed_task_with_worktree`, `git_available`) is `pub(crate)` in
`bin/src/cli/test_util.rs`. Any `lib` function needing a real repo is therefore
testable only from `bin`, which is why `ops/pr.rs` has five tests in 95 lines
while `cli/pr.rs` has 1011 test lines covering `ops/pr.rs`'s hardest paths, and
why `resolve_wait_targets` is tested four times in `bin` and three times in
`ops/worker.rs` with no shared fixtures. Before calling any `lib` file
undertested, check whether its tests are sitting in `bin/src/cli/*`. §10's
follow-up 1 is a ~40-line move and unblocks the rest.

**(h) `AgentKind` fans out across the tree.** Adding a fourth agent means editing
three places inside `metrics.rs` alone (`discover_session_files`,
`parse_session_file`, a new parser), and beyond it: 11 `AgentKind::Claude` arms
in `scanner.rs` plus a three-arm dispatch repeated at `:1091`, `:1138`, `:1169`,
`:1190`; four arms each in `config.rs`, `app/command.rs` and `live_view.rs`;
three each in `orchestrator/mod.rs`, `cli/worker.rs` and `spawn.rs`; one each in
`platform/process.rs` and `ops/worker.rs`. This is the same fan entry 3 proposes
to collapse behind one trait, seen from every other section. Counted once, in §8.

**(i) The modal state seam has two faces and one fix.** In `app/mod.rs` it is a
flat 21-variant `View` enum plus ~10 sibling `Option` payload fields, with
nothing tying a variant to its payload — `view = View::ModelPicker` with
`model_picker: None` compiles and renders an empty modal (§1). In `popups.rs` it
is seven renderers opening with `let Some(x) = app.<field>.as_ref() else {
return; }`, each re-proving a payload the dispatcher already proved (§3). The fix
is one change: fold each payload into its `View` variant, the way
`PendingConfirm` (`app/mod.rs:587`, `:345`) already does for confirmations. It
deletes ~10 `Option` fields and all seven guards, lets the picker renderers take
`&ModelPickerState` instead of `&App`, and lets the enter/close/move/submit
quartets collapse onto one generic list-picker. **Entries 1 and 4 must be
sequenced against each other for this reason** — extracting a widget that still
takes `&App` bakes the guards into the new abstraction.

**(j) The layer-vs-feature conflict dissolves — the port stalled on the feature
line.** §1 cuts `app/mod.rs` by feature; §2 observes that `command.rs` is
organized by layer and warns the two cuts cross. §5's data resolves it: 54 of 163
key arms are `Command`-mapped (33%), and what converted is Sessions, Global and
Tasks, while what did not is Projects, Metrics and **all sixteen modal views**.
The port did not stall at a layer boundary; it stopped exactly where a feature
was finished. The layer cut and the feature cut are the same cut, so §1 and §2
were never proposing incompatible things — which is why entry 13 says
`command.rs`'s arms move with §1's methods rather than against them.

## 1. `lib/src/app/mod.rs` (3883 code lines / 5966 total)

**What it is.** The TUI's single view-model and state root. `App` owns every
tab's view state, every modal's state, the scan→group pipeline that turns
scanner snapshots into the sessions grid, and direct calls into the
orchestrator/PR/task stores. Both binaries drive the UI through it; the key
layer (`bin/src/keys.rs`) and the renderer (`lib/src/ui/`) read and mutate it.
189 methods hang off `App` in this file, 56 more in `app/command.rs`. The test
module (3884–5966) is 2083 lines — 35% of the file.

**Problems**

- `mod.rs:566–650` — `App` has 38 fields covering nine unrelated concerns:
  runtime handle, five per-tab view states, render geometry, ten modal payloads,
  three input buffers, a dispatch queue, spawn watchdogs, image picker, usage,
  bookmarks. Six fields (`sessions`, `projects`, `metrics`, `tasks`, `todo`,
  `render`) were already grouped into substructs; the other 32 stayed loose, so
  the precedent exists and was simply not finished.
- `mod.rs:184–223` + `mod.rs:602–620` — modal state is split between a flat
  21-variant `View` enum and ~10 sibling `Option` payload fields. Nothing ties a
  variant to its payload: `view = View::ModelPicker` with `model_picker: None`
  compiles and renders an empty modal. `pending_confirm: Option<PendingConfirm>`
  (`mod.rs:587`, `345`) shows the fix — mutual exclusion made structural — but
  only for confirmations. 97 `View::` references and 65 `self.view = `
  assignments spread the invariant across the file.
- `mod.rs:2210`, `2223`, `2271`, `2286` / `2231`, `2240`, `2245`, `2251` /
  `2321`, `2379`, `2386`, `2395` — every picker repeats the same
  enter/close/move/submit quartet, hand-written per modal. Three copies of one
  shape, differing only in which `Option` field they poke.
- `mod.rs:1988–2077` — `approve_review_task` reads a PR, calls
  `ops::pr::pr_approve`, queries the merge lock, writes task state, and formats
  four status strings, in one 90-line method on the view-model. Its failure
  path is a two-phase protocol the *caller* must complete:
  `rollback_merging_to_review` (`mod.rs:2089`) exists solely to undo it when the
  post-approve notify finds no live orchestrator. The contract lives in a
  doc comment, not in a type.
- 39 `crate::orchestrator::` call sites and 35 filesystem/`ops::`/store call
  sites sit in this file; `update_sessions` opens with a disk read
  (`mod.rs:3304`). Domain writes are inlined into state transitions, so no
  task/PR path can be exercised without a real store on disk.
- `mod.rs:3300–3426` — `update_sessions` runs eight steps in sequence (reload
  sidecar, apply acks, check spawn watches, adopt pending names, bind task
  sessions, rebuild groups, structural diff, focus jump, GC name suppressions)
  and returns early at `3364` through a path that duplicates the tail's
  assign-and-return. With `build_groups` (`3496`), `adopt_groups` (`3662`),
  `check_spawn_watches` (`3260`), `adopt_pending_spawn_names` (`3434`),
  `task_badge` (`3622`) and the free helper `cluster_by_task` (`80`), this is a
  ~450-line subsystem with its own vocabulary, cohabiting with
  `ModelPickerState::push_filter` (`438`).
- `mod.rs:674–689` — placeholder sessions are typed by string prefix:
  `spawning_session_id` formats `"spawning:{tmux}"`, `spawning_tmux_of` parses
  it back. A synthetic card and a real one share a type and differ by an id
  prefix, so every consumer has to know the convention or silently treat a
  placeholder as real.
- `mod.rs:697` — `config::get()` is reached from state methods directly, while
  agent spawning was already abstracted behind `Arc<dyn AgentRuntime>`
  (`mod.rs:567`) for exactly the same testability reason. Two policies for one
  problem.
- 30 `self.set_status` calls put message formatting inside state transitions.
  `approve_review_task` communicates through both a return value *and* a status
  side effect, which its own doc comment has to explain (`mod.rs:225–231`).

**Severity: High.** This is the file every UI change touches, its 189 methods
put unrelated concerns in one merge-conflict surface, and the inlined store I/O
makes the board and PR paths untestable in isolation.

**Proposed decomposition**

Sibling child modules already host `impl App` blocks — `app/command.rs:170`
carries 56 methods and reaches `App`'s private fields (`self.runtime` at
`command.rs:413`, `self.default_session_agent_id` at `command.rs:448`), because
Rust grants descendant modules access to a parent's private items. Moving
method groups into `app/*.rs` therefore needs no visibility changes, no
signature changes, and reviews as pure relocation.

| new module | moves from `mod.rs` | ≈lines |
|---|---|---|
| `app/session_sync.rs` | `update_sessions` 3300, `build_groups` 3496, `adopt_groups` 3662, `rebuild_groups` 3719, `sync_known_session_ids` 3734, `check_spawn_watches` 3260, `adopt_pending_spawn_names` 3434, `select_session_by_id` 3235, `task_badge` 3622, `task_priority` 3600, `cluster_by_task` 80 | 520 |
| `app/spawn_watch.rs` | `SpawnWatch` 657, consts 670/674, `spawning_*` 679/687/696, `watch_spawn` 3212, `should_autoprompt_rename` 736 | 130 |
| `app/task_board.rs` | 839–1830: task input/rename/tags/info/attach, column & selection, move & priority, places & assign, resume/delete/undo, filters | 990 |
| `app/projects_ops.rs` | 1881–2102: `update_projects`, kanban/backlog accessors, `approve_review_task`, `rollback_merging_to_review` | 220 |
| `app/pickers.rs` | 2177–2642 picker methods + `ModelPickerState`/`AgentPickerState` 359–553 | 660 |
| `app/modals.rs` | 2643–3199: tmux pane, prompt input, rename session, confirm dialogs, status/scroll/popup/live-tail/state-debug + `Pending*` structs 300–358 | 570 |
| `app/dispatch.rs` | 2805–2914 queue + `PendingDispatch`/`DispatchAction` 554–564 | 120 |

Residual `mod.rs`: the `App` struct, `View`, `Tab`, `new_with_runtime`, and the
re-exports — under 400 lines. Move the matching tests with each group.

Two follow-ups that change types rather than locations, and are worth doing
after the split lands:

1. Fold each modal's payload into its `View` variant (or into one `modal:
   Option<Modal>` field), the way `PendingConfirm` already does for
   confirmations. Removes ~10 `Option` fields and the whole "variant set,
   payload `None`" bug class, and lets the enter/close/move/submit quartets
   collapse onto one generic list-picker.
2. Push the task/PR mutations down into `ops::` and have `App` methods return an
   intent instead of performing the write. `approve_review_task` becomes a
   selection read plus one call, `rollback_merging_to_review` stops being a
   caller obligation, and the board/PR paths become testable without a store.

Order matters: the split is behavior-preserving and makes the rest reviewable;
doing the type changes first would spread them over a 3883-line file.

## 2. `lib/src/app/command.rs` (733 code lines / 1889 total)

**What it is.** The intent-dispatch layer between keys and state. `bin/`
translates key events into a `Command`, `App::execute` applies every in-process
consequence, and returns `Effect`s only for what needs the terminal, a tokio
channel, or the window manager (`command.rs:1–14`). One `impl App` block, 10
methods, 5 enums, 49 command variants. **The size in the ledger was wrong:
the test module is `#[cfg(all(test, unix))]` (`command.rs:734`), which the
`^#\[cfg\(test\)\]` probe missed — 1155 of the 1889 lines are tests (61%), so
the code is 733 lines, not 1889.** By code this is a mid-sized file, and it
should drop several places in the ranking.

**Is it a good precedent for unit 1's split?** Yes as a mechanism, no as a
destination shape. It proves a child module can carry `impl App` with zero
visibility churn, and it is not a dumping ground — it has one documented job and
a real contract (`Effect`). But it is organized by *layer* while unit 1's
proposed modules are organized by *feature*, and the two cuts cross. Detail
below.

**Problems**

- `command.rs:215–363` and `command.rs:467–628` — two match statements, 149 and
  162 lines, one arm per variant, are 40% of the file's code. Nothing in them is
  complex; they are long purely because every arm is written out.
- `command.rs:486`, `512`, `519`, `532`, `553`, `560`, `595` — the same
  seven-line shape repeats: call a method returning `Option<String>`,
  `set_status` the `Some`, `set_status` a hardcoded fallback on `None`, return
  `Vec::new()`. `command.rs:316`, `321`, `336`, `526`, `541`, `547`, `573`, `579`
  repeat the `bool` variant of it. ~15 arms differ only in which method they
  call and which literal they fall back to. The failure *text* is chosen by the
  caller, which does not know why the call failed — five arms all fall back to
  the same `"no task focused"`.
- `command.rs:585–592`, `command.rs:602–614` — where a real reason exists, it
  travels through a side channel: both arms call
  `self.tasks.take_persistence_error()` after the fact, and `SubmitInput` must
  snapshot `self.tasks.renaming` *before* the call (`command.rs:603`) to pick
  between two fallback strings. An out-param implemented as a mutable field on
  another struct.
- `command.rs:1–14` — the port is unfinished, by its own admission: Projects and
  Metrics arms still live in `bin/src/keys.rs`, and modal buffer-edit keys stay
  as thin `bin` arms. `Command` (`command.rs:23`) has only `Global`, `Sessions`,
  `Tasks`. Two dispatch mechanisms are live at once, so "where does this key's
  logic go" has no single answer.
- ~43 of the 49 arms `return Vec::new()`; only `OpenDetailPopup` (`245`),
  `OpenStateDebug` (`252`), `OpenShellHere` (`285`), `FocusSelected` (`284`),
  `FocusAgent` (`571`) and the metrics-scan path (`207`) ever produce one. Every
  effect-free arm still allocates the type and says so explicitly.
- 46 `set_status` calls in 733 code lines — one every 16 lines. That is the
  documented design (`command.rs:172–173`), but it only half holds: unit 1 found
  30 more inside `app/mod.rs`, so `move_selected_task` formats the *success*
  message (`app/mod.rs:1379`) while `MoveTaskRight` formats the *failure*
  (`command.rs:486`). One message pair, two layers.
- `command.rs:730–733` — the test module is unix-gated because every test
  constructs an `App`, which touches the on-disk task store, so tests must
  redirect `$HOME`. The comment records that a board migration once ran against
  a developer's real `~/.cc-hub`. The command layer is testable without a
  terminal exactly as designed; it is not testable without a filesystem, and the
  cause is upstream in `App`.

**Severity: Medium.** Cohesive, documented, and small once tests are discounted;
its defects are arm boilerplate and a half-finished migration, not entanglement.
Blast radius is contained to the key→state path.

**Proposed decomposition**

Not a size problem — do not split for length. The one structural move is to stop
the layer cut and unit 1's feature cut from crossing. After unit 1's split, a
Tasks-board change would touch `command.rs` (the arm), `app/task_board.rs` (the
method) and `bin/src/keys.rs` (the keymap). Co-locating each feature's arms with
its methods brings that back to two.

| new module | moves from `command.rs` | ≈lines |
|---|---|---|
| `app/task_board.rs` (unit 1) | `TasksCommand` 91, `execute_tasks` 467, `focus_task_agent` 635, `promote_selected_task` 683, `resolve_project_for_cwd` 716 | 290 |
| `app/sessions_cmd.rs` (or unit 1's sessions module) | `SessionsCommand` 43, `execute_sessions` 215, `focus_selected_session` 368, `spawn_agent_here` 444 | 300 |

`command.rs` keeps the layer contract only — `Command`, `GlobalCommand`,
`Effect`, `execute`, `execute_global`, `metrics_scan_effect` — about 120 lines,
which is what makes it readable as *the* description of the key→state boundary.

Three follow-ups, in this order:

1. Give the fallible `App` methods a uniform outcome type (`Result<String,
   String>`, or a small `Outcome`) instead of `bool` / `Option<String>` plus
   `tasks.persistence_error`. Each affected arm collapses to one line, the
   failure text moves next to the code that knows the reason, and the
   `take_persistence_error` side channel disappears.
2. Finish the port: add `Command::Projects` and `Command::Metrics`, move the
   remaining arms out of `bin/src/keys.rs`, and decide explicitly whether
   buffer-edit keys are commands. One dispatch mechanism, not two.
3. Only then consider `Option<Effect>` or a builder over `Vec<Effect>`; it is
   cosmetic next to the first two.

**Notes for unit 12**

- Status text is formatted in both `app/mod.rs` and `command.rs` for the same
  actions — no single owner of user-facing messaging.
- `App::new()` touches the on-disk store, so this layer's tests are unix-gated
  and `$HOME`-redirecting.
- The keys→command port is half done; `bin/src/keys.rs` still holds live arms.

## 3. `lib/src/ui/popups.rs` (2071 code lines / 2472 total)

**What it is.** Every modal the TUI draws, in one file: 21 render functions, 19
of them popup entry points with the identical `(frame, area, app)` signature,
which `ui/mod.rs` selects between with a `match app.view` over the `View` enum.
It is the render half of the modal state that section 1 found scattered across
`View` plus ~10 loose `Option` fields. 401 lines of tests sit in seven separate
gated modules (`popups.rs:2072` onward) and cover 5 of the 21 renderers.

**Problems**

- `popups.rs:299`, `359`, `1073`, `1160`, `1260`, `1331` — six single-line input
  popups, ~490 lines, are copy-paste variants of one widget. `render_task_input`
  (`1073–1153`) and `render_task_tags` (`1260–1329`) are identical for ~45 of
  their ~80 lines: same width, same `wrap_width = desired_w - 6`, same
  `div_ceil` row estimate, same `Clear`/block/inner/zero-size guard, same
  four-line body, same footer span vector. They differ in a title, a hint, one
  footer verb, and one prefix glyph — `"  + "` green (`1140`) versus `"  # "`
  cyan (`1316`). The code says so itself: `popups.rs:1155–1157` documents the
  third one as "Same shape as the task input popup".
- `popups.rs:19`, `148`, `464`, `592`, `654` — five list pickers, ~640 lines,
  repeat one chrome: filter line with a `▎` cursor, right-aligned
  `matched/total` count, the `start = selected.saturating_sub(visible - 1)`
  scroll window, `highlight_spans` over fuzzy match indices, and an inverse
  selected-row bar. `popups.rs:651–653` again admits it — "Same chrome and
  interaction as the model picker". `render_task_link_picker` (`654–848`, 195
  lines) is the largest function in the file and is mostly that shared chrome.
- `popups.rs:20`, `300`, `465`, `593`, `655`, `1405`, `1519` — seven renderers
  open with `let Some(x) = app.<field>.as_ref() else { return; }`. This is the
  seam from section 1 seen from the render side: the dispatcher has already
  proved the `View` variant, and each renderer then re-proves the payload,
  because the two are independent fields. The cost is a dead branch per modal
  and a failure mode that renders a blank screen instead of failing to compile.
  Folding each payload into its variant deletes all seven guards and lets these
  functions take `&ModelPickerState` instead of `&App`.
- `popups.rs:1401`, `1515`, `1690` — three renderers take `&mut App` so they can
  clamp scroll mid-draw; `render_state_debug` writes `app.render.state_debug_scroll`
  at `1553–1554`. The clamp genuinely needs the viewport, which only exists
  during layout, so draw is not a pure function of state: these three cannot be
  rendered against a fixed `App`, and any nav method reading that scroll depends
  on a draw having happened first.
- `popups.rs:363`, `470`, `597`, `662`, `1078` — popup widths are the literals
  80, 60, 64, 72, 70 with no named constant, and the height formula
  `5 + rows, min 9` is written out at `377`, `1088`, `1173`, `1272`, then
  hand-specialized to a bare `9` at `1336`. The `- 6` at `1081` encodes border
  plus prefix width; change the chrome and five sites must be found and agreed.
- 169 raw `Color::` literals against 25 uses of the four palette constants
  imported at `popups.rs:12`, including 28 hard-coded RGB triples.
  `Color::Rgb(90, 90, 100)` is written four times (`231`, `555`, `748`, `1636`),
  `Color::Rgb(200, 200, 210)` three (`114`, `236`, `996`),
  `Color::Rgb(230, 180, 90)` twice (`1701`, `1853`). A palette module exists and
  is mostly bypassed, so "dim gray" has four definitions that can drift apart.
- `popups.rs:1583`, `1801`, `1889`, `1919`, `1935`, `1982`, `1996`, `2005`,
  `2036`, `2049` — ~490 lines that format conversation content (transcript
  bullets, tool calls, thinking blocks, preview parsing, turn stats) and never
  draw a popup. Two generic text utilities, `wrap_text` (`849`) and
  `wrapped_total_rows` (`909`), are also parked here. The file holds three jobs.
- Coverage is lopsided: the 401 test lines exercise `wrap_text`,
  `wrapped_total_rows`, and 5 of 21 renderers; the 195-line task-link picker is
  tested only for one status-chip color (`popups.rs:2255`).

**Severity: High.** Largest remaining file by code; roughly 1130 of its 2071
lines are two copy-paste families that one widget each would collapse; and it is
the second face of the `View`/payload seam, so it has to move together with
section 1's fix or the two sides will re-diverge.

**Proposed decomposition**

Split by job first — it is mechanical and makes the duplication sit side by side
where it can be extracted.

| new module | moves from `popups.rs` | ≈lines |
|---|---|---|
| `ui/popups/pickers.rs` | `render_folder_picker` 19, `render_places_picker` 148, `highlight_spans` 275, `render_model_picker` 464, `render_agent_picker` 592, `render_task_link_picker` 654 | 660 |
| `ui/popups/inputs.rs` | `render_gh_create_input` 299, `render_prompt_input` 359, `render_task_input` 1073, `render_task_attach_input` 1160, `render_task_tags` 1260, `render_rename_session` 1331 | 490 |
| `ui/popups/transcript.rs` | `build_state_debug_content` 1583, `build_live_tail_content` 1801, `render_turn_stats` 1889, `is_placeholder_preview` 1919, `parse_preview` 1935, `render_prompt_block` 1982, `render_asst_bullet` 1996, `render_tool_bullet` 2005, `render_thinking` 2036, `push_bullet_block` 2049 | 400 |
| `ui/text.rs` | `wrap_text` 849, `wrapped_total_rows` 909 + their two test modules 2072/2106 | 140 |

`ui/popups/mod.rs` keeps the five one-off modals — todo panel `923`, tmux pane
`1401`, confirm close `1441`, state debug `1515`, live tail `1690` — at roughly
380 lines.

Then the extraction that actually removes code:

1. One `InputPopup { title, hint, prefix, prefix_style, footer_verb, buffer }`
   widget. The six input renderers become a handful of lines each: ~490 lines
   down to ~150, and the sizing formula gets one definition instead of five.
2. One `FilterList` widget owning the filter line, match count, scroll window
   and selection bar, parameterized by a per-row span builder. The five pickers
   shrink to their row formatting: ~660 down to ~300.
3. Move the repeated RGB triples into `ui/palette.rs` and use the constants.

Sequencing: do this after — or at least designed against — section 1's fold of
modal payloads into their `View` variants. Extracting a widget that still takes
`&App` would bake the seven `as_ref() else return` guards into the new
abstraction and make the later fold harder than it is today.

**Notes for unit 12**

- The modal payload seam has two faces: `View` + loose `Option`s in `app/mod.rs`,
  `as_ref() else return` guards in seven renderers here.
- `ui/palette.rs` exists but 169 raw `Color::` literals bypass it, several
  repeated verbatim across the file.
- Draw is not pure: three renderers take `&mut App` to clamp scroll against the
  viewport during layout.

## 4. `lib/src/orchestrator/mod.rs` (1382 code / 1996 total) + `orchestrator/prompts.rs` (511 / 720)

**What it is.** The domain core: on-disk layout for projects and tasks, the
`TaskState` record and its status machine, the locked read-mutate-write store,
the project registry, and — in `prompts.rs` — the prompt that briefs an
orchestrator agent plus the three entry points that launch one. Everything above
it reaches through this module; section 1 counted 39 `crate::orchestrator::`
call sites in `app/mod.rs` alone.

**Size correction.** `mod.rs` has *two* test modules, at `805` and `1578`, so
the code is lines 1–804 **plus** 1000–1577 — about 1382 lines, not the 804 the
"first `cfg(test)` line" rule reports. A mid-file test module makes that rule a
floor rather than a boundary. `prompts.rs` at 511/720 is correct.

**Problems**

- `mod.rs:468–536` — `TaskState::new` and `TaskState::new_backlog` are
  identical across all 29 fields except `status` (`485` vs `520`): 34 lines
  duplicated to vary one value. `new_personal` (`541–548`) already shows the
  right shape, delegating to `new` and overriding four fields.
- `prompts.rs:330–392` and `prompts.rs:406–510` — `start_backlog_task` and
  `restart_task` are the same claim-first protocol written twice: check the
  precondition, claim under the lock with a `lost_race` flag, launch, roll the
  claim back behind a "still ours" guard, then record tmux and agent identity in
  a second locked update. They have already diverged where it matters most —
  the rollback restores `Backlog` unconditionally at `371`, but restores a
  captured `claimed_from` plus the old tmux at `478–484`. This is the
  concurrency protocol; a copy that drifts here strands orchestrators.
- `prompts.rs:68–263` — `build_orchestrator_prompt` is 196 lines holding a
  single `format!`. The orchestrator's entire operating manual, including the
  `cc-hub` subcommands it is told to run, is a string literal, and nothing links
  it to the CLI in `bin/src/cli/` at compile time. One test checks that ids
  substitute (`prompts.rs:564`); the instructions themselves are unverified.
- `mod.rs:356–466` — `TaskState` carries 29 fields, each with a serde
  attribute, spanning identity, project binding, board metadata, agent runtime
  refs, orchestrator runtime refs, and history. One struct serves both personal
  and orchestrated tasks, so `require_project()` (`mod.rs:567`) exists to
  recover at runtime a distinction the type does not express.
- `mod.rs:53–107` — 11 path helpers, every one returning `Option<PathBuf>`
  because `cc_hub_home()` may be `None`. That `Option` then propagates into
  every caller of every derived path, for a condition that is fixed at startup.
- `mod.rs:745–803` and `mod.rs:1000–1046` — six named entry points over one
  implementation (`try_update_task_state_inner`), varying on touch/no-touch,
  project/personal, and try/infallible. The layering is honest — each wrapper is
  genuinely one line, not copy-paste — but it is a wide surface for callers to
  choose wrongly from, and `_for` / `_no_touch` / `try_` suffixes encode the
  axes in names rather than parameters.

**Severity: Medium.** This is well-tested domain code with a real locking
discipline and careful comments; its defects are two local duplications, a wide
API surface, and one oversized struct — not entanglement. The claim-first
duplication is the part worth doing first, because divergence there is a
concurrency bug rather than a readability cost.

**Proposed decomposition**

`mod.rs` is four cohesive layers stacked in one file; the seams are already
visible in the ordering.

| new module | moves from `mod.rs` | ≈lines |
|---|---|---|
| `orchestrator/types.rs` | `TaskStatus` 181, `TaskKind` 250, `TaskPriority` 264, `Worker` 290, `MergeOutcome` 304, `MergeRecord` 323, `Artifact` 334, `TodoItem` 350, `TaskState` 356 + constructors 467 | 400 |
| `orchestrator/store.rs` | read/write/lock/validate 585–804, update wrappers 1000–1046, `status_transition_tests` 805 | 270 |
| `orchestrator/registry.rs` | `Project` 1187, `ProjectsFile` 1197, load/save/lock/update 1213–1299, `ensure_project_registered` 1300, `remove_project` 1325 | 230 |
| `orchestrator/paths.rs` | path helpers 53–107, id and time helpers 108–178 | 130 |

`mod.rs` keeps cleanup (`1047–1186`), `delete_task` (`1415`),
`list_task_states` (`1515`) and the re-exports — roughly 330 lines. Move each
test module with the code it covers.

Then, in order:

1. Extract the claim-first protocol into one function parameterized by the
   precondition, the claim mutation, and the rollback. `start_backlog_task` and
   `restart_task` keep only what actually differs: which statuses are
   restartable, and whether an old tmux gets killed after a successful spawn.
2. Collapse `new`/`new_backlog` into one constructor taking the initial status,
   or derive `Default` (all 29 fields already carry serde attributes) and
   override, as `new_personal` does.
3. Optional: move the prompt template into an `include_str!` file so the
   orchestrator's operating manual can be read and diffed as text rather than as
   a 196-line format argument.

**Notes for unit 12**

- Mid-file test modules mean the "first `cfg(test)`" size rule is a floor;
  `mod.rs` is 1382 code lines, not 804.
- The orchestrator's operating manual is a string literal with no compile-time
  link to the CLI it instructs.
- The claim → launch → rollback concurrency protocol is duplicated across two
  functions and has already diverged.

## 5. `bin/src/main.rs` (1415 code lines / 1489 total) + `bin/src/keys.rs` (1055 / 1055)

**What it is.** The binary crate's entry point and its key dispatcher. `main.rs`
parses argv, hands off to `cli::` or runs the TUI: it owns logging setup, the
panic hook, terminal enter/restore, the `ScanMsg` protocol, six mpsc channels,
ten `tokio::spawn` background workers, and the render/input loop. `keys.rs` is
the `(View, KeyCode)` match that `run()` used to hold inline; it calls back into
`main.rs` helpers (`spawn_dispatch`, `popup_pane_size`, `open_path_detached`) and
forward into `effects::apply_effect`. Nineteen top-level fns in `main.rs`, four
in `keys.rs`. `run()` is 646 lines (46% of `main.rs`); `handle_key` is 951 lines
(90% of `keys.rs`). Across `keys.rs` and its `keys/tasks.rs` sibling there are
163 key arms: 54 map to a `Command`, 109 still mutate `App` in place. An event
loop is *supposed* to be long, so `run()`'s length is not by itself the charge
here — what it owns is.

**Problems**

- `keys.rs:105–1055` — one function is the file. `handle_key` holds 87 match
  arms in 951 lines and takes 11 parameters behind an
  `#[allow(clippy::too_many_arguments)]` at `keys.rs:104`. Every input feature,
  from folder picking to task deletion, is edited in the same function body.
- `keys.rs:118` vs `keys.rs:135` — two dispatch styles run side by side, and the
  port is a third done. 54 arms build a `Command` and go through
  `App::execute` + `effects::apply_effect` (26 in `map_sessions_command`
  `keys.rs:45–98`, 28 in `keys/tasks.rs:10–58`); 109 arms mutate `App` directly
  (87 in `handle_key`, 22 in `keys/tasks.rs:60–121`). What stayed behind is not
  a random remainder — it is whole categories: all 17 Projects Grid arms, all 4
  Metrics Grid arms, all 16 modal views (`FolderPicker` 12 arms, `TodoPanel` 11,
  `ProjectsResult` 8, `PromptInput`/`GhCreateInput`/`Backlog` 5 each,
  `RenameSession`/`LiveTail` 4, `TmuxPane`/`StateDebug`/`Popup` 3,
  `ConfirmClose`/`ModelPicker`/`AgentPicker`/`TaskLinkPicker` 1 each), plus every
  buffer-edit key. Sessions, Global and the Tasks board are the only converted
  surfaces.
- `keys.rs:310–448` — a single match arm runs 139 lines. It resolves a task's
  agent kind, picks a session store, spawns or attaches a tmux pane, and falls
  back to a log viewer. Four more arms are oversized: `keys.rs:585–675` (91),
  `keys.rs:162–224` (63), `keys.rs:948–993` (46), `keys.rs:479–517` (39).
- `keys.rs:495`, `keys.rs:587`, `keys.rs:597`, `keys.rs:640`, `keys.rs:963` —
  domain I/O inlined into the key layer. 33 `cc_hub_lib::` calls sit inside
  `handle_key`, 19 of them `orchestrator::` (`start_backlog_task`,
  `remove_project`, `delete_task`, `restart_task`,
  `spawn_orchestrator_for_new_task`), the rest `agent::`, `clipboard::`,
  `scanner::`, `gh::`. Pressing a key writes the store and spawns processes, so
  no arm can be tested without a live `$HOME`, tmux, and a terminal. `keys.rs`
  has zero test modules.
- `main.rs:116–228` and `main.rs:229–363` — `queue_missing_titles` and
  `queue_missing_task_titles` are the same 113/135-line function twice. Identical
  four-`Arc` signature, identical inflight-sentinel insert, semaphore permit held
  across `spawn_blocking`, active-set mark/unmark, and success/failure cooldown
  write-back. Only four things differ: the collection walked, the skip predicate,
  which text field feeds the titler, and where the title lands. The comments
  explaining *why* the protocol is shaped this way survive only in the first
  copy, so the second is already the harder one to maintain correctly.
- `main.rs:764–1073` — `run()`'s first 310 lines are pure wiring before the loop
  starts: legacy-board migration, ack-tracker swap, four title `Arc`s, six mpsc
  channels, and ten `tokio::spawn` workers (usage refresh, backlog triage,
  auto-review, watcher fan-in, session scanner, project scanner, metrics). None
  of it is loop logic, and none of it can be exercised without running the TUI.
- `main.rs:1089–1197` — the loop body owns render *policy* and telemetry, not
  just sequencing: mouse-capture toggling, the dirty/clock redraw gate, per-frame
  byte accounting, the CPR terminal round-trip probe (`main.rs:1160–1180`), and
  OSC 52 clipboard replay out of the tmux pane. `main.rs` carries 32 `log::`
  calls; the stall report at `main.rs:1392` is a fifth concern layered on top.
- `keys.rs` — 49 `set_status` calls, the largest concentration of user-facing
  status text in the binary. Wording, truncation and error phrasing are decided
  arm by arm.
- `main.rs:1420–1489` — the only tests cover `extract_claude_config_dir` and
  `expand_tilde` (74 lines, 4 cases). `run()`, `apply_scan_msg`, and both title
  queues have none, which is a consequence of the two bullets above, not an
  oversight.

**Severity: High.** `keys.rs` hits the rubric twice — every input feature change
lands in the same 951-line function, and 33 inlined domain calls make any key
untestable without a live environment; `main.rs` on its own would be Medium.

**Proposed decomposition**

Cheap, and there is precedent in the tree: `keys/tasks.rs` already splits one
view group into a sibling module with a `pub(super) fn` pair, so each new group
costs zero visibility changes. Arms are keyed on `View` first and the `View` sets
below are disjoint, so a per-group `handle` returning `bool` preserves order;
`Grid` is the only view spanning groups, and its Projects/Metrics arms are
already separated by `on_projects` / `on_metrics` guards.

| new module | moves from `X.rs` | ≈lines |
|---|---|---|
| `bin/src/keys/projects.rs` | `keys.rs` 136–460 (Grid+ProjectsResult arms), 518–547 (Metrics arms) | 360 |
| `bin/src/keys/pickers.rs` | `keys.rs` 684–816 (FolderPicker), 870–923 (Model/Agent/TaskLink pickers) | 190 |
| `bin/src/keys/panels.rs` | `keys.rs` 461–517 (Backlog), 548–584 (StateDebug, TmuxPane), 994–1051 (TodoPanel, Popup, LiveTail) | 150 |
| `bin/src/keys/text_input.rs` | `keys.rs` 817–869 (GhCreateInput), 924–993 (RenameSession, PromptInput) | 120 |
| `bin/src/keys/confirm.rs` | `keys.rs` 585–683 (ConfirmClose: delete, remove project, restart) | 100 |
| `bin/src/titles.rs` | `queue_missing_titles` `main.rs:116`, `queue_missing_task_titles` `main.rs:229`, collapsed onto one protocol | 150 |
| `bin/src/runtime.rs` | `main.rs` 764–1073: channel construction and the ten background workers | 310 |
| `bin/src/startup.rs` | `main.rs` 16–115 (`hot`, frame counter, `log_loadavg`), 479–580 (`init_logging`, `restore_terminal`, `install_panic_hook`, `extract_claude_config_dir`, `expand_tilde`) | 200 |
| `bin/src/scan_apply.rs` | `enum ScanMsg` `main.rs:326`, `apply_scan_msg` `main.rs:663` | 140 |
| `bin/src/actions.rs` | `main.rs` 364–478: `open_path_detached`, `dispatch_picked_cwd`, `spawn_dispatch`, `pick_from_folder_picker`, `popup_pane_size` — the `pub(crate)` helpers `keys.rs` calls | 110 |

`keys.rs` keeps the `KeyOutcome` type, `map_command`, and the group fan-out
(≈140 lines). `main.rs` keeps `main()`, `run_no_tui()`, and `run()`'s loop
(≈420 lines) — the loop stays long, and should.

Follow-ups, type-level:

1. Introduce a `Runtime` struct owning the six channels, the four title `Arc`s,
   and the metrics kick, and pass it as `handle_key`'s single context parameter.
   Kills the 11-argument signature, the `too_many_arguments` allow, and the
   `spawn_metrics` closure threaded through purely because it captures `run()`
   locals (`keys.rs:100–103`).
2. Finish the `Command` port for the remaining 109 arms — `ProjectsCommand` and
   a modal-editing command family — so the key layer's whole type is
   `KeyEvent -> Option<Command>` and every `orchestrator::` call moves behind
   `Effect`. Kills the class where a key handler writes the store, which is what
   makes input behavior untestable today.
3. Make the title queue generic over a small trait (id, source text, skip
   predicate, write-back) instead of two copies of the protocol. Kills silent
   divergence in the inflight/cooldown/permit handling, which has already lost
   its explanatory comments in one copy.

Do 1 before 2: the 11-parameter signature is precisely what makes moving arms out
of `handle_key` expensive. 3 is independent and can land at any time.

**Notes for unit 12**

- 54 of 163 key arms are `Command`-mapped (33%); Projects, Metrics and all 16
  modal views stayed legacy. This is the count the layer-vs-feature ruling needs.
- 49 `set_status` calls in `keys.rs` — status text has no owner here either.
- 19 `orchestrator::` calls inline in the key handler: domain I/O in the UI layer
  again, this time in the input path rather than the render path.
- `bin/src/keys/tasks.rs` is a working in-tree precedent for splitting a
  view-keyed match into sibling modules.

## 6. `lib/src/metrics.rs` (1400 code lines / 1481 total) + `lib/src/ui/metrics.rs` (609 / 609)

**What it is.** The cost/usage analyzer and its renderer. `metrics.rs` walks
every agent transcript on disk, parses three JSONL dialects, and folds them into
one `MetricsAnalysis` (`metrics.rs:225`, 16 fields over 15 public types); it is
called from `ui/mod.rs`, `app/metrics_view.rs`, `app/mod.rs` and `bin/src/main.rs`.
`ui/metrics.rs` turns that struct into a scrollable `Vec<Line>` and 25 bar-chart
colors. **The layer split between them is clean and worth saying so:**
`metrics.rs` contains one `format!` call, zero `ratatui` imports and no code
dependency on `crate::ui` (the single mention at `metrics.rs:255` is a doc link);
`ui/metrics.rs` imports four analysis types read-only. Two apparent copy-paste
families here are honest one-line wrapper fans and are *not* charged below:
`analyze` over `analyze_with_progress` (`metrics.rs:978–984`) and
`parse_session_file`'s three-way dispatch (`metrics.rs:342–347`).

**Problems**

- `metrics.rs:985–1313` — `analyze_with_progress` is a 329-line function with 14
  mutable accumulators declared before its main loop (`metrics.rs:998–1014`) and
  a 213-line loop body (`metrics.rs:1016–1228`) that itself opens 10 more
  per-session accumulators. Nine independent statistics — model, project, day,
  tool, shell, MCP, interruption, growth, peak context — are computed in one
  interleaved pass, so none can be changed, tested, or read in isolation.
- `metrics.rs:356–483`, `metrics.rs:484–723`, `metrics.rs:724–883` — three
  parsers with a shared envelope and no shared code. The record-schema middle is
  genuinely per-format and earns its lines, but each one re-implements the same
  prologue (open, `BufReader` line loop, blank skip, `serde_json` parse,
  timestamp tracking, cwd→project) and each ends with the *identical* 11-field
  `ParsedSession` construction (`metrics.rs:470–482`, `712–723`, `870–882`). The
  Codex parser even admits the copy in a comment, citing Pi.
- `metrics.rs:480` — the envelope has already diverged, silently. Codex passes
  `in_flight_tool_use_ids: HashSet::new()` while Claude (`metrics.rs:696`) and Pi
  (`metrics.rs:735`) both compute it, and `metrics.rs:1091` gates interruption
  and orphan-cost counting on that set. Codex sessions therefore can never be
  reported as interrupted. The file documents its other Codex caveat carefully
  (`metrics.rs:348–355`, token accounting) — this one is undocumented, which is
  exactly what a copied envelope produces.
- `metrics.rs:39–63` — `pricing_for` hardcodes nine model ids and their prices in
  a match, with `DEFAULT_PRICING` (`metrics.rs:32`) as a silent fallback. Every
  cost in the report is wrong, with no warning, for any model released after this
  arm was last edited, and nothing counts how many calls fell through.
- `metrics.rs:1401–1481` — the only tests (five, all about in-flight tool
  detection) drive the parser through a `tempfile::NamedTempFile`
  (`metrics.rs:1412–1418`) because parsers take `&Path` and open the file
  themselves. The Codex and Pi parsers have zero tests, and so does every one of
  the nine aggregations.
- `ui/metrics.rs:105–463` — `build_metrics_content` is 359 lines, 59% of its
  file: ten report sections appended to one `Vec<Line>` while a parallel
  `row_lines: Vec<usize>` records which pushed line each selectable row landed
  on. The selection index is maintained by hand across ten sections, so adding or
  reordering a section silently shifts selection.
- `ui/metrics.rs:186–192`, `216–237`, `254–259` — aggregation in the view.
  `by_model` is sorted, the 30-day series is rebuilt, and four `max` scans are
  recomputed on every frame from data `MetricsAnalysis` could have carried
  precomputed. `top_projects` is already precomputed (`metrics.rs:240`), so the
  boundary exists — it is just applied inconsistently.
- `ui/metrics.rs:305–347`, `350–381`, `384–435` — the interruption, peak-context
  and token-spike sections are the same 40-line shape three times: header,
  empty-state span, then a loop pushing `row_lines.len()` and building a row.
  ~130 lines that differ only in which finding fields print.
- `ui/metrics.rs:16` — `render_metrics_body` takes `&mut App` and writes
  `app.render.metrics_scroll` during draw (`ui/metrics.rs:52–70`) to clamp the
  selected row into view. 25 raw `Color::Rgb` literals sit alongside a single
  two-constant import from `ui/palette.rs` (`ui/metrics.rs:8`).

**Severity: Medium.** `metrics.rs` earns it on the rubric's Medium clause
exactly — long functions and a triplicated parse envelope, but one real job and
contained blast radius; `ui/metrics.rs` alone would be Low, and the clean
compute/render boundary is why neither is High.

**Proposed decomposition**

Cheap on both sides. `metrics.rs` becomes a directory whose public surface is
already a single struct, so every consumer keeps importing `crate::metrics::*`
unchanged; only three helpers (`ToolUse`, `AssistantCall`, `ParsedSession`) cross
the new internal boundaries and all three are private. `ui/metrics.rs` has no
callers except `ui/mod.rs`.

| new module | moves from `X.rs` | ≈lines |
|---|---|---|
| `metrics/aggregate.rs` | `analyze` `metrics.rs:978`, `analyze_with_progress` `metrics.rs:985`, `extract_bash_commands` `metrics.rs:1314`, `score_growth` `metrics.rs:1370` | 340 |
| `metrics/parse/claude.rs` | `parse_claude_session_file` `metrics.rs:484`, plus the five tests at `metrics.rs:1401` | 240 |
| `metrics/types.rs` | `Tokens` `metrics.rs:77` through `MetricsAnalysis` `metrics.rs:225`, `selectable_sessions` `metrics.rs:258` | 200 |
| `metrics/parse/pi.rs` | `parse_pi_session_file` `metrics.rs:724` | 160 |
| `metrics/parse/codex.rs` | `parse_codex_session_file` `metrics.rs:356` | 130 |
| `metrics/parse/mod.rs` | `ToolUse`/`AssistantCall`/`ParsedSession` `metrics.rs:245–341`, `parse_session_file` `metrics.rs:342`, the shared envelope | 110 |
| `metrics/pricing.rs` | `ModelPricing` `metrics.rs:25`, `DEFAULT_PRICING` `metrics.rs:32`, `pricing_for` `metrics.rs:39`, `strip_date_suffix` `metrics.rs:65`, `cost_of` `metrics.rs:97` | 90 |
| `metrics/discover.rs` | `project_name_from_cwd` `metrics.rs:884`, `discover_session_files` `metrics.rs:892` | 90 |
| `ui/metrics/charts.rs` | `ui/metrics.rs` 144–277 (cost breakdown, by-model, daily sparkline, top projects), `MetricsStyles` 458, `render_bar_chart_section` 464 | 190 |
| `ui/metrics/findings.rs` | `ui/metrics.rs` 305–435: interruptions, peak context, token spikes | 130 |
| `ui/metrics/colors.rs` | `tool_color` `ui/metrics.rs:517`, `selection_row_style` `ui/metrics.rs:537`, `model_color` `ui/metrics.rs:598` | 90 |

`metrics/mod.rs` keeps only re-exports (≈30 lines). `ui/metrics/mod.rs` keeps
`render_metrics_body`, the `build_metrics_content` spine that calls the section
builders, `section_header` and `format_session_row` (≈200 lines).

Follow-ups, type-level:

1. Give the three parsers one envelope — a `parse_jsonl(path, &mut impl
   RecordSink) -> ParsedSession` helper, or a trait whose only required method is
   the per-record match. Each agent then contributes its schema and nothing else.
   Kills the divergence class already realized at `metrics.rs:480`, where a field
   that gates interruption reporting is populated by two of three parsers.
2. Make each of the nine statistics an accumulator type with `observe(&call)` and
   `finish()`, and have `analyze_with_progress` fold the call stream through a
   list of them. Kills the 14-binding preamble and makes each statistic testable
   against a synthetic call list, without touching disk.
3. Move pricing into `config.rs` (already read at `metrics.rs:986` for the same
   analysis) and have `MetricsAnalysis` carry a count of calls that fell through
   to `DEFAULT_PRICING`. Kills silently-wrong totals for unrecognized models.

Do 1 before 2: the accumulators in 2 consume `AssistantCall`, whose three
producers must agree on what they populate first. 3 is independent.

**Notes for unit 12**

- Confirms section 3's smell in a second file: `ui/metrics.rs` has 25 raw
  `Color::Rgb` literals against 2 imported `ui/palette.rs` constants, and its one
  renderer takes `&mut App` to clamp scroll during draw. Unit 7 makes three.
- Adding a fourth agent means editing three places in one file:
  `discover_session_files` `metrics.rs:892`, `parse_session_file` `metrics.rs:342`,
  and a new parser. `AgentKind` fan-out is a repo shape, not a metrics quirk.
- Two wrapper fans here were checked and cleared, not charged — the
  parallel-outline trap is real in this file and the verdict was "honest".

## 7. `lib/src/ui/sessions.rs` (857 code lines / 1245 total) + `lib/src/ui/projects/task_cards.rs` (449 / 1198) + `lib/src/ui/projects/result_popup.rs` (382 / 767)

**What it is.** The three card-and-popup renderers of the two main tabs.
`sessions.rs` draws the Sessions grid (`ui/mod.rs:72`) and the session detail
popup (`ui/mod.rs:78`): 16 functions, 388 test lines, all of them on
`render_card`. `task_cards.rs` draws the Projects board cards in two variants,
called from `board.rs:211` and `:223` and nowhere else; it holds the heaviest
test ratio in the table, 749 lines to 449 of code. `result_popup.rs` is one
356-line function (`ui/mod.rs:93`) plus 385 test lines. Acquittals: the shared
helpers are real and are used — `ctx_bar`, `ctx_color`, `state_indicator`,
`state_color`, `centered_rect`, `popup_block` all come from `ui/common.rs`, the
seven agent/badge helpers from `projects/agents.rs`, and `result_popup.rs`
delegates all five artifact bodies to `projects/cards.rs`, so no wrapper fan
here is fake. `sessions.rs:436–644` is already six pure `(&SessionInfo, usize)
-> Line` builders, and `task_cards.rs` touches no `App` at all — which is why it
is the best-tested file in the unit. Its 749 test lines render through the two
public functions into a `TestBackend` and assert on buffer rows
(`task_cards.rs:818`, `:844`), so they survive internal extraction and break
only on a signature change. `ls lib/src/ui/projects` also shows `cards.rs`,
`chips.rs` and `diff.rs` alongside the four files unit 13's inventory missed;
none of the seven is in any unit.

**Problems**

- `task_cards.rs:24–190` and `:213–428` — two renderers, 383 of the file's 449
  code lines, are one card written twice: same `(border_type, border_color)`
  pick from `selected`, same title-span vector with icon + `task_card_header_text`,
  same `Block`/`inner`/`inner.height == 0` guard, same `Paragraph::new(lines)
  .wrap(...)` tail, same badge footer. The footer has already drifted. Six spans
  build in both; four disagree: the clock glyph is `LABEL_GRAY` at `:125` and
  `META_GRAY` at `:387`, the age text `FAINT_TEXT` at `:126` and a raw triple at
  `:388`, the artifact count two different purples (`:131`, `:401`), the todo
  count `FAINT_TEXT` versus another raw triple (`:137`, `:407`). Only the
  tool-use badge still matches. Every board-card change is two edits and the
  styling proves nobody makes both.
- `result_popup.rs:27–382` — one function, seven jobs: status/title mapping
  (`36–56`), header and note headline (`73–115`), artifact ordering (`117–122`),
  card heights and scroll clamping (`124–246`), the placeholder canvas
  (`248–276`), per-card overlay clipping (`279–360`), footer (`361–382`). The
  arithmetic in `124–246` — canvas tops, expanded overscroll budget, the
  `canvas_scroll` versus `scroll` split at `:246` that three comment blocks
  defend — is pure integer math with no unit-test seam, so its 385 test lines
  all pay for a `TestBackend` render to assert one row.
- `sessions.rs:230–435` — `render_card` is 205 lines, and `239–367` of them are
  chrome: border color, border type, role prefix, agent badge, title text, the
  attention chip, the block, and the task-link badge with its priority corner.
  Only `368–435` assembles the body, from helpers that are already extracted.
  The largest function in the file contains no card content.
- `sessions.rs:105–115`, `:683–684`, `result_popup.rs:229–238` — four renderers
  take `&mut App` (`sessions.rs:25`, `:78`, `:645`, `result_popup.rs:27`) and
  three of them write scroll state during draw. This is the third file family to
  show it after sections 3 and 6. `result_popup.rs:329` goes further: it decodes
  an image from disk mid-draw via `ensure_image_decoded`, then mutates the cache
  at `:337`. Draw is not a pure function of state and now performs I/O.
- `sessions.rs:239–367`, `task_cards.rs:34–113`, `:227–270` — a third copy of
  the card envelope, across the file boundary. Its one realized casualty: the
  context bar. `sessions.rs:615–640` fixes the bar at 8 cells and falls back to
  icon-plus-percent when `avail > bar_cols` fails; `task_cards.rs:149–162` sizes
  it from remaining width capped at 20 and falls back below 4. Same helper, two
  width policies, so the two cards degrade differently at the same terminal
  width and neither author can see the other's threshold.
- `task_cards.rs:23`, `:212` — both renderers carry
  `#[allow(clippy::too_many_arguments)]`, at 9 and 10 parameters differing only
  by `lock_holder`. `board.rs:205–235` passes the other eight positionally in
  two structurally identical call sites; six of them are the same type family
  (`&TaskState`, `bool`, `usize`, `u64`, `bool`), so a transposition compiles.
- 90 raw `Color::` literals against 30 uses of the palette constants — 48/13 in
  `sessions.rs`, 31/13 in `task_cards.rs`, 11/4 in `result_popup.rs`. Same
  denominator section 6 used. 42 are hard-coded RGB triples and 12 are repeats:
  `Color::Rgb(140, 145, 160)` appears three times inside one function
  (`task_cards.rs:388`, `:394`, `:408`) and `Color::Rgb(180, 200, 160)` three
  times across both card renderers.
- `sessions.rs:78–184` — `render_grid` fuses four jobs in 106 lines: group
  offset accumulation, the selection auto-scroll with its two-branch fit test,
  per-cell geometry including the last-column width fixup at `:152`, and the
  paint loop. None of it is tested; the file's 388 test lines all target
  `render_card`, leaving `render_grid`, `render_popup` and the 139-line
  `build_popup_content` (`718–857`) with zero coverage.

**Severity: Medium.** `task_cards.rs` earns it — 383 of 449 code lines are one
card written twice with four styles already drifted apart — but its blast radius
stops at `board.rs`, its only caller, and each file has one real job, which is
the Medium clause verbatim. `sessions.rs` alone would also be Medium (205-line
chrome function, a second job in the detail popup); `result_popup.rs` alone
Medium (one 356-line function, no unit seam under it).

**Proposed decomposition**

`sessions.rs` splits along a seam that already exists — the pure line builders
at `436–644` never touch `App`, and the popup half shares nothing with the grid
half but the module. The `projects/` rows are cheaper still: both files already
live in a directory whose `mod.rs` re-exports their items, so new siblings cost
zero visibility changes.

| new module | moves from `X.rs` | ≈lines |
|---|---|---|
| `ui/sessions/card.rs` | `sessions.rs`: `role_prefix` 203, `render_card` 230, `activity_line` 436, `branch_line` 492, `model_line` 504, `message_lines` 528, `meta_line` 556, `footer_line` 590, plus all 388 test lines | 440 |
| `ui/sessions/grid.rs` | `sessions.rs`: `render_sessions_body` 25, `render_no_sessions` 33, `render_group_header` 41, `render_grid` 78, `spinner_frame` 185, `starting_frame` 195 | 180 |
| `ui/sessions/detail.rs` | `sessions.rs`: `render_popup` 645, `build_popup_content` 718 | 210 |
| `ui/projects/result_popup/layout.rs` | `result_popup.rs` 117–246: render order, `card_meta`, `canvas_card_tops`, expanded budget, scroll clamp | 130 |
| `ui/projects/result_popup/overlay.rs` | `result_popup.rs` 279–360: visibility clipping, `body_rect`, `body_scroll_lines`, the `CardKind` dispatch | 80 |

`ui/sessions/mod.rs` keeps the two constants and re-exports (≈30 lines).
`result_popup/mod.rs` keeps the header, the canvas assembly and the footer
(≈170). `task_cards.rs` gets no table row: splitting two functions into two
files would freeze the duplication instead of removing it. Do the follow-ups.

1. One `task_meta_row(t, sum, inner_w) -> Line` for the badge footer, replacing
   `task_cards.rs:118–163` and `:379–423`. Kills the drift class already
   realized in four of six spans, and gives the artifact/todo/tool badges one
   definition instead of two.
2. Replace the two positional parameter lists with a `TaskCardCtx<'a>` carrying
   `t`, `sessions_by_tmux`, `pr_summary`, `lock_holder`, `now_secs`,
   `titling_in_flight`. Both `#[allow(clippy::too_many_arguments)]` go away and
   the two same-shape call sites at `board.rs:211`/`:223` stop being
   transposable. With one context type the two renderers can then share the
   envelope at `:34–113`/`:227–270`, leaving only the middle rows distinct.
3. Clamp scroll before draw, not during it: give `RenderState` a
   `clamp(viewport)` called once per frame from `ui/mod.rs`, so these four
   renderers take `&App`. Kills the class where a nav key reads a scroll value
   that only a prior draw could have produced. Repo-wide — the same three
   renderers in section 3 and the one in section 6 are on this fix.

Do 2 before 1: the shared footer builder wants one argument, not six, and
writing it against the current lists means writing it twice.

**Notes for unit 12**

- Third data point on the palette: 90 raw `Color::` against 30 palette-constant
  uses here, on section 6's denominator. With sections 3 and 6 that is 259
  raw against 55 across five files — a repo shape, not a file quirk.
- Fourth, fifth, sixth and seventh `&mut App` renderer, three of them writing
  scroll during draw, and one (`result_popup.rs:329`) reading the filesystem
  during draw.
- Read this section before charging `ui/tasks.rs` with card duplication: the
  family named here is three renderers — `sessions.rs:230`, `task_cards.rs:24`
  and `:213` — and a fourth may be there.
- `projects/cards.rs`, `chips.rs` and `diff.rs` are in no unit; `diff.rs:20`
  defines `TASK_META_DIM`, a palette constant living outside `palette.rs`.

## 8. `lib/src/scanner.rs` (1206 code lines / 1511 total) + `lib/src/codex_conversation.rs` (584 / 930) + `lib/src/pi_conversation.rs` (540 / 1139) + `lib/src/pi_scanner.rs` (535 / 616) + `lib/src/codex_scanner.rs` (504 / 678) + `lib/src/conversation/state.rs` (365 / 627)

**What it is.** Everything that turns transcript files on disk into
`SessionInfo`. Three scanners walk three session roots and three transcript
parsers read three JSONL dialects: Claude's parser is the `conversation/`
directory (nine files, of which `state.rs` is in this unit), Pi's and Codex's
are one flat module each. `scanner.rs` is both the Claude scanner and the
cross-agent aggregator — `scan_sessions` (`:1091`) merges all three and is the
entry point for `bin/src/main.rs:646`, `ops/worker.rs:46` and `:516`. The
duplication verdict is not uniform, so the acquittals matter. The state machine
is genuinely shared and well done: `conversation/classify.rs` holds one
`classify` function and a `TranscriptDialect` trait, each backend supplies only
syntax (`state.rs:144`, `pi_conversation.rs:114`, `codex_conversation.rs:64`),
and a parity matrix asserts the dialects agree. Tool-use counting is shared too
— `tool_use_count.rs` serves all three agents from one incremental cache. Pi's
empty `blocking_tool` and `is_interrupt_marker` stubs (`pi_conversation.rs:139`,
`:143`) looked like the `metrics.rs:480` pattern and are not: the divergence is
deliberate and documented twice (`classify.rs:193–196`, `pi_scanner.rs:307`),
because Pi has no `AskUserQuestion` tool. Sizes: 296 test lines in `scanner.rs`,
345 in `codex_conversation.rs`, 598 in `pi_conversation.rs`, 261 in `state.rs`,
173 in `codex_scanner.rs`, 80 in `pi_scanner.rs`. Four `conversation/` siblings
are in no unit and in no size table: `cache.rs` (385 total), `classify.rs`
(332), `messages.rs` (276), `render.rs` (221), plus `mod.rs` (36); so are
`tool_use_count.rs` and `dir_cache.rs`.

**Problems**

- `metrics.rs:356`, `:484`, `:724` versus this unit — **the repo parses every
  agent's transcript twice, in two families that share one function.** The
  metrics parsers and the scanner parsers read the same records: comparing the
  JSON keys each side reads, all 11 of the Codex metrics parser's keys are also
  read by `codex_conversation.rs`, 16 of 19 for Claude, 16 of 19 for Pi. They
  also discover the same files twice — `metrics.rs:892` walks
  `paths::claude_home()/projects`, `paths::pi_sessions_dir()` and
  `paths::codex_sessions_dir()` itself, the same three roots the scanners reach
  through `scanner.rs:31–35`, `pi_scanner.rs:37` and `codex_scanner.rs:40`. The
  only code the two families share is `parse_timestamp_ms`, imported at
  `metrics.rs:15`. The cost is already paid: the bug at `metrics.rs:480`, where
  the Codex parser passes an empty in-flight tool-call set and Codex sessions
  can therefore never be reported as interrupted, is *solved in the other
  family* — `codex_conversation.rs:279–305` builds exactly that set, pairing
  `call_id` on `is_tool_call` against `is_tool_call_output`. The repo knows how;
  the copy that needed it could not see it.
- `conversation/io.rs:31`, `pi_conversation.rs:44`, `codex_conversation.rs:139`
  — `read_jsonl_tail_for_state` is written three times and is otherwise
  identical: same `INITIAL` 64 KiB, same `MAX` 4 MiB, same doubling loop, same
  three exit conditions. The three differ in exactly one line, the predicate
  that decides "this window has enough". That predicate is a dialect question,
  and `codex_conversation.rs:162` already grew a private `role_present` for it
  — the method the trait should have had. A window-policy change is three edits
  today; a `read_jsonl_tail_until(path, pred)` makes it one.
- `pi_scanner.rs:45–122`, `codex_scanner.rs:218–283`, `scanner.rs:658–713` —
  three copies of one pipeline: read head and tail, `extract_metadata`,
  `extract_first_user_message`, `extract_current_tool`, `is_currently_thinking`,
  `extract_context_tokens`, count tool uses, build a `SessionInfo`. The three
  bodies differ only in which `*_conversation` module is named. Above them the
  same four entry points repeat per agent — `scan`, `load_detail`,
  `load_state_explanation`, `find_orchestrator_session` — dispatched by a
  three-arm `match info.agent_kind` at `scanner.rs:1169` and `:1190`. This is a
  trait with no trait, one level above the trait that exists.
- `conversation/classify.rs:196–321` — the parity matrix, which the module
  header sells as the drift net ("a backend that drifts fails the parity matrix
  below instead of silently misclassifying in production"), has no Codex cases.
  `Case` carries `claude` and `pi` fields only (`:187–192`) and `parity_matrix`
  (`:304`) asserts two dialects. The doc comments were never updated either:
  `:3` still says "(Claude JSONL, Pi JSONL)" and `:50` documents `NAME` as
  `("claude", "pi")`. The newest and least-understood backend is the one outside
  the net.
- `scanner.rs:17–1043` versus `:1044–1214` — one file, two jobs. Everything up
  to `sort_stable` is the Claude scanner: the `~/.claude` path encoding, the
  clears-history cache (`:202–315`), the orphan-JSONL index (`:358–461`), the
  raw-session reader (`:504`), `scan_claude_sessions` (`:801`, 243 lines). After
  it sits the cross-agent aggregator that calls the other two scanners. The
  aggregator is what every caller wants and it is 170 lines at the bottom of the
  repo's sixth-largest file; a Claude-parsing change and an agent-dispatch
  change collide in the same file for no reason.
- `scanner.rs:17`, `pi_scanner.rs:14`, `codex_scanner.rs:48` — `mtime_age_secs`
  is byte-identical in all three. So is `project_name` (`scanner.rs:644`,
  `pi_scanner.rs:29`, `codex_scanner.rs:32`), and so is `truncate_str`
  (`pi_conversation.rs:537`, `codex_conversation.rs:581`).
  `default_pi_agent`/`default_codex_agent` (`pi_scanner.rs:41`,
  `codex_scanner.rs:44`) differ only in an enum variant, and `read_head`
  (`codex_scanner.rs:58`) re-wraps `conversation::read_jsonl_head`. The one copy
  that has drifted is `tool_display`/`tool_brief_arg`
  (`conversation/render.rs:137` versus `pi_conversation.rs:506`): the Claude
  version truncates at 60 *bytes* with a char-boundary walk, the Pi version at
  60 *characters*, so the same tool line renders at two lengths depending on
  which agent produced it.
- `conversation/cache.rs:106`, `:150` — Claude derivations are memoized by
  mtime, so an unchanged transcript is not re-parsed on the next scan tick. Pi
  and Codex have no equivalent: `pi_scanner.rs:80–93` and
  `codex_scanner.rs:230–256` re-read and re-parse a head and a tail window per
  session per tick. `scanner.rs:1119–1122` states the asymmetry ("conversation's
  caches are Claude-only") without treating it as a gap, and `dir_cache.rs:4`
  shows the same cost was noticed and fixed for Pi's *directory* walk but not
  its parse. The scan loop's cost is bounded for one agent and linear in
  transcript size for the other two.
- `conversation/explain.rs:48–287` versus `pi_conversation.rs:373–437` and
  `codex_conversation.rs:505–563` — the state-debug explanation is 240 lines for
  Claude and about 60 for each of the others, which do not summarize the tail or
  name the blocking tool. The feature is nominally per-agent and effectively
  Claude-only; a user debugging why a Codex card says Processing gets a
  materially thinner answer, and nothing marks that.

**Severity: High.** All six grade alike, because the grade is a property of the
fan rather than of any one file: adding or changing an agent format touches six
files here plus a seventh family in `metrics.rs`, which is the "every change to
the feature touches this file" clause. `conversation/state.rs` alone would be
Medium at worst — it is 365 lines behind a shared trait with 261 lines of tests.

**Proposed decomposition**

Two of the three moves are unusually cheap because the precedent is already in
the tree: `conversation/classify.rs` shows exactly how this repo separates
per-dialect syntax from shared semantics, and `tool_use_count.rs` shows a shared
counter serving all three agents. The work is extending both patterns upward,
not inventing one.

| new module | moves from `X.rs` | ≈lines |
|---|---|---|
| `scanner/claude.rs` | `scanner.rs`: `encode_path` 45, `find_jsonl` 49, `find_jsonl_anywhere` 66, `read_clears_from_history` 226, `read_clears_uncached` 260, `resolve_jsonl_paths` 316, `OrphanIndex` 358, `read_raw_sessions` 504, `synthesize_inactive_from_jsonl` 658, `scan_orphan_jsonls` 738, `scan_claude_sessions` 801 | 900 |
| `agent/transcript.rs` | new `Transcript` trait + the one generic `build_session_info`, replacing `pi_scanner.rs:45`, `codex_scanner.rs:218` and `scanner.rs:658`'s field assembly | 130 |
| `agent/discover.rs` | `discover_session_files` `metrics.rs:892`, plus the root accessors `scanner.rs:27–43`, `pi_scanner.rs:37`, `codex_scanner.rs:40` | 120 |
| `conversation/io.rs` (extend) | one `read_jsonl_tail_until(path, pred)` replacing `io.rs:31`, `pi_conversation.rs:44`, `codex_conversation.rs:139` | 30 |
| `agent/util.rs` | `mtime_age_secs` ×3, `project_name` ×3, `truncate_str` ×2, `default_*_agent` ×2 | 40 |

`scanner.rs` keeps only the aggregator — `scan_sessions` 1091, `load_detail`
1169, `load_state_explanation` 1190, `refresh_process_liveness` 1155,
`session_pid_alive` 1138, `sort_stable` 1044, `find_orchestrator_session` 1060 —
at roughly 190 lines. The `agent/discover.rs` row supersedes section 6's
`metrics/discover.rs` row rather than adding to it.

Follow-ups, type-level:

1. Widen `TranscriptDialect` into the `Transcript` trait the code already wants:
   add `role_present` (it exists privately at `codex_conversation.rs:162`) and
   the fourteen extractors every backend defines under the same names —
   `extract_state`, `extract_metadata`, `extract_current_tool`,
   `extract_context_tokens`, `extract_messages`, `extract_token_totals`,
   `is_currently_thinking`, `explain_state` and the rest. The three scanners
   then share one generic pipeline and a new agent supplies one impl. Kills the
   class where a capability lands for one agent and silently not the others —
   the explanation depth and the cache asymmetry above are two live instances.
2. Make the metrics parsers consumers of that trait instead of a second
   implementation of it: `ParsedSession` becomes a fold over the same record
   stream the scanners already walk. This is section 6's follow-up 1 seen from
   the other side — do them as one change, or the shared envelope gets written
   twice too. Kills the `metrics.rs:480` class outright.
3. Extend the parity matrix to Codex — turn `Case` into a per-dialect map rather
   than two named fields — before either move above. It is the only test that
   would catch a refactor of this family changing what a state means.

Ordering: 3, then 1, then 2. The matrix is the safety net for 1, and 2's shape
depends on the trait 1 defines.

**Notes for unit 12**

- The `AgentKind` fan-out section 6 saw inside `metrics.rs` is repo-wide: 11
  `AgentKind::Claude` match arms in `scanner.rs` alone, and the three-arm
  dispatch repeats at `scanner.rs:1091`, `:1138`, `:1169`, `:1190`.
- `conversation/cache.rs`, `classify.rs`, `messages.rs`, `render.rs` and
  `mod.rs` are in no unit and no size table; `classify.rs` is the best-designed
  file this survey has read and should not be refactored, only extended.
- `tool_use_count.rs` and `dir_cache.rs` are also unlisted, and both are
  performance caches whose coverage across agents is uneven.

## 9. `lib/src/ops/task.rs` (739 / 844) + `lib/src/tasks.rs` (682 / 1082) + `bin/src/cli/task.rs` (614 / 1080)

**What it is.** `ops/task.rs` holds the bodies of every task verb — create,
start, orchestrate start, report, delete, gc, auto-review, plus the artifact and
todos mutators — as 17 functions called from 22 sites in five files
(`cli/task.rs` 12, `cli/orchestrate.rs` 3, `app/mod.rs` 4, `ui/tasks.rs` 2,
`ops/pr.rs` 1). `cli/task.rs` is its 21-function CLI front end. `tasks.rs` is a
different subject that landed in the same unit: `PersonalBoard`, the Tasks-tab
board, which since the task-model unification stores a plain `TaskState` with
`project_id: None` and never touches `ops::`. **Acquittal: `ops::` over
`cli::` is honest layering, not a copy-paste family.** `ops/mod.rs:1–20` states
the contract (ops take typed params and return typed results; `println!` /
`print_json` stay in the caller) and 11 of the 13 CLI verbs keep it — each is a
`parse_flags` → `require_task` → one `ops::task::` call → one `json!` literal,
39 lines at the longest. Only the two read verbs break it, which is bullet 2.
Also checked and dropped: `ops::task::task_delete`, `task_gc` and
`task_artifact_list` are one-line adapters over `orchestrator::`, but they exist
to map `io::Error` onto `OpError` for the TUI, which cannot see `CliError` —
not a redundant wrapper fan.

**Problems**

- `ops/task.rs:119–189` — `orchestrate_start` hand-rolls a spawn sequence that
  `orchestrator/prompts.rs:264` already factors out as
  `launch_orchestrator_session`, and both other spawn paths call it
  (`prompts.rs:311`, `:360`). The copy diverged on the part that matters: the
  shared helper's callers claim the task before spawning and roll the status
  back at `prompts.rs:369–373` when the spawn fails, while `ops/task.rs:161`
  writes only `orchestrator_tmux` and the agent identity — no claim, no
  rollback. This extends section 4's finding (`prompts.rs:371` vs `478–484`)
  from two sites to three; do not count it as a new smell. Evidence crosses the
  unit boundary into unit 4's file.
- `bin/src/cli/task.rs:214–307` — `task_list` reimplements
  `orchestrator::list_task_states` (`orchestrator/mod.rs:1515`), whose doc
  comment says it exists so "one corrupt task can't blind a gc / **list** pass";
  it is called from `orchestrator/gc.rs:63` and nowhere else. The two disagree
  on policy: the CLI copy filters directory entries on a `t-`/`tk-` prefix that
  no other reader applies, and reports parse failures on stderr instead of
  `log::warn`. Counting `projects_scan.rs:298` and `tasks.rs:151`, four
  implementations walk a tasks dir and parse `state.json`, with four different
  answers to what a corrupt file means. Evidence crosses into units 11 and 13.
- `ops/task.rs:207–361` — `task_report` is 154 lines, of which 100 are one
  closure passed to `try_update_task_state`. It carries four rejection guards
  (`:230`, `:240`, `:262`, `:277`), the done→Review routing at `:306`, three
  field writes and two derived writes, and threads results back out through two
  captured `Option`s (`rejection`, `locked_prev`) because the closure can only
  return `bool`. Every new task-state rule lands inside this one closure, and
  the sentinel pattern means a guard added in the wrong order silently changes
  which error the caller sees.
- `ops/task.rs:662`, `:684`, `:710` — the todos mutators take
  `project_id: &str` while the artifact mutators three functions above take
  `project_id: Option<&str>` and route through `update_task_for` (`:457`), whose
  doc comment explains that `None` means the personal store. One file therefore
  holds two incompatible answers to "can this verb address a board task", and
  only one of them is documented. A personal task can carry artifacts but its
  todos are unreachable through `ops::`.
- `lib/src/tasks.rs:470–639` — 169 lines migrating the pre-unification
  single-file `~/.cc-hub/tasks.json` board, plus ~97 test lines (`:944–1041`),
  inside the file the TUI reads on every board load. It is one-shot code that
  can never change again, and it is 25% of the file's code.
- `lib/src/tasks.rs:233–324` — six mutators (`rename`, `set_priority`,
  `set_tags`, `set_status`, `assign`, `rebind_tmux`) repeat one envelope: an
  `is_none_or` unchanged-guard, `update_personal_task(id, |s| …)`, `self.apply`,
  `Ok(true)`. Eight methods return `io::Result<bool>` on that shape. The bodies
  differ only in the guard predicate and one to five field assignments, so a new
  field means a seventh copy rather than an argument.
- `bin/src/cli/task.rs:345–419` — `task_show` reads the store directly
  (`orchestrator::read_task_state` at `:350`) and builds both output shapes
  inline, so the TUI has no way to reuse the CLI's notion of what a task's
  summary is. With `task_list` this is 167 of 614 code lines, 27% of the file,
  living outside the layer the other 73% observes.
- `lib/src/tasks.rs:151–178` — `PersonalBoard::load_result` propagates the
  `read_task_state_for` error with `?`, so one unparseable task file fails the
  whole board load. This is admitted and tested
  (`malformed_task_file_is_reported_without_replacing_it`, `:908`) and `:182`
  explains the `load()` / `load_result()` split, so it argues down — but it is
  the fourth corruption policy in bullet 2, and the only one that loses every
  task rather than one.

**Severity: Medium**, earned entirely by `ops/task.rs`: long functions and a
spawn sequence duplicated from a helper that already exists, with the blast
radius contained to the task verbs it is the single implementation of.
**`lib/src/tasks.rs` and `bin/src/cli/task.rs` are each Low alone** — both have
one real job, both are heavily tested (400 and 466 test lines against 682 and
614 of code), and neither needs splitting to be safe to work in; their bullets
are deletions and delegations, not decompositions.

**Proposed decomposition**

Cheap: `ops/task.rs` has no shared state and no `impl` block, so every group
below moves as whole functions behind `pub use`. `ops/mod.rs:22` already lists
`pub mod task;`, so an `ops/task/mod.rs` directory costs no call-site changes.

| new module | moves from `ops/task.rs` | ≈lines |
|---|---|---|
| `ops/task/report.rs` | `ReportOpts` 189, `ReportOutcome` 198, `task_report` 207 | 170 |
| `ops/task/artifacts.rs` | `looks_like_url` 451, `task_artifact_add` 473, `task_artifact_list` 547, `task_artifact_add_text` 559, `task_artifact_remove` 609 | 210 |
| `ops/task/todos.rs` | `task_todos_set` 662, `task_todos_mark` 684, `task_todos_clear` 710 | 60 |

| new module | moves from `tasks.rs` | ≈lines |
|---|---|---|
| `tasks/legacy.rs` | `mod legacy` 470, `legacy_tasks_path` 516, `legacy_item_to_state` 520, `migrate_legacy_board` 557, plus tests `:944–1041` | 170 |
| `tasks/parse.rs` | `parse_tags` 46, `QuickAdd` 70, `parse_quick_add` 80 | 70 |

`ops/task.rs` keeps the lifecycle verbs (create / start / orchestrate start /
delete / gc / auto-review), `update_task_for` and `resolve_worktree_path`, ~300
lines. `tasks.rs` keeps `PersonalBoard`, board meta and archiving, ~440.
**`bin/src/cli/task.rs` gets no rows.** Splitting it would freeze the layer
violation into a second file; the fix is to delete `task_list`'s body in favour
of `orchestrator::list_task_states` and move `task_show`'s read into
`ops::task`, after which the file is 21 uniform wrappers and wants no seams.

Type-level follow-ups:

1. Give `orchestrate_start` the same claim-and-rollback contract as
   `start_backlog_task` by routing both through `launch_orchestrator_session`
   (`prompts.rs:264`). Kills the class where a spawn path forgets a step the
   other two take — the missing rollback and the missing claim are two live
   instances.
2. Replace the `bool`-returning closure of `try_update_task_state` with one that
   returns `Result<(), OpError>`, so `task_report`'s guards return their
   rejection instead of setting a captured `Option`. Kills the class where a
   guard sets `rejection` and forgets `return false`, or vice versa.
3. Make `project_id: Option<&str>` the signature of every `ops::task` verb, with
   `update_task_for` (`:457`) the single router. Kills the class where a verb
   silently does not exist for board tasks.

Ordering: 1 first — it is the only one with a live defect behind it. 3 before 2,
because 3 rewrites the signatures 2 then changes the bodies of.

**Notes for unit 12**

- `lib/src/todo.rs` is a *third* todo store (`add`/`toggle`/`remove`/
  `clear_completed`), separate from both `TaskState.todos` and `PersonalBoard`.
  It is in no unit and no size table.
- The four tasks-dir enumerators in bullet 2 span units 9, 11 and 13; whoever
  ranks them should treat them as one finding.

## 10. `lib/src/ops/pr.rs` (983 / 1084) + `bin/src/cli/pr.rs` (495 / 1506) + `lib/src/merge_lock.rs` (375 / 680)

**What it is.** `ops/pr.rs` holds the bodies of the ten `pr` verbs — create,
approve, request-changes, reopen, comment, close, merge, continue, lock-phase,
finalize — in 17 functions, of which two hold 465 lines (47% of the file):
`pr_merge` (`:360–702`, 342) and `pr_finalize` (`:832–955`, 123). `merge_lock.rs`
is the project-wide mutual exclusion those two drive: a flock-guarded JSON record
under the project state dir, with phase and prior-ref fields. `cli/pr.rs` is the
front end; its `total` is 1506 against 495 of code, so 1011 test lines — rank it
on `code`. **Acquittal 1: `cli/pr.rs` keeps the `ops/mod.rs` contract more
strictly than unit 9's `cli/task.rs` did.** All ten mutating verbs delegate; its
140-line `pr_merge` (`:209–349`) is three lines of flag parsing, one op call and
a 125-line render match over the seven `MergeOutcomeOp` variants, which is
exactly the job `ops/pr.rs:313` assigns it. **Acquittal 2: the six small review
verbs are not a copy-paste family.** They share a `read_pr` → guard →
`update_pr` → `update_task_state` envelope, but the common guard is already
extracted as `guard_pr_mutable` (`:88`) and the residue differs per verb — a
`diff -u` of `pr_approve` against `pr_reopen` is 82 lines over 48 and 43. **No
live defect was found in this unit**; unlike section 9, the lock protocol here
holds everywhere I checked.

**Problems**

- `ops/pr.rs:360–702` — `pr_merge` releases the merge lock at 21 hand-written
  sites: 11 bare `let _ = merge_lock::release(…)` before an early return
  (`:446`, `:461`, `:469`, `:478`, `:491`, `:505`, `:522`, `:584`, `:609`,
  `:649`, and `:466` via `return Err`), and 10 routed through the `unlock`
  closure defined at `:440`. I verified all eight post-lock `?` operators are
  funnelled through `unlock` today, so there is no leak now — the cost is that
  the invariant is maintained by hand across 342 lines, and the comment at
  `:435–445` states the price of one omission: the lock strands on an exited
  process and "wedges every subsequent `pr merge` for the project" until
  `STALE_TTL`. A guard whose `Drop` releases makes this structural instead of
  vigilant.
- `ops/pr.rs:990–1084` against `bin/src/cli/pr.rs:496–1506` — the tests for this
  file's hardest code live in a different crate. `ops/pr.rs` has five tests in
  95 lines, covering only `pr_create` and `pr_approve`; `cli/pr.rs` has 1011
  test lines covering finalize (7 tests), close (3), reopen, show, merge (1:
  `pr_merge_refuses_dirty_worktree_without_demoting`, `:1338`) and the
  merged-PR refusals. The mechanism is a crate boundary: `init_repo`
  (`bin/src/cli/test_util.rs:47`) and `seed_task_with_worktree` (`:60`) are
  `pub(crate)` in the binary, while `lib`'s own `test_util` (`lib/src/lib.rs:32`)
  offers `with_temp_home` and no git repo at all. So a 342-line function with a
  21-site lock invariant has one test, reachable only downstream and only when
  `git_available()`. This is the rubric's "inlined I/O blocks testing" clause
  with the blocking mechanism named. Evidence crosses the unit boundary into
  `bin/src/cli/test_util.rs` and `lib/src/lib.rs`.
- `ops/pr.rs:832–955` — `pr_finalize`'s correctness is an *order*, not a
  structure. `:824–831` documents it as restore → release → `update_pr` →
  `update_task_state`, and `:911–917` explains why the HEAD restore must happen
  while the lock is still held (a queued `pr merge --wait` acquires the instant
  we release, and its checkout could land the successor's PR on the user's
  branch). Nothing but the comment and one test
  (`pr_finalize_keeps_task_merging_when_release_fails`, `cli/pr.rs:1230`) stops
  a future edit from reordering four statements.
- `ops/pr.rs:360–702` — the same function is preflight, lock manager, git
  driver and state machine. It re-reads task state and PR twice (`:366`/`:457`,
  `:373`/`:463`) because the `--wait` path can make the pre-lock read 30 minutes
  stale (`:451–456`), then runs three git merges/checkouts and seven distinct
  failure classifications. Six of the seven `MergeOutcomeOp` variants are
  constructed inside it, so any new preflight means editing the enum, this
  function, and the CLI's render match together.
- `bin/src/cli/pr.rs:107–121` — `pr_show` reads the store directly
  (`cc_hub_lib::pr::read_pr` at `:111`) with no `ops::` counterpart, the same
  read-verb asymmetry section 9 charged. **The count is the point: one verb and
  14 lines here, 2.8% of the file's code, against section 9's two verbs and 167
  lines, 27%.** It also reads through the store's own accessor rather than
  reimplementing an enumerator, so it carries none of section 9's divergent-policy
  cost. Noted for the ranking, not worth fixing on its own.
- `ops/pr.rs:955`, `:970` — `git_rev_parse` and `git_conflicting_paths` are
  private `Result<_, String>` wrappers over `orchestrator::run_git`, while
  `orchestrator/git.rs` (319 code) is the module that owns exactly this job and
  already exports `run_git`, `dirty_paths`, `branch_changed_paths` and
  `merge_branch`. Two more git primitives landed here because this is where they
  were needed; nothing is wrong today, but the next one lands here too.

**Severity: High** (`lib/src/ops/pr.rs`) — inlined git I/O blocks testing, with
the block located precisely: the harness that could test `pr_merge` exists but
is `pub(crate)` in the wrong crate, leaving one test on a 342-line function that
guards a project-wide lock. **`lib/src/merge_lock.rs` and `bin/src/cli/pr.rs`
are each Low alone.** `merge_lock.rs` is the best-defended file this survey has
read: the mutation path is serialized by an advisory flock on a stable
`merge.guard` sidecar with the inode rationale written down (`:322–328`), the
record itself is tempfile+rename'd, staleness has a documented liveness proxy
beyond the TTL (`:297–307`), and 305 of its 680 lines are 20 tests including
`concurrent_acquires_yield_exactly_one_winner`,
`acquire_over_existing_is_atomic_create_then_held` and
`acquire_recovers_from_corrupt_lock_record`. It should be extended, not
refactored. `cli/pr.rs` is Low on acquittal 1.

**Proposed decomposition**

Cheap: `ops/pr.rs` has no shared state and no `impl` block, and `ops/mod.rs:23`
already declares `pub mod pr;`, so an `ops/pr/mod.rs` directory with `pub use`
costs no call-site changes — the same move section 9 proposes for `ops/task.rs`,
and the two should be done together so the directory convention is set once.

| new module | moves from `ops/pr.rs` | ≈lines |
|---|---|---|
| `ops/pr/merge.rs` | `MergeLockHolder` 302, `MergeOutcomeOp` 315, `pr_merge` 360, `capture_head_ref` 702 | 420 |
| `ops/pr/review.rs` | `guard_pr_mutable` 88, `pr_approve` 104, `pr_request_changes` 152, `pr_reopen` 187, `pr_comment` 230, `pr_close` 250 | 210 |
| `ops/pr/finalize.rs` | `run_build_command` 780, `tail_lines` 795, `FinalizeOpts` 802, `FinalizeOutcome` 809, `pr_finalize` 832 | 180 |

`ops/pr.rs` keeps `pr_create`, `pr_continue` + `ContinueOutcome`, `pr_lock_phase`
and the two git wrappers, ~170 lines. **`merge_lock.rs` and `bin/src/cli/pr.rs`
get no rows** — the first is correctly factored at 375 lines and splitting it
would separate the invariants from the tests that pin them; the second is a
render layer whose largest function is a match arm per outcome variant, which is
the shape it should have.

Type-level follow-ups:

1. Move the git test harness (`init_repo`, `git_run`, `seed_task_with_worktree`,
   `git_available`) from `bin/src/cli/test_util.rs` into `lib`'s `test_util`
   (`lib/src/lib.rs:32`), which already owns `HOME_TEST_LOCK` that any git test
   needs anyway. Kills the class where a `lib` function's only coverage lives in
   a downstream crate — `pr_merge` is the worst instance, not the only one.
2. Make `merge_lock::acquire` return an RAII guard that releases on `Drop`,
   with an explicit consuming method for `pr_merge`'s one success path that
   must keep the lock held for `pr_finalize`. The 21 release sites collapse to
   one `Drop` plus one hand-off. Kills the wedged-project class outright.
3. Only if 2 leaves `pr_merge` untestable: put the git steps behind a narrow
   trait so the seven `MergeOutcomeOp` classifications can be exercised without
   a real repo.

Ordering: 1, then 2, then 3. 1 is the safety net — without tests that can drive
a real merge, 2 rewrites the lock protocol of a 342-line function on the strength
of one downstream test. 3 is contingent and may prove unnecessary.

**Notes for unit 12**

- `lib/src/pr.rs` (220 / 327) is the PR store every file in this unit reads and
  writes, and it is in the size table but in no unit. It is the natural home for
  follow-up 2's guard type.
- `orchestrator/git.rs` (319 code) is also in no unit, and this section's last
  bullet is a claim about where its boundary should be.

## 11. `lib/src/platform/process.rs` (761 / 953) + `lib/src/ops/worker.rs` (605 / 670) + `bin/src/cli/mod.rs` (573 / 796) + `lib/src/title.rs` (431 / 601) + `lib/src/spawn.rs` (397 / 581)

**What it is.** The survey's remainder unit, and its five files are unrelated by
design — this is a leftovers bucket, not a family. `platform/process.rs` is the
host-OS process layer every scanner sits on (`scanner.rs:486`, `:1147`,
`codex_scanner.rs:337–358`, `pi_scanner.rs:191–310`, `focus.rs:43–108`,
`send.rs:80`). `ops/worker.rs` holds the bodies of `spawn-worker`,
`merge-worktree` and `worker wait`, and is *also* the primitive library
`ops/task.rs:11` imports from. `bin/src/cli/mod.rs` is the CLI's front door:
dispatch, error contract, and the argument parser all seven verb modules call.
`title.rs` generates and caches session titles via a headless `claude -p`.
`spawn.rs` builds the shell command that launches an agent inside tmux.
Acquittals: (a) the `ops/mod.rs:1–20` layering contract holds in `ops/worker.rs`
— zero `println!`/`print_json` — and `cli/mod.rs:572–578` is a model adapter,
mapping `PromptStatus` to its JSON string and emitting the human warning on the
CLI side rather than duplicating the enum; (b) the read-verb question that
produced section 9's best finding comes back clean here — all three worker front
ends (`cli/spawn_worker.rs` 40 lines, `cli/merge_worktree.rs` 58,
`cli/worker.rs` 106) are parse → call `ops::worker` → `print_json`, with no verb
stranded in `bin/`; (c) `title.rs:190–364` resolving `claude` through
`$SHELL -ic` looks like a second copy of `spawn.rs`'s launcher and is not —
`spawn.rs` hands a string to the interactive shell inside tmux, which expands
aliases for it, while `title.rs` runs the binary directly and must resolve the
alias itself; (d) `config.rs:33–44` folds the legacy `spawn.command` key into the
`claude` agent's `command`, so the two files launch the same configured binary
from one key, not two. Three of these files carry `AgentKind` arms; that fan is
counted in section 8 and not recounted here.

**Problems**

- `platform/process.rs:14–28` vs `:445–761` — two competing platform
  abstractions in one file, and the documented one has no users. The
  `ProcessInfo` trait, which `platform/mod.rs:5–7` advertises as the mechanism,
  is implemented three times (`:48`, `:105`, `:194`) and named nowhere outside
  this file; every external caller uses free functions instead. Those free
  functions carry the *other* half of the OS coupling with no abstraction at
  all: five of them are cfg-forked by hand (`terminate` `:445`/`:453`,
  `command_line` `:472`/`:485`/`:557`, `current_dir` `:562`/`:569`/`:613`,
  `list_pids` `:618`/`:629`/`:654`, `open_codex_rollouts` `:664`/`:736`/`:757`),
  24 `#[cfg]` attributes in all. Adding a platform capability means picking one
  of two conventions, and the file's own author picked the undocumented one
  every time.
- `platform/process.rs:557–561` — the `not(any(linux, macos))` `command_line`
  stub returns `""`, which turns the Codex interactivity filter into a no-op on
  Windows. `is_agent_process` (`:426–435`) accepts a `codex.exe` on name alone
  and then asks `codex_is_interactive("")`, which finds no positional token and
  returns `true` at `:378–379` — the "bare interactive `codex`" branch. So on
  Windows every `codex exec` one-shot, `app-server` and `mcp-server` reports as
  a live session, and `codex_model_arg`/`codex_resume_session_arg` (`:384`,
  `:390`) always answer `None`. The stub is invisible at the call site; grouping
  by function rather than by OS is what hides it.
- `bin/src/cli/mod.rs:172–229` and `:231–395` — a 40-field `Flags` struct and a
  36-arm parser serve all 27 `parse_flags` call sites across seven verb modules,
  and no verb declares which flags it accepts. `--task delete --skip-build` and
  `pr create --backlog` parse without complaint; the unknown-flag guard at
  `:389–391` only catches spellings, never misapplications. Every new flag on
  any verb widens the struct every other verb sees.
- `bin/src/cli/mod.rs:400–412`, `:432–444`, `:457–480` — the tokenizer exists
  twice and the flag taxonomy lives in two hand-maintained const lists.
  `FREE_TEXT_FLAGS` (11 entries) and `BOOL_FLAGS` (11) must stay in sync with 36
  match arms with nothing checking them, and `args_request_help` re-implements
  `next_value`'s swallowing rules — its doc comment at `:464–468` says "mirror
  `next_value`'s tokenizer", which is the admission. The two lists agree with
  the arms today; nothing keeps them agreeing.
- `lib/src/ops/worker.rs:38–73` vs `lib/src/app/mod.rs:2826–2887` — the
  prompt-readiness ladder is written twice, and the copies have already
  diverged. `ops/worker.rs:32` names its twin ("same shape as
  `App::poll_pending_dispatch`"); both carry the same two rungs, the same 5s
  cold-boot constant, and a byte-identical log line (`:54–58` /
  `app/mod.rs:2870–2874`). What differs: the TUI reads its timeout from
  `config.ui.pending_dispatch_timeout()` (`app/mod.rs:2883`) while the ops loop
  takes a caller-supplied one defaulting to the local const at `:19`, so the
  configured value governs one path and not the other; the TUI throttles the
  `pane_ready_for_input` probe and skips it once `aged_in` (`:2856–2867`), the
  ops loop probes unconditionally; and the ops loop calls
  `scanner::scan_sessions()` synchronously at `:46`, a full rescan every 500ms
  for up to 120s, where the TUI reads the scan thread's cache. This is
  cross-boundary evidence: `app/mod.rs` is section 1's file.
- `lib/src/ops/worker.rs:19–98` vs `:99–605` — one file is two things. Lines
  19–98 are the shared primitive library (`DEFAULT_PROMPT_WAIT_SECS`,
  `find_by_tmux`, `wait_until_idle_and_send`, `PromptStatus`) and are the only
  part siblings import: `ops/task.rs:11` takes three of the four, and
  `cli/mod.rs:572` renders `PromptStatus`. Lines 99–605 are three independent
  verb bodies (`spawn_worker` 97 lines, `merge_worktree` 123,
  `worker_wait` 109 plus its 98-line resolver) that share nothing with each
  other. A sibling op that wants the prompt helper must depend on the module
  that also owns the merge-lock protocol.
- `lib/src/ops/worker.rs:486–489` and `:583–595` — `WorkerWaitOutcome.workers`
  is `Vec<serde_json::Value>`, so the domain layer builds its caller's JSON. The
  `ops/mod.rs:14–15` convention says the caller reconstructs output from typed
  data, and `:486–488` states the exception it is taking. The cost is that the
  TUI cannot reuse `worker_wait` without parsing JSON back into fields the
  scanner already had as types. An explained trade-off, so it argues down, but
  it is the one place in three `ops::` modules where the contract bends.
- `bin/src/cli/mod.rs:532–559` — `scan_projects_for_task` walks
  `~/.cc-hub/projects/*` and skips every failure silently: unreadable dir
  (`:536`), bad file type (`:541`), non-UTF-8 name (`:547`). It is the
  projects-dir sibling of the four tasks-dir enumerators section 9 filed
  (`orchestrator/mod.rs:1515`, `projects_scan.rs:298`, `cli/task.rs:223–247`,
  `tasks.rs:151–178`), with a fifth policy — no log at all — so a project whose
  directory is unreadable reports as "task not found under any registered
  project" (`:507–510`). Extends section 9's finding; not a new one.
- `lib/src/title.rs:190–364` — 175 lines, 40% of the file, resolve a shell alias
  and have nothing to do with titles; `run_claude_blocking` (`:365–403`) is the
  app's only headless-agent invoker and `triage.rs:174` already reaches into the
  title module to get it. The module is four services under one name: shutdown
  signalling (`:22–29`), the on-disk title cache (`:32–110`), command resolution
  (`:112–364`), and title generation proper (`:404–431`). Its tests confirm the
  imbalance — 15 of 16 cover `parse_resolution` and `sanitize_title`, none touch
  `save`/`load`/`persist_title` or `run_with_timeout`.
- `bin/src/cli/worker.rs:43–56` and `:151–204` — `resolve_wait_targets` is
  wrapped in `bin/` and tested on both sides of the crate boundary: four cases
  in `bin` against three in `ops/worker.rs:632–668`, covering the same function
  with no shared fixtures. Section 10 already diagnosed why `lib` logic ends up
  tested from `bin`; this is another instance, not a new smell.

**Severity: Medium** (`platform/process.rs`, `ops/worker.rs`, `bin/src/cli/mod.rs`
— each is locally painful with a contained blast radius and one real job);
`title.rs` and `spawn.rs` are **Low** alone. `title.rs` earns it on a positive
exhibit: the scratch-cwd trick is designed, documented at `:1–7`, and honored on
the other side by the scanner filter (`scanner.rs:656`, `:715`), so title
generation never pollutes the session grid; the resolver cache splits its TTL by
outcome (`:194–195`, 1h on success, 60s on failure) so a transient shell hiccup
cannot disable titling for the process lifetime; and `parse_resolution` carries
ten adversarial tests — rc chatter before the alias, foreign assignments,
prefix-named assignments, Windows function bodies. `spawn.rs` earns it the way
`merge_lock.rs` did: `ensure_path_trusted` (`:221–320`) documents why it must
target the `CLAUDE_CONFIG_DIR` copy, takes a sidecar flock with the
inode-follows-rename rationale written down, and names its own residual
limitation (the running `claude` does not take the lock, so the window is
narrowed, not closed); `prefix_env` (`:91–98`) is deliberately parameterised on
`windows: bool` so the pwsh branch is unit-testable on a host with no pwsh; and
`build_agent_command`'s three-agent flag ordering is pinned by five tests
including two Codex regressions (`:481`, `:504`).

**Proposed decomposition**

`platform/process.rs` regroups by OS instead of by function — the cfg forks
already draw the lines, and the trait plus the free functions for one platform
become one module, which makes a missing capability a missing function rather
than an invisible stub. `ops/worker.rs` is the third `ops/<verb>/` directory
proposed in this report (sections 9 and 10 have the others), so the convention
should be set once for all three. `bin/src/cli/mod.rs` splits along three seams
that share no state.

| new module | moves from `X.rs` | ≈lines |
|---|---|---|
| `platform/process/detect.rs` | `matches_pi_command` 268, `matches_codex_command` 294, `CODEX_*` consts 294–344, `codex_is_interactive` 351, `codex_model_from_cmd` 394, `codex_resume_session_from_cmd` 407: pure string detectors, host-independent, with the 8 pure tests | 150 |
| `platform/process/linux.rs` | `ProcessInfo` impl 30–73 plus the linux arms of `command_line` 472, `current_dir` 562, `list_pids` 618, `open_codex_rollouts` 736 | 150 |
| `platform/process/macos.rs` | `ProcessInfo` impl 74–148, `parent_pid_ps` 93, macos arms of `command_line` 485, `current_dir` 569, `list_pids` 654, `open_codex_rollouts` 664 | 230 |
| `platform/process/windows.rs` | `ProcessInfo` impl 149–230, `with_entries` 163, `exe_name` 185, `terminate` 453, `list_pids` 629 | 110 |
| `ops/prompt.rs` | `DEFAULT_PROMPT_WAIT_SECS` 19, `find_by_tmux` 21, `wait_until_idle_and_send` 38, `PromptStatus` 76: the whole shared-primitive surface, and all of what `ops/task.rs:11` and `cli/mod.rs:572` import | 80 |
| `ops/worker/spawn.rs` | `SpawnWorkerOpts` 99, `SpawnWorkerOutcome` 108, `spawn_worker` 121 | 120 |
| `ops/worker/merge.rs` | `MergeWorktreeOutcome` 218, `merge_worktree` 230, `capture_head_ref` 353 | 160 |
| `ops/worker/wait.rs` | `resolve_wait_targets` 377, `WaitProgress` 475, `WorkerWaitOutcome` 482, `worker_wait` 497, and the three target-resolution tests 632–668 | 230 |
| `cli/flags.rs` | `Flags` 172, `parse_flags` 231, `next_value` 414, `FREE_TEXT_FLAGS` 400, `BOOL_FLAGS` 432, `args_request_help` 457, and the six parser tests | 290 |
| `cli/error.rs` | `CliError` 97, `kind` 121, `into_message_and_recipe` 134, the three `From` impls 145–169, `handle` 65 | 105 |
| `cli/resolve.rs` | `require_task` 482, `resolve_project_id` 488, `cwd_id_has_task_state` 521, `scan_projects_for_task` 532, and the five resolution tests | 130 |

`platform/process/mod.rs` keeps the `ProcessInfo` trait, the cross-OS composites
(`walk_ancestors` 231, `collect_pid_chain` 245, `is_agent_process` 415,
`codex_model_arg` 384, `codex_resume_session_arg` 390) and the per-OS `pub use`
— about 120 lines. `ops/worker/mod.rs` keeps only re-exports, about 20.
`bin/src/cli/mod.rs` keeps the module list, `dispatch` 35, `print_json` 561 and
`report_prompt_status` 572 — about 60 lines, which is what a front door should
be. `title.rs` gets one optional row: `run_with_timeout` 144, `detach_from_tty`
125, `ResolveCache` 112, `resolve_spawn_command` 190, `compute_resolve` 216,
`resolve_command` 253, `parse_resolution` 281, `path_resolves_cmd` 329,
`alias_body_for` 343, `is_windows_exe_path` 351 and `run_claude_blocking` 365
move to an `agent/headless.rs` (≈270 lines), leaving `title.rs` at ~160 as the
cache plus the generator. That is a rename with a real second consumer
(`triage.rs:174`), not a size fix, and it is optional. `spawn.rs` gets no
decomposition row: `build_agent_command`'s three arms are the one place per-agent
CLI shape is written down, and splitting them into three files would scatter the
comparison the tests depend on.

1. Give each verb its own flag set. A per-verb `&[&str]` allowlist checked
   inside `parse_flags`, or a small `Flags::require(verb)`, kills the class where
   a flag silently applies to a verb that ignores it — the failure mode an
   orchestrator cannot see, because the CLI exits 0.
2. Derive the two const lists from the match arms (a single table of
   `(name, arity, free_text)` that `parse_flags`, `next_value` and
   `args_request_help` all read). One source, and the second tokenizer becomes a
   walk over the same table instead of a copy of the first.
3. Give `WorkerWaitOutcome.workers` a typed shape (`WorkerSnapshot`) and let the
   CLI serialise it, so `ops::` stops depending on `serde_json` for output and
   the TUI can call `worker_wait`.

Ordering: `platform/process.rs` first — it is the only file here whose defect is
a live one, and the regrouping is what exposes the Windows stub. Then follow-up
2 before follow-up 1, because the flag table is what a per-verb allowlist would
index into. The `ops/worker.rs` split should land with sections 9 and 10 in the
one change that sets the `ops/<verb>/` convention.

**Notes for unit 12**

- `lib/src/config.rs` (unit 13) owns the `spawn.command` → `claude` agent shim
  at `:33–44` that makes this unit's launcher acquittal true; a change there
  silently changes what `title.rs` resolves.
- `lib/src/send.rs` (144 code, in no unit) is the two-function seam
  (`pane_ready_for_input`, `send_prompt`) both copies of the readiness ladder
  call, and is the natural home for a single shared ladder.

## 12. `lib/src/ui/tasks.rs` (730 / 994) + `lib/src/ui/sessions_list.rs` (426 / 489) + `lib/src/ui/mod.rs` (388 / 388) + `lib/src/ui/common.rs` (329 / 340)

**What it is.** The UI core the other nine `ui/` modules sit on. `ui/mod.rs` is
the render entry point `lib.rs`'s hot-reload shim calls — layout split, title
bar, tab strip, status bar, and the overlay dispatch. `ui/common.rs` is the
shared helper module 11 of the 16 UI files import. `ui/tasks.rs` draws the Tasks
board: filter bar, four status columns, task cards, and the attachment info
popup. `ui/sessions_list.rs` draws the Sessions list layout. This section also
opens `ui/palette.rs` (49 lines), which sections 3, 6 and 7 all reason about and
none had read: it is 15 `pub(crate)` `Color::Rgb` constants, and its module doc
at `:1–4` states the policy that governs the counting those sections did —
"consolidating only *exact* duplicate values… no RGB value is altered".
Acquittals: `main_layout` (`ui/mod.rs:47–60`) and `BAND_BG` (`:31–34`) are real
single-source-of-truth moves, both documented as such and both shared with
overlays; `common.rs`'s helper fan is genuine reuse, not a wrapper fan — section
7 already verified six of its exports are used by the card renderers, and
`task_color` (`:58–68`) hashes task ids into a fixed eight-hue table so linked
cards keep one identity colour across restarts. `ui/tasks.rs` was checked for a
`render_task_column`/`render_task_card` twin pair like `task_cards.rs`'s and does
not have one — the two functions nest rather than duplicate.

**Problems**

- `ui/common.rs:21–68` and `:146–264` against `ui/palette.rs:1–4` — the repo has
  two colour authorities and neither knows about the other. `palette.rs`
  consolidates by *value* and its doc restricts it to exact duplicates;
  `common.rs` decides colour by *role* — `task_status_meta` (status → colour),
  `priority_color`, `TASK_COLORS` (identity hues), `bar_color`, `ctx_color`,
  `state_color` — and mixes named ANSI with raw triples inside one decision
  (`:21–31`: two palette constants and four `Color::Light*`). The realized cost:
  `Color::Rgb(60, 60, 80)` is written four times (`tasks.rs:399`, `:488`,
  `projects/board.rs:153`, `projects/backlog.rs:148`) ten blue points away from
  `SEP_GRAY` (`palette.rs:26–27`), which is documented as the constant for
  "unfocused card borders". Under the stated exact-duplicates-only policy a
  near-miss can never be absorbed, so the second border grey is permanent by
  design. Evidence crosses this unit's boundary into `board.rs` and
  `backlog.rs`, which are in no unit.
- `ui/tasks.rs:473–552` — the fourth card renderer, which section 7 predicted
  and did not have in scope. Same envelope as its three siblings: the
  `(border_type, border_style)` pick from `selected` (`:483–490`), the
  `Block` + `inner` + `inner.width == 0 || inner.height == 0` guard
  (`:503–523`), the `Paragraph::new(lines)` tail (`:551`), and a `meta_line`
  footer (`:555–654`). Two things differ and neither is a design choice anyone
  wrote down: the badges ride the border title here (`:494–518`) instead of a
  body row, and the unfocused border is the raw `Rgb(60, 60, 80)` above rather
  than `SEP_GRAY`. Section 7's follow-up 1 therefore has three call sites, not
  two.
- `ui/mod.rs:252–326` — the keybinding hints are 25 string literals in the
  renderer, structurally disconnected from `bin/src/keys.rs`, where the
  bindings they describe actually live. Nothing links `"j/k:scroll  esc:close
  q:close"` (`:264`) to the arms that implement it; a rebinding leaves the hint
  stale and no test fails. Spot-checked `View::Backlog` (`:323`) against
  `keys.rs:461–479` — it agrees today, with one gap already: `Up`/`Down` are
  bound at `keys.rs:464`/`:467` and go unadvertised, while `View::ModelPicker`'s
  hint at `:276` does advertise `↑/↓`. Cross-boundary evidence into section 5's
  file.
- `ui/mod.rs:62–105`, `:252–326`, `:328–336` — three parallel matches on `View`
  in one 388-line file: the overlay dispatch (21 arms), the hint table (25), and
  a `(View, Tab)` tuple match for the space-key verb. With the two in `keys.rs`
  that section 5 counted, adding one overlay is five edits across two files and
  the compiler catches only the first.
- `ui/tasks.rs:127–338` — `render_task_info` is 211 lines and does seven things:
  popup geometry, title, header spans, prompt wrapping and capping (`:210–221`),
  the attachment-card canvas (`:233–297`), the scroll clamp (`:299–314`), and the
  footer hint. It takes `&mut App` and writes `app.render.task_info_scroll`
  during draw at `:302–311` — the ninth such renderer in this survey after the
  eight sections 3, 6 and 7 counted, and the fourth that clamps scroll mid-draw.
  It is also the only overlay in `ui/mod.rs`'s dispatch (`:98`) that lives
  outside `popups.rs` or `projects/`.
- `ui/tasks.rs:696–727` and `ui/popups.rs:849–880` — `wrap_text` exists twice
  with the same signature and different bodies, and the semantics have already
  forked: at width 0 the tasks copy returns one empty string (`:697–699`) while
  the popups copy clamps to `width.max(1)` (`:850`) and wraps into
  single-character rows. This is a reimplementation, not a copy-paste, which is
  why no comment marks either as shared. Cross-boundary evidence into section
  3's file.
- `ui/common.rs:1–329` — 24 helpers, zero tests, and the module's only
  `#[cfg(test)]` item is `buffer_to_string` (`:330–340`), test infrastructure it
  provides to everyone else. Nine of its functions are pure and trivially
  testable (`format_duration_secs`, `format_tokens`, `fmt_cost`, `short_model`,
  `short_tool`, `format_tool_label`, `context_window_size`, `format_reset`,
  `task_color`), and `tasks.rs` (8 tests) and `sessions_list.rs` (6) both spend
  a `TestBackend` render to assert on output these functions produced. The most
  reused module in `ui/` is the least covered one.
- `ui/mod.rs:228–388` — `render_status_bar` is 160 lines, larger than the render
  dispatch it shares the file with, and 106 of them (`:251–357`) are one `else`
  branch holding the hint table. `ui/mod.rs` has no tests at all, so the status
  bar, the title bar (`:149–207`) and `build_session_count_spans` (`:208–227`)
  are unexercised.
- `ui/sessions_list.rs:249–426` — `render_row` is 178 lines, and five
  near-identical blocks build the column cluster (`:268`, `:290`, `:302`,
  `:314`, and the run to `:355`), each testing its own `cols.*` flag, pushing a
  `Cell` and repeating the same truncate-and-style shape. The `Cell` type at
  `:227–248` is the abstraction that would collapse them and is only half used.
- 29 raw `Color::Rgb` against 48 imported palette-constant uses across these
  four files — 8/28 in `tasks.rs`, 1/5 in `sessions_list.rs`, 6/5 in `mod.rs`,
  14/10 in `common.rs`, on section 6's denominator. This inverts the trend of
  sections 3, 6 and 7 (259 against 55): the UI core is the part of the tree that
  *does* use the palette, and 8 of `common.rs`'s 14 raw literals are the
  `TASK_COLORS` identity table (`:47–56`), which is a deliberate second palette
  rather than a bypass. Unit 14 should read the repo-wide ratio with that split
  in mind.

**Severity: Medium** (`ui/mod.rs`, `ui/tasks.rs` — three parallel `View` matches
plus a 160-line status bar in one, a 211-line scroll-mutating popup and the
fourth card renderer in the other); `ui/sessions_list.rs` and `ui/common.rs` are
**Low** alone. `sessions_list.rs` earns it on the seam the rest of the UI lacks:
`plan_columns` (`:78–115`) is a pure `(width, task_need) -> ListColumns` with
four tests covering the base width, growth into leftover, the cap, and the hidden
case, and `body_row_offsets` (`:116–130`) is pure with two more — layout
arithmetic separated from painting and tested directly, which is exactly what
section 7 said `result_popup.rs:124–246` had no way to do. `common.rs` earns it
as the survey's successful anti-duplication module: 11 of 16 UI files import it,
section 7 verified six of its exports are load-bearing in the card renderers, and
`task_color` (`:58–68`) solves cross-restart identity colour with a documented
FNV-1a hash into a fixed table. Its zero-test count is a real gap but costs
little, because its callers' render tests catch output changes indirectly.

**Proposed decomposition**

`ui/mod.rs` splits cleanly: the dispatch and layout are 100 lines, the chrome is
280, and they share only `BAND_BG`. `ui/tasks.rs` has the same seam section 7
used on `sessions.rs` — the board half and the popup half share nothing but the
module. `ui/common.rs` gets no split rows: its problem is a missing test module,
not a missing boundary, and cutting a 329-line helper file that 11 modules import
into themed pieces would multiply the import lines without removing anything.

| new module | moves from `X.rs` | ≈lines |
|---|---|---|
| `ui/chrome.rs` | `ui/mod.rs`: `render_tab_strip` 107, `render_title_bar` 149, `build_session_count_spans` 208, `render_status_bar` 228 | 280 |
| `ui/hints.rs` | `ui/mod.rs` 252–336: the `View` hint table and the `(View, Tab)` space-verb match, as one `fn keybind_hint(view, tab, app) -> &str` | 90 |
| `ui/tasks/board.rs` | `ui/tasks.rs`: `render_tasks_body` 29, `render_filter_bar` 84, `column_meta` 339, `render_task_column` 352 | 190 |
| `ui/tasks/card.rs` | `ui/tasks.rs`: `render_task_card` 473, `meta_line` 555, `tags_title_text` 655, `dir_basename` 686, plus the six card tests | 350 |
| `ui/tasks/info.rs` | `ui/tasks.rs`: `INFO_EXCERPT_LINES` 121, `render_task_info` 127, plus the two popup tests | 280 |
| `ui/sessions_list/row.rs` | `ui/sessions_list.rs`: `Cell` 227, `render_row` 249 | 200 |

`ui/mod.rs` keeps the module list, `BAND_BG`, `cell_height`, `now_ms`,
`main_layout` and `render` — about 100 lines, which is what a render entry point
should be. `ui/tasks/mod.rs` keeps `wrap_text` until follow-up 2 removes it
(≈40); `sessions_list.rs` keeps `plan_columns`, `body_row_offsets`, `render_list`
and all six tests (≈230).

1. Give `View` its hints. Move the 25 strings into a method on `View` (or a
   table beside the enum) that `ui/hints.rs` reads, so adding a variant fails to
   compile until it is described. That does not link a hint to its binding, but
   it converts the silent-omission class into a compiler error and puts the hint
   next to the thing `keys.rs` matches on.
2. One `wrap_text` in `ui/common.rs`, replacing `tasks.rs:696` and
   `popups.rs:849`, with the zero-width behaviour decided once and the first
   tests `common.rs` has ever had. Cheapest item in this section.
3. Fold the fourth card into section 7's follow-up 1. The shared
   `task_meta_row` now serves `task_cards.rs:118–163`, `:379–423` and
   `tasks.rs:555–654`, and the shared envelope serves four renderers, not three.
   Decide the unfocused border grey while doing it — either `SEP_GRAY` absorbs
   `Rgb(60, 60, 80)` or the palette gains a second named constant, but four raw
   copies of a near-miss is the outcome nobody chose.

Ordering: 2, then 1, then 3. 2 is self-contained and gives `common.rs` a test
module the rest can grow into; 3 is the largest and should land with sections 7's
follow-ups 1 and 2 rather than before them.

**Notes for unit 12**

- `ui/palette.rs` is now read and its policy is quoted above; `projects/diff.rs`
  (in no unit) still defines `TASK_META_DIM` outside it, which is the third
  colour authority after `palette.rs` and `ui/common.rs`.
- `lib/src/app/render_state.rs` (84 total, in no unit) holds the scroll fields
  four renderers now mutate during draw, and is where section 7's follow-up 3
  `clamp(viewport)` would live. `ui/mod.rs:63` already calls
  `app.update_grid_cols` once per frame, so the once-per-frame hook that
  follow-up needs exists and has a caller.

## 13. `lib/src/platform/window.rs` (577 / 577) + `lib/src/tmux_pane.rs` (517 / 573) + `lib/src/config.rs` (446 / 562) + `lib/src/projects_scan.rs` (381 / 500)

**What it is.** Four unrelated infrastructure files, grouped by size rather than
by any relationship — `window.rs` raises OS windows for `focus.rs` (111 total, in
no unit, its only consumer), `tmux_pane.rs` embeds a tmux attach in a PTY for the
TUI, `config.rs` is the `OnceLock` knob layer 53 `config::get()` sites read, and
`projects_scan.rs` builds the kanban snapshot. Two of the four do share one real
shape, though: an immutable input with a derived view rebuilt on every call
(`Config::resolved_agents`, `ProjectsSnapshot::roles_by_tmux`). Test lines by
file: `window.rs` 0, `tmux_pane.rs` 56 (6 tests, all on the OSC 52 scanner),
`config.rs` 115 (8 tests), `projects_scan.rs` 118 (2 tests) — so the assignment's
premise that `config.rs` has zero tests is wrong; it is the second best-tested
file here. Three acquittals. `ProjectsSnapshot.titling` looks dead —
`scan():222` hard-codes it to `HashSet::new()` — but `bin/src/main.rs:323` stamps
it after the scan returns; cleared in one grep. A corrupt-`state.json` card
flicker was suspected to be a torn read; cleared, because every writer goes
through tempfile+rename (`orchestrator/mod.rs:620`), so a parse failure is real
corruption. `window.rs`'s Windows silence is not an undocumented gap:
`README.md:316–330` lists "Focus / close the OS window … Windows: no-op". None of
the four contain a `Color::` literal, so the palette running total is unchanged
at 288/103.

**Problems**

- `platform/window.rs:534` — `focus` returns `raised && activated`, and the
  comment eight lines above says the opposite. `:503–508` states that on apps
  which do not expose the `_AXUIElementGetWindow` mapping "raise is a no-op and
  we still get correct app-level focus from osascript below". The `&&` throws
  that success away, `Chain::focus` (`:64–66`) reports false, and `focus.rs:51`
  tells the user to grant Accessibility access on a window that just came to the
  front. macOS only; the wrong operator is the entire defect.
- `config.rs:431–443` — one misspelled key discards the whole file. Every struct
  carries `deny_unknown_fields`, so `toml::from_str` rejects the document, and
  `load` answers `Config::default()` with a `log::warn` the TUI never shows. The
  module doc (`:1–4`) promises "missing file, missing section, and missing field
  all fall back to `Default`" and is true for all three; a *typo* silently
  reverts all ten sections instead of one field. The `unknown_field_rejected`
  test (`:498`) proves the rejection and nothing proves its cost.
- `projects_scan.rs:374` — a task whose `state.json` will not parse vanishes
  from the kanban, and the repo already knows this is wrong. The evidence
  crosses a unit boundary: `orchestrator/mod.rs:1550–1556` exists solely because
  "a genuinely live task with a momentarily/persistently corrupt state would
  otherwise vanish", and `orchestrator/gc.rs:68` uses it to fail safe and keep
  the worktree. The render path has no equivalent, so gc protects the worktree of
  a task the board has already stopped showing.
- `config.rs:33–62` — `resolved_agents` rebuilds a `BTreeMap` plus a
  `default_claude_models()` `Vec` on every call, for a value that a `OnceLock`
  guarantees can never change. 12 call sites, and the hot ones are not
  incidental: `ui/popups.rs:431` calls it inside a renderer to ask
  `.len() > 1`, and `scanner.rs:1092`, `:1102`, `:1110` build it three times per
  scan tick. `agent()` (`:64–66`) builds the whole map to `remove` one key.
- `projects_scan.rs:137–165` — `roles_by_tmux` is the same shape one layer up:
  it walks every task and every worker of every project, allocating a `String`
  key per entry, and its own doc says "building one map per frame keeps the
  lookup O(1) per card". Two renderers call it per frame (`ui/sessions.rs:125`,
  `ui/sessions_list.rs:188`) plus `app/mod.rs:3497`, so "one map per frame" is
  actually two, on a snapshot that is immutable between scans and could have
  carried the index as a field.
- `platform/window.rs:36–51` — `detect()` is the file's only policy and the only
  part that could be tested, and it is not. The chain order is
  hyprland → xdotool → macos, so a macOS host with `xdotool` on `PATH` (the
  probe at `:164–168` is not `cfg`-gated) puts an X11 backend ahead of the native
  one and pays two failed subprocess spawns per focus. `Chain::focus`/`close`
  (`:64–70`) are eight lines of `any()` over a `Box<dyn WindowManager>` — a fake
  manager would test both the order and the short-circuit for a ~30-line test
  module. This is the answer to "untestable I/O or trivially correct dispatch":
  ~560 of the 577 lines are genuinely untestable subprocess and CoreFoundation
  FFI, and the ~16 that are not are the ones carrying a decision.
- `tmux_pane.rs:389–444` — `encode_key` is 128 lines of pure, table-driven
  `KeyEvent → Vec<u8>` with zero tests, in the one file that proved it knows how
  to test a pure function. Five arms drop modifiers the arrow/tilde helpers
  honour: `Enter` (`:419`), `Tab` (`:420`), `BackTab`, `Backspace` (`:428`) and
  `Esc` (`:429`) ignore ALT and CONTROL entirely, while `csi_arrow` (`:461`)
  routes them through `modifier_code`. Alt+Backspace — delete-word, in an
  embedded agent prompt — therefore reaches tmux as a bare `0x7f`, and nothing
  records whether that is intended.
- `tmux_pane.rs:206` — the DSR handshake is scanned per chunk while OSC 52 is
  scanned statefully, in the same loop, three lines apart. `Osc52Scanner`
  (`:22–34`) documents exactly why a per-chunk scan is wrong ("a large selection
  easily out-sizes one 8 KiB pty read"); the four-byte `\x1b[6n` gets
  `buf[..n].windows(…)`, which cannot see a query split across two reads. The
  file's own comment (`:10–13`) says psmux "blocks until it gets an answer", so
  the miss hangs the attach on Windows rather than degrading.
- `projects_scan.rs:350` vs `:374` — `visited.insert` runs before the parse, so
  a corrupt file keeps its stale cache entry alive forever: the mtime guard
  (`:357`) never serves it and the eviction pass (`:212`) never drops it,
  because the path is in `visited`. Small leak, but it is the same off-by-one
  reasoning that produced the bullet above.
- `projects_scan.rs:298` — extends, does not reopen, the tasks-dir enumerator
  finding. This walker's policy is skip+warn with an mtime cache and it is
  already filed; what is new is that `orchestrator/mod.rs:1565` is a *second*
  walker inside an already-counted file, walking the same directory for the
  express purpose of naming what `:298` dropped. The two live in different
  crates' modules and neither calls the other.
- `config.rs:406–445` — `get`/`load` are the only untested lines in the file and
  they hold all three failure branches (no home dir, read error, parse error).
  All 8 tests call `toml::from_str::<Config>` directly, which is the right
  choice for the knob semantics and leaves the I/O path with no coverage at all;
  `lib`'s `with_temp_home` (`lib/src/lib.rs:32`, per section 10) already exists
  and would cover two of the three.

**Severity: Medium** (`platform/window.rs`, `projects_scan.rs`); `tmux_pane.rs`
and `config.rs` are Low alone. The two Mediums are the contained-blast-radius
clause: each has one real job and a defect that costs a user something, but
nothing outside `focus.rs` and the projects renderers can collide with them.
The two Lows are structural Lows with named exhibits, not empty ones.
`tmux_pane.rs`'s hardest component is its best-tested: `Osc52Scanner` is a
documented state machine with a stated memory bound (`OSC52_MAX`, `:18–20`) and
six adversarial tests including split-across-chunks (`:539`), clipboard-query
suppression (`:559`) and ESC-aborts-then-reopens (`:566`), backed by a real
end-to-end pane in `lib/tests/pane_smoke.rs`; `spawn_owned` (`:263–265`)
documents that a construction failure kills the session so the caller cannot
leak it, and `drop(pair.slave)` (`:173–174`) carries its EOF rationale.
`config.rs` earns its Low on `deny_unknown_fields` across all ten sections
(typos are rejected, not silently absorbed), one `Default` impl per section with
the shipped value written exactly once, and the `spawn.command` → built-in
`claude` shim (`:33–44`) that keeps pre-`[agents]` configs working — with
`legacy_spawn_maps_to_default_claude_agent` (`:508`) pinning that shim, which is
what makes section 11's launcher acquittal true. Neither Low is a size Low:
both files earn it on evidence for the file, and both still carry a bug entry.

**Proposed decomposition**

Only `window.rs` and `tmux_pane.rs` should move. `window.rs` is mechanically
free — the three backends are already `mod` blocks with private `use` lists and
no shared state beyond the `WindowManager` trait, so this is a file move with no
visibility change, and `platform/` already holds five siblings.

| new module | moves from `platform/window.rs` | ≈lines |
|---|---|---|
| `platform/window/macos.rs` | `mod macos` 226–577: FFI decls, `CfString`, `find_owner_and_window`, `activate_via_osascript`, `with_ax_window`, the `Macos` impl | 350 |
| `platform/window/hyprland.rs` | `mod hyprland` 73–152 | 80 |
| `platform/window/xdotool.rs` | `mod xdotool` 154–223 | 70 |

| new module | moves from `tmux_pane.rs` | ≈lines |
|---|---|---|
| `tmux_pane/osc52.rs` | `Osc52Scanner` 35–118 plus its 6 tests | 90 |
| `tmux_pane/encode.rs` | `encode_key` 389, `modifier_code` 446, `csi_arrow` 461, `csi_tilde` 470, `mouse_button_code` 479, `button_base` 492, `function_key` 500 | 130 |

`window/mod.rs` keeps the trait, `current`, `detect`, `Chain` and the module doc
— about 70 lines, and for the first time a testable unit. `tmux_pane/mod.rs`
keeps `TmuxPaneView`, its PTY plumbing and `Drop` — about 300. `config.rs` and
`projects_scan.rs` get **no rows**: `config.rs` is ten declarative structs and
their `Default`s, so splitting by section multiplies files without removing
anything, and `projects_scan.rs` is four functions around one snapshot type.
Both of their real fixes are type-level, below.

1. Precompute the derived views. Give `Config` a memoized `resolved_agents`
   (a second `OnceLock` beside `CFG`, or a field built in `load`) and make
   `roles_by_tmux` a field of `ProjectsSnapshot` filled once in `scan`. Kills
   the class "immutable input, derived collection rebuilt per frame" in both
   files, and lets `agent()` borrow instead of building-then-`remove`.
2. Make config load failure visible. Split a `try_load() -> Result<Config, _>`
   out of `load`, keep `get()` infallible, and stash the error where the TUI can
   surface "config.toml ignored: <err>". Kills the class "a typo silently
   reverts every knob with warn-only evidence", and gives `load`'s three
   branches something to test against `with_temp_home`.
3. Give the board the fail-safe gc already has. Have `load_tasks_from_dir`
   return unparsed ids alongside the states, mirroring
   `orchestrator/mod.rs:1550`, and let the kanban render a corrupt card rather
   than drop it. Kills the class "a live task disappears from the UI because one
   file is mid-corruption".

Ordering: 2, then 3, then 1. 2 is self-contained and small; 3 changes one
signature and one caller; 1 touches 12 call sites in `config.rs`'s case and
should land last.

**Notes for unit 12**

- `lib/src/focus.rs` (111 total, in no unit and no size table) is `window.rs`'s
  only consumer and the file that turns its `bool` into user-facing status —
  the macOS `&&` bug is invisible without it, and `close_window` (`:88`) escalates
  a false to killing the agent process directly.
- `lib/src/platform/mux.rs` (397 / 471, in the size table but in no unit) owns
  `attach_argv` and `configure_clipboard`, the two calls `TmuxPaneView::spawn`
  opens with (`tmux_pane.rs:141–142`, `:154`); every psmux-vs-tmux difference
  this unit's Windows bullets depend on is decided there.
- `orchestrator/mod.rs:1550` is a fail-safe *pattern*, not just a function: the
  repo has already decided that "present but unparseable" must not read as
  "absent", and applied it in exactly one of the five tasks-dir walkers.
