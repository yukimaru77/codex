fn run_job(root: &Path, cli: JobCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        JobSubcommand::List(args) => list_jobs(&team_dir, args),
        JobSubcommand::Start(args) => start_team_job(&team_dir, args),
        JobSubcommand::Status(args) => show_job_status(&team_dir, &args.id),
        JobSubcommand::Logs(args) => show_job_logs(&team_dir, args),
        JobSubcommand::Stop(args) => stop_team_job(&team_dir, &args.id),
        JobSubcommand::Artifact(args) => add_job_artifact(&team_dir, args),
    }
}

fn run_wait(root: &Path, cli: WaitCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        WaitSubcommand::Add(args) => add_team_wait(&team_dir, args),
        WaitSubcommand::List(args) => {
            print!("{}", format_waits_text_filtered(&team_dir, &args)?);
            Ok(())
        }
        WaitSubcommand::Set(args) => set_team_wait(&team_dir, args),
    }
}

fn list_jobs(team_dir: &Path, args: JobListArgs) -> Result<()> {
    print!("{}", format_jobs_text_filtered(team_dir, &args)?);
    Ok(())
}

fn format_jobs_text_filtered(team_dir: &Path, args: &JobListArgs) -> Result<String> {
    let mut jobs = load_jobs(team_dir)?;
    jobs.retain(|job| {
        if let Some(owner) = args.owner.as_deref()
            && job.owner.as_deref() != Some(owner)
        {
            return false;
        }
        if let Some(task) = args.task.as_deref()
            && job.task_id.as_deref() != Some(task)
        {
            return false;
        }
        if let Some(status) = args.status.as_ref()
            && &job.status != status
        {
            return false;
        }
        true
    });
    jobs.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if let Some(limit) = args.limit {
        let keep_from = jobs.len().saturating_sub(limit);
        jobs = jobs.split_off(keep_from);
    }
    if jobs.is_empty() {
        return Ok("No jobs.\n".to_string());
    }
    let mut out = String::new();
    for job in jobs {
        out.push_str(&format!(
            "{:<18} {:<10} node={:<16} owner={:<12} task={:<6} pid={} cwd={} command={}\n",
            job.id,
            format!("{:?}", job.status),
            job.node,
            job.owner.as_deref().unwrap_or("-"),
            job.task_id.as_deref().unwrap_or("-"),
            job.pid.unwrap_or_default(),
            job.cwd,
            job.command
        ));
    }
    Ok(out)
}

fn format_waits_text_filtered(team_dir: &Path, args: &WaitListArgs) -> Result<String> {
    let mut waits = load_waits(team_dir)?;
    waits.retain(|wait| {
        if let Some(owner) = args.owner.as_deref()
            && wait.owner.as_deref() != Some(owner)
        {
            return false;
        }
        if let Some(task) = args.task.as_deref()
            && wait.task_id.as_deref() != Some(task)
        {
            return false;
        }
        if let Some(status) = args.status.as_ref()
            && &wait.status != status
        {
            return false;
        }
        true
    });
    waits.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if let Some(limit) = args.limit {
        let keep_from = waits.len().saturating_sub(limit);
        waits = waits.split_off(keep_from);
    }
    if waits.is_empty() {
        return Ok("No waits.\n".to_string());
    }
    let mut out = String::new();
    for wait in waits {
        let evidence = wait.evidence.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "{:<18} {:<10} owner={:<12} task={:<6} node={:<14} evidence={:<20} title={} condition={} progress={}\n",
            wait.id,
            wait.status,
            wait.owner.as_deref().unwrap_or("-"),
            wait.task_id.as_deref().unwrap_or("-"),
            wait.node.as_deref().unwrap_or("-"),
            evidence,
            wait.title,
            wait.condition,
            wait.progress
        ));
    }
    Ok(out)
}

fn add_team_wait(team_dir: &Path, args: WaitAddArgs) -> Result<()> {
    let _lock = lock_team_state(team_dir)?;
    let config = load_config(team_dir)?;
    let id = allocate_wait_id(team_dir)?;
    let owner = args
        .owner
        .or_else(|| std::env::var("CODEX_TEAM_MEMBER").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "lead".to_string());
    ensure_member_exists(&config, &owner)?;
    let task_id = args.task.filter(|value| !value.trim().is_empty());
    if let Some(task_id) = task_id.as_deref() {
        let tasks = load_tasks(team_dir)?;
        let Some(task) = tasks.iter().find(|task| task.id == task_id) else {
            bail!("task `{task_id}` does not exist");
        };
        if let Some(task_owner) = task.owner.as_deref()
            && task_owner != owner
            && owner != config.lead
        {
            bail!("task `{task_id}` is owned by `{task_owner}`, not `{owner}`");
        }
        record_task_wait_registration(team_dir, task_id, &id, &args.title)?;
    }
    if let Some(node_id) = args.node.as_deref() {
        let mut nodes = load_nodes(team_dir)?;
        ensure_local_node(&mut nodes);
        if !nodes.iter().any(|node| node.id == node_id) {
            bail!("node `{node_id}` not found");
        }
    }
    let now = now();
    let wait = TeamWait {
        id: id.clone(),
        title: args.title,
        owner: Some(owner.clone()),
        task_id: task_id.clone(),
        node: args.node.filter(|value| !value.trim().is_empty()),
        condition: args.condition,
        status: args.status,
        progress: args.progress,
        evidence: args.evidence.filter(|value| !value.trim().is_empty()),
        created_at: now.clone(),
        updated_at: now,
    };
    fs::create_dir_all(waits_dir(team_dir))?;
    write_json_atomic(&wait_path(team_dir, &id), &wait)?;
    append_event(
        team_dir,
        "wait_registered",
        serde_json::json!({
            "wait": id,
            "owner": owner,
            "task": task_id,
            "status": wait.status.to_string(),
            "condition": wait.condition.as_str(),
            "evidence": wait.evidence.as_deref(),
        }),
    )?;
    println!("Registered wait {}", wait.id);
    Ok(())
}

fn set_team_wait(team_dir: &Path, args: WaitSetArgs) -> Result<()> {
    let mut wait = load_wait(team_dir, &args.id)?;
    let previous_status = wait.status.clone();
    if let Some(status) = args.status {
        wait.status = status;
    }
    if let Some(progress) = args.progress {
        wait.progress = progress;
    }
    if args.clear_evidence {
        wait.evidence = None;
    }
    if let Some(evidence) = args.evidence {
        wait.evidence = if evidence.trim().is_empty() {
            None
        } else {
            Some(evidence)
        };
    }
    validate_wait_status_transition(team_dir, &wait, &previous_status)?;
    wait.updated_at = now();
    write_json_atomic(&wait_path(team_dir, &wait.id), &wait)?;
    append_event(
        team_dir,
        "wait_updated",
        serde_json::json!({
            "wait": wait.id,
            "previous_status": previous_status.to_string(),
            "status": wait.status.to_string(),
            "owner": wait.owner.as_deref(),
            "task": wait.task_id.as_deref(),
            "evidence": wait.evidence.as_deref(),
        }),
    )?;
    handle_wait_status_change(team_dir, &wait, previous_status)?;
    println!("Updated wait {}", wait.id);
    Ok(())
}

fn validate_wait_status_transition(
    team_dir: &Path,
    wait: &TeamWait,
    previous_status: &TeamWaitStatus,
) -> Result<()> {
    if wait.status == TeamWaitStatus::Failed
        && previous_status.is_open()
        && wait_looks_like_external_long_wait(wait)
        && !wait_has_terminal_failure_evidence(team_dir, wait)
    {
        bail!(
            "refusing to mark external wait `{}` as failed without terminal failure evidence. \
             Keep it running/polling/blocked while the external tool is still pending, or provide \
             a real failure artifact/URL with --evidence, or include `terminal_failure:` in --progress.",
            wait.id
        );
    }
    Ok(())
}

