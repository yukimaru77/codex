fn team_workers(config: &TeamConfig) -> Vec<TeamMember> {
    config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .cloned()
        .collect()
}

fn send_system_message_to_members(
    team_dir: &Path,
    config: &TeamConfig,
    from: &str,
    members: &[TeamMember],
    message: &str,
) -> Result<()> {
    ensure_member_exists(config, from)?;
    for member in members {
        let msg = MailMessage {
            from: from.to_string(),
            to: member.name.clone(),
            message: message.to_string(),
            timestamp: now(),
            read: false,
        };
        append_jsonl(&mailbox_path(team_dir, &msg.to), &msg)?;
    }
    append_event(
        team_dir,
        "message_sent",
        serde_json::json!({
            "from": from,
            "to": members.iter().map(|member| member.name.clone()).collect::<Vec<_>>(),
            "system": true,
            "message": message,
        }),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_codex_exec(
    codex_exe: &Path,
    cwd: &Path,
    team_id: &str,
    member_name: &str,
    member_role: &str,
    prompt: &str,
    log_path: &Path,
    last_message_path: &Path,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<std::process::ExitStatus> {
    let stdout =
        fs::File::create(log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("-o")
        .arg(last_message_path)
        .env("CODEX_TEAM_ID", team_id)
        .env("CODEX_TEAM_MEMBER", member_name)
        .env("CODEX_TEAM_ROLE", member_role)
        .env("CODEX_TEAM_CLI", codex_exe)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    if let Some(sandbox) = sandbox {
        command.arg("--sandbox").arg(sandbox);
    }
    if dangerously_bypass_approvals_and_sandbox {
        command.arg("--dangerously-bypass-approvals-and-sandbox");
    }
    command.arg(prompt);
    command
        .spawn()
        .with_context(|| format!("spawn Codex member `{member_name}`"))?
        .wait()
        .with_context(|| format!("wait for Codex member `{member_name}`"))
}

fn build_discussion_prompt(
    config: &TeamConfig,
    tasks: &[TeamTask],
    member: &TeamMember,
    round: u32,
    total_rounds: u32,
) -> String {
    let assigned_tasks = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
        .map(|task| format!("- {} [{}]: {}", task.id, task.status, task.subject))
        .collect::<Vec<_>>()
        .join("\n");
    let member_lines = config
        .members
        .iter()
        .map(|member| format!("- {} ({})", member.name, member.role))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are participating in a Codex agent team discussion round.

Team: {team_id}
Goal: {goal}
Member: {member_name}
Role: {role}
Round: {round}/{total_rounds}

Use the normal Codex runtime exactly as configured for this machine. Your job in this turn is coordination, not implementation.
Do not start long-running tools, MCP/external-tool calls, builds, downloads, training, Docker work, `team job`, or `team wait` during a discussion round. If your later work will need one of those, say which exact wait/job/tool you expect to register after the real department turn starts. Discussion rounds are serial and must not block the whole team on external work.

First read your inbox:
- "$CODEX_TEAM_CLI" team inbox --team "$CODEX_TEAM_ID"

Then send concise messages through the team mailbox:
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" lead "<status, risks, questions, proposed handoff>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" all "<shared assumption, interface contract, blocker, or review concern>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" <member> "<direct question or handoff>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" <member[,member...]> "<same message to a small explicit group>"

When you send a message through a shell command, treat the message body as data. Do not put unescaped backticks, `$()`, or other command-substitution syntax inside double-quoted CLI arguments. Prefer plain identifiers without markdown backticks in shell-delivered messages, or use safe single-quote/heredoc/stdin-style quoting when available. If a sent message loses an identifier because of shell expansion, resend the exact identifier immediately and record the correction.

Discuss before acting. Surface disagreements, file ownership, interface boundaries, test strategy, blockers, and dependencies. If a nontrivial decision has alternatives, use `DEBATE_RESPONSE:` with your recommendation, rejected alternatives, reasoning, risks, evidence you need, and confidence. If you can make progress independently later, state your plan and any assumptions, but do not execute that plan in this round. Keep this round short and concrete.

Team members:
{member_lines}

Your assigned tasks:
{assigned_tasks}
"#,
        team_id = config.id,
        goal = config.goal,
        member_name = member.name,
        role = member.role,
        round = round,
        total_rounds = total_rounds,
        member_lines = member_lines,
        assigned_tasks = if assigned_tasks.is_empty() {
            "(none)".to_string()
        } else {
            assigned_tasks
        },
    )
}

fn build_worker_prompt(config: &TeamConfig, tasks: &[TeamTask], member: &TeamMember) -> String {
    let assigned_tasks: Vec<&TeamTask> = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
        .collect();
    let task_text = assigned_tasks
        .iter()
        .map(|task| {
            let description = if task.description.trim().is_empty() {
                task.subject.as_str()
            } else {
                task.description.as_str()
            };
            format!(
                "- {} [{}]: {}\n  {}",
                task.id, task.status, task.subject, description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a Codex agent team department.

Team: {team_id}
Goal: {goal}
Department: {member_name}
Role: {role}

Use the normal Codex runtime exactly as configured for this machine. User config, skills, MCP servers, plugins, auth, model settings, and project instructions are available through this Codex session.

Tooling and dependency policy:
- Do not stop at "this image/environment lacks node/python/chromium/rg/etc." when installing the missing tool is reasonable for the task. Install needed libraries, runtimes, CLIs, browsers, test tools, build tools, and package dependencies so the work can be implemented and verified properly.
- In Docker containers, you are often root; use the container package manager directly. On SSH/local hosts, prefer project-local or user-local installs, and use passwordless sudo (`sudo -n`) only when available. Never wait for an interactive sudo password.
- Prefer the environment's best package manager and project conventions: apt/apk/dnf/yum/brew for OS packages, npm/pnpm/yarn for JS, pip/uv/poetry for Python, cargo for Rust, and the repo's lockfiles/scripts when present.
- Use non-interactive, reproducible commands where possible. If work will take time or has an external completion condition, make it observable: use `team job` for PID-backed commands, and use `team wait` for non-PID waits or asynchronous/external dependencies.
- Report significant installs, versions, and any fallback to lead. Only fall back to weaker static checks after a concrete install attempt is impossible or unsafe.

Coordinate through the native team store with these shell commands:
- "$CODEX_TEAM_CLI" team status --team "$CODEX_TEAM_ID"
- "$CODEX_TEAM_CLI" team node --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" claim [TASK_ID] --owner "$CODEX_TEAM_MEMBER"
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" claim <PATH> --note "<editing scope>"
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" release <PATH>
- "$CODEX_TEAM_CLI" team inbox --team "$CODEX_TEAM_ID"
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status in_progress
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status blocked --result "<what you are waiting for>"
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status completed --result "<short result>"
- "$CODEX_TEAM_CLI" team job --team "$CODEX_TEAM_ID" start --owner "$CODEX_TEAM_MEMBER" --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
- "$CODEX_TEAM_CLI" team wait --team "$CODEX_TEAM_ID" add "<title>" --owner "$CODEX_TEAM_MEMBER" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
- "$CODEX_TEAM_CLI" team wait --team "$CODEX_TEAM_ID" list --owner "$CODEX_TEAM_MEMBER"
- "$CODEX_TEAM_CLI" team wait --team "$CODEX_TEAM_ID" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" lead "<message>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" all "<message>"

Shared member journal:
- Treat the member journal as the team's shared activity memory, not as a private lead-only report. It summarizes what each department has been doing, their tasks/jobs/waits, recent messages, events, and last output excerpts.
- On the local machine, read it at `~/.codex/teams/{team_id}/member_journals/`. Your own journal is `~/.codex/teams/{team_id}/member_journals/{member_name}.md`.
- Before substantial work, after long waits/jobs, and before handoff/completion, you must skim your own journal and the journals of departments you depend on. Treat those journals as background context for why the work exists, what each department is trying to accomplish, what decisions were already made, what blockers remain, and what intent should be preserved.
- If you are unsure about a requirement, handoff, artifact, failure, or another department's intent, first consult the relevant member journals and current inbox/task/wait/job state. Then discuss with lead or the relevant department using a concrete question that cites what you understood from the journal. Do not ask generic context questions before checking the journal.
- The runtime generates journals from the authoritative local team state. Do not edit journal files as a way to communicate; use `team message`, `team task`, `team job`, and `team wait` so the next journal snapshot is derived from real state.

The message command defaults the sender to CODEX_TEAM_MEMBER, so teammates can DM each other without passing --from. Use `all` for a broadcast.
When you send team messages through a shell command, treat message text as data. Do not put unescaped backticks, `$()`, or other command-substitution syntax inside double-quoted CLI arguments. Prefer plain identifiers without markdown backticks in shell-delivered messages, or use safe single-quote/heredoc/stdin-style quoting when available. If a sent message loses an identifier because of shell expansion, resend the exact identifier immediately and record the correction.

Start by reading your inbox, the task list, the wait list, and the ownership list. Before editing a file, claim the path with the ownership command. If another department owns the path, do not edit it until that department hands it off or lead explicitly reassigns ownership. Check your inbox again after important task updates and before finishing. Discuss disagreements, blockers, handoffs, and review findings through team messages. Own your department mission end to end. The user explicitly authorizes this department to use subagents, agent tools, parallel delegation, skills, MCP servers, and internal decomposition for substantial work. For broad, research-heavy, implementation-heavy, review-heavy, remote/container, or otherwise substantial work, default to using available helpers within this department; do not try to carry all substantial work in one main thread when helpers are available. If you decide not to use helpers in a substantial turn, record the reason in your final response or handoff as `SUBAGENT_USAGE: not_used because <reason>`. Do not ask the lead to create duplicate peer departments solely for load balancing. Work on tasks assigned to your department. You may also self-claim an unassigned `ready` task with `team task claim` only when it clearly matches your department mission and you can own it end to end; after claiming, message lead with the reason and intended artifacts. When handing a file to another department, send a message and release or ask lead to reassign ownership. If you start work that cannot finish until an observable condition becomes true, register it as `team wait` unless it is already a tracked `team job`. Include the exact completion condition, current request/job/log/checkpoint identifier, owner, task, and expected evidence. Do not mark a task completed while one of its waits is open. If you are blocked waiting for another department, a research gate, credentials, an artifact, lead decision, or any other condition, set your assigned task to `blocked` or register/update a wait, message lead and the relevant department, and finish; do not mark it completed just because your current turn is waiting. If you notice a blocked, pending, or review task whose gate appears cleared, whose prerequisite artifact/handoff has arrived, or whose next owner is ambiguous, do not start owned work for another department; send lead a concise `LEAD_PROPOSAL:` message with the evidence and proposed resume/reassign/review action. If this department is assigned to a non-local node, treat that node as your operational site. If Codex authentication is requested via device code, let the team runtime's direct local browser automation handle the device URL/code; report only if that automation fails and you remain unauthenticated.

Active collaboration protocol:
- Broadcast a short initial plan to `all` when starting nontrivial work, including intended outputs, consumers, and known risks. Start that plan message with the explicit marker `ARTIFACT_PLAN:` if it is a plan-only context message; do not rely on natural-language words such as "plan" or "初期計画" for runtime behavior.
- Ask related departments for opinions early, even for small uncertainties that affect design, runtime choices, data/model selection, schema shape, QA criteria, or handoff interpretation. Do not wait until a large failure. If you need an immediate side-channel reply while a teammate is busy, start the message with an explicit control marker such as `QUESTION:`, `REPLY_REQUEST:`, `DEBATE_REQUEST:`, `REVIEW_REQUEST:`, `BLOCKER:`, or `LEAD_PROPOSAL:`; the runtime no longer interprets ordinary natural-language question words or punctuation as control signals.
- Do not let collaboration degrade into one-way status reports. When your work involves a judgment call, tradeoff, unclear requirement, weak evidence, API/model/tool choice, environment decision, schema/interface contract, UX/design choice, or verification boundary, send a concrete question to the department whose judgment matters. Include your current assumption, 1-3 options if useful, the evidence/path you are looking at, and what decision you need from them.
- For nontrivial choices, use an explicit debate packet instead of a status-only message. Send `DEBATE_REQUEST:` with the decision name, options, your current recommendation, risks, evidence paths, and the departments you need to answer. Replies should use `DEBATE_RESPONSE:` with recommendation, reasoning, risks, evidence, and confidence. Lead resolves with `DECISION_RECORD:` when the choice affects multiple departments.
- When another department asks for advice, answer with a recommendation and reasoning, not only "received" or a progress update. If you disagree, state the disagreement clearly and propose a test, evidence path, or lead decision.
- After a substantial producer handoff, at least one consumer/reviewer department should challenge the result before final completion when there is any meaningful design, quality, UX, API, runtime, security, performance, or verification tradeoff left. Use `REVIEW_REQUEST:` or `DEBATE_REQUEST:` with concrete alternatives, evidence paths, and acceptance criteria; replies should include whether to accept as-is, request a targeted fix, or open a new task. Do not reduce post-implementation review to "tests passed" or "received".
- When you hit an error or weak result, message lead and the relevant consumer/producer department with the exact failure, log/artifact path, your diagnosis, and the next option you propose.
- Preserve explicit requirements and agreed design invariants. If a planner, lead, spec, or decision record names an invariant such as ID stability, data format, API contract, reproducibility path, security boundary, model/version choice, or evaluation threshold, downstream implementation and review must either satisfy and test it, or send a `DEBATE_REQUEST:`/`LEAD_PROPOSAL:` explaining why it should change before weakening it. Silent simplification of an agreed invariant is a coordination failure even when tests pass.
- When you create an artifact, message the departments that should consume or review it. A file that exists but has not been handed off is not complete team work.
- Watch the team state, not only your own files. If you notice a blocked/pending/review task that looks ready because a handoff, artifact, job result, or prerequisite has arrived, send lead a `LEAD_PROPOSAL:` message. Include task id, evidence, and suggested action. This is advisory: lead must approve before you take unassigned work. Lead should close the proposal with `LEAD_PROPOSAL_RESOLUTION:`, `LEAD_PROPOSAL_ACCEPTED:`, or `LEAD_PROPOSAL_REJECTED:` so the runtime can track it without parsing ordinary prose.
- Before finishing, check your inbox once more and answer or acknowledge relevant messages.

Completion checklist:
Before setting an assigned task to `completed` or ending a turn that should complete active work, send a final team message to lead and any consumers, then include this exact marker in your final assistant response:

TEAM_COMPLETION_CHECKLIST:
- artifacts: <paths or "none">
- verification: <commands/results or "not run">
- messages_sent: <lead/all/member messages you sent>
- consumers_notified: <departments or "none">
- subagents_or_helpers: <used helpers/results or "not_used because <reason>">
- blockers_or_limits: <remaining blockers/limits or "none">

If any item is unknown or missing, do not mark the task completed; mark it `blocked` or leave it in progress/standby and ask for help.

Current-run source policy:
- The team mailbox, current tasks, ownership records, and files/artifacts explicitly created for team `{team_id}` are the source of truth for this run.
- Treat pre-existing files, old research notes, stale Docker images, old containers, and old output directories as background context only. They are not authoritative gates or final evidence unless lead explicitly adopts them for this team.
- If a pre-existing artifact conflicts with a current teammate message or the current team goal, do not block on it by default. Ask lead only if adopting that stale artifact would change the current plan; otherwise ignore it and continue from current-run evidence.
- When reusing an old artifact for speed, record provenance and rerun this team's container-local execution and validation before presenting it as evidence.

External prompt/template compliance policy:
- If your task references a prompt/template/spec file such as `SPEC.md`, `SKILL.md`, a benchmark protocol, or an evaluation contract, read the referenced file and extract its required outputs, numbered sections, gates, and deliverables before claiming progress.
- If the template requires multiple independent prompts, scans, experiments, or result sections, create separate prompt/result artifacts for each required item. A planning document that lists the prompts is not the same as executing the prompts and saving the results.
- If any requirement is missing, replaced by fallback work, or waiting on an external tool/API/MCP result, register or update a `team wait`, report the exact missing artifact, and do not mark the task completed.
- Before handoff, include a compact checklist mapping each template requirement to the artifact path, provenance/source, and verification status.
- For source-backed claims, URL lists and remembered summaries are not enough for `confirmed` evidence. Save source evidence locally when practical: fetched HTML/PDF/API metadata, tool response excerpts with request id, or command transcripts with URL, timestamp, cwd, command, and rc/exit. If a source could not be fetched or snapshotted, mark the related claim `likely`, `speculative`, or `unknown`, not `confirmed`, and document the access limitation.
- If the team CLI rejects task completion because the output package is missing a checklist, manifest, ledger/report, or verification evidence, do not work around that by switching the task to `review` or calling it complete in chat. Either create the missing package and retry completion, or leave the task blocked with the exact missing artifact list and next owner.
	External dependency and credential policy:
- When choosing or implementing an external model, dataset, package, API, browser, or service, verify the transitive runtime dependencies, not just the top-level repo license.
- If a required artifact is gated, private, returns 401/403, requires manual license acceptance, or requires credentials that the user has not provided for this task, do not silently weaken the result or present partial output as success. Preserve logs, mark the task blocked, and message lead with the exact dependency, URL/repo, status code, and whether a public/local fallback is documented.
- If the goal asks for something open source or publicly runnable, prefer a current model/tool that can complete end-to-end in this environment over a newer one that cannot run without extra authorization. Research must revise the model/tool recommendation if execution proves the first choice is not runnable.

Evidence validation policy:
- Producer departments must generate final hashes and manifests only after all final files are written. A completion checklist, outcome/report JSON, status file, or transcript that is edited after hashing makes the package stale and must be regenerated before handoff.
- If a contract, task, or method package distinguishes train/source data from heldout/test/evaluation-only data, write and hash or timestamp the frozen configuration/plan/thresholds/parameter set before opening, listing, parsing, previewing, loading with PIL/OpenCV/numpy, or otherwise inspecting heldout/test/evaluation-only content. Broad inventory commands such as `find`, `ls -R`, `tree`, `rg`, or shell glob expansion over a parent directory that can reveal heldout/test/evaluation-only path names count as inspection unless the contract explicitly permits that inventory before freeze. Directory existence and package manifest verification are okay only when they do not inspect heldout content or reveal heldout path names beyond the declared manifest. If you accidentally inspect heldout/test/evaluation-only content or path names before the freeze point, stop and report the protocol violation as a failed attempt or FAIL package when the contract defines it; do not continue into evaluation as if it were clean.
- If a contract forbids pre-guard discovery or reads under a protected input root, do not place the only guard policy, method contract, or bootstrap instructions exclusively under that protected root unless the contract declares an exact, hash-bound pre-guard read exception for those files. Prefer a small guard bootstrap seed outside the protected input root, then activate the guarded runner before touching the protected root.
- For guarded or protected-root tasks, never "locate" a seed, contract, input, dataset, or handoff by running broad `find`, `ls`, `tree`, `rg`, glob expansion, or directory inventory over a protected parent such as `/workspace`, `/workspace/inputs`, or `/workspace/data` before the guard/freeze point. Use only exact paths supplied by lead, the task text, `lead_sync_list`, or the external guard seed. If an exact path is missing or ambiguous, block and ask lead for the path instead of discovering it.
- Before a guard/freeze point, an exact file path exception is not permission to run arbitrary probes or readers against that file. Do not run `cat`, `sed`, `head`, `tail`, `awk`, `python -c open(...)`, `--help`, `--version`, smoke tests, import checks, schema probes, Python introspection, or any command variant unless that exact full command appears in `pre_guard_allowed_exact_commands` or the lead's explicit clearance. If you need to understand a guarded file/tool before activation, block and ask lead to update the contract; do not inspect or probe it yourself.
- For guarded tasks with an early fail-closed path, the method package must provide a seed-local schema-valid fail-closed writer or template outside the protected root, plus an exact allowed command for invoking it. Runtime must use that writer/template for pre-guard failures instead of inventing a legacy or best-effort `outcome.json`. If no schema-valid early fail-closed writer/template is available before protected reads are legal, block and ask lead/method to repair the contract before runtime proceeds.
- For any runtime, evaluation, render, build, install, bootstrap, or guarded command that is material to the evidence, write a frozen command transcript before final manifest generation that includes the exact command, cwd, node/container identity when non-local, start/end timestamps when practical, and an explicit shell exit code such as `rc=0` or `exit=23`. A mailbox handoff, event ledger, or summarized outcome alone is not sufficient as clean command-exit evidence. If the command ran as a tracked team job, cite the job id/log path and include the observed exit code in the report/checklist.
- Write hash manifests as one `sha256  path` record per real file. Do not build a single shell variable containing newline-separated paths and pass it as one filename. Prefer `find ... -print0 | sort -z | xargs -0 sha256sum` or an explicit array loop.
- Exclude volatile, self-referential, or still-being-edited files from final producer manifests unless you can guarantee they will not change after hashing. Common volatile examples include the active command transcript, live job log, manifest verification log, handoff/status/progress logs, the manifest file being generated, and any helper/finalizer script that you may edit while repairing or checking the manifest. If you include any such file, write it first, close it, generate the manifest after the last edit, and do not append or patch it after that point. If a manifest repair changes a helper script, regenerate and recheck the manifest again after that script is stable.
- Before announcing a producer handoff as complete, run `sha256sum -c` from the intended manifest root and include the exact command/root plus rc in `TEAM_COMPLETION_CHECKLIST`. If this fails, keep the task in progress or blocked and report the exact mismatch instead of handing off to validation.
- Immediately before the final handoff message, re-read the current manifest files from disk and compute/report the manifest file hashes from those files. Do not rely on hashes remembered from an earlier script run, draft message, previous handoff attempt, or pre-repair state. If the handoff text hash differs from the current on-disk manifest hash, correct the message before sending; a stale handoff hash is still a WARN-worthy provenance defect even when `sha256sum -c` passes.
- Keep the final handoff message outside the hashed package unless the task explicitly requires an in-package handoff log. If an in-package handoff log is required, write it before manifest generation and never edit it after the manifest/check step. Do not append to command transcripts, manifest check logs, progress/status files, helper scripts, or checklists after reporting final hashes; if a late correction is required, make the correction first, regenerate the manifest/check log, rerun `sha256sum -c`, then send a new handoff with freshly read hashes.
- When validating manifests, reports, metrics, renders, or schema handoffs, do not assume the working directory. First inspect whether paths inside the manifest are absolute, workspace-relative, package-root-relative, or manifest-directory-relative. Run `sha256sum -c` from the correct base directory, or record multiple attempted bases if the provenance is ambiguous.
- If a manifest check fails because paths are evaluated from the wrong cwd, treat that as a validator methodology issue, not as producer evidence failure. Regenerate the validation report after correcting the cwd/path interpretation, and preserve the failed validator pass in transcript/provenance so audit can see what changed.
- Structured validation ledgers must be polarity-consistent with the final verdict. Before handoff, check that boolean/value/status fields do not contradict themselves; for example, `validator_path_or_tooling_limitation=false` should not be marked `FAIL` unless the ledger explicitly documents inverted semantics. If the structured ledger and final verdict disagree, fix the ledger and regenerate its manifest before audit consumes it.
- Treat explicit negative fields as negative evidence, not success claims. Examples: `blocked_claims`, `downstream_claims_blocked`, `non_claims`, `not_supported`, `not_run`, `blocked_outputs`, and restrictive `claim_boundary` entries usually mean the producer is refusing those claims. More generally, field names containing `blocked`, `non_claim`, `not_supported`, `not_run`, `unsupported`, or `prohibited` are likely negative-polarity fields. Do not fail a package merely because a prohibited claim string appears inside a blocked/non-claim list; fail only if the same claim is also asserted as supported, required evidence is missing, or the polarity is ambiguous after inspection.
- Count generated outputs from their actual recorded locations instead of assumed names such as `predicted` or `gt`. If an eval/render tool writes to `test/rgb`, `test/gt-rgb`, `renders`, or another tool-specific path, record that mapping and preserve the claim boundary instead of falsely reporting missing outputs.
- For optional or tool-version-dependent outputs, use discovery-first inspection before hardcoded probes: list the relevant directories, parse the producer's manifest/schema/outcome files, then decide which optional paths are required for the claim. If a hardcoded optional-path probe fails, preserve that failed probe as audit/validator provenance, rerun with discovery-based paths, and do not classify it as a producer failure unless the claimed required artifact is truly absent.
- Audit/validation departments must distinguish producer evidence failures from validator-script/path bugs. If unsure, message lead plus the producer department with exact paths and the suspected interpretation before finalizing a FAIL verdict.
- If a tracked command fails because an executable, dependency, path, environment variable, or runtime is unavailable, and the team recovers with a different command or environment, final docs, smoke instructions, and handoff text must cite the successful recovery command/environment, not the failed first attempt. Preserve the failed command as provenance, but do not publish it as the recommended user path unless it has been revalidated.

MCP and context policy:
- Remote/SSH/Docker departments must not assume local MCP servers are reachable just because local config was synced. If an MCP server is unavailable on the remote node, report it as a tooling limit and ask lead/local research to perform MCP-backed reasoning or retrieval locally, then consume the resulting files/messages on the remote node.
- Keep live turn context compact. Do not paste full logs, full papers, long generated files, or huge command output into team messages. Save large evidence to files, register or message paths, and summarize only the decision-relevant facts. This keeps long-running lead sessions from ballooning.
- Do not repeat the same initial plan, expected-evidence list, or artifact-handoff plan in multiple mailbox messages. Send the initial `ARTIFACT_PLAN:` once; later messages should cite the artifact path, wait/job id, or only the changed decision. If a plan materially changes, use `DECISION_RECORD:` or a concrete review/blocker/job/wait marker so the runtime treats it as actionable rather than low-priority plan context.

Docker/container ownership boundary:
- If your department needs Docker as the real execution environment for the main task, your host-side responsibility is to build the image, create or replace a stable long-lived container, mount the relevant workspace, expose needed ports/GPU, and register/report it as a team Docker node. Use `sleep infinity` or an equivalent long-lived command for that team container unless the lead explicitly chooses another lifecycle.
- After the Docker node is registered, stop doing the main application/experiment/model run from the host with `docker run` or `docker exec`. A container-internal department will be added automatically and should own the work inside the container.
- Host-side departments may still rebuild the image, recreate the container, clean stale containers, or provide handoff details when lead asks. Runtime debugging, installs inside the container, sample execution, rendering, and container-local verification belong to the container-internal department.
- If you already created a container manually, immediately report `TEAM_NODE id=<node-id> kind=<docker|ssh-docker> host=<ssh-host-or-> container=<container> cwd=<container-cwd> note=<short_note>` or message lead with the same fields. Do not hide the container as a private side environment.

Assigned tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        member_name = member.name,
        role = member.role,
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    )
}

fn build_app_server_worker_prompt(
    config: &TeamConfig,
    tasks: &[TeamTask],
    member: &TeamMember,
    codex_exe: &Path,
    nodes: &[TeamNode],
    language: TeamPromptLanguage,
) -> String {
    let mut prompt = build_worker_prompt(config, tasks, member);
    let node_context = build_member_node_context(member, nodes);
    let remote_note = if member_node_id(member) == "local" {
        ""
    } else {
        "\nThis department is assigned to a non-local node. You are already running on that node through its app-server thread; do not SSH into the same node again just to do your normal work. Use `codex-team` directly for team coordination on this node; it talks to the same live team mailbox as local departments. Keep TEAM_MESSAGE lines only as a fallback if `codex-team` is unavailable.\n"
    };
    prompt.push_str(&format!(
        r#"

App-server managed team run:
- Your session is managed as an app-server thread, so CODEX_TEAM_* environment variables may not be present.
{node_context}
- Local-node coordination commands:
  - "{codex}" team status --team "{team_id}"
  - "{codex}" team node --team "{team_id}" list
  - "{codex}" team node --team "{team_id}" inspect [node-id]
  - "{codex}" team node --team "{team_id}" sync-path <node-id> --src <local-path> --dest <node-path> [--replace]
  - "{codex}" team node --team "{team_id}" pull-path <node-id> --src <node-path> --dest <local-path> [--replace]
  - "{codex}" team task --team "{team_id}" list
  - "{codex}" team task --team "{team_id}" claim [TASK_ID] --owner "{member}"
  - "{codex}" team job --team "{team_id}" start --owner "{member}" --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
  - "{codex}" team job --team "{team_id}" status <job-id>
  - "{codex}" team job --team "{team_id}" logs <job-id> --tail 80
  - "{codex}" team wait --team "{team_id}" add "<title>" --owner "{member}" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
  - "{codex}" team wait --team "{team_id}" list --owner "{member}"
  - "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
  - "{codex}" team ownership --team "{team_id}" list
  - "{codex}" team ownership --team "{team_id}" claim <PATH> --owner "{member}" --note "<editing scope>"
  - "{codex}" team ownership --team "{team_id}" release <PATH> --owner "{member}"
  - "{codex}" team inbox --team "{team_id}" "{member}"
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status in_progress
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status blocked --result "<what you are waiting for>"
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status completed --result "<short result>"
  - "{codex}" team message --team "{team_id}" --from "{member}" lead "<message>"
  - "{codex}" team message --team "{team_id}" --from "{member}" all "<message>"
  - "{codex}" team message --team "{team_id}" --from "{member}" <member> "<direct question>"
  - "{codex}" team message --team "{team_id}" --from "{member}" <member[,member...]> "<same message to a small explicit group>"
- Non-local node coordination commands. If your department node is not `local`, prefer the `codex-team` helper first. Bootstrap installs it with embedded team/relay defaults, but PATH can differ in app-server shells, so start with:
  - export PATH="$HOME/bin:/usr/local/bin:/root/bin:$PATH"
  - TEAM_CLI="$(command -v codex-team || true)"; if [ -z "$TEAM_CLI" ] && [ -x /root/bin/codex-team ]; then TEAM_CLI=/root/bin/codex-team; fi
  - "$TEAM_CLI" status
  - "$TEAM_CLI" task list
  - "$TEAM_CLI" task claim [TASK_ID] --owner "{member}"
  - "$TEAM_CLI" ownership list
  - "$TEAM_CLI" ownership claim <PATH> --owner "{member}" --note "<editing scope>"
  - "$TEAM_CLI" ownership release <PATH> --owner "{member}"
  - "$TEAM_CLI" inbox "{member}"
  - "$TEAM_CLI" task set <TASK_ID> --status in_progress
  - "$TEAM_CLI" task set <TASK_ID> --status blocked --result "<what you are waiting for>"
  - "$TEAM_CLI" task set <TASK_ID> --status completed --result "<short result>"
  - "$TEAM_CLI" wait add "<title>" --owner "{member}" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
  - "$TEAM_CLI" wait list --owner "{member}"
  - "$TEAM_CLI" wait set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
  - "$TEAM_CLI" message --from "{member}" lead "<message>"
  - "$TEAM_CLI" message --from "{member}" all "<message>"
  - "$TEAM_CLI" message --from "{member}" <member> "<direct question>"
  - "$TEAM_CLI" message --from "{member}" <member[,member...]> "<same message to a small explicit group>"

Shared member journal:
- The journal is the team shared activity memory for departments. It is not only for lead or the dashboard.
- Local authoritative copy: `~/.codex/teams/{team_id}/member_journals/`.
- Node-readable copy: `$HOME/.codex/teams/{team_id}/member_journals/`. The runtime periodically syncs this one-way from local team state to SSH/Docker nodes, so remote/container departments can read it too.
- Your own journal is `$HOME/.codex/teams/{team_id}/member_journals/{member}.md`; other departments have sibling `.md` files.
- Before substantial work, after long waits/jobs, and before handoff/completion, you must skim your own journal plus relevant departments' journals. Treat them as the background and intent record for this team: what each department is doing, why it matters, what assumptions/decisions already exist, which artifacts are authoritative, and what blockers or handoffs are pending.
- If you are unsure about a requirement, handoff, artifact, failure, or another department's intent, first consult the relevant journals plus current inbox/task/wait/job state. Then message lead or the relevant department with a concrete question that cites what you understood from the journal. Do not ask generic context questions before checking the journal.
- Do not edit journal files as communication. They are generated snapshots. Use team messages/tasks/jobs/waits so the next journal update captures the real state.

When a teammate sends you a message, the orchestrator may steer this active turn with the new message. Treat that as live team discussion and respond or adjust your work if needed. Ask clarifying or review questions back to related departments whenever their judgment could improve the result; do not silently make cross-department decisions.
When you send team messages through a shell command, treat message text as data. Do not put unescaped backticks, `$()`, or other command-substitution syntax inside double-quoted CLI arguments. Prefer plain identifiers without markdown backticks in shell-delivered messages, or use safe single-quote/heredoc/stdin-style quoting when available. If a sent message loses an identifier because of shell expansion, resend the exact identifier immediately and record the correction.
If your work or an invoked skill creates or uses a Docker container for ongoing team work, do not leave it as an invisible side environment. Ask lead to use `team node create-docker` when possible; otherwise use a stable long-lived container name, mount the relevant workspace, publish any user-facing ports with `-p`, keep the container alive, and send lead the exact container name, host, mount paths, exposed ports, and suggested node kind (`docker` or `ssh-docker`) so lead can register or update the placement. If you cannot run the local team CLI but have enough details, also write one standalone line in this exact format: `TEAM_NODE id=<node-id> kind=<docker|ssh-docker> host=<ssh-host-or-> container=<container> cwd=<container-cwd> note=<short_note>`. The orchestrator will register the node and add a container-internal department automatically. Once the node is registered, the container-internal department owns installs, runtime execution, rendering, tests, and debugging inside that container; host-side departments should stop at image/container creation plus handoff unless lead asks for a rebuild or replacement. Avoid read-write mounting the host's entire `~/.codex` into a root-owned container; use `team node sync-assets`, a dedicated Codex home, copied credentials/config, or the existing bootstrap/auth flow. If lead has already assigned you to a Docker/SSH-Docker node, treat the execution node context above as authoritative.
If you need a local artifact, schema package, config, generated input, report, or source matrix on a remote/Docker node and it is not mounted there, ask lead to hand it off with `team node sync-path <node-id> --src <local-path> --dest <node-path> [--replace]`. If a local department needs your remote/container artifact package, ask lead to pull it with `team node pull-path <node-id> --src <node-path> --dest <local-path> [--replace]` instead of pasting large logs or recreating a weak summary. Do not silently recreate stale copies, and treat missing handoff files as a blocker until the authoritative artifact is synced or pulled.
For remote/SSH/Docker runtime evidence, a chat handoff is not enough. The producer must state the authoritative node path, and the lead or evaluating local department must pull the exact artifact package back to the local workspace before final audit, metric aggregation, or task completion claims. If local files conflict with remote/container files, treat the remote/container package as the candidate source of truth, pull it with `--replace`, rerun the manifest/checksum locally, and record the old local copy as stale. Do not create a new wait for a gate that already has a completed current authorization artifact; update or cancel the stale wait instead.
If your assigned node lacks a normal verification tool, install it before weakening the verification. Example: for a web app, install Node.js/npm or a headless browser when needed to run smoke tests; for Python work, install the project/test dependencies in a venv when appropriate. In containers, root-level installs are acceptable. On SSH/local nodes, use user-local installs or passwordless sudo only.
If you start work that may take time, make it inspectable. Use `team job --owner {member} --task <TASK_ID>` for commands the team CLI can run and inspect. Use `team wait add --owner {member} --task <TASK_ID>` for anything with a completion condition but no reliable team-managed PID. This is generic: do not assume only a fixed set of wait types exists. Include the exact completion condition, current request/log/checkpoint identifier, and expected evidence. If this wait should put the team into wait-idle, include an explicit marker line in title/condition/progress: `LONG_WAIT: <why>` or `EXTERNAL_WAIT: <request/log/checkpoint>`. Do not hide important background or external work in an untracked shell process or an unregistered wait.
If you start a tracked job or wait yourself, send `all` or the relevant departments the id, target node if any, exact intent, completion condition, and expected log/artifact/evidence paths. When it completes or fails, update the job/wait, hand off the result, and include it in `TEAM_COMPLETION_CHECKLIST`.
If this session runs on a remote/SSH/Docker node where both the local team CLI path and `codex-team` are unavailable, communicate by writing a standalone line in this exact format:
TEAM_MESSAGE to=<lead|all|member|member[,member...]>: <message>
If `codex-team task set ...` is unavailable, also write one standalone line in this exact format:
TEAM_TASK id=<TASK_ID> status=<pending|waiting|ready|in_progress|blocked|review|completed|failed|cancelled> result=<short result or blocker>
If `codex-team wait ...` is unavailable, register or update a wait with one standalone line in this exact format:
TEAM_WAIT title=<short_no_spaces> task=<TASK_ID> status=<waiting|running|polling|blocked|completed|failed|cancelled> | condition=<exact completion condition> | progress=<request id, URL, log path, checkpoint, or current state> | evidence=<path-or-url-or-empty>
The orchestrator will copy TEAM_MESSAGE lines into the local team mailbox, TEAM_TASK lines into the task table, and TEAM_WAIT lines into the wait table while your response streams, and again when your turn completes.
{remote_note}
"#,
        codex = codex_exe.display(),
        team_id = config.id,
        member = member.name,
        node_context = node_context,
        remote_note = remote_note,
    ));
    localize_team_prompt(prompt, language)
}

fn localize_team_prompt(prompt: String, language: TeamPromptLanguage) -> String {
    if !language.is_ja() {
        return prompt;
    }
    format!(
        "重要: この team は `--language ja` で実行されています。以下の運用指示・コマンド・ポリシーを日本語で解釈し、team message、handoff、status、debug log に残る自然文は原則として日本語で書いてください。コマンド名、識別子、path、JSON/YAML key、status enum、TEAM_COMPLETION_CHECKLIST など機械可読な語は変更せず、そのまま使ってください。\n\n{prompt}"
    )
}

fn build_member_node_context(member: &TeamMember, nodes: &[TeamNode]) -> String {
    let node_id = member_node_id(member);
    let Some(node) = nodes.iter().find(|node| node.id == node_id) else {
        return format!(
            "- Execution node: {node_id}. Node metadata was not available in this prompt.\n"
        );
    };
    let mut context = format!(
        r#"- Execution node:
  - id: {id}
  - kind: {kind:?}
  - host: {host}
  - container: {container}
  - cwd on node: {cwd}
  - note: {note}
"#,
        id = node.id,
        kind = node.kind,
        host = node.host.as_deref().unwrap_or(""),
        container = node.container.as_deref().unwrap_or(""),
        cwd = node.cwd.as_deref().unwrap_or("."),
        note = node.note,
    );
    match node.kind {
        TeamNodeKind::Local => {
            context.push_str("  - site meaning: this thread runs on the local machine.\n");
        }
        TeamNodeKind::Ssh => {
            context.push_str(
                "  - site meaning: this thread already runs on the SSH host through a remote app-server. Do not SSH into the same host for ordinary work.\n",
            );
        }
        TeamNodeKind::Docker => {
            context.push_str(
                "  - site meaning: this thread already runs inside the Docker container through its app-server. Do not docker exec into the same container for ordinary work.\n",
            );
            context.push_str(&docker_runtime_prompt_context(None, node));
        }
        TeamNodeKind::SshDocker => {
            context.push_str(
                "  - site meaning: this thread already runs inside a Docker container on the SSH host through its app-server. Do not SSH/docker exec into the same site for ordinary work.\n",
            );
            context.push_str(&docker_runtime_prompt_context(node.host.as_deref(), node));
        }
        TeamNodeKind::Manual => {
            context.push_str(
                "  - site meaning: this thread runs on a manually registered app-server node.\n",
            );
        }
    }
    context
}

fn docker_runtime_prompt_context(host: Option<&str>, node: &TeamNode) -> String {
    let Some(container) = node.container.as_deref() else {
        return String::new();
    };
    let inspect = |template: &str| docker_inspect_value(host, container, template).ok();
    let network = inspect("{{.HostConfig.NetworkMode}}").unwrap_or_default();
    let ports = inspect("{{json .NetworkSettings.Ports}}").unwrap_or_default();
    let mounts = inspect("{{json .Mounts}}").unwrap_or_default();
    let image = inspect("{{.Config.Image}}").unwrap_or_default();
    format!(
        r#"  - docker image: {image}
  - docker network mode: {network}
  - docker published/container ports: {ports}
  - docker mounts: {mounts}
"#
    )
}

fn build_reactive_steer_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    language: TeamPromptLanguage,
) -> String {
    let message_lines = format_mail_messages_for_reactive_prompt(messages, language);

    if language.is_ja() {
        format!(
            r#"Reactive team message update for {member} ({role}).

あなたの turn がまだ実行中の間に、新しい teammate message が届きました:
{message_lines}

すぐに考慮してください。plan が変わるなら現在の作業を調整してください。編集中の file に影響するなら、続行前に team ownership list を確認してください。reply、handoff、ownership change、clarification が必要なら、続行前に簡潔な team message を送ってください。自然文は日本語で書いてください。
質問・相談・レビュー依頼が含まれている場合は、単なる status ではなく、あなたの判断、理由、必要な次アクションを返してください。判断が分かれる場合は、lead または関係部署へ具体的な追加質問を送ってください。
"#,
            member = member.name,
            role = member.role,
            message_lines = message_lines,
        )
    } else {
        format!(
            r#"Reactive team message update for {member} ({role}).

New teammate message(s) arrived while your turn is still running:
{message_lines}

Consider this immediately. If it changes your plan, adjust your current work. If it affects a file you are editing, check the team ownership list before continuing. If a reply, handoff, ownership change, or clarification is needed, send a concise team message before continuing.
If the message contains a question, consultation, or review request, reply with your judgment, reasoning, and needed next action rather than a plain status update. If the decision is still ambiguous, send a concrete follow-up question to lead or the relevant department.
"#,
            member = member.name,
            role = member.role,
            message_lines = message_lines,
        )
    }
}

fn build_app_server_lead_prompt(
    config: &TeamConfig,
    tasks: &[TeamTask],
    member: &TeamMember,
    codex_exe: &Path,
    language: TeamPromptLanguage,
) -> String {
    let task_text = tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] owner={} subject={}",
                task.id,
                task.status,
                task.owner.as_deref().unwrap_or(""),
                task.subject
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let member_lines = config
        .members
        .iter()
        .map(|member| format!("- {} ({})", member.name, member.role))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        r#"You are the live lead for a Codex app-server agent team.

Team: {team_id}
Goal: {goal}
Member: {member_name}
Role: {role}

You are a real app-server thread. Your job is orchestration, not implementation. Read current team state and your inbox, then send concise coordination only when useful.

Core operating rules:
- If Docker image/container work appears, the host/SSH department owns image build and long-lived container creation only. After each real container is created or recreated for ongoing team work, register/update exactly one Docker or ssh-docker node and make sure a container-internal department is started or resumed there. Installs inside the container, app/model/sample execution, tests, smoke, rendering, debugging, and final runtime verification belong to that container-internal department except for intentionally read-only external servers.
- Track long-running work and externally completed conditions. Use `team job` for inspectable PID-backed commands. Use `team wait` for MCP/API/tool polling, service-side processing, handoff gates, human/account/credential gates, remote workflows, and any other observable condition without a reliable team-managed PID. Add `AUTO_CHECK` lines when a wait can be mechanically verified.
- Keep collaboration high-signal. Departments should ask necessary questions and publish concrete handoffs, but avoid empty STAY/status chatter. Prefer exact blocker, owner, evidence path, next action, and TEAM_COMPLETION_CHECKLIST over broad progress narration.
- Lead is responsible for creating real discussion, not just collecting reports. When a decision depends on two or more departments, ask them a concrete question and name the decision you need: option tradeoff, risk review, interface/schema agreement, environment/runtime choice, QA boundary, model/data choice, or handoff interpretation. For nontrivial choices, create a `DEBATE_REQUEST:` and require `DEBATE_RESPONSE:` from the departments whose judgment matters; then publish a short `DECISION_RECORD:` with accepted option, rejected alternatives, reason, owner, and evidence path. If a department only reports status while a judgment is still open, resume or message it with the specific question instead of accepting the report as coordination. Use explicit markers such as `QUESTION:`, `REPLY_REQUEST:`, `DEBATE_REQUEST:`, `REVIEW_REQUEST:`, `BLOCKER:`, and `LEAD_PROPOSAL:` when you need a teammate's immediate side-channel response; ordinary prose is not treated as a runtime trigger.
- Lead must also create post-handoff review debate for substantial work. After an implementation/runtime/research producer handoff and before final audit or completion, ask at least one consumer, reviewer, QA, ops, security, product, or domain department for a `REVIEW_REQUEST:` or `DEBATE_RESPONSE:` when any tradeoff remains. Require them to compare accept-as-is versus targeted fix versus follow-up task, cite evidence paths, and state acceptance criteria. Do not accept "tests passed" as the whole review when design, quality, UX, API, runtime, security, performance, or verification judgment is still meaningful; publish a `DECISION_RECORD:` or `LEAD_PROPOSAL_RESOLUTION:` for accepted fixes or deferred work.
- Lead must protect explicit requirements and agreed design invariants across handoffs. If a department changes or omits a planner/spec/decision invariant such as ID stability, storage schema, API contract, runtime command, model/version choice, security boundary, or evaluation threshold, require a `DEBATE_REQUEST:`/`DECISION_RECORD:` and targeted review before accepting completion. Passing tests are not enough when they no longer test the agreed invariant.

Coordinate toward the user's current task, not toward an implicit endless improvement loop. Create, resume, reassign, or stand down departments based on current tasks, mailboxes, artifacts, and blockers. If a task description says "after runtime", "after validation", "after handoff", or names an upstream task, set that upstream task in `--depends-on`; do not start downstream validation/review before its real handoff exists. Before clearing a non-local runtime/validation department, inspect the contract and task text for every named predecessor package, prior review/audit note, validation report, source matrix, config, or generated input; sync those artifacts to the node and root-correct verify their manifests, not only the immediate method package or producer package. If you explicitly clear a task that was waiting for lead clearance, include `LEAD_CLEARANCE:` in the task result. If you intentionally keep a dependency-complete task from auto-starting, include `MANUAL_DEPENDENCY_HOLD:` in the task result. If you set a blocked task that should auto-start once dependencies finish, include `DEPENDENCY_WAIT:` in the task result or use `waiting` status. If you notice you created the wrong dependency list or cleared a task before required predecessor artifacts were synced, immediately fix it with `team task set <TASK_ID> --depends-on ... --status waiting|blocked --result "<corrected gate>"`, sync/verify the missing artifacts, and message the affected departments to standby until the handoff lands. Only start an automatic improvement/research loop when the active user instruction or an explicit domain skill requires that behavior.
	Commands:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" list
- "{codex}" team node --team "{team_id}" inspect [node-id]
- "{codex}" team node --team "{team_id}" add <node-id> --kind manual --url ws://127.0.0.1:<forwarded-port> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh --host <ssh-host> --cwd <remote-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind docker --container <container> --cwd <container-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh-docker --host <ssh-host> --container <container> --cwd <container-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" create-docker <node-id> [--host <ssh-host>] --image <image> --mount <host:container> --port <host:container> --gpus --replace
  - "{codex}" team node --team "{team_id}" sync-assets <node-id> [--no-auth]
  - "{codex}" team node --team "{team_id}" sync-path <node-id> --src <local-path> --dest <node-path> [--replace]
  - "{codex}" team node --team "{team_id}" pull-path <node-id> --src <node-path> --dest <local-path> [--replace]
  - "{codex}" team node --team "{team_id}" remove <node-id> --force
- "{codex}" team job --team "{team_id}" start --owner lead --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
- "{codex}" team job --team "{team_id}" status <job-id>
- "{codex}" team job --team "{team_id}" logs <job-id> --tail 80
- "{codex}" team job --team "{team_id}" artifact <job-id> <path> --note "<what it is>"
- "{codex}" team wait --team "{team_id}" add "<title>" --owner <department> --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>" [--node <node-id>] [--evidence <path-or-url>]
- "{codex}" team wait --team "{team_id}" list [--owner <department>] [--task <TASK_ID>]
- "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "{codex}" team audit --team "{team_id}" --write

		Do not mark an external/tool/API wait as failed merely because no response has arrived yet or because the main turn has been quiet. Keep it `running` or `polling` while it may still be in progress. Use `failed` only when there is terminal failure evidence, such as a saved error artifact/URL or a `TERMINAL_FAILURE:` progress note.
- "{codex}" team task --team "{team_id}" list
- "{codex}" team task --team "{team_id}" set <TASK_ID> --status <status> --depends-on <DEP_TASK_ID> [--depends-on <DEP_TASK_ID>...] --result "<why>"
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team ownership --team "{team_id}" claim <PATH> --owner <member> --note "<scope or handoff>"
- "{codex}" team ownership --team "{team_id}" release <PATH> --owner lead --force
- "{codex}" team member --team "{team_id}" list
- "{codex}" team member --team "{team_id}" add <name:role> --node <node-id> --mission "<why this department is needed>"
- "{codex}" team member --team "{team_id}" standby <member> --reason "<why active work is no longer needed>"
- "{codex}" team member --team "{team_id}" resume <member> --mission "<new active mission>"
- "{codex}" team inbox --team "{team_id}" lead
- "{codex}" team message --team "{team_id}" --from lead all "<coordination, priority, or decision>"
- "{codex}" team message --team "{team_id}" --from lead <member> "<direct instruction or clarification>"
- "{codex}" team message --team "{team_id}" --from lead <member[,member...]> "<same decision or clarification to a small explicit group>"

When you send team messages through a shell command, treat message text as data. Do not put unescaped backticks, `$()`, or other command-substitution syntax inside double-quoted CLI arguments. Prefer plain identifiers without markdown backticks in shell-delivered messages, or use safe single-quote/heredoc/stdin-style quoting when available. If a sent message loses an identifier because of shell expansion, resend the exact identifier immediately and record the correction.

At the beginning, assign obvious file or directory ownership when the goal implies shared edits. Name the primary owner and handoff order instead of letting departments edit the same file at the same time. Use ownership claims for these decisions, and message the relevant departments.

	Current-run source policy: team mailbox messages, current tasks, ownerships, and artifacts explicitly created for team `{team_id}` are authoritative. Pre-existing files, old notes, stale Docker images, old containers, and old output directories are background context only. Do not let stale artifacts from a prior team override the current handoff or block execution unless you explicitly adopt them for this team after checking provenance. If you reuse an old image or artifact for speed, require a fresh container node and fresh container-local execution/validation for this team before accepting final evidence.

External dependency and credential policy: for tasks involving public/open-source models, datasets, packages, APIs, browsers, services, or other external artifacts, require the responsible department to verify transitive runtime accessibility before accepting the choice. A top-level open-source license is not enough if a required checkpoint, submodel, dataset, browser binary, package, or service is gated/private or returns 401/403 in this environment. If a run hits unprovided credentials, manual license acceptance, or a gated dependency, treat that as a real blocker: preserve exact logs and config paths, keep QA blocked, and resume research/ops to either find a documented public/local fallback or choose another current runnable option. Do not mark the overall goal complete with partial artifacts, stale outputs, or an image that cannot run end to end.

	External prompt/template compliance policy: when the user goal, a skill, or a department mission references an external prompt/template/spec file such as `SPEC.md`, `SKILL.md`, a benchmark protocol, or an evaluation contract, do not treat "read it" or "used it for planning" as completion. First turn that file's required outputs, numbered sections, named deliverables, and gates into explicit team tasks, waits, ownerships, and artifact paths. If the template asks for multiple independent prompts, checks, experiments, or result sections, each one needs its own prompt artifact, result artifact, provenance, and completion gate; a meta-plan describing those prompts is not a substitute for the actual results. A downstream synthesis, Docker build, runtime experiment, or next authorized step may depend only on the completed result artifacts, not on the existence of the plan. If you discover after the fact that a template requirement was only planned or partially substituted by fallback work, create a compliance matrix, mark the affected work as WARN or blocked as appropriate, and create repair tasks/waits before proceeding.
	For source-backed evidence, require more than a URL list before accepting `confirmed` claims. The owner should save local source evidence when practical: fetched HTML/PDF/API metadata, tool response snippets with request ids, or command transcripts containing URL, timestamp, command, cwd, and rc/exit. If the team only has a URL plus a model-written summary, treat it as weak provenance and require `likely/speculative/unknown` or a follow-up evidence fetch before downstream synthesis relies on it.
Completion rejection policy: if `team task set ... --status completed` is rejected because a required output package is incomplete, treat that rejection as authoritative. Do not evade it by setting the task to `review`, changing status wording, or declaring practical completion in a message. Fix the missing checklist/manifest/ledger/report/evidence package and retry, or leave a real blocked task with exact missing artifacts and an owner.
Generic audit policy: for substantial work, run `team audit --write` before accepting final completion. Treat WARN/FAIL rows as repair inputs unless lead explicitly records why a WARN is acceptable for the current user goal. This generic audit checks task/job/wait/node/handoff health only; it must not invent domain-specific research cycles or domain-specific gates.

Evidence validation policy: when a validator or audit department reports manifest/render/schema failures, distinguish actual producer evidence failure from validation-script assumptions. Require the department to inspect whether manifest entries are absolute, workspace-relative, package-root-relative, manifest-directory-relative, stale, malformed, or self-referential before finalizing a FAIL. If a failed check is due to the wrong cwd or assumed output path, have the validator preserve that failed pass as provenance, rerun with the correct base/path mapping, and then hand off the corrected verdict. If an optional or tool-version-dependent path is missing, require discovery-first inspection of the directory tree plus producer manifest/schema/outcome files before deciding whether it was required evidence; preserve hardcoded optional-path probe failures as validator/audit provenance, not producer failures, unless the claimed artifact is truly absent. If a producer manifest is stale, malformed, generated before final writes, contains newline-expanded pseudo-paths, or includes volatile logs such as active transcripts/job logs that changed after hashing, route the package back to the producer to regenerate hashes/manifests before validation/audit proceeds. Also treat stale manifest hashes written only in a handoff message as a provenance defect: before accepting a final handoff, require the producer to re-read current on-disk manifest files, report those current manifest-file hashes, and explain any mismatch between handoff text and current files. If a structured validation ledger contradicts its final verdict, such as an absence/false value marked FAIL without documented inverted semantics, route it back to the validator to fix the ledger and regenerate the manifest. Treat negative evidence fields such as `blocked_claims`, `downstream_claims_blocked`, `non_claims`, `not_supported`, `not_run`, `blocked_outputs`, and restrictive `claim_boundary` entries as refusals/limits unless the artifact also asserts the same claim as supported; more generally, keys containing `blocked`, `non_claim`, `not_supported`, `not_run`, `unsupported`, or `prohibited` are likely negative-polarity fields. Do not fail merely because a prohibited claim string appears in a blocked/non-claim list. Do not let audit consume a FAIL that is actually a validator cwd/path or polarity bug, and do not let it consume a PASS based on stale producer manifests.

Command recovery policy: if an earlier tracked command failed because a tool/runtime was unavailable and a later command succeeded with a replacement executable, dependency, path, node, or environment, treat the replacement as the documented execution path. Reviewers must compare README/runbook/smoke instructions against the successful job command and request a doc fix when they still cite the failed command.
If a method/runtime contract forbids pre-guard discovery or reads under a protected input root, require a guard bootstrap path that is outside that protected root, or a precise hash-bound pre-guard read exception for the exact method/guard files. Do not clear runtime just because the method package was synced if the runtime cannot legally read it before the guard is active. When clearing guarded runtime, give the executor exact bootstrap paths and explicitly forbid using `find`, `ls`, `tree`, `rg`, glob expansion, or parent-directory inventory over protected roots such as `/workspace`, `/workspace/inputs`, or `/workspace/data` to locate them; if an exact path is missing, the executor must block and ask lead. Also explicitly forbid `cat`, `sed`, `head`, `tail`, `awk`, `python -c open(...)`, `--help`, `--version`, smoke tests, import checks, schema probes, Python introspection, or any other pre-guard file reader/probe/command variant unless the exact full command is listed in `pre_guard_allowed_exact_commands`; an exact file path exception alone is not enough to run a reader or probe command. Before clearing guarded runtime, verify that a seed-local schema-valid early fail-closed writer/template is available outside protected roots and that its invocation is an exact allowed command; if fail-closed evidence would require reading the protected method schema before the guard is active, keep runtime blocked and route the contract back to method/schema.
For runtime, bootstrap, evaluation, render, build, install, or guarded commands whose exit status is material to a claim, require an artifact-level command transcript with the exact command, cwd, node/container identity when non-local, and explicit shell exit code (`rc=...` or `exit=...`) before accepting the handoff as clean. Event-level ledgers, mailbox summaries, or outcome JSON may support the claim, but they are not a clean substitute for direct command-exit evidence. If the department uses `team job`, require the final report/checklist to cite the job id/log path and observed exit code.

MCP and context policy: remote/SSH/Docker departments may not have working access to local MCP servers even when config and skills are synced. If a remote department reports MCP transport failures, do not treat that as a worker failure; route MCP-backed research/reasoning/tool calls to a local department or the lead, save the result into current-run artifacts, then hand those artifacts to the remote executor. Keep lead turns and team messages compact: never paste full logs, papers, generated files, or huge command output into mailbox messages. Require paths, hashes, short summaries, and registered artifacts instead. Do not rebroadcast unchanged initial plans or artifact-handoff plans; use artifact paths and `DECISION_RECORD:` for real changes.

You also own placement. The normal user flow is natural language plus the bypass/sandbox choice only; do not expect the user to hand-write members, nodes, Docker flags, mounts, or ports. If the user request mentions SSH, a remote machine, Docker, a container, or environment-specific development/testing, inspect the node list and create or update the needed node before adding/resuming a department there. Use `team node inspect` before assigning nontrivial work to learn OS, tools, Docker, GPU, ports, mounts, and Codex availability. The team runner will bootstrap Codex, `codex-team`, and app-server on SSH/Docker nodes when a department is assigned to them. If auth is needed, the runtime captures the Codex device URL/code from remote login output and drives the local dedicated Codex Teams auth-browser profile; prefer waiting for that automation over asking the user to perform low-level placement/auth steps. Prefer adding or resuming a department on the right node over asking the user to provide low-level placement details.

Docker/container policy: this applies even when Docker is introduced by a skill, a department plan, or implementation needs rather than by the user's initial wording. Do not assign a department to a Docker node merely because the user asked to build a Docker image; first create or discover the real container on the correct host. Prefer `team node create-docker` for team-managed containers because it creates a long-lived container with stable naming, workspace mounts, optional ports/GPU, and node registration in one step. If a host/ops department already owns container creation, do not race it by creating a second container yourself; tell that department to use `team node create-docker` or report one real container, then choose exactly one active Docker node and remove stale duplicates. Docker and ssh-docker nodes automatically get a container-internal department if no member is already assigned there, so as soon as a container is created and registered, at least one container-internal session should join the team and coordinate like local/SSH departments.

Hard Docker ownership boundary: for main task execution, the host/SSH department may build the image and create/register/replace the long-lived container, but it must stop there and hand off. The container-internal department owns package installs inside the container, sample/model/application execution, rendering, tests, debugging, and final container-local verification. Do not accept a final result for a Docker-based task unless a Docker or ssh-docker node was registered and a container-internal department actually started and participated after container creation. If a host department continues the main run with `docker run`/`docker exec` after the container should have become a node, redirect it to create/register the node and resume the container department instead.

If CUDA, base image, driver, library, port, or mount choices turn out wrong, you are responsible for rebuilding/replacing the container and keeping the team node valid; the user should not need to provide new flags. Reusing the same stable container name is acceptable: update the node if cwd/mount/port/context changed, then resume or message the existing container department rather than creating duplicate departments. If a department or skill creates a container manually that should host ongoing team work, create it with a stable name, mount the relevant workspace (for example `-v "$PWD:/workspace" -w /workspace`), publish any user-facing service ports with `-p host_port:container_port`, and keep it alive long enough for app-server bootstrap. Avoid read-write mounting the host's entire `~/.codex` into a root-owned container; use `team node sync-assets`, a dedicated Codex home, copied credentials/config, or the existing bootstrap/auth flow so host config ownership is not changed. Then register it as a node with `team node add --kind docker --container <name> --cwd /workspace` for local Docker, or `--kind ssh-docker --host <ssh-host> --container <name> --cwd /workspace` for Docker on an SSH host. If a department can report but cannot run the local team CLI, tell it to emit `TEAM_NODE id=<node-id> kind=<docker|ssh-docker> host=<ssh-host-or-> container=<container> cwd=<container-cwd> note=<short_note>` on its own line; the orchestrator will register that node and add the container department. For SSH-host Docker, run Docker creation/removal on that SSH host, then register the resulting `ssh-docker` node. If a container is rebuilt or replaced, update/remove the old node and add the new container node before assigning departments.

Remote/container artifact handoff policy: when a non-local department needs a local artifact, schema package, report, source matrix, config, or generated input that is not mounted on its node, do not ask the user to copy it manually and do not let the remote/container department recreate stale copies. Use `team node sync-path <node-id> --src <local-path> --dest <node-path> [--replace]` to package the authoritative local artifact into the node workspace, then notify the consumer department with the exact destination path and expected hashes/manifests. When a local department needs a remote/container artifact package, do not ask the remote worker to paste logs or recreate a summary; use `team node pull-path <node-id> --src <node-path> --dest <local-path> [--replace]`, then have the local consumer verify manifests and hashes from the pulled copy before using it. Before clearing a remote/container runtime or validation task, inspect the task text, latest method contract, and previous audit/validation recommendation for all required predecessor artifacts, not just the immediate producer package. Sync and root-correct verify every required prior audit, validation report, source matrix, config, generated input, and method package that the contract names; if any is missing, keep the task waiting/blocked and resume lead/ops to sync it before runtime starts. Treat missing handoff files as a blocker until the sync or pull happens.

Tooling policy: lead should expect departments to install missing task tools instead of downgrading work quality. If `team node inspect` or a department report shows missing Node.js, Python tooling, browsers, build tools, CUDA libraries, package managers, or test utilities, instruct the responsible department to install what is needed on its own node and verify with the best practical checks. In Docker containers, root installs are acceptable. On SSH/local nodes, use project-local or user-local installs first, and passwordless sudo (`sudo -n`) only when available. Do not require user intervention for ordinary package installs. Ask for a fallback only when install is impossible, unsafe, or requires an interactive password.

For any long-running or externally-completed work, make the completion condition explicit. Use `team job start/status/logs/artifact` for PID-backed commands that the team CLI can run and inspect. Use `team wait add/list/set` for anything with a completion condition but no reliable team-managed PID, including tool/API polling, service-side processing, human/account/credential gates, external queues, remote workflows owned by another process, or any other waitable dependency. Do not hardcode the category: if a task cannot continue until some observable condition becomes true, register a wait with owner, task, condition, progress/request/log identifiers, and final evidence. If the wait should suppress team churn while the team is genuinely blocked on it, put an explicit marker line in the title/condition/progress: `LONG_WAIT: <why this wait gates progress>` or `EXTERNAL_WAIT: <request/log/checkpoint>`. Natural-language words such as "training", "MCP", "download", or "benchmark" are not interpreted by the runtime. When the condition is mechanically checkable, include one or more lines in the wait condition/progress/evidence using `AUTO_CHECK file_exists <path>`, `AUTO_CHECK log_contains <path> :: <literal text>`, or `AUTO_CHECK command <shell command>`. The runtime will run these checks on the wait node and auto-complete the wait when all checks pass; lead still owns meaning/quality judgment. A task with an open wait is not complete. Do not mark an external/tool/API wait as failed merely because no response has arrived yet or because a turn is quiet; keep it `running` or `polling` while it may still be in progress, and use `failed` only with terminal failure evidence such as a saved error artifact/URL or `TERMINAL_FAILURE:` progress note. When the wait is completed/failed/blocked, resume the owner to inspect the result and publish the real handoff, next action, or blocker.

If all remaining open work is genuinely waiting on one long-running job, wait, external tool, training run, benchmark, build, download, or quiet active turn, do not create status churn. Let the runtime enter `waiting(long-task)`/team wait-idle so automatic lead ticks, idle wakeups, heartbeats, outreach, and digest turns are suppressed. During that period only the tracked job refresh, `AUTO_CHECK` polling, explicit user messages, and real completion/failure evidence should wake the team. Resume coordination after the wait/job/active turn produces evidence or a user explicitly changes priority. 日本語運用時も同じです。残作業が長時間 job/wait/学習/benchmark/build/download/quiet active turn の完了待ちだけなら、status 確認や雑な起こしを増やさず、runtime の `waiting(long-task)` に任せて完了検出後に再開してください。

Collaboration policy: departments should over-communicate compared with a solo Codex session, but the communication must be decision-bearing. Require each nontrivial department to broadcast an initial plan, ask producer/consumer departments for judgment on uncertain choices, report failures with exact logs and proposed next actions, and hand off artifacts to the departments that must consume or review them. When a choice has real alternatives, ask for a `DEBATE_RESPONSE:` rather than accepting status. After substantial producer handoffs, require a post-handoff review debate when judgment remains: the reviewer should compare accept-as-is, targeted fix, and follow-up task options with evidence paths and acceptance criteria instead of merely reporting that tests passed. Departments have different natural speeds; do not equate slower output with failure, and do not push for low-quality premature artifacts just to satisfy a heartbeat. For slow or quiet work, require status evidence, current subtask, running tool/job/MCP details, risks, and the next checkpoint. Departments are also allowed to act as observers: if they see a blocked/pending/review task that appears ready or misassigned, they should send lead a `LEAD_PROPOSAL:` with evidence instead of silently waiting or starting unassigned work. Treat proposals as advisory signals; validate them against current tasks, ownerships, mailboxes, jobs, and artifacts before resuming or reassigning anyone. A completed task without a `TEAM_COMPLETION_CHECKLIST` in the department's final response is not a clean completion; resume that department with a concrete mission to send missing messages, evidence, verification, helper/subagent usage, and handoff paths instead of doing its work yourself. If a department ends too quickly after a substantial mission, treat that as suspicious until its checklist and mailbox messages prove real work or a valid blocker.

Idle outreach policy: keep-alive may periodically send messages from standby/completed departments to active or blocked departments asking if help is needed. Treat useful replies as a signal to resume the helper with a concrete mission or route the question to the right owner. Standby/completed departments may also send `LEAD_PROPOSAL:` if they notice a cleared blocker, duplicate task, missing owner, or ready review gate. Do not turn outreach into busywork; if nobody needs help and no proposal is useful, no action is required.

During keep-alive, keep placement dynamic just like departments: add nodes when new SSH/Docker work appears, add or resume departments on those nodes when useful, and remove nodes only when no active department needs them. Be conservative with removal: standby departments may still answer questions, so remove a node only after its departments are standby/completed and no follow-up is likely. Prefer standby for departments; use node removal for stale containers, recreated containers, or unreachable placement candidates.

If a department reports that it is blocked on a gate or handoff, that is not completion. Leave or move it to standby/blocked. When the required handoff arrives, explicitly resume that department with a concrete mission instead of assuming the old completed turn will continue automatically. If another department notices that the handoff has arrived and proposes a resume, verify the evidence and then act or explain why not.

During the run, add a new department only when the existing departments cannot reasonably cover a distinct ownership domain. Long-running waits are normal; do not treat a quiet active wait as a reason to clone a department. If the only problem is that an owner is blocked on a long external wait but independent artifact recovery would help, create a temporary helper/recovery department with narrow ownership, explicit handoff requirements, and no authority to replace the original owner or clear downstream gates. When teammate messages arrive later, the orchestrator may either steer this active turn or start a new lead turn in this same thread. Reply with decisions, unblockers, ownership changes, placement changes, department changes, or handoffs. Keep each lead turn short and finish when no coordination is needed.

Team members:
{member_lines}

Current tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        member_name = member.name,
        role = member.role,
        codex = codex_exe.display(),
        member_lines = member_lines,
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    );
    localize_team_prompt(prompt, language)
}

fn build_reactive_lead_turn_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    codex_exe: &Path,
    team_id: &str,
    team_dir: &Path,
    language: TeamPromptLanguage,
) -> String {
    let message_lines = format_mail_messages_for_reactive_prompt(messages, language);
    let state_summary = build_reactive_lead_state_summary(team_dir, language);

    let prompt = format!(
        r#"Reactive lead update for {member} ({role}).

New message(s) arrived for lead while the lead turn was idle:
{message_lines}

Current compact team state:
{state_summary}

Use the team CLI if you need context:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" list
- "{codex}" team node --team "{team_id}" inspect [node-id]
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh --host <ssh-host> --cwd <remote-cwd>
- "{codex}" team node --team "{team_id}" add <node-id> --kind docker --container <container> --cwd <container-cwd>
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh-docker --host <ssh-host> --container <container> --cwd <container-cwd>
- "{codex}" team node --team "{team_id}" create-docker <node-id> [--host <ssh-host>] --image <image> --mount <host:container> --port <host:container> --gpus --replace
- "{codex}" team node --team "{team_id}" sync-assets <node-id> [--no-auth]
- "{codex}" team node --team "{team_id}" sync-path <node-id> --src <local-path> --dest <node-path> [--replace]
- "{codex}" team node --team "{team_id}" pull-path <node-id> --src <node-path> --dest <local-path> [--replace]
- "{codex}" team node --team "{team_id}" remove <node-id> --force
- "{codex}" team job --team "{team_id}" start --owner lead --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
- "{codex}" team job --team "{team_id}" status <job-id>
- "{codex}" team job --team "{team_id}" logs <job-id> --tail 80
- "{codex}" team wait --team "{team_id}" add "<title>" --owner <department> --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
- "{codex}" team wait --team "{team_id}" list [--owner <department>] [--task <TASK_ID>]
- "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "{codex}" team task --team "{team_id}" list
- "{codex}" team task --team "{team_id}" set <TASK_ID> --status <status> --depends-on <DEP_TASK_ID> [--depends-on <DEP_TASK_ID>...] --result "<why>"
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team member --team "{team_id}" list
- "{codex}" team member --team "{team_id}" add <name:role> --node <node-id> --mission "<why this department is needed>"
- "{codex}" team member --team "{team_id}" standby <member> --reason "<why active work is no longer needed>"
- "{codex}" team member --team "{team_id}" resume <member> --mission "<new active mission>"
- "{codex}" team inbox --team "{team_id}" lead

Respond as lead only if coordination, prioritization, clarification, ownership reassignment, placement add/remove, department add/standby/resume, job/wait tracking, tooling setup, a handoff, a `DEBATE_REQUEST:`/`DECISION_RECORD:`, or a `LEAD_PROPOSAL:` is useful. Use the compact state above first; inspect full logs/artifacts only when a cited task, wait, job, manifest, or evidence path requires it. If a message reveals SSH/Docker/container work, inspect/create/update the placement node and assign/resume a department there. If Docker image/container work appears, host/SSH departments may build or recreate the image/container, but ongoing installs, runtime execution, tests, smoke, rendering, debugging, and final verification must move to a registered Docker/ssh-docker node with a container-internal department. If a teammate sends `LEAD_PROPOSAL:`, validate it against current tasks, ownerships, waits, jobs, and artifacts, then act or reject with `LEAD_PROPOSAL_RESOLUTION:`, `LEAD_PROPOSAL_ACCEPTED:`, or `LEAD_PROPOSAL_REJECTED:` plus a concrete reason. If a blocked department's gate has cleared, resume it with a concrete next mission. If a teammate starts long-running work, use `team job` for a trackable command or `team wait` for external/non-PID completion; require exact condition and evidence, including `AUTO_CHECK` when mechanically checkable. Avoid generic STAY/status chatter; ask for or send concise blocker, owner, evidence, next action, debate, or handoff data. Keep this turn short and concrete.
"#,
        member = member.name,
        role = member.role,
        codex = codex_exe.display(),
        team_id = team_id,
        message_lines = message_lines,
        state_summary = state_summary,
    );
    localize_team_prompt(prompt, language)
}

fn build_reactive_lead_state_summary(team_dir: &Path, language: TeamPromptLanguage) -> String {
    let tasks = load_tasks(team_dir).unwrap_or_default();
    let waits = load_waits(team_dir).unwrap_or_default();
    let jobs = load_jobs(team_dir).unwrap_or_default();
    let lead_messages =
        read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).unwrap_or_default();

    let open_tasks = tasks
        .iter()
        .filter(|task| {
            !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            )
        })
        .take(12)
        .map(|task| {
            format!(
                "- task {} [{}] owner={} {}",
                task.id,
                task.status,
                task.owner.as_deref().unwrap_or("unassigned"),
                compact_one_line(&task.subject, 140)
            )
        })
        .collect::<Vec<_>>();

    let open_waits = waits
        .iter()
        .filter(|wait| {
            !matches!(
                wait.status,
                TeamWaitStatus::Completed | TeamWaitStatus::Failed | TeamWaitStatus::Cancelled
            )
        })
        .take(10)
        .map(|wait| {
            format!(
                "- {} [{}] owner={} task={} node={} condition={} progress={}",
                wait.id,
                wait.status,
                wait.owner.as_deref().unwrap_or(""),
                wait.task_id.as_deref().unwrap_or(""),
                wait.node.as_deref().unwrap_or(""),
                compact_one_line(&wait.condition, 120),
                compact_one_line(&wait.progress, 120)
            )
        })
        .collect::<Vec<_>>();

    let running_jobs = jobs
        .iter()
        .filter(|job| matches!(job.status, TeamJobStatus::Running | TeamJobStatus::Unknown))
        .take(8)
        .map(|job| {
            format!(
                "- {} [{:?}] owner={} task={} node={} cwd={} cmd={}",
                job.id,
                job.status,
                job.owner.as_deref().unwrap_or(""),
                job.task_id.as_deref().unwrap_or(""),
                job.node,
                compact_one_line(&job.cwd, 70),
                compact_one_line(&job.command, 130)
            )
        })
        .collect::<Vec<_>>();

    let recent_decisions = lead_messages
        .iter()
        .rev()
        .filter(|message| {
            let text = message.message.as_str();
            text.contains("DECISION_RECORD:")
                || text.contains("LEAD_PROPOSAL:")
                || text.contains("DEBATE_REQUEST:")
                || text.contains("DEBATE_RESPONSE:")
                || text.contains("WAIT_STATUS:")
                || text.contains("JOB_STATUS:")
        })
        .take(6)
        .map(|message| {
            format!(
                "- [{}] {} -> {}: {}",
                message.timestamp,
                message.from,
                message.to,
                compact_prompt_message(&message.message, 260)
            )
        })
        .collect::<Vec<_>>();

    let none = if language.is_ja() { "(なし)" } else { "(none)" };
    format!(
        "Open tasks:\n{open_tasks}\nOpen waits:\n{open_waits}\nRunning jobs:\n{running_jobs}\nRecent decisions/proposals:\n{recent_decisions}",
        open_tasks = if open_tasks.is_empty() {
            none.to_string()
        } else {
            open_tasks.join("\n")
        },
        open_waits = if open_waits.is_empty() {
            none.to_string()
        } else {
            open_waits.join("\n")
        },
        running_jobs = if running_jobs.is_empty() {
            none.to_string()
        } else {
            running_jobs.join("\n")
        },
        recent_decisions = if recent_decisions.is_empty() {
            none.to_string()
        } else {
            recent_decisions.join("\n")
        },
    )
}

