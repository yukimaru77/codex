fn create_task(team_dir: &Path, args: TaskAddArgs) -> Result<TeamTask> {
    let id = allocate_task_id(team_dir)?;
    let created_at = now();
    let depends_on = normalize_task_dependencies(args.depends_on, Some(&id))?;
    validate_task_dependencies_exist(team_dir, &depends_on)?;
    let task = TeamTask {
        id: id.clone(),
        subject: args.subject,
        description: args.description,
        owner: args.owner,
        status: if depends_on.is_empty() {
            TaskStatus::Pending
        } else {
            TaskStatus::Waiting
        },
        depends_on,
        result: None,
        created_at: created_at.clone(),
        updated_at: created_at,
    };
    write_json_atomic(&task_path(team_dir, &id), &task)?;
    Ok(task)
}

fn create_or_reuse_similar_open_task(
    team_dir: &Path,
    args: TaskAddArgs,
) -> Result<(TeamTask, bool)> {
    let candidate_depends_on = normalize_task_dependencies(args.depends_on.clone(), None)?;
    if let Some(task) = find_similar_open_task_for_add(team_dir, &args, &candidate_depends_on)? {
        return Ok((task, true));
    }
    Ok((create_task(team_dir, args)?, false))
}

fn find_similar_open_task_for_add(
    team_dir: &Path,
    args: &TaskAddArgs,
    candidate_depends_on: &[String],
) -> Result<Option<TeamTask>> {
    let candidate_text = format!("{} {}", args.subject, args.description);
    let mut best = None::<(TeamTask, f32)>;
    for task in load_tasks(team_dir)? {
        if matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
        ) {
            continue;
        }
        if task.owner != args.owner {
            continue;
        }
        if task.depends_on != candidate_depends_on {
            continue;
        }
        let existing_text = format!("{} {}", task.subject, task.description);
        let similarity = task_text_containment_similarity(&existing_text, &candidate_text);
        if similarity < 0.75 {
            continue;
        }
        if best.as_ref().is_none_or(|(_, score)| similarity > *score) {
            best = Some((task, similarity));
        }
    }
    Ok(best.map(|(task, _)| task))
}

fn task_text_containment_similarity(left: &str, right: &str) -> f32 {
    let left_tokens = task_similarity_tokens(left);
    let right_tokens = task_similarity_tokens(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let common = left_tokens.intersection(&right_tokens).count();
    common as f32 / left_tokens.len().min(right_tokens.len()) as f32
}

fn task_similarity_tokens(text: &str) -> HashSet<String> {
    const STOPWORDS: &[&str] = &[
        "a", "an", "and", "as", "for", "in", "of", "on", "or", "the", "to", "with", "を", "と",
        "の", "に", "は",
    ];
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter_map(|token| {
            let token = token.trim().to_ascii_lowercase();
            if token.len() < 3 || STOPWORDS.contains(&token.as_str()) {
                None
            } else {
                Some(token)
            }
        })
        .collect()
}

fn create_or_reuse_resume_task(
    team_dir: &Path,
    member: &str,
    mission: &str,
) -> Result<(TeamTask, bool)> {
    if let Some(task) = reuse_task_referenced_by_resume_mission(team_dir, member, mission)? {
        return Ok((task, true));
    }
    let subject = format!(
        "Department mission for {member}: {mission}\n\nOperate as one department-level Codex session."
    );
    if let Some(task) = reuse_resume_task(team_dir, member, &subject, mission)? {
        return Ok((task, true));
    }
    let task = create_task(
        team_dir,
        TaskAddArgs {
            subject,
            description: String::new(),
            owner: Some(member.to_string()),
            depends_on: Vec::new(),
        },
    )?;
    Ok((task, false))
}

fn reuse_task_referenced_by_resume_mission(
    team_dir: &Path,
    member: &str,
    mission: &str,
) -> Result<Option<TeamTask>> {
    let referenced_ids = task_ids_referenced_in_text(mission);
    if referenced_ids.is_empty() {
        return Ok(None);
    }

    let mut tasks = load_tasks(team_dir)?;
    for referenced_id in referenced_ids {
        let Some(task) = tasks.iter_mut().find(|task| task.id == referenced_id) else {
            continue;
        };
        if task.owner.as_deref() != Some(member) {
            bail!(
                "resume mission references task {}, but it is owned by {}; reassign or update that task explicitly instead of creating a duplicate mission task",
                task.id,
                task.owner.as_deref().unwrap_or("no owner")
            );
        }
        if matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
        ) {
            bail!(
                "resume mission references task {} which is already {}; reopen it explicitly with `team task set {} --status in_progress --result \"reopened by lead: ...\"` before resuming, or create a clearly different task",
                task.id,
                task.status,
                task.id
            );
        }
        if matches!(
            task.status,
            TaskStatus::Pending | TaskStatus::Blocked | TaskStatus::Review | TaskStatus::Ready
        ) {
            task.status = TaskStatus::InProgress;
        }
        task.result = Some(append_result_note(
            task.result.as_deref(),
            &format!(
                "Resumed referenced task without creating a duplicate mission task. Resume mission: {mission}"
            ),
        ));
        task.updated_at = now();
        let reused = task.clone();
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "task_reused_for_resume",
            serde_json::json!({
                "task": reused,
                "member": member,
                "mission": mission,
                "source": "referenced_task",
            }),
        )?;
        return Ok(Some(reused));
    }

    Ok(None)
}

fn task_ids_referenced_in_text(text: &str) -> Vec<String> {
    let tokens = text
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let candidate = if token == "task" {
            tokens
                .get(index + 1)
                .filter(|next| next.chars().all(|ch| ch.is_ascii_digit()))
                .cloned()
        } else {
            token
                .strip_prefix("task")
                .filter(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
                .map(str::to_string)
        };
        if let Some(id) = candidate
            && seen.insert(id.clone())
        {
            ids.push(id);
        }
    }
    ids
}

fn reuse_resume_task(
    team_dir: &Path,
    member: &str,
    subject: &str,
    mission: &str,
) -> Result<Option<TeamTask>> {
    let mut tasks = load_tasks(team_dir)?;
    let open_owned = tasks
        .iter()
        .filter(|task| {
            task.owner.as_deref() == Some(member)
                && !matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
                )
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    if open_owned.is_empty() {
        return Ok(None);
    }

    let normalized_subject = normalize_task_text(subject);
    let selected_id = open_owned
        .iter()
        .find(|id| {
            tasks
                .iter()
                .find(|task| task.id == **id)
                .is_some_and(|task| normalize_task_text(&task.subject) == normalized_subject)
        })
        .cloned()
        .or_else(|| {
            if open_owned.len() == 1 {
                open_owned.first().cloned()
            } else {
                tasks
                    .iter()
                    .filter(|task| open_owned.iter().any(|id| id == &task.id))
                    .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
                    .map(|task| task.id.clone())
            }
        });
    let Some(selected_id) = selected_id else {
        return Ok(None);
    };

    let now = now();
    let mut reused = None;
    for task in &mut tasks {
        if task.id == selected_id {
            if matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Blocked | TaskStatus::Review
            ) {
                task.status = TaskStatus::InProgress;
            }
            let note =
                format!("Resumed without creating a duplicate task. Resume mission: {mission}");
            task.result = Some(append_result_note(task.result.as_deref(), &note));
            task.updated_at = now.clone();
            reused = Some(task.clone());
            break;
        }
    }
    for task in &tasks {
        write_json_atomic(&task_path(team_dir, &task.id), task)?;
    }
    touch_config(team_dir)?;
    if let Some(task) = reused.as_ref() {
        append_event(
            team_dir,
            "task_reused_for_resume",
            serde_json::json!({
                "task": task,
                "member": member,
                "mission": mission,
            }),
        )?;
    }
    Ok(reused)
}

