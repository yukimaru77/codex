#[derive(Debug, Serialize)]
struct TeamListRow {
    id: String,
    runtime: String,
    updated_at: String,
    open_tasks: usize,
    open_waits: usize,
    idle_for_sec: Option<u64>,
    run_pid: Option<u32>,
    ui_pid: Option<u32>,
    goal: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MemberJournalEntry {
    timestamp: String,
    member: String,
    role: String,
    status: String,
    node: String,
    fingerprint: u64,
    summary: String,
    tasks: Vec<String>,
    jobs: Vec<String>,
    waits: Vec<String>,
    messages_sent: Vec<String>,
    messages_received: Vec<String>,
    events: Vec<String>,
    last_output_excerpt: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct MemberDigestJournalState {
    #[serde(default)]
    members: HashMap<String, MemberDigestMemberState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MemberDigestMemberState {
    fingerprint: u64,
    last_trigger: String,
    last_spawned_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MemberDigestLaunchEntry {
    timestamp: String,
    member: String,
    trigger: String,
    fingerprint: u64,
    digest_path: String,
    log_path: String,
    prompt_path: String,
}

fn list_teams(root: &Path, args: ListArgs) -> Result<()> {
    let mut teams = load_team_summaries(root)?;
    if teams.is_empty() {
        println!("No teams found.");
        return Ok(());
    }
    teams.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let mut rows = Vec::new();
    for team in teams {
        let team_dir = root.join(&team.id);
        let mut status = team_run_status_for_dir(&team_dir, &team.id);
        if args.live_only && matches!(status, UiTeamRunStatus::Exiting | UiTeamRunStatus::Unknown) {
            continue;
        }
        let tasks = load_tasks(&team_dir).unwrap_or_default();
        let waits = load_waits(&team_dir).unwrap_or_default();
        let open_tasks = open_task_count(&tasks);
        let open_waits = open_wait_count(&waits);
        let idle_for_sec = if matches!(status, UiTeamRunStatus::Stop) {
            team_keep_alive_idle_age_secs(&team_dir)?
        } else {
            None
        };
        if args.pause_idle_after_sec > 0
            && matches!(status, UiTeamRunStatus::Stop)
            && idle_for_sec.is_some_and(|age| age >= args.pause_idle_after_sec)
        {
            stop_one_team_runtime(root, &team_dir, &team, false, false)?;
            status = UiTeamRunStatus::Exiting;
        }
        rows.push(TeamListRow {
            id: team.id.clone(),
            runtime: status.label().to_string(),
            updated_at: team.updated_at.clone(),
            open_tasks,
            open_waits,
            idle_for_sec,
            run_pid: read_team_run_pid(&team_dir),
            ui_pid: read_ui_team_pid(root, &team.id),
            goal: compact_one_line(&team.goal, 240),
        });
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{:<24} {:<14} {:>5} {:>5} {:>9}  {:<25} {}",
        "TEAM", "RUNTIME", "TASKS", "WAITS", "IDLE", "UPDATED", "GOAL"
    );
    for row in rows {
        let idle = row
            .idle_for_sec
            .map(format_idle_seconds)
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<24} {:<14} {:>5} {:>5} {:>9}  {:<25} {}",
            row.id,
            row.runtime,
            row.open_tasks,
            row.open_waits,
            idle,
            timestamp_for_ui(&row.updated_at),
            row.goal
        );
    }
    Ok(())
}

fn format_idle_seconds(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn member_journal_dir(team_dir: &Path) -> PathBuf {
    team_dir.join("member_journals")
}

fn member_journal_entries_path(team_dir: &Path, member: &str) -> PathBuf {
    member_journal_dir(team_dir).join(format!("{}.jsonl", sanitize_id(member)))
}

fn member_journal_markdown_path(team_dir: &Path, member: &str) -> PathBuf {
    member_journal_dir(team_dir).join(format!("{}.md", sanitize_id(member)))
}

fn member_digest_markdown_path(team_dir: &Path, member: &str) -> PathBuf {
    member_journal_dir(team_dir).join(format!("{}.digest.md", sanitize_id(member)))
}

fn member_digest_log_path(team_dir: &Path, member: &str) -> PathBuf {
    member_journal_dir(team_dir).join(format!("{}.digest.log", sanitize_id(member)))
}

fn member_digest_prompt_path(team_dir: &Path, member: &str) -> PathBuf {
    member_journal_dir(team_dir).join(format!("{}.digest.prompt.md", sanitize_id(member)))
}

fn member_digest_launches_path(team_dir: &Path, member: &str) -> PathBuf {
    member_journal_dir(team_dir).join(format!("{}.digest.jsonl", sanitize_id(member)))
}

fn member_digest_state_path(team_dir: &Path) -> PathBuf {
    member_journal_dir(team_dir).join("digest_state.json")
}

fn update_member_journals(team_dir: &Path, config: &TeamConfig) -> Result<()> {
    fs::create_dir_all(member_journal_dir(team_dir))?;
    let tasks = load_tasks(team_dir).unwrap_or_default();
    let jobs = load_jobs(team_dir).unwrap_or_default();
    let waits = load_waits(team_dir).unwrap_or_default();
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).unwrap_or_default();
    let now_ts = now();
    for member in &config.members {
        let entry =
            build_member_journal_entry(team_dir, member, &tasks, &jobs, &waits, &events, &now_ts)?;
        let path = member_journal_entries_path(team_dir, &member.name);
        let mut entries = read_jsonl::<MemberJournalEntry>(&path).unwrap_or_default();
        let should_append = entries
            .last()
            .map(|latest| latest.fingerprint != entry.fingerprint)
            .unwrap_or(true);
        if should_append {
            entries.push(entry);
            let keep_from = entries.len().saturating_sub(240);
            let retained = entries.into_iter().skip(keep_from).collect::<Vec<_>>();
            write_jsonl_atomic(&path, &retained)?;
            write_member_journal_markdown(team_dir, member, &retained)?;
        } else if !member_journal_markdown_path(team_dir, &member.name).exists() {
            write_member_journal_markdown(team_dir, member, &entries)?;
        }
    }
    Ok(())
}

fn maybe_sync_member_journals_to_nodes(
    team_dir: &Path,
    nodes: &[TeamNode],
    node_clients: &HashMap<String, TeamAppServerNodeClient>,
    last_sync: &mut HashMap<String, Instant>,
    interval: Duration,
) -> Result<()> {
    let src = member_journal_dir(team_dir);
    if !src.exists() {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    let now_instant = Instant::now();
    let dest = format!("$HOME/.codex/teams/{}/member_journals", config.id);
    for node in nodes {
        if matches!(node.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
            continue;
        }
        if !node_clients.contains_key(&node.id) {
            continue;
        }
        if last_sync
            .get(&node.id)
            .is_some_and(|last| now_instant.duration_since(*last) < interval)
        {
            continue;
        }
        last_sync.insert(node.id.clone(), now_instant);
        match build_path_sync_command(node, &src, &dest, true)
            .and_then(|(command, _)| run_shell_command(&command, "sync member journals to node"))
        {
            Ok(()) => {
                append_event(
                    team_dir,
                    "member_journals_node_synced",
                    serde_json::json!({
                        "node": node.id,
                        "dest": dest,
                        "source": "local_team_state",
                        "direction": "local_to_node",
                    }),
                )?;
            }
            Err(err) => {
                append_event(
                    team_dir,
                    "member_journals_node_sync_failed",
                    serde_json::json!({
                        "node": node.id,
                        "dest": dest,
                        "error": err.to_string(),
                    }),
                )?;
            }
        }
    }
    Ok(())
}

fn maybe_generate_member_digest_journals(
    team_dir: &Path,
    codex_exe: &Path,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<()> {
    fs::create_dir_all(member_journal_dir(team_dir))?;
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir).unwrap_or_default();
    let jobs = load_jobs(team_dir).unwrap_or_default();
    let waits = load_waits(team_dir).unwrap_or_default();
    let mut state = read_json::<MemberDigestJournalState>(&member_digest_state_path(team_dir))
        .unwrap_or_default();
    let mut changed = false;
    for member in &config.members {
        let Some((fingerprint, trigger)) =
            member_digest_trigger_fingerprint(team_dir, member, &tasks, &jobs, &waits)
        else {
            continue;
        };
        if state
            .members
            .get(&member.name)
            .is_some_and(|existing| existing.fingerprint == fingerprint)
        {
            continue;
        }
        if state
            .members
            .get(&member.name)
            .and_then(|existing| parse_rfc3339_utc(&existing.last_spawned_at).ok())
            .is_some_and(|last| Utc::now().signed_duration_since(last).num_seconds() < 300)
        {
            continue;
        }
        let launch = spawn_member_digest_generation(
            team_dir,
            &config,
            member,
            &trigger,
            fingerprint,
            codex_exe,
            model,
            profile,
            sandbox,
            dangerously_bypass_approvals_and_sandbox,
        )?;
        append_jsonl(
            &member_digest_launches_path(team_dir, &member.name),
            &launch,
        )?;
        state.members.insert(
            member.name.clone(),
            MemberDigestMemberState {
                fingerprint,
                last_trigger: trigger,
                last_spawned_at: launch.timestamp.clone(),
            },
        );
        changed = true;
    }
    if changed {
        write_json_atomic(&member_digest_state_path(team_dir), &state)?;
    }
    Ok(())
}

fn member_digest_trigger_fingerprint(
    team_dir: &Path,
    member: &TeamMember,
    tasks: &[TeamTask],
    jobs: &[TeamJob],
    waits: &[TeamWait],
) -> Option<(u64, String)> {
    let member_name = member.name.as_str();
    let mut lines = Vec::<String>::new();
    if matches!(
        member.status,
        MemberStatus::Standby | MemberStatus::Completed | MemberStatus::Failed
    ) {
        lines.push(format!("member {} status={:?}", member.name, member.status));
    }
    for task in tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member_name))
    {
        if matches!(
            task.status,
            TaskStatus::Completed
                | TaskStatus::Blocked
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        ) {
            lines.push(format!(
                "task {} status={} subject={} result={}",
                task.id,
                task.status,
                compact_one_line(&task.subject, 160),
                compact_one_line(task.result.as_deref().unwrap_or(""), 220)
            ));
        }
    }
    for job in jobs
        .iter()
        .filter(|job| job.owner.as_deref() == Some(member_name))
    {
        if matches!(
            job.status,
            TeamJobStatus::Completed | TeamJobStatus::Failed | TeamJobStatus::Stopped
        ) {
            lines.push(format!(
                "job {} status={:?} task={} exit={} command={}",
                job.id,
                job.status,
                job.task_id.as_deref().unwrap_or("-"),
                job.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                compact_one_line(&job.command, 180)
            ));
        }
    }
    for wait in waits
        .iter()
        .filter(|wait| wait.owner.as_deref() == Some(member_name))
    {
        if matches!(
            wait.status,
            TeamWaitStatus::Completed
                | TeamWaitStatus::Failed
                | TeamWaitStatus::Blocked
                | TeamWaitStatus::Cancelled
        ) {
            lines.push(format!(
                "wait {} status={} task={} title={} progress={} evidence={}",
                wait.id,
                wait.status,
                wait.task_id.as_deref().unwrap_or("-"),
                compact_one_line(&wait.title, 120),
                compact_one_line(&wait.progress, 220),
                compact_one_line(wait.evidence.as_deref().unwrap_or(""), 160)
            ));
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort();
    let machine_fingerprint =
        read_jsonl::<MemberJournalEntry>(&member_journal_entries_path(team_dir, &member.name))
            .ok()
            .and_then(|entries| entries.last().map(|entry| entry.fingerprint))
            .unwrap_or(0);
    let mut hasher = DefaultHasher::new();
    member.name.hash(&mut hasher);
    lines.hash(&mut hasher);
    machine_fingerprint.hash(&mut hasher);
    Some((hasher.finish(), lines.join("\n")))
}

#[allow(clippy::too_many_arguments)]
fn spawn_member_digest_generation(
    team_dir: &Path,
    config: &TeamConfig,
    member: &TeamMember,
    trigger: &str,
    fingerprint: u64,
    codex_exe: &Path,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<MemberDigestLaunchEntry> {
    let timestamp = now();
    let digest_path = member_digest_markdown_path(team_dir, &member.name);
    let log_path = member_digest_log_path(team_dir, &member.name);
    let prompt_path = member_digest_prompt_path(team_dir, &member.name);
    let prompt = build_member_digest_prompt(team_dir, config, member, trigger, fingerprint)?;
    write_text_atomic(&prompt_path, &prompt)?;

    let stdout = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("-C")
        .arg(team_dir)
        .arg("-o")
        .arg(&digest_path)
        .env("CODEX_TEAM_ID", &config.id)
        .env("CODEX_TEAM_MEMBER", format!("{}_digest", member.name))
        .env("CODEX_TEAM_ROLE", "journal_digest")
        .stdin(Stdio::null())
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
    let child = command
        .spawn()
        .with_context(|| format!("spawn AI digest journal for `{}`", member.name))?;
    append_event(
        team_dir,
        "member_digest_journal_started",
        serde_json::json!({
            "member": member.name,
            "pid": child.id(),
            "trigger_fingerprint": fingerprint,
            "digest": digest_path,
            "log": log_path,
            "prompt": prompt_path,
        }),
    )?;
    Ok(MemberDigestLaunchEntry {
        timestamp,
        member: member.name.clone(),
        trigger: trigger.to_string(),
        fingerprint,
        digest_path: digest_path.display().to_string(),
        log_path: log_path.display().to_string(),
        prompt_path: prompt_path.display().to_string(),
    })
}

fn build_member_digest_prompt(
    team_dir: &Path,
    config: &TeamConfig,
    member: &TeamMember,
    trigger: &str,
    fingerprint: u64,
) -> Result<String> {
    let machine_journal = fs::read_to_string(member_journal_markdown_path(team_dir, &member.name))
        .unwrap_or_else(|_| "No machine journal exists yet.".to_string());
    let previous_digest = fs::read_to_string(member_digest_markdown_path(team_dir, &member.name))
        .unwrap_or_else(|_| "No previous AI digest exists yet.".to_string());
    let language_note = if config.language.unwrap_or_default().is_ja() {
        "Write the digest in Japanese."
    } else {
        "Write the digest in English."
    };
    Ok(format!(
        r#"You are writing an AI digest journal for one Codex Teams department.

This digest is an interpretation layer, not the source of truth. Do not invent facts. Base the digest on the machine journal, trigger, tasks/jobs/waits/messages/events, and previous digest below. If evidence is missing, say so.

{language_note}

Team: {team_id}
Goal: {goal}
Department: {member}
Role: {role}
Node: {node}
Trigger fingerprint: {fingerprint}

Trigger that caused this digest:
```text
{trigger}
```

Write the digest with exactly these sections:

# AI Digest Journal: {member}

- updated_at:
- trigger:
- scope:

## この部署が考えていたこと

## 進めたこと

## 詰まっていること

## 判断・前提

## 他部署への依存

## 次にやるべきこと

## 根拠
- tasks:
- jobs:
- waits:
- messages:
- artifacts:

Guidelines:
- Explain what this department was trying to accomplish and why.
- Explain what changed since the previous digest.
- Preserve uncertainties and blockers; do not convert weak evidence into success.
- Keep it useful for another department that wants to understand this member's background before asking questions.
- Prefer concise but substantive bullets over generic status prose.

Previous AI digest:
```md
{previous_digest}
```

Current machine journal:
```md
{machine_journal}
```
"#,
        language_note = language_note,
        team_id = config.id,
        goal = compact_one_line(&config.goal, 1200),
        member = member.name,
        role = member.role,
        node = member_node_id(member),
        fingerprint = fingerprint,
        trigger = trigger,
        previous_digest = tail_chars(&previous_digest, 6000),
        machine_journal = tail_chars(&machine_journal, 14000),
    ))
}

fn build_member_journal_entry(
    team_dir: &Path,
    member: &TeamMember,
    tasks: &[TeamTask],
    jobs: &[TeamJob],
    waits: &[TeamWait],
    events: &[TeamEventRecord],
    timestamp: &str,
) -> Result<MemberJournalEntry> {
    let member_name = member.name.as_str();
    let node = member_node_id(member);
    let task_lines = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member_name))
        .take(12)
        .map(|task| {
            format!(
                "T{} [{}] {}{}",
                task.id,
                task.status,
                compact_one_line(&task.subject, 140),
                task.result
                    .as_deref()
                    .filter(|result| !result.trim().is_empty())
                    .map(|result| format!(" | result={}", compact_one_line(result, 120)))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    let job_lines = jobs
        .iter()
        .filter(|job| job.owner.as_deref() == Some(member_name))
        .rev()
        .take(10)
        .map(|job| {
            format!(
                "{} [{:?}] node={} task={} exit={} cmd={}",
                job.id,
                job.status,
                job.node,
                job.task_id.as_deref().unwrap_or("-"),
                job.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                compact_one_line(&job.command, 120)
            )
        })
        .collect::<Vec<_>>();
    let wait_lines = waits
        .iter()
        .filter(|wait| wait.owner.as_deref() == Some(member_name))
        .rev()
        .take(10)
        .map(|wait| {
            format!(
                "{} [{}] task={} condition={} progress={} evidence={}",
                wait.id,
                wait.status,
                wait.task_id.as_deref().unwrap_or("-"),
                compact_one_line(&wait.condition, 120),
                compact_one_line(&wait.progress, 120),
                compact_one_line(wait.evidence.as_deref().unwrap_or("-"), 80)
            )
        })
        .collect::<Vec<_>>();
    let (messages_sent, messages_received) =
        collect_recent_member_mail(team_dir, member_name, 8).unwrap_or_default();
    let event_lines = collect_recent_member_events(events, member_name, 10);
    let last_output_excerpt = read_member_last_output_excerpt(team_dir, member_name, 900);
    let open_tasks = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member_name) && task_is_open(task))
        .count();
    let active_jobs = jobs
        .iter()
        .filter(|job| {
            job.owner.as_deref() == Some(member_name)
                && matches!(job.status, TeamJobStatus::Running)
        })
        .count();
    let open_waits = waits
        .iter()
        .filter(|wait| wait.owner.as_deref() == Some(member_name) && wait.status.is_open())
        .count();
    let summary = format!(
        "status={:?}, node={}, open_tasks={}, active_jobs={}, open_waits={}, recent_sent={}, recent_received={}",
        member.status,
        node,
        open_tasks,
        active_jobs,
        open_waits,
        messages_sent.len(),
        messages_received.len()
    );
    let fingerprint = member_journal_fingerprint(
        &summary,
        &task_lines,
        &job_lines,
        &wait_lines,
        &messages_sent,
        &messages_received,
        &event_lines,
        &last_output_excerpt,
    );
    Ok(MemberJournalEntry {
        timestamp: timestamp.to_string(),
        member: member.name.clone(),
        role: member.role.clone(),
        status: format!("{:?}", member.status),
        node,
        fingerprint,
        summary,
        tasks: task_lines,
        jobs: job_lines,
        waits: wait_lines,
        messages_sent,
        messages_received,
        events: event_lines,
        last_output_excerpt,
    })
}

fn member_journal_fingerprint(
    summary: &str,
    tasks: &[String],
    jobs: &[String],
    waits: &[String],
    sent: &[String],
    received: &[String],
    events: &[String],
    last_output: &str,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    summary.hash(&mut hasher);
    tasks.hash(&mut hasher);
    jobs.hash(&mut hasher);
    waits.hash(&mut hasher);
    sent.hash(&mut hasher);
    received.hash(&mut hasher);
    events.hash(&mut hasher);
    last_output.hash(&mut hasher);
    hasher.finish()
}

fn collect_recent_member_mail(
    team_dir: &Path,
    member_name: &str,
    limit: usize,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut sent = Vec::new();
    let mut received = Vec::new();
    let mailbox_dir = team_dir.join("mailboxes");
    let Ok(entries) = fs::read_dir(&mailbox_dir) else {
        return Ok((sent, received));
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        for msg in read_jsonl::<MailMessage>(&path).unwrap_or_default() {
            let line = format!(
                "{} {} -> {}: {}",
                timestamp_for_ui(&msg.timestamp),
                msg.from,
                msg.to,
                compact_one_line(&msg.message, 220)
            );
            if msg.from == member_name {
                sent.push(line.clone());
            }
            if msg.to == member_name {
                received.push(line);
            }
        }
    }
    sent.sort();
    received.sort();
    sent.reverse();
    received.reverse();
    sent.truncate(limit);
    received.truncate(limit);
    Ok((sent, received))
}

fn collect_recent_member_events(
    events: &[TeamEventRecord],
    member_name: &str,
    limit: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for event in events.iter().rev() {
        if !event_mentions_member(event, member_name) {
            continue;
        }
        out.push(format!(
            "{} {} {}",
            timestamp_for_ui(&event.timestamp),
            event.event,
            compact_one_line(&event.data.to_string(), 260)
        ));
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn event_mentions_member(event: &TeamEventRecord, member_name: &str) -> bool {
    ["member", "from", "to", "owner", "lead"]
        .iter()
        .any(|key| event.data.get(*key).and_then(|value| value.as_str()) == Some(member_name))
        || event
            .data
            .get("recipients")
            .and_then(|value| value.as_array())
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|value| value.as_str() == Some(member_name))
            })
}

fn read_member_last_output_excerpt(team_dir: &Path, member_name: &str, max_chars: usize) -> String {
    let path = team_dir
        .join("last_messages")
        .join(format!("{}.md", sanitize_id(member_name)));
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    tail_chars(text.trim(), max_chars)
}

fn write_member_journal_markdown(
    team_dir: &Path,
    member: &TeamMember,
    entries: &[MemberJournalEntry],
) -> Result<()> {
    let Some(latest) = entries.last() else {
        return Ok(());
    };
    let mut out = String::new();
    out.push_str(&format!("# Member Journal: {}\n\n", member.name));
    out.push_str(&format!("- role: {}\n", member.role));
    out.push_str(&format!("- updated_at: {}\n", latest.timestamp));
    out.push_str(&format!("- summary: {}\n\n", latest.summary));
    out.push_str("## Current Tasks\n\n");
    push_markdown_list(&mut out, &latest.tasks);
    out.push_str("\n## Jobs\n\n");
    push_markdown_list(&mut out, &latest.jobs);
    out.push_str("\n## Waits\n\n");
    push_markdown_list(&mut out, &latest.waits);
    out.push_str("\n## Recent Sent Messages\n\n");
    push_markdown_list(&mut out, &latest.messages_sent);
    out.push_str("\n## Recent Received Messages\n\n");
    push_markdown_list(&mut out, &latest.messages_received);
    out.push_str("\n## Recent Runtime Events\n\n");
    push_markdown_list(&mut out, &latest.events);
    if !latest.last_output_excerpt.trim().is_empty() {
        out.push_str("\n## Last Output Excerpt\n\n```text\n");
        out.push_str(&latest.last_output_excerpt);
        out.push_str("\n```\n");
    }
    out.push_str("\n## Snapshot History\n\n");
    for entry in entries.iter().rev().take(16) {
        out.push_str(&format!(
            "- {}: {} tasks={} jobs={} waits={} messages_sent={} messages_received={}\n",
            entry.timestamp,
            entry.summary,
            entry.tasks.len(),
            entry.jobs.len(),
            entry.waits.len(),
            entry.messages_sent.len(),
            entry.messages_received.len()
        ));
    }
    write_text_atomic(&member_journal_markdown_path(team_dir, &member.name), &out)
}

fn push_markdown_list(out: &mut String, values: &[String]) {
    if values.is_empty() {
        out.push_str("- none\n");
    } else {
        for value in values {
            out.push_str("- ");
            out.push_str(value);
            out.push('\n');
        }
    }
}