fn build_reactive_member_turn_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    codex_exe: &Path,
    team_id: &str,
    standby: bool,
    language: TeamPromptLanguage,
) -> String {
    let message_lines = format_mail_messages_for_reactive_prompt(messages, language);
    let mode = if standby {
        "You are currently in standby: answer questions, clarify prior work, and help with handoffs, but do not take new implementation work unless lead explicitly resumes you."
    } else {
        "Your main task turn has completed, but the team still needs a short follow-up answer or handoff."
    };

    let prompt = format!(
        r#"Reactive department follow-up for {member} ({role}).

{mode}

New teammate message(s):
{message_lines}

Use the team CLI if you need context:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" inspect [node-id]
- "{codex}" team task --team "{team_id}" list
- "{codex}" team task --team "{team_id}" claim [TASK_ID] --owner "{member}"
- "{codex}" team wait --team "{team_id}" list --owner "{member}"
- "{codex}" team wait --team "{team_id}" add "<title>" --owner "{member}" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
- "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team inbox --team "{team_id}" "{member}"
- "{codex}" team message --team "{team_id}" --from "{member}" lead "<answer, blocker, or handoff>"
- "{codex}" team message --team "{team_id}" --from "{member}" all "<short update when useful>"
- "{codex}" team message --team "{team_id}" --from "{member}" <member[,member...]> "<same answer or question to a small explicit group>"

If the follow-up asks for work that needs missing normal tools, install them when reasonable before weakening implementation or verification. Docker root installs, user-local installs, and passwordless sudo (`sudo -n`) are allowed; do not wait for an interactive sudo password.

If the follow-up asks for review, keep the answer scoped to the named target path, line/section, checklist, evidence, or manifest. If the target is missing, ask for it instead of reading broad artifact trees. If the follow-up exposes an uncertainty, missing input, weak result, long wait, or cross-department decision, ask the relevant department for judgment instead of answering only to lead. If work cannot continue until an observable condition becomes true, register/update a `team wait` with the exact condition, current progress/request/log/checkpoint, task, and evidence path when available. Include TEAM_COMPLETION_CHECKLIST only when this follow-up genuinely completes the owned task or a real handoff with concrete artifacts and verification. Do not include TEAM_COMPLETION_CHECKLIST for acknowledgement, steering, side-channel status, or any response whose artifacts would be `none`.
If this is an idle outreach message and you need help, reply with the exact blocker/question and what kind of help would unblock you. If you do not need help, no reply is required.

If lead or the task table exposes an unassigned `ready` task that clearly matches your department mission, you may claim it with `team task claim`, then message lead with the reason and intended artifacts. Otherwise do not start unrelated work from a follow-up.

Respond only if useful. Send concise team messages for answers, handoffs, installed tooling, verification results, blockers, or a self-claim notice, then finish.
"#,
        member = member.name,
        role = member.role,
        mode = mode,
        message_lines = message_lines,
        codex = codex_exe.display(),
        team_id = team_id,
    );
    localize_team_prompt(prompt, language)
}