fn normalize_task_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn append_result_note(existing: Option<&str>, note: &str) -> String {
    match existing
        .map(str::trim)
        .filter(|existing| !existing.is_empty())
    {
        Some(existing) => format!("{existing}\n\n{note}"),
        None => note.to_string(),
    }
}

fn is_soft_dependency_wait(task: &TeamTask) -> bool {
    if task.depends_on.is_empty() {
        return false;
    }
    if task_has_manual_dependency_hold(task) {
        return false;
    }
    if matches!(task.status, TaskStatus::Waiting) {
        return true;
    }
    if !matches!(task.status, TaskStatus::Blocked) {
        return false;
    }
    task.result.as_deref().is_none_or(|result| {
        let trimmed = result.trim();
        trimmed.is_empty() || result_has_marker(trimmed, "DEPENDENCY_WAIT:")
    })
}

fn task_has_manual_dependency_hold(task: &TeamTask) -> bool {
    let Some(result) = task.result.as_deref() else {
        return false;
    };
    result_has_marker(result, "MANUAL_DEPENDENCY_HOLD:")
}

fn task_dependencies_completed(task: &TeamTask, tasks: &[TeamTask]) -> bool {
    task.depends_on.iter().all(|dependency| {
        tasks.iter().any(|candidate| {
            candidate.id == *dependency && matches!(candidate.status, TaskStatus::Completed)
        })
    })
}

fn open_waits_by_task(waits: &[TeamWait]) -> HashMap<String, Vec<String>> {
    let mut by_task: HashMap<String, Vec<String>> = HashMap::new();
    for wait in waits.iter().filter(|wait| wait.status.is_open()) {
        let Some(task_id) = wait.task_id.as_deref() else {
            continue;
        };
        by_task
            .entry(task_id.to_string())
            .or_default()
            .push(wait.id.clone());
    }
    by_task
}

fn task_has_positive_lead_clearance(task: &TeamTask) -> bool {
    let Some(result) = task.result.as_deref() else {
        return false;
    };
    result_has_marker(result, "LEAD_CLEARANCE:")
}

fn result_has_marker(result: &str, marker: &str) -> bool {
    result.lines().any(|line| {
        line.trim_start()
            .to_ascii_uppercase()
            .starts_with(marker)
    })
}

fn task_requires_contract_input_clearance(
    config: &TeamConfig,
    contract_inputs: &HashMap<String, Vec<ContractDeclaredInput>>,
    task: &TeamTask,
) -> bool {
    if task_has_positive_lead_clearance(task) {
        return false;
    }
    if !contract_inputs.contains_key(task.id.as_str()) {
        return false;
    }
    let Some(owner) = task.owner.as_deref() else {
        return false;
    };
    let Some(member) = config.members.iter().find(|member| member.name == owner) else {
        return false;
    };
    member_node_id(member) != "local"
}

fn task_is_ready(task: &TeamTask, tasks: &[TeamTask]) -> bool {
    matches!(task.status, TaskStatus::Pending | TaskStatus::Ready)
        && task_dependencies_completed(task, tasks)
}

fn auto_promote_dependency_waits(team_dir: &Path) -> Result<Vec<TeamTask>> {
    let mut config = load_config(team_dir)?;
    let mut tasks = load_tasks(team_dir)?;
    let waits = load_waits(team_dir)?;
    let open_waits_by_task = open_waits_by_task(&waits);
    let contract_inputs = load_contract_declared_inputs(&load_ownerships(team_dir)?)?;
    let snapshot = tasks.clone();
    let updated_at = now();
    let waiting_ids = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Ready))
        .filter(|task| !task.depends_on.is_empty())
        .filter(|task| !task_dependencies_completed(task, &snapshot))
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let ready_ids = tasks
        .iter()
        .filter(|task| is_soft_dependency_wait(task))
        .filter(|task| task_dependencies_completed(task, &snapshot))
        .filter(|task| !open_waits_by_task.contains_key(task.id.as_str()))
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let open_wait_hold_ids = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Waiting | TaskStatus::Blocked
            )
        })
        .filter(|task| task_dependencies_completed(task, &snapshot))
        .filter(|task| open_waits_by_task.contains_key(task.id.as_str()))
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let contract_clearance_hold_ids = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Waiting | TaskStatus::Blocked
            )
        })
        .filter(|task| task_dependencies_completed(task, &snapshot))
        .filter(|task| task_requires_contract_input_clearance(&config, &contract_inputs, task))
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut promoted = Vec::new();
    let mut held_for_contract_clearance = Vec::new();
    let mut held_for_open_waits = Vec::new();
    let mut reactivated_members = Vec::new();
    let mut reactivated_tasks = Vec::new();
    let mut tasks_changed = false;
    let mut config_changed = false;
    for task in &mut tasks {
        if waiting_ids.contains(&task.id) {
            task.status = TaskStatus::Waiting;
            task.updated_at = updated_at.clone();
            task.result = Some(append_result_note(
                task.result.as_deref(),
                "Soft-waiting for dependency task(s).",
            ));
            tasks_changed = true;
        }
        if open_wait_hold_ids.contains(&task.id) {
            let wait_ids = open_waits_by_task
                .get(task.id.as_str())
                .map(|ids| ids.join(","))
                .unwrap_or_default();
            let note = format!(
                "Dependency gate may be clear, but task has open wait item(s): {wait_ids}. Do not READY_TO_START until waits close."
            );
            let already_noted = task
                .result
                .as_deref()
                .is_some_and(|result| result.contains("task has open wait item(s)"));
            let old_status = task.status;
            if matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Waiting
            ) {
                task.status = TaskStatus::Waiting;
            }
            if task.status != old_status || !already_noted {
                task.updated_at = updated_at.clone();
                if !already_noted {
                    task.result = Some(append_result_note(task.result.as_deref(), &note));
                }
                held_for_open_waits.push(task.clone());
                tasks_changed = true;
            }
        } else if contract_clearance_hold_ids.contains(&task.id) {
            let note = "Dependency gate cleared, but this non-local task has contract-declared inputs. Await explicit lead root-correct verification clearance before READY_TO_START.";
            let already_noted = task
                .result
                .as_deref()
                .is_some_and(|result| result.contains(note));
            let already_waiting = matches!(task.status, TaskStatus::Waiting);
            if !already_waiting || !already_noted {
                task.status = TaskStatus::Waiting;
                task.updated_at = updated_at.clone();
                if !already_noted {
                    task.result = Some(append_result_note(task.result.as_deref(), note));
                }
                held_for_contract_clearance.push(task.clone());
                tasks_changed = true;
            }
        } else if ready_ids.contains(&task.id) {
            task.status = TaskStatus::Ready;
            task.updated_at = updated_at.clone();
            task.result = Some(append_result_note(
                task.result.as_deref(),
                "Dependency gate cleared automatically; task is ready.",
            ));
            promoted.push(task.clone());
            tasks_changed = true;
        }
        if matches!(task.status, TaskStatus::Ready | TaskStatus::Pending)
            && let Some(owner) = task.owner.as_deref()
            && let Some(member) = config
                .members
                .iter_mut()
                .find(|member| member.name == owner)
            && matches!(
                member.status,
                MemberStatus::Standby | MemberStatus::Completed
            )
        {
            member.status = MemberStatus::Online;
            config.updated_at = updated_at.clone();
            config_changed = true;
            reactivated_members.push(owner.to_string());
            reactivated_tasks.push(task.clone());
        }
    }
    if !tasks_changed && !config_changed {
        return Ok(Vec::new());
    }
    if tasks_changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
    }
    if config_changed {
        write_json_atomic(&team_dir.join("config.json"), &config)?;
    }
    touch_config(team_dir)?;
    if !waiting_ids.is_empty() || !reactivated_members.is_empty() {
        append_event(
            team_dir,
            "task_dependency_reconciled",
            serde_json::json!({
                "waiting_tasks": waiting_ids,
                "reactivated_members": reactivated_members,
            }),
        )?;
    }
    for task in &promoted {
        append_event(
            team_dir,
            "task_dependency_unblocked",
            serde_json::json!({ "task": task }),
        )?;
        send_ready_to_start_message(team_dir, task)?;
    }
    for task in &held_for_contract_clearance {
        append_event(
            team_dir,
            "task_contract_input_clearance_required",
            serde_json::json!({ "task": task }),
        )?;
        send_contract_input_clearance_required_message(team_dir, task)?;
    }
    for task in &held_for_open_waits {
        append_event(
            team_dir,
            "task_dependency_open_wait_hold",
            serde_json::json!({ "task": task }),
        )?;
        send_open_wait_hold_message(team_dir, task)?;
    }
    let promoted_ids = promoted.iter().map(|task| &task.id).collect::<HashSet<_>>();
    for task in &reactivated_tasks {
        if !promoted_ids.contains(&task.id)
            && !contract_clearance_hold_ids.contains(&task.id)
            && !open_wait_hold_ids.contains(&task.id)
        {
            send_ready_to_start_message(team_dir, task)?;
        }
    }
    Ok(promoted)
}