fn wait_looks_like_external_long_wait(wait: &TeamWait) -> bool {
    let haystack =
        format!("{}\n{}\n{}", wait.title, wait.condition, wait.progress).to_ascii_lowercase();
    [
        "deep_thinker",
        "deep-researcher",
        "deep_researcher",
        "deep research",
        "mcp",
        "chatgpt",
        "external tool",
        "external api",
        "api/tool",
        "request id",
        "service-side",
        "polling",
        "external queue",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

fn wait_has_terminal_failure_evidence(team_dir: &Path, wait: &TeamWait) -> bool {
    let progress = wait.progress.to_ascii_lowercase();
    if progress.contains("terminal_failure:")
        || progress.contains("confirmed terminal failure")
        || progress.contains("final_error:")
    {
        return true;
    }
    let Some(evidence) = wait.evidence.as_deref().map(str::trim) else {
        return false;
    };
    if evidence.is_empty() {
        return false;
    }
    if evidence.starts_with("http://") || evidence.starts_with("https://") {
        return true;
    }
    let path = Path::new(evidence);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        team_dir.join(path)
    };
    if resolved.exists() {
        return true;
    }
    !(evidence.starts_with('/')
        || evidence.starts_with('.')
        || evidence.contains('/')
        || evidence.ends_with(".md")
        || evidence.ends_with(".json")
        || evidence.ends_with(".jsonl")
        || evidence.ends_with(".log")
        || evidence.ends_with(".txt")
        || evidence.ends_with(".yaml")
        || evidence.ends_with(".yml"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TeamWaitAutoCheck {
    FileExists(String),
    FileContains { path: String, pattern: String },
    Command(String),
}

fn auto_complete_wait_checks(team_dir: &Path) -> Result<Vec<String>> {
    let waits = load_waits(team_dir)?;
    let mut completed = Vec::new();
    for wait in waits.into_iter().filter(|wait| wait.status.is_open()) {
        let checks = parse_wait_auto_checks(&wait);
        if checks.is_empty() {
            continue;
        }
        match run_wait_auto_checks(team_dir, &wait, &checks) {
            Ok(()) => {
                set_team_wait(
                    team_dir,
                    WaitSetArgs {
                        id: wait.id.clone(),
                        status: Some(TeamWaitStatus::Completed),
                        progress: Some(format!(
                            "auto_completed: all {} AUTO_CHECK item(s) passed. Previous progress: {}",
                            checks.len(),
                            wait.progress
                        )),
                        evidence: wait.evidence.clone(),
                        clear_evidence: false,
                    },
                )?;
                append_event(
                    team_dir,
                    "wait_auto_completed",
                    serde_json::json!({
                        "wait": wait.id,
                        "owner": wait.owner.as_deref(),
                        "task": wait.task_id.as_deref(),
                        "checks": checks.len(),
                    }),
                )?;
                completed.push(wait.id);
            }
            Err(_) => {}
        }
    }
    Ok(completed)
}

fn parse_wait_auto_checks(wait: &TeamWait) -> Vec<TeamWaitAutoCheck> {
    let mut checks = Vec::new();
    for line in format!(
        "{}\n{}\n{}",
        wait.condition,
        wait.progress,
        wait.evidence.as_deref().unwrap_or_default()
    )
    .lines()
    {
        let trimmed = line.trim();
        let Some(rest) = trimmed
            .strip_prefix("AUTO_CHECK ")
            .or_else(|| trimmed.strip_prefix("auto_check "))
        else {
            continue;
        };
        if let Some(path) = rest
            .strip_prefix("file_exists ")
            .or_else(|| rest.strip_prefix("path_exists "))
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            checks.push(TeamWaitAutoCheck::FileExists(path.to_string()));
            continue;
        }
        if let Some(spec) = rest
            .strip_prefix("file_contains ")
            .or_else(|| rest.strip_prefix("log_contains "))
            .map(str::trim)
            && let Some((path, pattern)) = spec.split_once("::")
        {
            let path = path.trim();
            let pattern = pattern.trim();
            if !path.is_empty() && !pattern.is_empty() {
                checks.push(TeamWaitAutoCheck::FileContains {
                    path: path.to_string(),
                    pattern: pattern.to_string(),
                });
            }
            continue;
        }
        if let Some(command) = rest
            .strip_prefix("command ")
            .or_else(|| rest.strip_prefix("cmd "))
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            checks.push(TeamWaitAutoCheck::Command(command.to_string()));
        }
    }
    checks
}

fn run_wait_auto_checks(
    team_dir: &Path,
    wait: &TeamWait,
    checks: &[TeamWaitAutoCheck],
) -> Result<()> {
    let node = load_wait_check_node(team_dir, wait)?;
    for check in checks {
        match check {
            TeamWaitAutoCheck::FileExists(path) => {
                let command = format!("test -e {}", shell_quote(path));
                run_wait_check_command(&node, &command)
                    .with_context(|| format!("file does not exist: {path}"))?;
            }
            TeamWaitAutoCheck::FileContains { path, pattern } => {
                let command = format!("grep -F -- {} {}", shell_quote(pattern), shell_quote(path));
                run_wait_check_command(&node, &command)
                    .with_context(|| format!("pattern not found in {path}: {pattern}"))?;
            }
            TeamWaitAutoCheck::Command(command) => {
                run_wait_check_command(&node, command)
                    .with_context(|| format!("AUTO_CHECK command failed: {command}"))?;
            }
        }
    }
    Ok(())
}

fn load_wait_check_node(team_dir: &Path, wait: &TeamWait) -> Result<TeamNode> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node_id = wait.node.as_deref().unwrap_or("local");
    nodes
        .into_iter()
        .find(|node| node.id == node_id)
        .with_context(|| format!("wait `{}` AUTO_CHECK node `{node_id}` not found", wait.id))
}

fn run_wait_check_command(node: &TeamNode, command: &str) -> Result<String> {
    let command = if let Some(cwd) = node.cwd.as_deref().filter(|value| !value.trim().is_empty()) {
        format!("cd {} && {}", shell_quote(cwd), command)
    } else {
        command.to_string()
    };
    run_node_command_capture(node, &command)
}

fn record_task_wait_registration(
    team_dir: &Path,
    task_id: &str,
    wait_id: &str,
    wait_title: &str,
) -> Result<()> {
    let mut tasks = load_tasks(team_dir)?;
    let mut changed = false;
    let note = format!("Waiting on `{wait_id}`: {wait_title}");
    let now = now();
    for task in &mut tasks {
        if task.id != task_id
            || matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        {
            continue;
        }
        if !matches!(task.status, TaskStatus::InProgress | TaskStatus::Review) {
            task.status = TaskStatus::Waiting;
        }
        if !task
            .result
            .as_deref()
            .is_some_and(|result| result.contains(&note))
        {
            task.result = Some(append_result_note(task.result.as_deref(), &note));
        }
        task.updated_at = now.clone();
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    for task in &tasks {
        write_json_atomic(&task_path(team_dir, &task.id), task)?;
    }
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "task_wait_registered",
        serde_json::json!({
            "task": task_id,
            "wait": wait_id,
            "preserve_active_status": true,
        }),
    )?;
    Ok(())
}

fn handle_wait_status_change(
    team_dir: &Path,
    wait: &TeamWait,
    previous_status: TeamWaitStatus,
) -> Result<()> {
    if wait.status == previous_status {
        return Ok(());
    }
    let Some(task_id) = wait.task_id.as_deref() else {
        return Ok(());
    };
    let config = load_config(team_dir)?;
    let owner = wait.owner.as_deref().unwrap_or(config.lead.as_str());
    let evidence = wait.evidence.as_deref().unwrap_or("-");
    match &wait.status {
        TeamWaitStatus::Completed => {
            set_task_status_if_open(
                team_dir,
                task_id,
                TaskStatus::InProgress,
                Some(&format!(
                    "Wait `{}` completed. Evidence: {evidence}. Owner must inspect the result and publish the final handoff/checklist or next blocker.",
                    wait.id
                )),
            )?;
            resume_wait_owner_after_wait_status_change(team_dir, wait, task_id)?;
        }
        TeamWaitStatus::Cancelled if task_has_other_open_wait(team_dir, task_id, &wait.id)? => {
            append_task_result_note_if_open(
                team_dir,
                task_id,
                &format!(
                    "Wait `{}` was cancelled while another wait for this task is still open. Treat this wait as obsolete/duplicate and keep following the remaining open wait(s). Progress: {}",
                    wait.id, wait.progress
                ),
            )?;
            resume_wait_owner_after_wait_status_change(team_dir, wait, task_id)?;
        }
        TeamWaitStatus::Failed | TeamWaitStatus::Cancelled | TeamWaitStatus::Blocked => {
            set_task_status_if_open(
                team_dir,
                task_id,
                TaskStatus::Blocked,
                Some(&format!(
                    "Wait `{}` ended as {}. Evidence/progress: {evidence} {}",
                    wait.id, wait.status, wait.progress
                )),
            )?;
            resume_wait_owner_after_wait_status_change(team_dir, wait, task_id)?;
        }
        TeamWaitStatus::Waiting | TeamWaitStatus::Running | TeamWaitStatus::Polling => {}
    }
    if config.members.iter().any(|member| member.name == owner) && owner != config.lead {
        set_member_status(team_dir, owner, MemberStatus::Online)?;
    }
    Ok(())
}

fn task_has_other_open_wait(team_dir: &Path, task_id: &str, current_wait_id: &str) -> Result<bool> {
    Ok(load_waits(team_dir)?.into_iter().any(|other| {
        other.id != current_wait_id
            && other.task_id.as_deref() == Some(task_id)
            && other.status.is_open()
    }))
}

fn append_task_result_note_if_open(team_dir: &Path, task_id: &str, note: &str) -> Result<bool> {
    let mut tasks = load_tasks(team_dir)?;
    let mut changed = false;
    for task in &mut tasks {
        if task.id != task_id
            || matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        {
            continue;
        }
        if task
            .result
            .as_deref()
            .is_some_and(|result| result.contains(note))
        {
            continue;
        }
        task.result = Some(append_result_note(task.result.as_deref(), note));
        task.updated_at = now();
        changed = true;
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
    }
    Ok(changed)
}

fn resume_wait_owner_after_wait_status_change(
    team_dir: &Path,
    wait: &TeamWait,
    task_id: &str,
) -> Result<()> {
    let config = load_config(team_dir)?;
    let Some(owner) = wait.owner.as_deref() else {
        return Ok(());
    };
    let evidence = wait.evidence.as_deref().unwrap_or("-");
    let language = config.language.unwrap_or_default();
    if owner != config.lead && config.members.iter().any(|member| member.name == owner) {
        set_member_status(team_dir, owner, MemberStatus::Online)?;
        let message = if language.is_ja() {
            format!(
                "WAIT_STATUS: task {task_id} に紐づく wait `{}` が `{}` になりました。condition=`{}` evidence=`{evidence}` progress=`{}`。結果を確認し、final handoff/checklist、次の task、または具体的 blocker を lead/all に送ってください。",
                wait.id, wait.status, wait.condition, wait.progress
            )
        } else {
            format!(
                "WAIT_STATUS: wait `{}` for task {task_id} is now `{}`. condition=`{}` evidence=`{evidence}` progress=`{}`. Inspect the result, then send lead/all the final handoff/checklist, next task, or concrete blocker.",
                wait.id, wait.status, wait.condition, wait.progress
            )
        };
        send_team_message_to_dir(team_dir, "system", owner, &message)?;
    }
    let lead_message = if language.is_ja() {
        format!(
            "WAIT_STATUS: @{owner} の task {task_id} に紐づく wait `{}` が `{}` になりました。condition=`{}` evidence=`{evidence}`。handoff/recovery のため owner を再開しました。",
            wait.id, wait.status, wait.condition
        )
    } else {
        format!(
            "WAIT_STATUS: @{owner}'s wait `{}` for task {task_id} is now `{}`. condition=`{}` evidence=`{evidence}`. Owner was resumed for handoff/recovery.",
            wait.id, wait.status, wait.condition
        )
    };
    send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
    append_event(
        team_dir,
        "wait_owner_resumed",
        serde_json::json!({
            "wait": wait.id,
            "task": task_id,
            "owner": owner,
            "status": wait.status.to_string(),
            "evidence": wait.evidence.as_deref(),
        }),
    )?;
    Ok(())
}

fn start_team_job(team_dir: &Path, args: JobStartArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node_id = sanitize_id(&args.node);
    let node = nodes
        .iter()
        .find(|node| node.id == node_id)
        .with_context(|| format!("node `{}` not found", args.node))?
        .clone();
    let id = args
        .id
        .map(|id| sanitize_id(&id))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| allocate_job_id(team_dir).unwrap_or_else(|_| "job-1".to_string()));
    if job_path(team_dir, &id).exists() {
        bail!("job `{id}` already exists");
    }
    let config = load_config(team_dir)?;
    let owner = args
        .owner
        .or_else(|| std::env::var("CODEX_TEAM_MEMBER").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "lead".to_string());
    ensure_member_exists(&config, &owner)?;
    let task_id = args.task.filter(|value| !value.trim().is_empty());
    if let Some(task_id) = task_id.as_deref() {
        let tasks = load_tasks(team_dir)?;
        let Some(task) = tasks.iter().find(|task| task.id == task_id) else {
            bail!("task `{task_id}` does not exist");
        };
        if let Some(task_owner) = task.owner.as_deref()
            && task_owner != owner
            && owner != config.lead
        {
            bail!("task `{task_id}` is owned by `{task_owner}`, not `{owner}`");
        }
        set_task_status_if_open(
            team_dir,
            task_id,
            TaskStatus::InProgress,
            Some(&format!("Tracked by job `{id}`.")),
        )?;
    }
    let command = args
        .command
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    let cwd = args
        .cwd
        .or_else(|| node.cwd.clone())
        .unwrap_or_else(|| ".".to_string());
    let remote_base = format!("/tmp/codex-team-jobs/{id}");
    let remote_log = format!("{remote_base}/job.log");
    let remote_exit = format!("{remote_base}/exit.code");
    let created_at = now();
    let mut job = TeamJob {
        id: id.clone(),
        node: node.id.clone(),
        command: command.clone(),
        cwd: cwd.clone(),
        owner: Some(owner.clone()),
        task_id: task_id.clone(),
        status: TeamJobStatus::Running,
        pid: None,
        log_path: remote_log.clone(),
        exit_path: remote_exit.clone(),
        exit_code: None,
        note: args.note,
        artifacts: Vec::new(),
        created_at: created_at.clone(),
        updated_at: created_at,
    };
    write_json_atomic(&job_path(team_dir, &id), &job)?;
    append_event(
        team_dir,
        "job_registered_before_remote_start",
        serde_json::json!({
            "job": id,
            "node": node.id,
            "owner": owner.clone(),
            "task": task_id.clone(),
            "log": remote_log,
            "exit": remote_exit,
        }),
    )?;
    let start_script = format!(
        "base={base}; exit_path={exit_path}; log_path={log}; mkdir -p \"$base\" && cd {cwd} && rm -f \"$exit_path\" && (trap 'code=$?; printf \"%s\" \"$code\" > \"$exit_path\"' EXIT; bash -lc {command} > \"$log_path\" 2>&1) & pid=$!; printf \"%s\" \"$pid\" > \"$base/pid\"; echo \"$pid\"",
        base = shell_quote(&remote_base),
        cwd = shell_quote(&cwd),
        exit_path = shell_quote(&remote_exit),
        command = shell_quote(&command),
        log = shell_quote(&remote_log),
    );
    let pid = run_node_command_capture(&node, &start_script)
        .context("start team job")?
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .to_string();
    job.pid = if pid.is_empty() { None } else { Some(pid) };
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &id), &job)?;
    append_event(
        team_dir,
        "job_started",
        serde_json::json!({
            "job": id,
            "node": node.id,
            "owner": owner,
            "task": task_id,
            "pid": job.pid,
        }),
    )?;
    if job.owner.as_deref() == Some(config.lead.as_str()) && job.task_id.is_none() {
        append_event(
            team_dir,
            "lead_job_without_department_task",
            serde_json::json!({
                "job": job.id,
                "node": job.node,
                "reason": "lead-started job is not tied to a department task",
                "recommendation": "prefer --owner <department> --task <id> so execution evidence is owned and auditable",
            }),
        )?;
    }
    println!(
        "Started job {} on node {} pid={}",
        job.id,
        job.node,
        job.pid.as_deref().unwrap_or("")
    );
    Ok(())
}