fn build_app_server_lead_final_prompt(
    config: &TeamConfig,
    team_dir: &Path,
    language: TeamPromptLanguage,
) -> Result<String> {
    let tasks = load_tasks(team_dir)?;
    let task_text = tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] owner={} subject={} result={}",
                task.id,
                task.status,
                task.owner.as_deref().unwrap_or(""),
                task.subject,
                task.result.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"Produce the final lead synthesis for this Codex agent team.

Team: {team_id}
Goal: {goal}
Team state directory: {team_dir}

Summarize:
- what each member did
- task status and important messages
- changed files or outputs
- checks or review results
- unresolved risks and recommended next action

Current tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        team_dir = team_dir.display(),
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    );
    Ok(localize_team_prompt(prompt, language))
}

fn build_lead_synthesis_prompt(team_dir: &Path) -> Result<String> {
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let task_text = tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] owner={} subject={} result={}",
                task.id,
                task.status,
                task.owner.as_deref().unwrap_or(""),
                task.subject,
                task.result.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let members = config
        .members
        .iter()
        .map(|member| {
            format!(
                "- {} ({}) status={:?} workspace={}",
                member.name,
                member.role,
                member.status,
                member.workspace_path.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(format!(
        r#"You are the lead Codex agent for a local agent team.

Team: {team_id}
Goal: {goal}
Team state directory: {team_dir}

Use the normal Codex runtime exactly as configured for this machine. User config, skills, MCP servers, plugins, auth, model settings, and project instructions are available through this Codex session.

Read the team state, worker logs, final messages, and any worktree diffs. Produce a concise final synthesis for the user:
- what each member did
- task status and important results
- changed files or relevant diffs if any
- tests or checks run
- unresolved issues and recommended next action

Do not merge worktrees automatically unless the user explicitly requested auto-merge. If worktrees exist, summarize the branches and how to inspect or merge them.

Members:
{members}

Tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        team_dir = team_dir.display(),
        members = members,
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    ))
}

fn load_team_summaries(root: &Path) -> Result<Vec<TeamConfig>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut teams = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Ok(config) = load_config(&entry.path())
        {
            teams.push(config);
        }
    }
    Ok(teams)
}