fn send_ready_to_start_message(team_dir: &Path, task: &TeamTask) -> Result<()> {
    let config = load_config(team_dir)?;
    let recipients = ready_task_recipients(&config, task);
    if recipients.is_empty() {
        return Ok(());
    }
    let deps = task.depends_on.join(",");
    let message = match task.owner.as_deref() {
        Some(owner) => format!(
            "READY_TO_START: task {} is ready for @{owner}; dependencies completed: {deps}.",
            task.id
        ),
        None => format!(
            "READY_TO_START: unassigned task {} is ready; dependencies completed: {deps}. Members may self-claim it with `team task claim {}` when it is within scope.",
            task.id, task.id
        ),
    };
    send_system_message_to_recipients(team_dir, &recipients, &message)
}

fn send_contract_input_clearance_required_message(team_dir: &Path, task: &TeamTask) -> Result<()> {
    let config = load_config(team_dir)?;
    let recipients = ready_task_recipients(&config, task);
    if recipients.is_empty() {
        return Ok(());
    }
    let owner = task.owner.as_deref().unwrap_or("unassigned");
    let deps = task.depends_on.join(",");
    let message = format!(
        "AWAITING_LEAD_CLEARANCE: task {} dependencies are complete for @{owner} ({deps}), but this non-local task has contract-declared inputs. Lead must sync/root-correct verify declared inputs, predecessor manifests, and guard/bootstrap requirements, then explicitly clear or resume the owner. Do not start from dependency completion alone.",
        task.id
    );
    send_system_message_to_recipients(team_dir, &recipients, &message)
}

fn send_open_wait_hold_message(team_dir: &Path, task: &TeamTask) -> Result<()> {
    let config = load_config(team_dir)?;
    let recipients = ready_task_recipients(&config, task);
    if recipients.is_empty() {
        return Ok(());
    }
    let owner = task.owner.as_deref().unwrap_or("unassigned");
    let message = format!(
        "WAIT_STILL_OPEN: task {} dependencies may be complete for @{owner}, but at least one linked wait is still open. Keep the task waiting/blocked until the wait reaches completed/failed/cancelled and the owner publishes the real handoff or blocker.",
        task.id
    );
    send_system_message_to_recipients(team_dir, &recipients, &message)
}

fn ready_task_recipients(config: &TeamConfig, task: &TeamTask) -> Vec<String> {
    let mut recipients = Vec::new();
    if let Some(owner) = task.owner.as_deref()
        && config.members.iter().any(|member| member.name == owner)
    {
        recipients.push(owner.to_string());
    }
    if recipients.is_empty() {
        recipients.extend(
            config
                .members
                .iter()
                .filter(|member| member.name != config.lead)
                .map(|member| member.name.clone()),
        );
    }
    if config
        .members
        .iter()
        .any(|member| member.name == config.lead)
        && !recipients.iter().any(|recipient| recipient == &config.lead)
    {
        recipients.push(config.lead.clone());
    }
    recipients
}

fn send_system_message_to_recipients(
    team_dir: &Path,
    recipients: &[String],
    message: &str,
) -> Result<()> {
    let mut seen = HashSet::new();
    let recipients = recipients
        .iter()
        .filter(|recipient| seen.insert((*recipient).clone()))
        .cloned()
        .collect::<Vec<_>>();
    for recipient in &recipients {
        let msg = MailMessage {
            from: "system".to_string(),
            to: recipient.clone(),
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
            "from": "system",
            "to": recipients,
            "message": message,
            "source": "task_ready",
        }),
    )?;
    Ok(())
}

fn claim_ready_task(team_dir: &Path, args: TaskClaimArgs) -> Result<()> {
    auto_promote_dependency_waits(team_dir)?;
    let config = load_config(team_dir)?;
    let owner = args.owner.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &owner)?;

    let mut tasks = load_tasks(team_dir)?;
    let snapshot = tasks.clone();
    let selected_id = match args.id {
        Some(id) => id,
        None => tasks
            .iter()
            .find(|task| task.owner.is_none() && task_is_ready(task, &snapshot))
            .map(|task| task.id.clone())
            .ok_or_else(|| anyhow!("no unassigned ready tasks found"))?,
    };

    let updated_at = now();
    let mut claimed = None;
    for task in &mut tasks {
        if task.id == selected_id {
            if !task_is_ready(task, &snapshot) {
                bail!("task {} is not ready to claim", task.id);
            }
            if let Some(existing_owner) = task.owner.as_deref()
                && existing_owner != owner
            {
                bail!("task {} is already owned by @{existing_owner}", task.id);
            }
            task.owner = Some(owner.clone());
            task.status = TaskStatus::InProgress;
            task.updated_at = updated_at.clone();
            claimed = Some(task.clone());
            break;
        }
    }
    let Some(claimed) = claimed else {
        bail!("task {selected_id} not found");
    };
    for task in &tasks {
        write_json_atomic(&task_path(team_dir, &task.id), task)?;
    }
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "task_claimed",
        serde_json::json!({ "task": claimed, "owner": owner }),
    )?;
    let message = format!(
        "READY_TO_START: @{owner} claimed task {} and moved it to in_progress.",
        claimed.id
    );
    send_team_message_to_dir(team_dir, &owner, &config.lead, &message)?;
    println!("Claimed task {}", claimed.id);
    Ok(())
}