fn show_job_status(team_dir: &Path, id: &str) -> Result<()> {
    print!("{}", format_job_status_text(team_dir, id)?);
    Ok(())
}

fn show_job_logs(team_dir: &Path, args: JobLogsArgs) -> Result<()> {
    print!("{}", job_logs_text(team_dir, &args.id, args.tail)?);
    Ok(())
}

fn format_job_status_text(team_dir: &Path, id: &str) -> Result<String> {
    let job = refresh_job_status(team_dir, id)?;
    let mut out = String::new();
    out.push_str(&format!(
        "{} status={:?} node={} pid={} exit={}\n",
        job.id,
        job.status,
        job.node,
        job.pid.as_deref().unwrap_or(""),
        job.exit_code
            .map(|code| code.to_string())
            .unwrap_or_default()
    ));
    out.push_str(&format!("cwd={}\n", job.cwd));
    out.push_str(&format!("log={}\n", job.log_path));
    out.push_str(&format!("command={}\n", job.command));
    if !job.artifacts.is_empty() {
        out.push_str("artifacts:\n");
        for artifact in job.artifacts {
            out.push_str(&format!("  {}  {}\n", artifact.path, artifact.note));
        }
    }
    Ok(out)
}

fn job_logs_text(team_dir: &Path, id: &str, tail: Option<usize>) -> Result<String> {
    let job = load_job(team_dir, id)?;
    let node = load_node_for_job(team_dir, &job)?;
    let script = match tail {
        Some(lines) => format!("tail -n {} {}", lines, shell_quote(&job.log_path)),
        None => format!("cat {}", shell_quote(&job.log_path)),
    };
    run_node_command_capture(&node, &script)
}

fn stop_team_job(team_dir: &Path, id: &str) -> Result<()> {
    let mut job = load_job(team_dir, id)?;
    let node = load_node_for_job(team_dir, &job)?;
    if let Some(pid) = job.pid.as_deref() {
        let script = format!("kill {} >/dev/null 2>&1 || true", shell_quote(pid));
        let _ = run_node_command_capture(&node, &script);
    }
    job.status = TeamJobStatus::Stopped;
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &job.id), &job)?;
    append_event(
        team_dir,
        "job_stopped",
        serde_json::json!({ "job": job.id }),
    )?;
    println!("Stopped job {}", job.id);
    Ok(())
}

fn add_job_artifact(team_dir: &Path, args: JobArtifactArgs) -> Result<()> {
    let mut job = load_job(team_dir, &args.id)?;
    job.artifacts.push(TeamArtifact {
        path: args.path,
        note: args.note,
        created_at: now(),
    });
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &job.id), &job)?;
    append_event(
        team_dir,
        "job_artifact_added",
        serde_json::json!({ "job": job.id, "artifacts": job.artifacts }),
    )?;
    handle_job_artifact_handoff(team_dir, &job)?;
    println!("Registered artifact for job {}", job.id);
    Ok(())
}