fn resolve_team_dir(root: &Path, team: Option<&str>) -> Result<PathBuf> {
    match team {
        Some(team) => {
            let dir = root.join(sanitize_id(team));
            if !dir.join("config.json").exists() {
                bail!("team `{team}` does not exist");
            }
            Ok(dir)
        }
        None => {
            let teams = load_team_summaries(root)?;
            let latest = teams
                .into_iter()
                .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
                .ok_or_else(|| anyhow!("no teams found; run `codex team start` first"))?;
            Ok(root.join(latest.id))
        }
    }
}

fn load_config(team_dir: &Path) -> Result<TeamConfig> {
    read_json(&team_dir.join("config.json"))
        .with_context(|| format!("failed to read {}", team_dir.join("config.json").display()))
}

fn touch_config(team_dir: &Path) -> Result<()> {
    let mut config = load_config(team_dir)?;
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn load_tasks(team_dir: &Path) -> Result<Vec<TeamTask>> {
    let task_dir = team_dir.join("tasks");
    if !task_dir.exists() {
        return Ok(Vec::new());
    }
    let mut tasks = Vec::new();
    for entry in fs::read_dir(task_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            tasks.push(read_json::<TeamTask>(&entry.path())?);
        }
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(tasks)
}

fn jobs_dir(team_dir: &Path) -> PathBuf {
    team_dir.join("jobs")
}

fn job_path(team_dir: &Path, job_id: &str) -> PathBuf {
    jobs_dir(team_dir).join(format!("{}.json", sanitize_id(job_id)))
}

fn waits_dir(team_dir: &Path) -> PathBuf {
    team_dir.join("waits")
}

fn wait_path(team_dir: &Path, wait_id: &str) -> PathBuf {
    waits_dir(team_dir).join(format!("{}.json", sanitize_id(wait_id)))
}

fn load_job(team_dir: &Path, job_id: &str) -> Result<TeamJob> {
    read_json(&job_path(team_dir, job_id)).with_context(|| format!("failed to read job `{job_id}`"))
}

fn load_jobs(team_dir: &Path) -> Result<Vec<TeamJob>> {
    let dir = jobs_dir(team_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            jobs.push(read_json::<TeamJob>(&entry.path())?);
        }
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(jobs)
}

fn load_wait(team_dir: &Path, wait_id: &str) -> Result<TeamWait> {
    read_json(&wait_path(team_dir, wait_id))
        .with_context(|| format!("failed to read wait `{wait_id}`"))
}

fn load_waits(team_dir: &Path) -> Result<Vec<TeamWait>> {
    let dir = waits_dir(team_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut waits = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            waits.push(read_json::<TeamWait>(&entry.path())?);
        }
    }
    waits.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(waits)
}