fn update_task(team_dir: &Path, args: TaskSetArgs) -> Result<()> {
    let path = task_path(team_dir, &args.id);
    let mut task: TeamTask = read_json(&path)?;
    let deps_changed = args.clear_depends || !args.depends_on.is_empty();
    if args.clear_owner {
        task.owner = None;
    }
    if let Some(owner) = args.owner {
        task.owner = Some(owner);
    }
    if deps_changed {
        let deps = if args.clear_depends {
            Vec::new()
        } else {
            args.depends_on
        };
        let deps = normalize_task_dependencies(deps, Some(&task.id))?;
        validate_task_dependencies_exist(team_dir, &deps)?;
        let tasks = load_tasks(team_dir)?;
        task.depends_on = deps;
        if !task.depends_on.is_empty()
            && !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
            && !task_dependencies_completed(&task, &tasks)
        {
            task.status = TaskStatus::Waiting;
            task.result = Some(format!(
                "Waiting for dependency task(s): {}.",
                task.depends_on.join(",")
            ));
        }
    }
    let requested_status = args.status;
    if let Some(status) = requested_status {
        task.status = status;
    }
    if let Some(result) = args.result {
        task.result = Some(result);
    }
    if requested_status == Some(TaskStatus::Completed)
        && let Some(issue) = task_completion_blocker(team_dir, &task)?
    {
        task.status = TaskStatus::Blocked;
        task.result = Some(append_result_note(
            task.result.as_deref(),
            &format!("Completion rejected: {issue}"),
        ));
    }
    task.updated_at = now();
    write_json_atomic(&path, &task)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "task_updated",
        serde_json::json!({ "task": task }),
    )?;
    auto_promote_dependency_waits(team_dir)?;
    println!("Updated task {}", args.id);
    Ok(())
}

fn normalize_task_dependencies(
    dependencies: Vec<String>,
    task_id: Option<&str>,
) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for dependency in dependencies {
        let dependency = dependency.trim();
        if dependency.is_empty() {
            continue;
        }
        if task_id.is_some_and(|id| dependency == id) {
            bail!("task {dependency} cannot depend on itself");
        }
        if seen.insert(dependency.to_string()) {
            normalized.push(dependency.to_string());
        }
    }
    Ok(normalized)
}

fn validate_task_dependencies_exist(team_dir: &Path, dependencies: &[String]) -> Result<()> {
    if dependencies.is_empty() {
        return Ok(());
    }
    let tasks = load_tasks(team_dir)?;
    let known = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    if let Some(missing) = dependencies
        .iter()
        .find(|dependency| !known.contains(dependency.as_str()))
    {
        bail!("dependency task {missing} not found");
    }
    Ok(())
}

fn claim_ownership(team_dir: &Path, args: OwnershipClaimArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    let owner = args.owner.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &owner)?;
    let path = normalize_ownership_path(&args.path)?;
    let mut ownerships = load_ownerships(team_dir)?;
    let now = now();
    if let Some(existing) = ownerships.iter_mut().find(|entry| entry.path == path) {
        if existing.owner != owner && !args.force {
            bail!(
                "`{}` is already owned by `{}`; ask them or lead for handoff, or pass --force",
                existing.path,
                existing.owner
            );
        }
        let previous_owner = existing.owner.clone();
        existing.owner = owner.clone();
        existing.note = args.note;
        existing.updated_at = now.clone();
        write_ownerships(team_dir, &ownerships)?;
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "ownership_claimed",
            serde_json::json!({
                "path": path,
                "owner": owner,
                "previousOwner": previous_owner,
                "forced": args.force,
            }),
        )?;
        println!("Claimed {path} for {owner}");
        return Ok(());
    }

    ownerships.push(FileOwnership {
        path: path.clone(),
        owner: owner.clone(),
        note: args.note,
        updated_at: now,
    });
    ownerships.sort_by(|a, b| a.path.cmp(&b.path));
    write_ownerships(team_dir, &ownerships)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "ownership_claimed",
        serde_json::json!({ "path": path, "owner": owner, "forced": false }),
    )?;
    println!("Claimed {path} for {owner}");
    Ok(())
}

fn release_ownership(team_dir: &Path, args: OwnershipReleaseArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    let owner = args.owner.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &owner)?;
    let path = normalize_ownership_path(&args.path)?;
    let mut ownerships = load_ownerships(team_dir)?;
    let Some(index) = ownerships.iter().position(|entry| entry.path == path) else {
        bail!("`{path}` is not claimed");
    };
    let existing = &ownerships[index];
    if existing.owner != owner && owner != config.lead && !args.force {
        bail!(
            "`{}` is owned by `{}`; only that owner, lead, or --force can release it",
            existing.path,
            existing.owner
        );
    }
    let released = ownerships.remove(index);
    write_ownerships(team_dir, &ownerships)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "ownership_released",
        serde_json::json!({
            "path": released.path,
            "owner": released.owner,
            "releasedBy": owner,
            "forced": args.force,
        }),
    )?;
    println!("Released {} from {}", released.path, released.owner);
    Ok(())
}

fn add_team_member(team_dir: &Path, args: MemberAddArgs) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let now = now();
    let mut member = parse_member(&args.member, &now)?;
    if let Some(node) = args.node {
        member.node = Some(sanitize_id(&node));
    }
    if let Some(node) = member.node.as_deref()
        && node != "local"
    {
        ensure_node_exists(team_dir, node)?;
    }
    if config
        .members
        .iter()
        .any(|existing| existing.name == member.name)
    {
        bail!(
            "member `{}` already exists in team `{}`",
            member.name,
            config.id
        );
    }
    let mission = if args.mission.trim().is_empty() {
        format!(
            "Department mission for {}: support the team goal where this department's role is useful.\n\nOperate as one department-level Codex session. The user explicitly authorizes departments to use subagents, agent tools, parallel delegation, skills, MCP servers, and internal decomposition for substantial work. If the mission is broad, heavy, or nontrivial, default to using available helpers inside this department; if you do not, record why they were unnecessary.",
            member.name
        )
    } else {
        format!(
            "Department mission for {}: {}\n\nOperate as one department-level Codex session. The user explicitly authorizes departments to use subagents, agent tools, parallel delegation, skills, MCP servers, and internal decomposition for substantial work. If the mission is broad, heavy, or nontrivial, default to using available helpers inside this department; if you do not, record why they were unnecessary.",
            member.name, args.mission
        )
    };
    config.members.push(member.clone());
    config.updated_at = now.clone();
    write_json_atomic(&team_dir.join("config.json"), &config)?;
    let task = create_task(
        team_dir,
        TaskAddArgs {
            subject: mission,
            description: String::new(),
            owner: Some(member.name.clone()),
            depends_on: Vec::new(),
        },
    )?;
    append_event(
        team_dir,
        "member_added",
        serde_json::json!({
            "member": member,
            "task": task,
        }),
    )?;
    println!("Added member {}", task.owner.as_deref().unwrap_or(""));
    Ok(())
}