fn handle_job_artifact_handoff(team_dir: &Path, job: &TeamJob) -> Result<()> {
    if !matches!(job.status, TeamJobStatus::Completed) {
        return Ok(());
    }
    let Some(task_id) = job.task_id.as_deref() else {
        return Ok(());
    };
    if !job_owner_matches_task_owner(team_dir, job, task_id)? {
        record_auxiliary_job_status(team_dir, job, task_id)?;
        return Ok(());
    }
    let artifact_summary = if job.artifacts.is_empty() {
        "none".to_string()
    } else {
        job.artifacts
            .iter()
            .map(|artifact| {
                if artifact.note.trim().is_empty() {
                    artifact.path.clone()
                } else {
                    format!("{} ({})", artifact.path, artifact.note)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let changed = set_task_status_if_open(
        team_dir,
        task_id,
        TaskStatus::InProgress,
        Some(&format!(
            "Job `{}` has registered artifact(s): {artifact_summary}. Owner must inspect them and publish the task's final report/json/manifest/checklist or a concrete blocker before review.",
            job.id
        )),
    )?;
    if changed {
        resume_job_owner_after_job_status_change(team_dir, job, task_id, TaskStatus::InProgress)?;
        append_event(
            team_dir,
            "job_artifact_requires_owner_handoff",
            serde_json::json!({
                "job": job.id,
                "task": task_id,
                "owner": job.owner,
                "artifacts": job.artifacts,
                "recommendation": "artifact registration revived the owner task for formal handoff/checklist or blocker",
            }),
        )?;
    }
    Ok(())
}

fn refresh_job_status(team_dir: &Path, id: &str) -> Result<TeamJob> {
    let mut job = load_job(team_dir, id)?;
    if matches!(job.status, TeamJobStatus::Stopped) {
        return Ok(job);
    }
    let previous_status = job.status.clone();
    if job.pid.is_none()
        && matches!(job.status, TeamJobStatus::Running)
        && job_start_grace_active(&job)
    {
        return Ok(job);
    }
    let node = load_node_for_job(team_dir, &job)?;
    let script = format!(
        "exit_path={exit_path}; log_path={log_path}; pid_value={pid}; pid_file=\"$(dirname \"$exit_path\")/pid\"; if [ -f \"$exit_path\" ]; then cat \"$exit_path\"; elif [ -n \"$pid_value\" ] && kill -0 \"$pid_value\" >/dev/null 2>&1; then echo RUNNING; elif [ -f \"$pid_file\" ] && pid_from_file=\"$(cat \"$pid_file\" 2>/dev/null)\" && [ -n \"$pid_from_file\" ] && kill -0 \"$pid_from_file\" >/dev/null 2>&1; then echo \"RUNNING_PID:$pid_from_file\"; elif [ -f \"$log_path\" ] && now_ts=\"$(date +%s)\" && log_ts=\"$(stat -c %Y \"$log_path\" 2>/dev/null)\" && [ -n \"$log_ts\" ] && [ $((now_ts - log_ts)) -lt 300 ]; then echo RUNNING_NO_PID; else echo UNKNOWN; fi",
        exit_path = shell_quote(&job.exit_path),
        log_path = shell_quote(&job.log_path),
        pid = shell_quote(job.pid.as_deref().unwrap_or("")),
    );
    let status = run_node_command_capture(&node, &script)
        .unwrap_or_else(|_| "UNKNOWN".to_string())
        .trim()
        .to_string();
    if status == "RUNNING" || status == "RUNNING_NO_PID" {
        job.status = TeamJobStatus::Running;
    } else if let Some(pid) = status.strip_prefix("RUNNING_PID:") {
        let pid = pid.trim();
        if !pid.is_empty() {
            job.pid = Some(pid.to_string());
        }
        job.status = TeamJobStatus::Running;
    } else if let Ok(code) = status.parse::<i32>() {
        job.exit_code = Some(code);
        job.status = if code == 0 {
            TeamJobStatus::Completed
        } else {
            TeamJobStatus::Failed
        };
    } else if !matches!(job.status, TeamJobStatus::Stopped) {
        if job.pid.is_some() {
            job.status = TeamJobStatus::Failed;
        } else {
            job.status = TeamJobStatus::Unknown;
        }
    }
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &job.id), &job)?;
    if job.status != previous_status {
        if !claim_job_status_notification(team_dir, &job.id, &job.status)? {
            return Ok(job);
        }
        append_event(
            team_dir,
            match job.status {
                TeamJobStatus::Completed => "job_completed",
                TeamJobStatus::Failed => "job_failed",
                TeamJobStatus::Stopped => "job_stopped",
                TeamJobStatus::Unknown => "job_unknown",
                TeamJobStatus::Running => "job_running",
            },
            serde_json::json!({
                "job": job.id,
                "node": job.node,
                "owner": job.owner,
                "task": job.task_id,
                "exit_code": job.exit_code,
                "artifacts": job.artifacts,
            }),
        )?;
        if let Some(task_id) = job.task_id.as_deref() {
            if !job_owner_matches_task_owner(team_dir, &job, task_id)? {
                record_auxiliary_job_status(team_dir, &job, task_id)?;
            } else if job_task_is_closed(team_dir, task_id)? {
                append_event(
                    team_dir,
                    "job_status_ignored_closed_task",
                    serde_json::json!({
                        "job": job.id,
                        "task": task_id,
                        "owner": job.owner,
                        "job_status": format!("{:?}", job.status),
                        "exit_code": job.exit_code,
                        "reason": "task is already closed; stale or superseded job status must not reopen or resume it",
                    }),
                )?;
            } else {
                match job.status {
                    TeamJobStatus::Completed => {
                        if job.artifacts.is_empty() {
                            set_task_status_if_open(
                                team_dir,
                                task_id,
                                TaskStatus::InProgress,
                                Some(&format!(
                                    "Job `{}` completed without registered artifacts; owner must continue the task and publish final artifacts/checklist or a blocker before review.",
                                    job.id
                                )),
                            )?;
                            resume_job_owner_after_job_status_change(
                                team_dir,
                                &job,
                                task_id,
                                TaskStatus::InProgress,
                            )?;
                            append_event(
                                team_dir,
                                "job_completed_without_artifacts",
                                serde_json::json!({
                                    "job": job.id,
                                    "task": task_id,
                                    "owner": job.owner,
                                    "recommendation": "if this was a read-only/probe job, do not register fake artifacts; continue the task and publish the final handoff/checklist or a blocker",
                                }),
                            )?;
                        } else {
                            set_task_status_if_open(
                                team_dir,
                                task_id,
                                TaskStatus::InProgress,
                                Some(&format!(
                                    "Job `{}` completed with registered artifacts; owner must inspect them and publish the task's final report/json/manifest/checklist or a blocker before review.",
                                    job.id
                                )),
                            )?;
                            resume_job_owner_after_job_status_change(
                                team_dir,
                                &job,
                                task_id,
                                TaskStatus::InProgress,
                            )?;
                            append_event(
                                team_dir,
                                "job_completed_requires_owner_handoff",
                                serde_json::json!({
                                    "job": job.id,
                                    "task": task_id,
                                    "owner": job.owner,
                                    "artifacts": job.artifacts,
                                    "recommendation": "treat job artifacts as intermediate evidence until the owner publishes a formal final handoff/checklist or blocker",
                                }),
                            )?;
                        }
                    }
                    TeamJobStatus::Failed | TeamJobStatus::Unknown => {
                        set_task_status_if_open(
                            team_dir,
                            task_id,
                            TaskStatus::Blocked,
                            Some(&format!(
                                "Job `{}` ended with status {:?}; inspect {}.",
                                job.id, job.status, job.log_path
                            )),
                        )?;
                        resume_job_owner_after_job_status_change(
                            team_dir,
                            &job,
                            task_id,
                            TaskStatus::Blocked,
                        )?;
                    }
                    TeamJobStatus::Running | TeamJobStatus::Stopped => {}
                }
            }
        }
    }
    Ok(job)
}

fn job_start_grace_active(job: &TeamJob) -> bool {
    let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(&job.created_at) else {
        return false;
    };
    (Utc::now() - created_at.with_timezone(&Utc)).num_seconds() < 60
}

fn claim_job_status_notification(
    team_dir: &Path,
    job_id: &str,
    status: &TeamJobStatus,
) -> Result<bool> {
    let status = match status {
        TeamJobStatus::Completed => "completed",
        TeamJobStatus::Failed => "failed",
        TeamJobStatus::Stopped => "stopped",
        TeamJobStatus::Unknown => "unknown",
        TeamJobStatus::Running => return Ok(true),
    };
    let dir = team_dir.join("job_status_notifications");
    fs::create_dir_all(&dir)?;
    let marker = dir.join(format!("{}.{}", sanitize_id(job_id), status));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(mut file) => {
            writeln!(file, "{}", now())?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(err).with_context(|| format!("failed to create {}", marker.display())),
    }
}

fn job_owner_matches_task_owner(team_dir: &Path, job: &TeamJob, task_id: &str) -> Result<bool> {
    let Some(job_owner) = job.owner.as_deref() else {
        return Ok(false);
    };
    let tasks = load_tasks(team_dir)?;
    Ok(tasks
        .iter()
        .find(|task| task.id == task_id)
        .and_then(|task| task.owner.as_deref())
        .is_some_and(|task_owner| task_owner == job_owner))
}

fn job_task_is_closed(team_dir: &Path, task_id: &str) -> Result<bool> {
    Ok(load_tasks(team_dir)?
        .iter()
        .find(|task| task.id == task_id)
        .is_some_and(|task| !task_is_open(task)))
}

fn record_auxiliary_job_status(team_dir: &Path, job: &TeamJob, task_id: &str) -> Result<()> {
    let task_owner = load_tasks(team_dir)?
        .into_iter()
        .find(|task| task.id == task_id)
        .and_then(|task| task.owner);
    append_event(
        team_dir,
        "auxiliary_job_status_no_task_update",
        serde_json::json!({
            "job": job.id,
            "task": task_id,
            "job_owner": job.owner,
            "task_owner": task_owner,
            "job_status": format!("{:?}", job.status),
            "exit_code": job.exit_code,
            "log_path": job.log_path,
            "recommendation": "job owner differs from task owner, so the task status/result was not modified automatically",
        }),
    )?;
    let config = load_config(team_dir)?;
    let message = format!(
        "AUX_JOB_STATUS: job `{job}` for task {task} ended with status {status:?} exit={exit}, but the job owner ({job_owner}) differs from the task owner ({task_owner}). I did not modify the task status/result automatically. Treat `{log}` as auxiliary evidence; lead/task owner should decide the next handoff, blocker, or clearance.",
        job = &job.id,
        task = task_id,
        status = &job.status,
        exit = job
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "-".to_string()),
        job_owner = job.owner.as_deref().unwrap_or("-"),
        task_owner = task_owner.as_deref().unwrap_or("-"),
        log = &job.log_path,
    );
    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
    if let Some(owner) = task_owner.as_deref()
        && owner != config.lead
        && config.members.iter().any(|member| member.name == owner)
    {
        send_team_message_to_dir(team_dir, "system", owner, &message)?;
    }
    Ok(())
}

fn resume_job_owner_after_job_status_change(
    team_dir: &Path,
    job: &TeamJob,
    task_id: &str,
    task_status: TaskStatus,
) -> Result<()> {
    let config = load_config(team_dir)?;
    let Some(owner) = job.owner.as_deref() else {
        return Ok(());
    };
    if owner == config.lead {
        return Ok(());
    }
    if !config.members.iter().any(|member| member.name == owner) {
        return Ok(());
    }
    set_member_status(team_dir, owner, MemberStatus::Online)?;
    let status_text = task_status.to_string();
    let language = config.language.unwrap_or_default();
    let followup_guidance = job_status_followup_guidance(job, language);
    let exit = job
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "-".to_string());
    let owner_message = if language.is_ja() {
        format!(
            "JOB_STATUS: あなたの task {task} に紐づく job `{job}` が status {job_status:?} exit={exit} で終了しました。task は現在 `{task_status}` です。`{log}` を確認してください。{followup_guidance}",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
            log = &job.log_path,
        )
    } else {
        format!(
            "JOB_STATUS: job `{job}` for your task {task} ended with status {job_status:?} exit={exit}. The task is now `{task_status}`. Inspect `{log}`. {followup_guidance}",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
            log = &job.log_path,
        )
    };
    send_team_message_to_dir(team_dir, "system", owner, &owner_message)?;
    let lead_message = if language.is_ja() {
        format!(
            "JOB_STATUS: @{owner} の task {task} に紐づく job `{job}` が status {job_status:?} exit={exit} で終了しました。task は現在 `{task_status}` で、handoff/recovery のため @{owner} を再開しました。",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
        )
    } else {
        format!(
            "JOB_STATUS: @{owner}'s job `{job}` for task {task} ended with status {job_status:?} exit={exit}; task is now `{task_status}` and @{owner} was resumed for handoff/recovery.",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
        )
    };
    send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
    append_event(
        team_dir,
        "job_owner_resumed",
        serde_json::json!({
            "job": &job.id,
            "task": task_id,
            "owner": owner,
            "task_status": &status_text,
            "job_status": format!("{:?}", job.status),
            "exit_code": job.exit_code,
        }),
    )?;
    Ok(())
}