fn allocate_job_id(team_dir: &Path) -> Result<String> {
    fs::create_dir_all(jobs_dir(team_dir))?;
    let mut high = 0_u64;
    for job in load_jobs(team_dir)? {
        if let Some(number) = job.id.strip_prefix("job-")
            && let Ok(number) = number.parse::<u64>()
        {
            high = high.max(number);
        }
    }
    Ok(format!("job-{}", high + 1))
}

fn allocate_wait_id(team_dir: &Path) -> Result<String> {
    fs::create_dir_all(waits_dir(team_dir))?;
    let mut high = 0_u64;
    for wait in load_waits(team_dir)? {
        if let Some(number) = wait.id.strip_prefix("wait-")
            && let Ok(number) = number.parse::<u64>()
        {
            high = high.max(number);
        }
    }
    Ok(format!("wait-{}", high + 1))
}

fn load_node_for_job(team_dir: &Path, job: &TeamJob) -> Result<TeamNode> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    nodes
        .into_iter()
        .find(|node| node.id == job.node)
        .with_context(|| format!("node `{}` for job `{}` not found", job.node, job.id))
}

fn nodes_path(team_dir: &Path) -> PathBuf {
    team_dir.join("nodes.json")
}

fn load_nodes(team_dir: &Path) -> Result<Vec<TeamNode>> {
    let path = nodes_path(team_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_json(&path).with_context(|| format!("failed to read {}", path.display()))
}

fn write_nodes(team_dir: &Path, nodes: &[TeamNode]) -> Result<()> {
    write_json_atomic(&nodes_path(team_dir), nodes)
}

fn set_node_connection(
    team_dir: &Path,
    node_id: &str,
    status: TeamNodeStatus,
    url: Option<String>,
) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let Some(node) = nodes.iter_mut().find(|node| node.id == node_id) else {
        append_event(
            team_dir,
            "node_status_update_skipped",
            serde_json::json!({
                "node": node_id,
                "status": status,
                "reason": "node not registered",
            }),
        )?;
        return Ok(());
    };
    node.status = status;
    if let Some(url) = url {
        node.url = Some(url);
    }
    node.updated_at = now();
    write_nodes(team_dir, &nodes)?;
    Ok(())
}