fn ensure_container_node_departments(team_dir: &Path) -> Result<()> {
    let nodes = load_nodes(team_dir)?;
    let config = load_config(team_dir)?;
    for node in nodes
        .iter()
        .filter(|node| matches!(node.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker))
    {
        if config
            .members
            .iter()
            .any(|member| member.node.as_deref() == Some(node.id.as_str()))
        {
            continue;
        }
        let member_name = unique_member_name(&config, &format!("{}-container", node.id));
        let host_text = node
            .host
            .as_deref()
            .map(|host| format!(" on SSH host `{host}`"))
            .unwrap_or_default();
        let container_text = node
            .container
            .as_deref()
            .unwrap_or("the registered container");
        let cwd_text = node.cwd.as_deref().unwrap_or("/workspace");
        let mission = format!(
            "Run as the container-internal department for node `{node}`{host_text}. You are expected to execute from inside Docker container `{container}` at `{cwd}` through the node app-server, not merely from the host. Take over the main runtime work that this container was created for: install missing tools inside the container, run the sample/application/model/experiment, render or test outputs, debug container-local failures, and produce container-local verification evidence. At the start of your turn, create a concrete runtime workspace such as `{cwd}/runtime_container` and immediately write an initial status/progress artifact there, even before the heavy work finishes. Verify mounts, ports, GPUs, package/tool availability, and run container-local smoke checks before heavy work. Any material command whose exit status matters must leave a command transcript with exact command, cwd, container identity, timestamps when practical, and `rc=`/`exit=`; any long or asynchronous work must be tracked with `team job` or `team wait` instead of being hidden inside an untracked shell or only described in chat. Do not include a live transcript, manifest check log, handoff log, progress file, or helper/finalizer script in a final manifest if you will append to or patch it afterward; either close it permanently before hashing or exclude it and hash a stable final copy. If repairing the manifest changes a script/report/log that is listed in the manifest, regenerate and recheck the manifest again after that file is stable. Immediately before final handoff, rerun manifest verification and report the fresh rc and current manifest/log hashes from disk. Coordinate with the host/SSH department only for image rebuilds, container replacement, mount/port/GPU fixes, or host-side resource issues. Report results and blockers to lead and other departments, and stay available for follow-up container debugging.",
            node = node.id,
            container = container_text,
            cwd = cwd_text,
        );
        add_team_member(
            team_dir,
            MemberAddArgs {
                member: format!("{member_name}:container"),
                node: Some(node.id.clone()),
                mission,
            },
        )?;
        append_event(
            team_dir,
            "container_department_auto_added",
            serde_json::json!({
                "node": node.id,
                "member": member_name,
                "kind": node.kind,
            }),
        )?;
    }
    Ok(())
}

fn unique_member_name(config: &TeamConfig, base: &str) -> String {
    let base = sanitize_id(base);
    if !config
        .members
        .iter()
        .any(|member| member.name == base.as_str())
    {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{base}-{index}");
        if !config.members.iter().any(|member| member.name == candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn standby_team_member(team_dir: &Path, args: MemberStandbyArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    if args.member == config.lead {
        bail!("lead cannot be moved to standby");
    }
    ensure_member_exists(&config, &args.member)?;
    set_member_status(team_dir, &args.member, MemberStatus::Standby)?;
    append_event(
        team_dir,
        "member_standby",
        serde_json::json!({
            "member": args.member,
            "reason": args.reason,
        }),
    )?;
    println!("Moved {} to standby", args.member);
    Ok(())
}

fn resume_team_member(team_dir: &Path, args: MemberResumeArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    ensure_member_exists(&config, &args.member)?;
    set_member_status(team_dir, &args.member, MemberStatus::Online)?;
    let (task, reused_task) = if let Some(mission) = args.mission {
        let (task, reused) = create_or_reuse_resume_task(team_dir, &args.member, &mission)?;
        (Some(task), reused)
    } else {
        (None, false)
    };
    append_event(
        team_dir,
        "member_resumed",
        serde_json::json!({
            "member": args.member,
            "task": task,
            "reused_task": reused_task,
        }),
    )?;
    println!("Resumed {}", args.member);
    Ok(())
}

fn run_node(root: &Path, cli: NodeCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        NodeSubcommand::List => list_team_nodes(&team_dir),
        NodeSubcommand::Inspect(args) => inspect_team_nodes(&team_dir, args),
        NodeSubcommand::CreateDocker(args) => create_docker_node(&team_dir, args),
        NodeSubcommand::SyncAssets(args) => sync_node_assets(&team_dir, args),
        NodeSubcommand::SyncPath(args) => sync_node_path(&team_dir, args),
        NodeSubcommand::PullPath(args) => pull_node_path(&team_dir, args),
        NodeSubcommand::Add(args) => add_team_node(&team_dir, args),
        NodeSubcommand::Remove(args) => remove_team_node(&team_dir, args),
    }
}

fn list_team_nodes(team_dir: &Path) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    for node in nodes {
        println!(
            "{}  {:?}  {:?}  url={}  host={}  container={}  cwd={}  {}",
            node.id,
            node.kind,
            node.status,
            node.url.unwrap_or_default(),
            node.host.unwrap_or_default(),
            node.container.unwrap_or_default(),
            node.cwd.unwrap_or_default(),
            node.note
        );
    }
    Ok(())
}

fn inspect_team_nodes(team_dir: &Path, args: NodeInspectArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let selected = match args.id.as_deref() {
        Some(id) => {
            let id = sanitize_id(id);
            nodes
                .into_iter()
                .filter(|node| node.id == id)
                .collect::<Vec<_>>()
        }
        None => nodes,
    };
    if selected.is_empty() {
        bail!("node not found");
    }
    for node in selected {
        if !args.raw {
            println!("== {} ({:?}) ==", node.id, node.kind);
        }
        let facts = collect_node_facts(&node)?;
        append_event(
            team_dir,
            "node_inspect_succeeded",
            serde_json::json!({
                "node": node.id,
                "kind": node.kind,
            }),
        )?;
        println!("{}", facts.trim_end());
        if matches!(node.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker)
            && let Some(container) = node.container.as_deref()
        {
            let ports = docker_inspect_value(
                node.host.as_deref(),
                container,
                "{{json .NetworkSettings.Ports}}",
            )
            .unwrap_or_default();
            let mounts = docker_inspect_value(node.host.as_deref(), container, "{{json .Mounts}}")
                .unwrap_or_default();
            println!("docker_ports_json={ports}");
            println!("docker_mounts_json={mounts}");
        }
        if !args.raw {
            println!();
        }
    }
    Ok(())
}

fn create_docker_node(team_dir: &Path, args: NodeCreateDockerArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    let id = sanitize_id(&args.id);
    if id.is_empty() {
        bail!("invalid node id `{}`", args.id);
    }
    let container = args.container.clone().unwrap_or_else(|| {
        format!(
            "codex-team-{}-{}",
            sanitize_id(&config.id),
            sanitize_id(&id)
        )
    });
    let mut mounts = args.mounts.clone();
    if mounts.is_empty() {
        let host_path = if args.host.is_some() {
            format!("/tmp/codex-team-workspaces/{}/{}", config.id, id)
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .display()
                .to_string()
        };
        mounts.push(format!("{host_path}:{}", args.cwd));
    }

    let docker_args = docker_run_args(&args, &container, &mounts);
    let command_text = if let Some(host) = args.host.as_deref() {
        let remote_mount_dirs = mounts
            .iter()
            .filter_map(|mount| mount.split_once(':').map(|(host_path, _)| host_path))
            .filter(|host_path| !host_path.starts_with('/') || !host_path.contains('*'))
            .map(shell_quote)
            .collect::<Vec<_>>();
        let mkdir = if remote_mount_dirs.is_empty() {
            String::new()
        } else {
            format!("mkdir -p {} && ", remote_mount_dirs.join(" "))
        };
        let replace = if args.replace {
            format!(
                "docker rm -f {} >/dev/null 2>&1 || true && ",
                shell_quote(&container)
            )
        } else {
            String::new()
        };
        let remote = format!("{mkdir}{replace}docker {}", docker_args.join(" "));
        run_ssh_command(host, &remote)?
    } else {
        let replace = if args.replace {
            format!(
                "docker rm -f {} >/dev/null 2>&1 || true && ",
                shell_quote(&container)
            )
        } else {
            String::new()
        };
        run_shell_capture(
            &format!("{replace}docker {}", docker_args.join(" ")),
            "run docker container",
        )?
    };

    let kind = if args.host.is_some() {
        TeamNodeKind::SshDocker
    } else {
        TeamNodeKind::Docker
    };
    add_team_node(
        team_dir,
        NodeAddArgs {
            id: id.clone(),
            kind,
            url: None,
            host: args.host,
            container: Some(container.clone()),
            cwd: Some(args.cwd),
            note: args.note,
        },
    )?;
    ensure_container_node_departments(team_dir)?;
    append_event(
        team_dir,
        "docker_node_created",
        serde_json::json!({
            "node": id,
            "container": container,
            "output": command_text,
            "mounts": mounts,
        }),
    )?;
    Ok(())
}

fn docker_run_args(args: &NodeCreateDockerArgs, container: &str, mounts: &[String]) -> Vec<String> {
    let mut docker_args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        shell_quote(container),
    ];
    if args.gpus {
        docker_args.push("--gpus".to_string());
        docker_args.push("all".to_string());
    }
    for mount in mounts {
        docker_args.push("-v".to_string());
        docker_args.push(shell_quote(mount));
    }
    docker_args.push("-w".to_string());
    docker_args.push(shell_quote(&args.cwd));
    for port in &args.ports {
        docker_args.push("-p".to_string());
        docker_args.push(shell_quote(port));
    }
    for env in &args.env {
        docker_args.push("-e".to_string());
        docker_args.push(shell_quote(env));
    }
    docker_args.push(shell_quote(&args.image));
    docker_args.push("bash".to_string());
    docker_args.push("-lc".to_string());
    docker_args.push(shell_quote(&args.command));
    docker_args
}