fn job_status_followup_guidance(job: &TeamJob, language: TeamPromptLanguage) -> &'static str {
    match job.status {
        TeamJobStatus::Completed if job.artifacts.is_empty() && language.is_ja() => {
            "この job は成果物を登録していません。読み取り専用の調査/検証 job だった場合でも、ここで止まらず owner task を継続し、task の本当の最終 report/json/manifest/checklist を書くか、具体的 blocker を記録してから lead/all に TEAM_COMPLETION_CHECKLIST 付きで通知してください。"
        }
        TeamJobStatus::Completed if job.artifacts.is_empty() => {
            "This job registered no artifacts. If it was only a read-only/probe/verification job, do not register fake artifacts and do not stop here; continue the owner task, write the task's real final report/json/manifest/checklist or mark a concrete blocker, then notify lead/all with TEAM_COMPLETION_CHECKLIST."
        }
        TeamJobStatus::Completed if language.is_ja() => {
            "登録済み job 成果物だけでは task 完了扱いにしません。成果物を確認し、owner task を継続して必要な final report/json/manifest/checklist または具体的 blocker を出し、成果物 path の検証を引用し、review 前に lead/all へ TEAM_COMPLETION_CHECKLIST 付きで通知してください。"
        }
        TeamJobStatus::Completed => {
            "Registered job artifacts are not sufficient by themselves. Inspect them, then continue the owner task and publish the required final report/json/manifest/checklist or a concrete blocker, cite verification for the artifact paths, and notify lead/all with TEAM_COMPLETION_CHECKLIST before review."
        }
        TeamJobStatus::Failed | TeamJobStatus::Unknown if language.is_ja() => {
            "確認が終わるまでは blocker として扱ってください。log path を保持し、失敗原因を診断し、修正して evidence 付きで再実行するか、task を具体的な次アクション付きで blocked にしてください。"
        }
        TeamJobStatus::Failed | TeamJobStatus::Unknown => {
            "Treat this as a blocker until inspected: preserve the log path, diagnose the failure, and either repair/rerun with evidence or mark the task blocked with exact next action."
        }
        TeamJobStatus::Running | TeamJobStatus::Stopped if language.is_ja() => {
            "現在の job 状態と次の checkpoint を lead に報告してください。"
        }
        TeamJobStatus::Running | TeamJobStatus::Stopped => {
            "Report the current job state and next checkpoint to lead."
        }
    }
}

fn add_team_node(team_dir: &Path, args: NodeAddArgs) -> Result<()> {
    let id = sanitize_id(&args.id);
    if id.is_empty() {
        bail!("invalid node id `{}`", args.id);
    }
    if id == "local" && !matches!(args.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
        bail!("node `local` must use kind local/manual");
    }
    if matches!(args.kind, TeamNodeKind::Manual | TeamNodeKind::Local) && args.url.is_none() {
        bail!("node `{id}` needs --url unless it is managed by the current team run");
    }
    if matches!(args.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker) {
        let container = args
            .container
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .with_context(|| format!("node `{id}` needs --container"))?;
        if matches!(args.kind, TeamNodeKind::SshDocker) && args.host.is_none() {
            bail!("node `{id}` needs --host for ssh-docker");
        }
        if !docker_container_exists(args.host.as_deref(), container) {
            bail!("node `{id}` container `{container}` does not exist or is not inspectable");
        }
    }
    let needs_container_department =
        matches!(args.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker);
    let mut nodes = load_nodes(team_dir)?;
    let now = now();
    if needs_container_department
        && let Some(existing_idx) = nodes.iter().position(|existing| {
            existing.id != id
                && same_container_node_target(
                    existing,
                    &args.kind,
                    args.host.as_deref(),
                    args.container.as_deref(),
                )
        })
    {
        let existing_id = nodes[existing_idx].id.clone();
        let previous_url = nodes[existing_idx].url.clone();
        let previous_cwd = nodes[existing_idx].cwd.clone();
        nodes[existing_idx].kind = args.kind;
        nodes[existing_idx].url = args.url.or(previous_url);
        nodes[existing_idx].host = args.host;
        nodes[existing_idx].container = args.container;
        nodes[existing_idx].cwd = args.cwd.or(previous_cwd);
        nodes[existing_idx].note = args.note;
        nodes[existing_idx].updated_at = now;
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        write_nodes(team_dir, &nodes)?;
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "node_duplicate_merged",
            serde_json::json!({
                "reported": id,
                "existing": existing_id,
                "reason": "same container target",
            }),
        )?;
        ensure_container_node_departments(team_dir)?;
        println!("Registered node {existing_id}");
        return Ok(());
    }
    let node = TeamNode {
        id: id.clone(),
        kind: args.kind,
        url: args.url,
        host: args.host,
        container: args.container,
        cwd: args.cwd,
        status: TeamNodeStatus::Pending,
        note: args.note,
        created_at: now.clone(),
        updated_at: now,
    };
    match nodes.iter_mut().find(|existing| existing.id == id) {
        Some(existing) => {
            let created_at = existing.created_at.clone();
            *existing = node;
            existing.created_at = created_at;
        }
        None => nodes.push(node),
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    write_nodes(team_dir, &nodes)?;
    touch_config(team_dir)?;
    append_event(team_dir, "node_added", serde_json::json!({ "node": id }))?;
    if needs_container_department {
        ensure_container_node_departments(team_dir)?;
    }
    println!("Registered node {id}");
    Ok(())
}