fn heartbeat_connected_app_server_nodes(
    team_dir: &Path,
    node_clients: &HashMap<String, TeamAppServerNodeClient>,
) -> Result<()> {
    heartbeat_connected_node_ids(team_dir, node_clients.keys().map(String::as_str))
}

fn heartbeat_connected_node_ids<'a>(
    team_dir: &Path,
    node_ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let node_ids = node_ids.into_iter().collect::<HashSet<_>>();
    if node_ids.is_empty() {
        return Ok(());
    }
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let now = now();
    let mut touched = Vec::new();
    for node in &mut nodes {
        if node_ids.contains(node.id.as_str()) {
            node.status = TeamNodeStatus::Online;
            node.updated_at = now.clone();
            touched.push(node.id.clone());
        }
    }
    if touched.is_empty() {
        return Ok(());
    }
    write_nodes(team_dir, &nodes)?;
    append_event(
        team_dir,
        "node_connection_heartbeat",
        serde_json::json!({
            "nodes": touched,
        }),
    )?;
    Ok(())
}

fn ensure_local_node(nodes: &mut Vec<TeamNode>) {
    if nodes.iter().any(|node| node.id == "local") {
        return;
    }
    let now = now();
    nodes.push(TeamNode {
        id: "local".to_string(),
        kind: TeamNodeKind::Local,
        url: None,
        host: None,
        container: None,
        cwd: None,
        status: TeamNodeStatus::Online,
        note: "Current machine; URL is resolved from --app-server-url, registry, or UI-managed app-server at run time.".to_string(),
        created_at: now.clone(),
        updated_at: now,
    });
}