fn sync_node_assets(team_dir: &Path, args: NodeSyncAssetsArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node = nodes
        .into_iter()
        .find(|node| node.id == sanitize_id(&args.id))
        .with_context(|| format!("node `{}` not found", args.id))?;
    let include_auth = args.include_auth || !args.no_auth;
    let (command, existing) = build_asset_sync_command(&node, &args.dest, include_auth)?;
    if args.dry_run {
        println!("{command}");
        return Ok(());
    }
    run_shell_command(&command, "sync Codex assets")?;
    append_event(
        team_dir,
        "node_assets_synced",
        serde_json::json!({
            "node": node.id,
            "dest": args.dest,
            "include_auth": include_auth,
            "paths": existing,
        }),
    )?;
    println!("Synced Codex assets to node {}", node.id);
    Ok(())
}

fn sync_node_path(team_dir: &Path, args: NodeSyncPathArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node = nodes
        .into_iter()
        .find(|node| node.id == sanitize_id(&args.id))
        .with_context(|| format!("node `{}` not found", args.id))?;
    let src = args
        .src
        .canonicalize()
        .with_context(|| format!("source path `{}` not found", args.src.display()))?;
    let (command, src_kind) = build_path_sync_command(&node, &src, &args.dest, args.replace)?;
    if args.dry_run {
        println!("{command}");
        return Ok(());
    }
    run_shell_command(&command, "sync team artifact path")?;
    append_event(
        team_dir,
        "node_path_synced",
        serde_json::json!({
            "node": node.id,
            "src": src,
            "dest": args.dest,
            "kind": src_kind,
            "replace": args.replace,
        }),
    )?;
    println!(
        "Synced {} to node {}:{}",
        args.src.display(),
        node.id,
        args.dest
    );
    Ok(())
}

fn pull_node_path(team_dir: &Path, args: NodePullPathArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node = nodes
        .into_iter()
        .find(|node| node.id == sanitize_id(&args.id))
        .with_context(|| format!("node `{}` not found", args.id))?;
    let dest = normalize_local_pull_dest(&args.dest)?;
    let (command, src_name) = build_path_pull_command(&node, &args.src, &dest, args.replace)?;
    if args.dry_run {
        println!("{command}");
        return Ok(());
    }
    run_shell_command(&command, "pull team artifact path")?;
    append_event(
        team_dir,
        "node_path_pulled",
        serde_json::json!({
            "node": node.id,
            "src": args.src,
            "dest": dest,
            "name": src_name,
            "replace": args.replace,
        }),
    )?;
    println!("Pulled node {}:{} to {}", node.id, args.src, dest.display());
    Ok(())
}

fn normalize_local_pull_dest(dest: &Path) -> Result<PathBuf> {
    if dest.as_os_str().is_empty() {
        bail!("destination path must not be empty");
    }
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create destination parent `{}`", parent.display()))?;
    }
    Ok(dest.to_path_buf())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContractDeclaredInput {
    label: String,
    src: PathBuf,
    dest: String,
    contract_path: PathBuf,
}