fn same_container_node_target(
    node: &TeamNode,
    kind: &TeamNodeKind,
    host: Option<&str>,
    container: Option<&str>,
) -> bool {
    if &node.kind != kind {
        return false;
    }
    let Some(container) = container.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if node.container.as_deref().map(str::trim) != Some(container) {
        return false;
    }
    if matches!(kind, TeamNodeKind::SshDocker) {
        let Some(host) = host.map(str::trim).filter(|value| !value.is_empty()) else {
            return false;
        };
        node.host.as_deref().map(str::trim) == Some(host)
    } else {
        true
    }
}

fn remove_team_node(team_dir: &Path, args: NodeRemoveArgs) -> Result<()> {
    let id = sanitize_id(&args.id);
    if id == "local" {
        bail!("node `local` cannot be removed");
    }
    let config = load_config(team_dir)?;
    if !args.force
        && config
            .members
            .iter()
            .any(|member| member.node.as_deref() == Some(id.as_str()))
    {
        bail!("node `{id}` is assigned to a member; pass --force to remove it");
    }
    let mut nodes = load_nodes(team_dir)?;
    let before = nodes.len();
    nodes.retain(|node| node.id != id);
    if nodes.len() == before {
        bail!("node `{id}` not found");
    }
    write_nodes(team_dir, &nodes)?;
    deactivate_removed_node_members(team_dir, &id)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "node_removed",
        serde_json::json!({ "node": id, "forced": args.force }),
    )?;
    println!("Removed node {id}");
    Ok(())
}

fn deactivate_removed_node_members(team_dir: &Path, node_id: &str) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let now = now();
    let members = config
        .members
        .iter_mut()
        .filter(|member| member.node.as_deref() == Some(node_id))
        .map(|member| {
            member.status = MemberStatus::Standby;
            member.name.clone()
        })
        .collect::<Vec<_>>();
    if members.is_empty() {
        return Ok(());
    }
    config.updated_at = now.clone();
    write_json_atomic(&team_dir.join("config.json"), &config)?;

    let mut tasks = load_tasks(team_dir)?;
    let mut changed_tasks = Vec::new();
    for task in &mut tasks {
        if task
            .owner
            .as_deref()
            .map(|owner| members.iter().any(|member| member == owner))
            .unwrap_or(false)
            && !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        {
            task.status = TaskStatus::Cancelled;
            task.updated_at = now.clone();
            task.result = Some(format!("Cancelled because node `{node_id}` was removed."));
            changed_tasks.push(task.id.clone());
        }
    }
    for task in &tasks {
        write_json_atomic(&task_path(team_dir, &task.id), task)?;
    }
    append_event(
        team_dir,
        "node_members_deactivated",
        serde_json::json!({
            "node": node_id,
            "members": members,
            "cancelled_tasks": changed_tasks,
        }),
    )?;
    Ok(())
}

fn assign_unowned_tasks_round_robin(team_dir: &Path) -> Result<()> {
    auto_promote_dependency_waits(team_dir)?;
    let config = load_config(team_dir)?;
    let workers: Vec<&TeamMember> = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .collect();
    if workers.is_empty() {
        return Ok(());
    }

    let mut tasks = load_tasks(team_dir)?;
    let snapshot = tasks.clone();
    let mut changed = false;
    let mut worker_idx = 0usize;
    for task in &mut tasks {
        if task.owner.is_none()
            && matches!(task.status, TaskStatus::Pending)
            && task_is_ready(task, &snapshot)
        {
            let member = workers[worker_idx % workers.len()];
            task.owner = Some(member.name.clone());
            task.updated_at = now();
            worker_idx += 1;
            changed = true;
        }
    }

    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "tasks_assigned",
            serde_json::json!({ "strategy": "round_robin" }),
        )?;
    }
    Ok(())
}

fn set_member_status(team_dir: &Path, member_name: &str, status: MemberStatus) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let Some(member) = config
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        bail!(
            "member `{member_name}` does not exist in team `{}`",
            config.id
        );
    };
    member.status = status;
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn normalize_stale_running_members_without_active_turns(
    team_dir: &Path,
    active: &HashMap<String, AppServerMemberRun>,
) -> Result<Vec<String>> {
    let mut config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let now = now();
    let mut normalized = Vec::new();
    for member in &mut config.members {
        if member.role == "lead" || !matches!(member.status, MemberStatus::Running) {
            continue;
        }
        let has_active_turn = active.get(&member.name).is_some_and(|run| !run.completed);
        let has_open_task = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()) && task_is_open(task));
        if has_active_turn || has_open_task {
            continue;
        }
        member.status = MemberStatus::Standby;
        normalized.push(member.name.clone());
    }
    if normalized.is_empty() {
        return Ok(normalized);
    }
    config.updated_at = now;
    write_json_atomic(&team_dir.join("config.json"), &config)?;
    append_event(
        team_dir,
        "member_status_normalized",
        serde_json::json!({
            "members": normalized,
            "from": "running",
            "to": "standby",
            "reason": "no active app-server turn or open owned task after runtime attach",
        }),
    )?;
    Ok(normalized)
}

fn member_status(team_dir: &Path, member_name: &str) -> Result<Option<MemberStatus>> {
    let config = load_config(team_dir)?;
    Ok(config
        .members
        .iter()
        .find(|member| member.name == member_name)
        .map(|member| member.status.clone()))
}

fn set_member_workspace(team_dir: &Path, member_name: &str, workspace_path: &Path) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let Some(member) = config
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        bail!(
            "member `{member_name}` does not exist in team `{}`",
            config.id
        );
    };
    member.workspace_path = Some(workspace_path.display().to_string());
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn set_member_thread(team_dir: &Path, member_name: &str, thread_id: &str) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let Some(member) = config
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        bail!(
            "member `{member_name}` does not exist in team `{}`",
            config.id
        );
    };
    member.thread_id = Some(thread_id.to_string());
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn prepare_member_worktree(
    team_dir: &Path,
    base_cwd: &Path,
    team_id: &str,
    member: &TeamMember,
) -> Result<PathBuf> {
    let worktrees_dir = team_dir.join("worktrees");
    fs::create_dir_all(&worktrees_dir)?;
    let worktree_path = worktrees_dir.join(&member.name);
    if worktree_path.exists() {
        set_member_workspace(team_dir, &member.name, &worktree_path)?;
        return Ok(worktree_path);
    }

    let branch = format!(
        "codex-team/{}/{}",
        sanitize_id(team_id),
        sanitize_id(&member.name)
    );
    let status = Command::new("git")
        .arg("-C")
        .arg(base_cwd)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch)
        .arg(&worktree_path)
        .arg("HEAD")
        .status()
        .with_context(|| format!("create git worktree for `{}`", member.name))?;
    if !status.success() {
        bail!("failed to create git worktree for `{}`", member.name);
    }
    set_member_workspace(team_dir, &member.name, &worktree_path)?;
    append_event(
        team_dir,
        "member_worktree_created",
        serde_json::json!({
            "member": member.name,
            "branch": branch,
            "path": worktree_path,
        }),
    )?;
    Ok(worktree_path)
}

fn mark_member_tasks(team_dir: &Path, member_name: &str, status: TaskStatus) -> Result<()> {
    auto_promote_dependency_waits(team_dir)?;
    let mut changed = false;
    let mut tasks = load_tasks(team_dir)?;
    let snapshot = tasks.clone();
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(task.status, TaskStatus::Pending | TaskStatus::Ready)
            && task_dependencies_completed(task, &snapshot)
        {
            task.status = status;
            task.updated_at = now();
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
    }
    Ok(())
}

fn set_task_status_if_open(
    team_dir: &Path,
    task_id: &str,
    status: TaskStatus,
    result: Option<&str>,
) -> Result<bool> {
    let mut changed = false;
    let mut completed_tasks = Vec::new();
    let mut rejected_completions = Vec::new();
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.id == task_id
            && !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        {
            if status == TaskStatus::Completed
                && let Some(issue) = task_completion_blocker(team_dir, task)?
            {
                task.status = TaskStatus::Blocked;
                task.result = Some(append_result_note(
                    result.or(task.result.as_deref()),
                    &format!("Completion rejected: {issue}"),
                ));
                rejected_completions.push((task.id.clone(), task.owner.clone(), issue));
            } else {
                task.status = status;
                if let Some(result) = result {
                    task.result = Some(result.to_string());
                }
                if status == TaskStatus::Completed {
                    completed_tasks.push(task.clone());
                }
            }
            task.updated_at = now();
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        notify_rejected_task_completions(team_dir, &rejected_completions)?;
        notify_completed_task_freezes(team_dir, &completed_tasks)?;
        auto_promote_dependency_waits(team_dir)?;
    }
    Ok(changed)
}