fn ensure_node_exists(team_dir: &Path, node_id: &str) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    if nodes.iter().any(|node| node.id == node_id) {
        Ok(())
    } else {
        bail!(
            "node `{node_id}` is not registered; run `codex team node add {node_id} --url ws://...` first"
        )
    }
}

fn load_ownerships(team_dir: &Path) -> Result<Vec<FileOwnership>> {
    let path = ownerships_path(team_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_json(&path).with_context(|| format!("failed to read {}", path.display()))
}

fn write_ownerships(team_dir: &Path, ownerships: &[FileOwnership]) -> Result<()> {
    write_json_atomic(&ownerships_path(team_dir), ownerships)
}

fn allocate_task_id(team_dir: &Path) -> Result<String> {
    let task_dir = team_dir.join("tasks");
    fs::create_dir_all(&task_dir)?;
    let mut high = 0_u64;
    for entry in fs::read_dir(&task_dir)? {
        let entry = entry?;
        if let Some(stem) = entry.path().file_stem().and_then(|stem| stem.to_str())
            && let Ok(n) = stem.parse::<u64>()
        {
            high = high.max(n);
        }
    }
    Ok((high + 1).to_string())
}

fn print_ownership(ownership: &FileOwnership) {
    let note = if ownership.note.trim().is_empty() {
        String::new()
    } else {
        format!("  {}", ownership.note)
    };
    println!(
        "  {:<24} {}{}",
        format!("@{}", ownership.owner),
        ownership.path,
        note
    );
}

fn print_task(task: &TeamTask) {
    let owner = task
        .owner
        .as_ref()
        .map(|owner| format!("@{owner}"))
        .unwrap_or_default();
    let deps = if task.depends_on.is_empty() {
        String::new()
    } else {
        format!(" deps:{}", task.depends_on.join(","))
    };
    let subject = compact_one_line(&task.subject, 260);
    println!(
        "  {:>3} {:<11} {:<16} {}{}",
        task.id, task.status, owner, subject, deps
    );
}

fn normalize_ownership_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("ownership path cannot be empty");
    }
    if trimmed.contains('\n') {
        bail!("ownership path cannot contain newlines");
    }
    let normalized = trimmed.trim_start_matches("./").replace('\\', "/");
    if normalized == "." || normalized.starts_with("../") || normalized == ".." {
        bail!("ownership path must stay inside the workspace");
    }
    Ok(normalized)
}