fn maybe_sync_contract_declared_inputs(
    team_dir: &Path,
    config: &TeamConfig,
    nodes: &[TeamNode],
    attempts: &mut HashSet<String>,
) -> Result<()> {
    let tasks = load_tasks(team_dir)?;
    let ownerships = load_ownerships(team_dir)?;
    let node_by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let member_by_name = config
        .members
        .iter()
        .map(|member| (member.name.as_str(), member))
        .collect::<HashMap<_, _>>();
    let contract_inputs = load_contract_declared_inputs(&ownerships)?;
    if contract_inputs.is_empty() {
        return Ok(());
    }

    for task in tasks.iter().filter(|task| {
        matches!(
            task.status,
            TaskStatus::Ready | TaskStatus::InProgress | TaskStatus::Waiting | TaskStatus::Blocked
        ) && task_dependencies_completed(task, &tasks)
    }) {
        let Some(owner) = task.owner.as_deref() else {
            continue;
        };
        let Some(member) = member_by_name.get(owner) else {
            continue;
        };
        let node_id = member_node_id(member);
        if node_id == "local" {
            continue;
        }
        let Some(node) = node_by_id.get(node_id.as_str()) else {
            continue;
        };
        if matches!(node.kind, TeamNodeKind::Manual) {
            continue;
        }

        let Some(inputs) = contract_inputs.get(task.id.as_str()) else {
            continue;
        };
        for input in inputs {
            let key = format!(
                "{}:{}:{}:{}",
                task.id,
                node.id,
                input.src.display(),
                input.dest
            );
            if !attempts.insert(key.clone()) {
                continue;
            }
            if !input.src.exists() {
                let message = format!(
                    "Contract-declared input sync warning for task {}: `{}` in `{}` points to missing local source `{}` for node `{}` destination `{}`. Keep @{owner} blocked or waiting until lead fixes the authoritative source path or produces the missing artifact.",
                    task.id,
                    input.label,
                    input.contract_path.display(),
                    input.src.display(),
                    node.id,
                    input.dest
                );
                send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
                send_team_message_to_dir(team_dir, "system", owner, &message)?;
                append_event(
                    team_dir,
                    "contract_declared_input_sync_missing_source",
                    serde_json::json!({
                        "task": task.id,
                        "owner": owner,
                        "node": node.id,
                        "label": input.label,
                        "contract": input.contract_path,
                        "src": input.src,
                        "dest": input.dest,
                    }),
                )?;
                continue;
            }

            let src = input
                .src
                .canonicalize()
                .with_context(|| format!("canonicalize {}", input.src.display()))?;
            let (command, src_kind) = build_path_sync_command(node, &src, &input.dest, true)?;
            match run_shell_command(&command, "sync contract-declared team input") {
                Ok(()) => {
                    let message = format!(
                        "Contract-declared input auto-sync for task {}: synced `{}` from `{}` to node `{}` destination `{}` using `{}`. This was declared by `{}`; @{owner} should root-correct verify the manifest before relying on it.",
                        task.id,
                        input.label,
                        src.display(),
                        node.id,
                        input.dest,
                        src_kind,
                        input.contract_path.display()
                    );
                    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
                    send_team_message_to_dir(team_dir, "system", owner, &message)?;
                    append_event(
                        team_dir,
                        "contract_declared_input_synced",
                        serde_json::json!({
                            "task": task.id,
                            "owner": owner,
                            "node": node.id,
                            "label": input.label,
                            "contract": input.contract_path,
                            "src": src,
                            "dest": input.dest,
                            "kind": src_kind,
                            "replace": true,
                        }),
                    )?;
                }
                Err(err) => {
                    let message = format!(
                        "Contract-declared input sync failed for task {}: `{}` from `{}` to node `{}` destination `{}` failed: {err:#}. Keep @{owner} blocked or waiting until lead/ops repairs the sync and verifies the manifest.",
                        task.id,
                        input.label,
                        src.display(),
                        node.id,
                        input.dest
                    );
                    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
                    send_team_message_to_dir(team_dir, "system", owner, &message)?;
                    append_event(
                        team_dir,
                        "contract_declared_input_sync_failed",
                        serde_json::json!({
                            "task": task.id,
                            "owner": owner,
                            "node": node.id,
                            "label": input.label,
                            "contract": input.contract_path,
                            "src": src,
                            "dest": input.dest,
                            "error": format!("{err:#}"),
                        }),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn load_contract_declared_inputs(
    ownerships: &[FileOwnership],
) -> Result<HashMap<String, Vec<ContractDeclaredInput>>> {
    let mut by_task = HashMap::<String, Vec<ContractDeclaredInput>>::new();
    let mut seen_contracts = HashSet::<PathBuf>::new();
    for ownership in ownerships {
        let base = Path::new(&ownership.path);
        let contract_path = if base.is_dir() {
            base.join("runtime_contract.yaml")
        } else if base
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "runtime_contract.yaml")
        {
            base.to_path_buf()
        } else {
            continue;
        };
        if !contract_path.exists() || !seen_contracts.insert(contract_path.clone()) {
            continue;
        }
        let content = fs::read_to_string(&contract_path)
            .with_context(|| format!("read {}", contract_path.display()))?;
        let yaml: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("parse {}", contract_path.display()))?;
        let Some(task_id) = contract_runtime_task_id(&yaml) else {
            continue;
        };
        let mut inputs = Vec::new();
        collect_contract_declared_inputs(&yaml, &contract_path, &mut inputs);
        if !inputs.is_empty() {
            by_task.entry(task_id).or_default().extend(inputs);
        }
    }
    for inputs in by_task.values_mut() {
        inputs.sort_by(|left, right| {
            left.dest
                .cmp(&right.dest)
                .then_with(|| left.src.cmp(&right.src))
                .then_with(|| left.label.cmp(&right.label))
        });
        inputs.dedup_by(|left, right| left.dest == right.dest && left.src == right.src);
    }
    Ok(by_task)
}

fn contract_runtime_task_id(value: &serde_yaml::Value) -> Option<String> {
    yaml_mapping_get(value, "runtime_task")
        .or_else(|| yaml_mapping_get(value, "runtime_task_id"))
        .or_else(|| yaml_mapping_get(value, "consumer_task"))
        .and_then(yaml_scalar_string)
}

fn collect_contract_declared_inputs(
    value: &serde_yaml::Value,
    contract_path: &Path,
    inputs: &mut Vec<ContractDeclaredInput>,
) {
    let serde_yaml::Value::Mapping(mapping) = value else {
        if let serde_yaml::Value::Sequence(values) = value {
            for item in values {
                collect_contract_declared_inputs(item, contract_path, inputs);
            }
        }
        return;
    };

    let source = [
        "host_path",
        "audit_root_host",
        "validation_root_host",
        "runtime_root_host",
        "provenance_root_host",
        "source_root_host",
        "local_path",
    ]
    .iter()
    .find_map(|key| mapping_get_string(mapping, key).map(|value| ((*key).to_string(), value)));
    let dest = [
        "expected_container_input_root",
        "expected_container_root",
        "container_root",
        "dest",
        "destination",
    ]
    .iter()
    .find_map(|key| mapping_get_string(mapping, key));
    if let (Some((source_key, src)), Some(dest)) = (source, dest)
        && is_probable_local_contract_source(&src)
        && is_probable_node_contract_destination(&dest)
    {
        inputs.push(ContractDeclaredInput {
            label: source_key,
            src: PathBuf::from(src),
            dest,
            contract_path: contract_path.to_path_buf(),
        });
    }

    for child in mapping.values() {
        collect_contract_declared_inputs(child, contract_path, inputs);
    }
}

fn yaml_mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    let serde_yaml::Value::Mapping(mapping) = value else {
        return None;
    };
    mapping.get(serde_yaml::Value::String(key.to_string()))
}

fn mapping_get_string(mapping: &serde_yaml::Mapping, key: &str) -> Option<String> {
    mapping
        .get(serde_yaml::Value::String(key.to_string()))
        .and_then(yaml_scalar_string)
}

fn yaml_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn is_probable_local_contract_source(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("$HOME/") || path.starts_with("~/")
}

fn is_probable_node_contract_destination(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("$HOME/") || path.starts_with("~/")
}

fn sync_codex_assets_to_node(
    node: &TeamNode,
    dest: &str,
    include_auth: bool,
) -> Result<Vec<String>> {
    let (command, existing) = build_asset_sync_command(node, dest, include_auth)?;
    run_shell_command(&command, "sync Codex assets")?;
    Ok(existing.into_iter().map(str::to_string).collect())
}

fn build_path_sync_command(
    node: &TeamNode,
    src: &Path,
    dest: &str,
    replace: bool,
) -> Result<(String, &'static str)> {
    let src_parent = src
        .parent()
        .with_context(|| format!("source path `{}` has no parent", src.display()))?;
    let src_name = src
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("source path `{}` has no UTF-8 file name", src.display()))?;
    let src_kind = if src.is_dir() { "directory" } else { "file" };
    let local_tar = format!(
        "tar -C {} -cf - {}",
        shell_quote_path(src_parent),
        shell_quote(src_name)
    );
    let remote_extract = remote_path_extract_script(src_name, dest, replace);
    let command = match node.kind {
        TeamNodeKind::Local => {
            format!("{local_tar} | bash -lc {}", shell_quote(&remote_extract))
        }
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().context("ssh node needs host")?;
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            format!(
                "{local_tar} | docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            let remote_command = format!(
                "docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            );
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_command)
            )
        }
        TeamNodeKind::Manual => bail!("manual node path sync is not supported"),
    };
    Ok((command, src_kind))
}