fn complete_member_tasks_if_active(team_dir: &Path, member_name: &str) -> Result<()> {
    let mut changed = false;
    let mut completed_tasks = Vec::new();
    let mut rejected_completions = Vec::new();
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Review
            )
        {
            if let Some(issue) = task_completion_blocker(team_dir, task)? {
                task.status = TaskStatus::Blocked;
                task.result = Some(append_result_note(
                    task.result.as_deref(),
                    &format!("Completion rejected: {issue}"),
                ));
                rejected_completions.push((task.id.clone(), task.owner.clone(), issue));
            } else {
                task.status = TaskStatus::Completed;
                if task.result.is_none() {
                    task.result = Some("Worker exited successfully.".to_string());
                }
                completed_tasks.push(task.clone());
            }
            task.updated_at = now();
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        notify_rejected_task_completions(team_dir, &rejected_completions)?;
        notify_completed_task_freezes(team_dir, &completed_tasks)?;
        auto_promote_dependency_waits(team_dir)?;
    }
    Ok(())
}

fn task_completion_missing_required_local_outputs(
    team_dir: &Path,
    task: &TeamTask,
) -> Result<Option<String>> {
    let paths = task_required_local_output_paths(team_dir, task)?;
    if paths.is_empty() {
        if task_requires_formal_handoff_package(task)
            && task_required_declared_non_local_output_paths(team_dir, task)?.is_empty()
        {
            return Ok(Some(
                "task requires a formal handoff package, but no task-specific local or node-side output package path was claimed or declared"
                    .to_string(),
            ));
        }
        return Ok(None);
    }
    let owner_has_completion_checklist_message = task
        .owner
        .as_deref()
        .map(|owner| owner_recent_completion_checklist_message(team_dir, owner))
        .transpose()?
        .unwrap_or(false);
    let mut issues = Vec::new();
    for path in paths {
        if let Some(issue) =
            inspect_local_handoff_path(&path, owner_has_completion_checklist_message)?
        {
            issues.push(format!("{}: {}", path.display(), issue));
        }
    }
    if issues.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "required local output package is incomplete ({})",
            issues.join("; ")
        )))
    }
}

fn task_requires_formal_handoff_package(task: &TeamTask) -> bool {
    let lower = format!(
        "{} {} {}",
        task.subject,
        task.description,
        task.result.as_deref().unwrap_or("")
    )
    .to_ascii_lowercase();
    lower.contains("team_completion_checklist")
        && (lower.contains("sha256_manifest")
            || lower.contains("manifest check")
            || lower.contains("sha256sum -c"))
}

fn task_completion_blocker(team_dir: &Path, task: &TeamTask) -> Result<Option<String>> {
    let open_waits = load_waits(team_dir)?
        .into_iter()
        .filter(|wait| wait.task_id.as_deref() == Some(task.id.as_str()))
        .filter(|wait| wait.status.is_open())
        .map(|wait| {
            format!(
                "{} status={} condition={}",
                wait.id, wait.status, wait.condition
            )
        })
        .collect::<Vec<_>>();
    if !open_waits.is_empty() {
        return Ok(Some(format!(
            "task has open wait item(s): {}",
            open_waits.join("; ")
        )));
    }
    if let Some(issue) = task_completion_missing_required_local_outputs(team_dir, task)? {
        return Ok(Some(issue));
    }
    task_completion_reported_local_hash_mismatch(team_dir, task)
}

fn task_completion_reported_local_hash_mismatch(
    team_dir: &Path,
    task: &TeamTask,
) -> Result<Option<String>> {
    let Some(result) = task.result.as_deref() else {
        return Ok(None);
    };
    let claims = extract_reported_hash_claims(result)?;
    if claims.is_empty() {
        return Ok(None);
    }
    let output_roots = task_required_local_output_paths(team_dir, task)?;
    if output_roots.is_empty() {
        return Ok(None);
    }

    for (label, expected) in claims {
        let candidates = resolve_reported_hash_claim_paths(&output_roots, &label)?;
        if candidates.len() != 1 {
            continue;
        }
        let path = &candidates[0];
        let actual = sha256sum_file(path)?;
        if actual != expected {
            return Ok(Some(format!(
                "reported handoff hash for `{label}` is stale or mismatched: result says {expected}, current {} is {actual}",
                path.display()
            )));
        }
    }
    Ok(None)
}

fn extract_reported_hash_claims(text: &str) -> Result<Vec<(String, String)>> {
    let mut claims = Vec::new();
    let file_hash = Regex::new(r"(?i)([A-Za-z0-9_./-]+)\s*=\s*([0-9a-f]{64})")?;
    for capture in file_hash.captures_iter(text) {
        let Some(label) = capture.get(1).map(|m| m.as_str()) else {
            continue;
        };
        if !reported_hash_label_looks_file_like(label) {
            continue;
        }
        let Some(hash) = capture.get(2).map(|m| m.as_str().to_ascii_lowercase()) else {
            continue;
        };
        claims.push((normalize_reported_hash_label(label), hash));
    }

    let manifest_hash = Regex::new(r"(?i)manifest\s+hash\s*=\s*([0-9a-f]{64})")?;
    for capture in manifest_hash.captures_iter(text) {
        let Some(hash) = capture.get(1).map(|m| m.as_str().to_ascii_lowercase()) else {
            continue;
        };
        claims.push(("manifest.sha256".to_string(), hash));
    }

    claims.sort();
    claims.dedup();
    Ok(claims)
}

fn reported_hash_label_looks_file_like(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    lower.contains('/')
        || lower.ends_with(".sha256")
        || lower.ends_with(".log")
        || lower.ends_with(".md")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".json")
}

fn normalize_reported_hash_label(label: &str) -> String {
    label
        .trim_matches(path_wrapper_or_trailing_punctuation)
        .to_string()
}

fn resolve_reported_hash_claim_paths(
    output_roots: &[PathBuf],
    label: &str,
) -> Result<Vec<PathBuf>> {
    let label_path = Path::new(label);
    if label_path.is_absolute() {
        return Ok(label_path
            .exists()
            .then(|| label_path.to_path_buf())
            .into_iter()
            .collect());
    }

    let mut candidates = Vec::new();
    for root in output_roots {
        let direct = root.join(label);
        if direct.is_file() {
            candidates.push(direct);
        }
    }
    if !candidates.is_empty() {
        candidates.sort();
        candidates.dedup();
        return Ok(candidates);
    }

    let Some(file_name) = Path::new(label).file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    for root in output_roots {
        collect_named_files(root, file_name, 0, &mut candidates)?;
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

fn collect_named_files(
    root: &Path,
    file_name: &str,
    depth: usize,
    matches: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > 4 || !root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_named_files(&path, file_name, depth + 1, matches)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == file_name)
        {
            matches.push(path);
        }
    }
    Ok(())
}

fn sha256sum_file(path: &Path) -> Result<String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .with_context(|| format!("run sha256sum {}", path.display()))?;
    if !output.status.success() {
        bail!("sha256sum failed for {}", path.display());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(|hash| hash.to_ascii_lowercase())
        .ok_or_else(|| anyhow!("sha256sum produced no hash for {}", path.display()))
}