fn ensure_member_exists(config: &TeamConfig, name: &str) -> Result<()> {
    if config.members.iter().any(|member| member.name == name) {
        Ok(())
    } else {
        bail!("member `{name}` does not exist in team `{}`", config.id)
    }
}

fn resolve_message_recipients(config: &TeamConfig, from: &str, to: &str) -> Result<Vec<String>> {
    let mut raw_recipients = Vec::new();
    for recipient in to.split(',') {
        let trimmed = recipient.trim();
        if trimmed.is_empty() {
            bail!("message recipient list contains an empty recipient");
        }
        raw_recipients.push(trimmed.to_string());
    }
    if raw_recipients.is_empty() {
        bail!("message recipient cannot be empty");
    }

    let all_count = raw_recipients
        .iter()
        .filter(|recipient| recipient.as_str() == "all" || recipient.as_str() == "@all")
        .count();
    if all_count > 0 && raw_recipients.len() > 1 {
        bail!("recipient `all` cannot be combined with explicit recipients");
    }

    if raw_recipients.len() == 1
        && (raw_recipients[0].as_str() == "all" || raw_recipients[0].as_str() == "@all")
    {
        let recipients = config
            .members
            .iter()
            .filter(|member| member.name != from)
            .map(|member| member.name.clone())
            .collect::<Vec<_>>();
        if recipients.is_empty() {
            bail!("team `{}` has no recipients for broadcast", config.id);
        }
        return Ok(recipients);
    }

    let mut recipients = Vec::new();
    let mut seen = HashSet::new();
    for recipient in raw_recipients {
        let recipient = sanitize_id(&recipient);
        if recipient.is_empty() {
            bail!("message recipient list contains an empty recipient");
        }
        if recipient != "user" {
            ensure_member_exists(config, &recipient)?;
        }
        if seen.insert(recipient.clone()) {
            recipients.push(recipient);
        }
    }
    Ok(recipients)
}

fn default_team_member_name() -> String {
    std::env::var("CODEX_TEAM_MEMBER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "lead".to_string())
}

fn mailbox_path(team_dir: &Path, member: &str) -> PathBuf {
    team_dir
        .join("mailboxes")
        .join(format!("{}.jsonl", sanitize_id(member)))
}

fn task_path(team_dir: &Path, task_id: &str) -> PathBuf {
    team_dir
        .join("tasks")
        .join(format!("{}.json", sanitize_id(task_id)))
}

fn ownerships_path(team_dir: &Path) -> PathBuf {
    team_dir.join("ownerships.json")
}

fn append_event(team_dir: &Path, event: &str, data: serde_json::Value) -> Result<()> {
    let config = load_config(team_dir)?;
    let entry = Event {
        event,
        timestamp: now(),
        team: &config.id,
        data,
    };
    append_jsonl(&team_dir.join("events.jsonl"), &entry)
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn write_jsonl_atomic<T: Serialize>(path: &Path, values: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    let mut out = String::new();
    for value in values {
        out.push_str(&serde_json::to_string(value)?);
        out.push('\n');
    }
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    let mut values = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str(line)
            .with_context(|| format!("failed to parse {} line {}", path.display(), idx + 1))?;
        values.push(value);
    }
    Ok(values)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("json")
    ));
    let json = serde_json::to_string_pretty(value)?;
    fs::write(&tmp, format!("{json}\n"))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn write_text_atomic(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("txt")
    ));
    fs::write(&tmp, value)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn append_text(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(value.as_bytes())?;
    Ok(())
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn sanitize_role(value: &str) -> String {
    let role = value.trim();
    if role.is_empty() {
        "worker".to_string()
    } else {
        role.to_string()
    }
}

fn tokyo_offset() -> FixedOffset {
    FixedOffset::east_opt(9 * 60 * 60).expect("valid Tokyo UTC offset")
}

fn tokyo_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&tokyo_offset())
}

fn timestamp_for_ui(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| {
            timestamp
                .with_timezone(&tokyo_offset())
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| value.to_string())
}

fn parse_rfc3339_utc(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn now() -> String {
    tokyo_now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