fn build_path_pull_command(
    node: &TeamNode,
    src: &str,
    dest: &Path,
    replace: bool,
) -> Result<(String, String)> {
    let src_name = node_path_basename(src)
        .with_context(|| format!("node source path `{src}` has no file name"))?;
    let remote_tar = node_path_tar_script(src);
    let local_extract = remote_path_extract_script(&src_name, &dest.display().to_string(), replace);
    let command = match node.kind {
        TeamNodeKind::Local => {
            format!(
                "bash -lc {} | bash -lc {}",
                shell_quote(&remote_tar),
                shell_quote(&local_extract)
            )
        }
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().context("ssh node needs host")?;
            format!(
                "ssh {} {} | bash -lc {}",
                shell_quote(host),
                shell_quote(&remote_tar),
                shell_quote(&local_extract)
            )
        }
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            format!(
                "docker exec {} bash -lc {} | bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_tar),
                shell_quote(&local_extract)
            )
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            let remote_command = format!(
                "docker exec {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_tar)
            );
            format!(
                "ssh {} {} | bash -lc {}",
                shell_quote(host),
                shell_quote(&remote_command),
                shell_quote(&local_extract)
            )
        }
        TeamNodeKind::Manual => bail!("manual node path pull is not supported"),
    };
    Ok((command, src_name))
}

fn node_path_tar_script(src: &str) -> String {
    format!(
        r#"set -euo pipefail
src={src}
if [ ! -e "$src" ]; then
  echo "pull-path: source does not exist: $src" >&2
  exit 19
fi
parent="$(dirname "$src")"
name="$(basename "$src")"
tar -C "$parent" -cf - "$name"
"#,
        src = shell_quote(src),
    )
}

fn node_path_basename(src: &str) -> Option<String> {
    let src = src.trim().trim_end_matches('/');
    if src.is_empty() {
        return None;
    }
    src.rsplit('/')
        .find(|part| !part.is_empty())
        .map(str::to_string)
}

fn remote_path_extract_script(src_name: &str, dest: &str, replace: bool) -> String {
    format!(
        r#"set -euo pipefail
{dest_assignment}
src_name={src_name}
replace={replace}
parent="$(dirname "$dest")"
mkdir -p "$parent"
tmp="$(mktemp -d)"
cleanup() {{
  rm -rf "$tmp"
}}
trap cleanup EXIT HUP INT TERM
tar -C "$tmp" -xf -
incoming="$tmp/$src_name"
if [ ! -e "$incoming" ]; then
  echo "sync-path: archive did not contain expected entry $src_name" >&2
  exit 18
fi
if [ -e "$dest" ]; then
  if [ "$replace" != "1" ]; then
    echo "sync-path: destination exists; rerun with --replace: $dest" >&2
    exit 17
  fi
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  backup_dir="$parent/.codex-team-handoff-backups/$stamp"
  mkdir -p "$backup_dir"
  mv "$dest" "$backup_dir/$(basename "$dest")"
fi
mv "$incoming" "$dest"
"#,
        dest_assignment = remote_path_dest_assignment(dest),
        src_name = shell_quote(src_name),
        replace = if replace { "1" } else { "0" },
    )
}

fn maybe_sync_remote_node_assets(
    team_dir: &Path,
    nodes: &[TeamNode],
    node_clients: &HashMap<String, TeamAppServerNodeClient>,
    last_sync: &mut HashMap<String, Instant>,
    interval: Duration,
) -> Result<()> {
    let now_instant = Instant::now();
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
        match sync_codex_assets_to_node(node, "$HOME/.codex", true) {
            Ok(paths) => {
                last_sync.insert(node.id.clone(), now_instant);
                append_event(
                    team_dir,
                    "node_assets_periodic_synced",
                    serde_json::json!({
                        "node": node.id,
                        "paths": paths,
                        "include_auth": true,
                    }),
                )?;
            }
            Err(err) => {
                last_sync.insert(node.id.clone(), now_instant);
                append_event(
                    team_dir,
                    "node_assets_periodic_sync_failed",
                    serde_json::json!({
                        "node": node.id,
                        "error": err.to_string(),
                    }),
                )?;
            }
        }
    }
    Ok(())
}

fn build_asset_sync_command<'a>(
    node: &TeamNode,
    dest: &str,
    include_auth: bool,
) -> Result<(String, Vec<&'a str>)> {
    let codex_home =
        codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
    let mut includes = vec!["config.toml", "skills", "rules", "memories", ".tmp/plugins"];
    if include_auth {
        includes.push("auth.json");
    }
    let existing = includes
        .into_iter()
        .filter(|path| codex_home.join(path).exists())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        bail!("no syncable Codex assets found in {}", codex_home.display());
    }
    let tar_args = existing
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    let local_tar = format!(
        "tar -C {} -cf - {}",
        shell_quote_path(&codex_home),
        tar_args
    );
    let backup_entries = existing
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    let remote_extract = format!(
        r#"set -euo pipefail
{dest_assignment}
mkdir -p "$dest"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup="$dest/.codex-team-backups/$stamp"
made_backup=0
for p in {backup_entries}; do
  if [ -e "$dest/$p" ]; then
    mkdir -p "$backup/$(dirname "$p")"
    cp -a "$dest/$p" "$backup/$p"
    made_backup=1
  fi
done
tar -C "$dest" -xf -
if [ "$made_backup" = "0" ]; then
  rmdir "$backup" 2>/dev/null || true
fi"#,
        dest_assignment = remote_codex_dest_assignment(dest),
        backup_entries = backup_entries
    );
    let command = match node.kind {
        TeamNodeKind::Local => {
            format!("{local_tar} | bash -lc {}", shell_quote(&remote_extract))
        }
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().context("ssh node needs host")?;
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            format!(
                "{local_tar} | docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            let remote_command = format!(
                "docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            );
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_command)
            )
        }
        TeamNodeKind::Manual => bail!("manual node asset sync is not supported"),
    };
    Ok((command, existing))
}

fn remote_codex_dest_assignment(dest: &str) -> String {
    let trimmed = dest.trim();
    if matches!(trimmed, "$HOME/.codex" | "${HOME}/.codex" | "~/.codex") {
        "dest=\"${HOME:-/root}/.codex\"".to_string()
    } else {
        format!("dest={}", shell_quote(trimmed))
    }
}

fn remote_path_dest_assignment(dest: &str) -> String {
    let trimmed = dest.trim();
    if trimmed == "$HOME" || trimmed == "${HOME}" || trimmed == "~" {
        "dest=\"${HOME:-/root}\"".to_string()
    } else if let Some(rest) = trimmed.strip_prefix("$HOME/") {
        format!("dest=\"${{HOME:-/root}}/{}\"", rest.replace('"', "\\\""))
    } else if let Some(rest) = trimmed.strip_prefix("${HOME}/") {
        format!("dest=\"${{HOME:-/root}}/{}\"", rest.replace('"', "\\\""))
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        format!("dest=\"${{HOME:-/root}}/{}\"", rest.replace('"', "\\\""))
    } else {
        format!("dest={}", shell_quote(trimmed))
    }
}