fn task_required_local_output_paths(team_dir: &Path, task: &TeamTask) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let ownerships = load_ownerships(team_dir)?;
    for ownership in ownerships
        .iter()
        .filter(|ownership| task.owner.as_deref() == Some(ownership.owner.as_str()))
        .filter(|ownership| ownership_mentions_task(ownership, task))
        .filter(|ownership| ownership_path_is_probably_local(team_dir, &ownership.path))
    {
        paths.push(PathBuf::from(&ownership.path));
    }

    for path in extract_probable_local_output_paths_from_task_text(team_dir, task) {
        paths.push(path);
    }

    if paths.is_empty() && task_requires_formal_handoff_package(task) {
        for ownership in ownerships
            .iter()
            .filter(|ownership| task.owner.as_deref() == Some(ownership.owner.as_str()))
            .filter(|ownership| ownership_path_is_probably_local(team_dir, &ownership.path))
            .filter(|ownership| ownership_looks_like_owner_handoff_output(ownership))
        {
            paths.push(PathBuf::from(&ownership.path));
        }
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn ownership_looks_like_owner_handoff_output(ownership: &FileOwnership) -> bool {
    let lower = format!("{} {}", ownership.path, ownership.note).to_ascii_lowercase();
    [
        "artifact",
        "artifacts",
        "handoff",
        "completion",
        "checklist",
        "manifest",
        "report",
        "evidence",
        "成果物",
        "完了",
        "レビュー",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn task_required_declared_non_local_output_paths(
    team_dir: &Path,
    task: &TeamTask,
) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for ownership in load_ownerships(team_dir)?
        .into_iter()
        .filter(|ownership| task.owner.as_deref() == Some(ownership.owner.as_str()))
        .filter(|ownership| ownership_mentions_task(ownership, task))
        .filter(|ownership| !ownership_path_is_probably_local(team_dir, &ownership.path))
        .filter(|ownership| path_looks_like_task_handoff_output(&ownership.path))
    {
        paths.push(ownership.path);
    }

    let text = format!(
        "{} {} {}",
        task.subject,
        task.description,
        task.result.as_deref().unwrap_or("")
    );
    for (path, path_start) in extract_absolute_paths_with_offsets_from_text(&text) {
        if !ownership_path_is_probably_local(team_dir, &path)
            && (path_looks_like_task_handoff_output(&path)
                || text_path_context_is_output(&text, path_start))
        {
            paths.push(path);
        }
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn path_looks_like_task_handoff_output(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        "/audit/cycle",
        "/method_schema/cycle",
        "/runtime/cycle",
        "/provenance/cycle",
        "schema_handoff_validation",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn extract_probable_local_output_paths_from_task_text(
    team_dir: &Path,
    task: &TeamTask,
) -> Vec<PathBuf> {
    let text = format!(
        "{} {}",
        task.description,
        task.result.as_deref().unwrap_or("")
    );
    text.split_whitespace()
        .scan(0usize, |cursor, token| {
            let rel = text[*cursor..].find(token).unwrap_or(0);
            let start = *cursor + rel;
            *cursor = start + token.len();
            Some((start, token))
        })
        .filter_map(|(token_start, token)| {
            let raw = clean_embedded_path_token(token)?;
            let path_start = token_start + token.find(raw).unwrap_or(0);
            if text_path_context_is_output(&text, path_start)
                && ownership_path_is_probably_local(team_dir, raw)
            {
                Some(PathBuf::from(raw))
            } else {
                None
            }
        })
        .collect()
}

fn text_path_context_is_output(text: &str, path_start: usize) -> bool {
    let before = text_context_before(text, path_start, 120).to_ascii_lowercase();
    let after = text_context_after(text, path_start, 120).to_ascii_lowercase();
    let output_before = [
        "produce",
        "output",
        "output root",
        "output_root",
        "artifact root",
        "artifact_root",
        "artifacts",
        "write to",
        "save under",
        "出力",
        "出力先",
        "成果物",
        "保存",
        "配下",
    ]
    .iter()
    .any(|marker| before.contains(marker));
    let output_after = [
        "必須 artifact",
        "必須成果物",
        "成果物",
        "with ",
        "containing ",
        "に保存",
        "に置き",
        "へ置き",
        "配下",
    ]
    .iter()
    .any(|marker| after.contains(marker));
    let input_before = [
        "input",
        "inputs",
        "accepted input",
        "authoritative input",
        "review root",
        "入力",
        "入力は",
        "入力 root",
        "accepted evidence",
        "lead-verified",
    ]
    .iter()
    .any(|marker| before.contains(marker));

    output_before && (!input_before || output_after)
}

fn text_context_before(text: &str, byte_idx: usize, max_chars: usize) -> String {
    text[..byte_idx.min(text.len())]
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn text_context_after(text: &str, byte_idx: usize, max_chars: usize) -> String {
    text[byte_idx.min(text.len())..]
        .chars()
        .take(max_chars)
        .collect()
}

fn clean_embedded_path_token(token: &str) -> Option<&str> {
    embedded_absolute_path_slice(token, &["/home/", "/tmp/"])
}

fn extract_absolute_paths_with_offsets_from_text(text: &str) -> Vec<(String, usize)> {
    text.split_whitespace()
        .scan(0usize, |cursor, token| {
            let rel = text[*cursor..].find(token).unwrap_or(0);
            let start = *cursor + rel;
            *cursor = start + token.len();
            Some((start, token))
        })
        .filter_map(|(token_start, token)| {
            let path = clean_embedded_absolute_path_token(token)?;
            let path_start = token_start + token.find(path.as_str()).unwrap_or(0);
            Some((path, path_start))
        })
        .collect()
}

fn clean_embedded_absolute_path_token(token: &str) -> Option<String> {
    let roots = [
        "/home/",
        "/tmp/",
        "/workspace/",
        "/data/",
        "/data2/",
        "/mnt/",
        "/opt/",
        "/root/",
    ];
    embedded_absolute_path_slice(token, &roots).map(str::to_string)
}

fn embedded_absolute_path_slice<'a>(token: &'a str, roots: &[&str]) -> Option<&'a str> {
    let trimmed = token.trim_matches(path_wrapper_or_trailing_punctuation);
    let start = roots.iter().filter_map(|root| trimmed.find(root)).min()?;
    let candidate = &trimmed[start..];
    let end = candidate
        .char_indices()
        .find_map(|(idx, ch)| (idx > 0 && path_token_terminator(ch)).then_some(idx))
        .unwrap_or(candidate.len());
    let path = candidate[..end].trim_matches(path_wrapper_or_trailing_punctuation);
    if roots.iter().any(|root| path.starts_with(root)) {
        Some(path)
    } else {
        None
    }
}

fn path_wrapper_or_trailing_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '`' | '\''
            | '"'
            | ','
            | '.'
            | ';'
            | ':'
            | ')'
            | '('
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '。'
            | '、'
            | '，'
            | '；'
            | '：'
            | '）'
            | '（'
            | '」'
            | '「'
            | '』'
            | '『'
    )
}

fn path_token_terminator(ch: char) -> bool {
    matches!(
        ch,
        '`' | '\''
            | '"'
            | ','
            | ';'
            | ':'
            | ')'
            | '('
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '。'
            | '、'
            | '，'
            | '；'
            | '：'
            | '）'
            | '（'
            | '」'
            | '「'
            | '』'
            | '『'
    )
}

fn notify_rejected_task_completions(
    team_dir: &Path,
    rejected: &[(String, Option<String>, String)],
) -> Result<()> {
    if rejected.is_empty() {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    for (task_id, owner, issue) in rejected {
        let owner_label = owner
            .as_deref()
            .map(|owner| format!("@{owner}"))
            .unwrap_or_else(|| "unassigned owner".to_string());
        let message = format!(
            "Task completion rejected: task {task_id} reported `completed`, but required local output artifacts are not complete. Owner: {owner_label}. Issue: {issue}. The task was kept blocked so downstream dependencies do not start from a missing handoff. Publish the formal package/checklist/manifest or explain the blocker."
        );
        send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
        if let Some(owner) = owner
            && config.members.iter().any(|member| member.name == *owner)
        {
            send_team_message_to_dir(team_dir, "system", owner, &message)?;
        }
        append_event(
            team_dir,
            "task_completion_rejected_missing_artifacts",
            serde_json::json!({
                "task": task_id,
                "owner": owner,
                "issue": issue,
            }),
        )?;
    }
    Ok(())
}

fn notify_completed_task_freezes(team_dir: &Path, completed: &[TeamTask]) -> Result<()> {
    if completed.is_empty() {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    for task in completed {
        let owner_label = task
            .owner
            .as_deref()
            .map(|owner| format!("@{owner}"))
            .unwrap_or_else(|| "unassigned owner".to_string());
        let subject = task.subject.trim();
        let subject_suffix = if subject.is_empty() {
            String::new()
        } else {
            format!(" `{subject}`")
        };
        let owner_message = format!(
            "TASK_COMPLETION_FREEZE: task {}{} is now completed. Treat declared artifacts, manifests, checklists, and handoff paths as frozen for downstream consumers. Do not mutate completed task artifacts or manifests unless lead explicitly reopens this task. If you discover a correction, send lead a `LEAD_PROPOSAL:` or blocker first with exact paths, old/new hashes, and why the task must be reopened. Preserve stale or superseded handoffs only as failed-attempt provenance, not final evidence.",
            task.id, subject_suffix
        );
        let lead_message = format!(
            "TASK_COMPLETION_FREEZE: task {}{} completed by {owner_label}. Downstream consumers may now rely on the declared handoff. If the owner reports a correction, reopen the task before resyncing, reclearing, or allowing downstream execution; do not allow silent post-completion mutation. Stale handoffs should be preserved only as failed-attempt provenance.",
            task.id, subject_suffix
        );
        send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
        if let Some(owner) = task.owner.as_deref()
            && owner != config.lead
            && config.members.iter().any(|member| member.name == owner)
        {
            send_team_message_to_dir(team_dir, "system", owner, &owner_message)?;
        }
        append_event(
            team_dir,
            "task_completion_freeze_notified",
            serde_json::json!({
                "task": task.id,
                "owner": task.owner,
            }),
        )?;
    }
    Ok(())
}

fn block_member_tasks_if_active(team_dir: &Path, member_name: &str, reason: &str) -> Result<()> {
    let mut changed = false;
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Review
            )
        {
            task.status = TaskStatus::Blocked;
            task.updated_at = now();
            task.result = Some(reason.to_string());
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        auto_promote_dependency_waits(team_dir)?;
    }
    Ok(())
}

