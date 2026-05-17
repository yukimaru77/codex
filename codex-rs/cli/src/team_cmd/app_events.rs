#[derive(Clone)]
struct AppServerMemberRun {
    member: TeamMember,
    node_id: String,
    cwd: PathBuf,
    thread_id: String,
    turn_id: String,
    completed: bool,
    failed: bool,
    standby_after_turn: bool,
    usage_category: String,
    team_message_scan_offset: usize,
    last_activity_at: Instant,
    last_activity_kind: String,
    last_stale_notice_at: Option<Instant>,
    retry_not_before: Option<Instant>,
    side_context_ids: Vec<String>,
}

struct TeamAppServerNodeClient {
    client: RemoteAppServerClient,
    request_counter: i64,
}

#[cfg(test)]
const TEAM_WAIT_IDLE_ACTIVE_QUIET_THRESHOLD_SECS: u64 = 180;

#[derive(Clone, Debug, PartialEq, Eq)]
struct TeamWaitIdleState {
    wait_ids: Vec<String>,
    job_ids: Vec<String>,
    task_ids: Vec<String>,
    active_members: Vec<String>,
}

impl TeamWaitIdleState {
    fn key(&self) -> String {
        format!(
            "waits={};jobs={};tasks={};active={}",
            self.wait_ids.join(","),
            self.job_ids.join(","),
            self.task_ids.join(","),
            self.active_members.join(",")
        )
    }

    fn is_empty(&self) -> bool {
        self.wait_ids.is_empty()
            && self.job_ids.is_empty()
            && self.task_ids.is_empty()
            && self.active_members.is_empty()
    }
}

fn team_multi_agent_config_overrides(
    language: TeamPromptLanguage,
) -> HashMap<String, serde_json::Value> {
    let mut config = HashMap::new();
    config.insert(
        "features.multi_agent_v2.usage_hint_enabled".to_string(),
        serde_json::Value::Bool(true),
    );
    config.insert(
        "features.multi_agent_v2.usage_hint_text".to_string(),
        serde_json::Value::String(team_multi_agent_tool_usage_hint(language)),
    );
    config.insert(
        "features.multi_agent_v2.root_agent_usage_hint_text".to_string(),
        serde_json::Value::String(team_multi_agent_root_usage_hint(language)),
    );
    config.insert(
        "features.multi_agent_v2.subagent_usage_hint_text".to_string(),
        serde_json::Value::String(team_multi_agent_subagent_usage_hint(language)),
    );
    config
}

fn append_team_multi_agent_developer_instructions(
    existing: Option<String>,
    language: TeamPromptLanguage,
) -> String {
    match existing {
        Some(existing) if !existing.trim().is_empty() => {
            format!(
                "{existing}\n\n{}",
                team_multi_agent_root_usage_hint(language)
            )
        }
        _ => team_multi_agent_root_usage_hint(language),
    }
}

fn team_multi_agent_tool_usage_hint(language: TeamPromptLanguage) -> String {
    if language.is_ja() {
        return r#"Codex Teams subagent policy:
- このスレッドが Codex Team の lead または部署として起動されている場合、ユーザーの team 実行依頼は、その team 内で必要な subagent を使う許可を含むものとして扱ってください。
- 追加の明示的なユーザー許可を待つ必要はありません。調査、実装分割、検証、ログ解析、レビュー、リモート/Docker作業など、実質的な部署作業では subagent/agent tools/parallel helpers の利用をデフォルトにしてください。使わない場合は「なぜこの turn では不要か」を短く記録してください。
- delegate する仕事は具体的かつ有界にし、team の ownership、node、cwd、成果物境界を尊重してください。
- 重複作業を避け、subagent の結果は team message、handoff、artifact、evidence として親部署が統合してください。
- lead が部署を作るべき仕事と、部署内部で subagent に任せるべき仕事を混同しないでください。subagent は部署の内部補助であり、team 全体の意思決定は lead が行います。"#
            .to_string();
    }
    r#"Codex Teams subagent policy:
- When this thread is running as a Codex Team lead or department, the user's request to run a team counts as authorization to use subagents inside that team.
- Do not wait for a separate explicit user request solely to delegate. For substantial department work, default to using subagents, agent tools, or parallel helpers for research, implementation slices, verification, log analysis, review, or remote/Docker support. If you do not use them in a substantial turn, briefly record why they were unnecessary.
- Keep delegated work concrete and bounded, and respect team ownership, node, cwd, and artifact boundaries.
- Avoid duplicate work. The parent department must integrate subagent results into team messages, handoffs, artifacts, or evidence.
- Do not confuse lead-created departments with department-internal subagents. Subagents are helpers inside a department; team-level orchestration remains the lead's responsibility."#
        .to_string()
}

fn team_multi_agent_root_usage_hint(language: TeamPromptLanguage) -> String {
    if language.is_ja() {
        return r#"Codex Teams root-session override:
あなたは Codex Team の lead または部署セッションです。通常の「ユーザーが明示的に subagent を頼んだ場合だけ spawn_agent を使う」という制限は、この team 実行中は上書きされます。team 起動依頼そのものが、部署内部の bounded な subagent 利用を許可しています。

実質的な部署作業では subagent/agent tools/parallel helpers の利用をデフォルトにしてください。使わない場合は理由を短く残してください。使う場合は目的、入力、成果物、担当範囲を明確にしてください。team の mailbox、task、wait、job、node、ownership と整合させ、結果は親部署が統合して lead や他部署に共有してください。"#
            .to_string();
    }
    r#"Codex Teams root-session override:
You are a Codex Team lead or department session. During this team run, the usual restriction that spawn_agent requires a separate explicit user request is overridden. The team request itself authorizes bounded subagent use inside departments.

For substantial department work, default to using subagents, agent tools, or parallel helpers. If you do not use them, briefly record why they were unnecessary. When you delegate, define the objective, inputs, outputs, and ownership clearly. Keep the work aligned with team mailbox, tasks, waits, jobs, nodes, and ownership. The parent department must integrate and share the results with the lead or other departments."#
        .to_string()
}

fn team_multi_agent_subagent_usage_hint(language: TeamPromptLanguage) -> String {
    if language.is_ja() {
        return r#"Codex Teams subagent context:
あなたは Codex Team の部署から呼ばれた subagent です。委任された範囲に集中し、勝手に team 部署や node を増減しないでください。発見、成果物、検証結果、blocker は親部署に簡潔に返してください。team 全体の判断や他部署への正式な handoff は親部署または lead が行います。"#
            .to_string();
    }
    r#"Codex Teams subagent context:
You are a subagent invoked by a Codex Team department. Stay within the delegated scope and do not create or remove team departments or nodes on your own. Return concise findings, artifacts, verification results, and blockers to the parent department. Team-level decisions and formal handoffs to other departments are handled by the parent department or lead."#
        .to_string()
}

async fn start_team_app_server_thread(
    node_client: &mut TeamAppServerNodeClient,
    team_dir: &Path,
    node_id: &str,
    member_name: &str,
    purpose: &str,
    mut params: ThreadStartParams,
    language: TeamPromptLanguage,
) -> Result<ThreadStartResponse> {
    let original_params = params.clone();
    params.developer_instructions = Some(append_team_multi_agent_developer_instructions(
        params.developer_instructions.take(),
        language,
    ));
    let mut config = params.config.take().unwrap_or_default();
    for (key, value) in team_multi_agent_config_overrides(language) {
        config.insert(key, value);
    }
    params.config = Some(config);

    match node_client
        .client
        .request_typed(ClientRequest::ThreadStart {
            request_id: next_request_id(&mut node_client.request_counter),
            params,
        })
        .await
    {
        Ok(thread) => Ok(thread),
        Err(err) => {
            let warning = format!(
                "Codex Teams multi-agent override failed for @{member_name} on node `{node_id}` ({purpose}); retrying without override: {err}"
            );
            eprintln!("warning: {warning}");
            let _ = append_event(
                team_dir,
                "team_multi_agent_override_failed",
                serde_json::json!({
                    "member": member_name,
                    "node": node_id,
                    "purpose": purpose,
                    "error": err.to_string(),
                    "fallback": "thread_start_without_multi_agent_override",
                }),
            );
            node_client
                .client
                .request_typed(ClientRequest::ThreadStart {
                    request_id: next_request_id(&mut node_client.request_counter),
                    params: original_params,
                })
                .await
                .map_err(|fallback_err| anyhow!(fallback_err))
        }
    }
}

async fn fork_team_app_server_thread(
    node_client: &mut TeamAppServerNodeClient,
    team_dir: &Path,
    node_id: &str,
    member_name: &str,
    purpose: &str,
    mut params: ThreadForkParams,
    language: TeamPromptLanguage,
) -> Result<ThreadForkResponse> {
    let original_params = params.clone();
    params.developer_instructions = Some(append_team_multi_agent_developer_instructions(
        params.developer_instructions.take(),
        language,
    ));
    let mut config = params.config.take().unwrap_or_default();
    for (key, value) in team_multi_agent_config_overrides(language) {
        config.insert(key, value);
    }
    params.config = Some(config);

    match node_client
        .client
        .request_typed(ClientRequest::ThreadFork {
            request_id: next_request_id(&mut node_client.request_counter),
            params,
        })
        .await
    {
        Ok(thread) => Ok(thread),
        Err(err) => {
            let warning = format!(
                "Codex Teams multi-agent override failed for @{member_name} fork on node `{node_id}` ({purpose}); retrying without override: {err}"
            );
            eprintln!("warning: {warning}");
            let _ = append_event(
                team_dir,
                "team_multi_agent_override_failed",
                serde_json::json!({
                    "member": member_name,
                    "node": node_id,
                    "purpose": purpose,
                    "error": err.to_string(),
                    "fallback": "thread_fork_without_multi_agent_override",
                }),
            );
            node_client
                .client
                .request_typed(ClientRequest::ThreadFork {
                    request_id: next_request_id(&mut node_client.request_counter),
                    params: original_params,
                })
                .await
                .map_err(|fallback_err| anyhow!(fallback_err))
        }
    }
}

struct AppServerSideReply {
    member: TeamMember,
    node_id: String,
    source_thread_id: String,
    side_thread_id: String,
    turn_id: String,
    usage_category: String,
    recipients: Vec<String>,
    messages: Vec<MailMessage>,
    buffer: String,
    started_at: Instant,
}

fn usage_category_for_event(event_name: &str) -> &'static str {
    match event_name {
        "app_server_lead_started" => "lead_initial",
        "app_server_lead_reactive_started" => "lead_reactive",
        "app_server_member_reactive_started" => "member_reactive",
        "app_server_dynamic_member_started" => "dynamic_member_start",
        "app_server_member_started" => "department_start",
        event if event.contains("idle_wakeup") => "idle_wakeup",
        event if event.contains("heartbeat") => "department_heartbeat",
        event if event.contains("lead") => "lead_turn",
        event if event.contains("reactive") => "reactive_turn",
        _ => "member_turn",
    }
}

fn usage_category_for_messages(default: &str, messages: &[MailMessage]) -> String {
    if messages.iter().any(|message| message.from == "user") {
        return "user_message".to_string();
    }
    if messages
        .iter()
        .any(|message| {
            let text = message.message.trim_start();
            text.starts_with("WAIT_STATUS:") || text.starts_with("WAIT_STILL_OPEN:")
        })
    {
        return "team_wait_status".to_string();
    }
    if messages.iter().any(|message| {
        let text = message.message.trim_start();
        text.starts_with("JOB_STATUS:") || text.starts_with("AUX_JOB_STATUS:")
    }) {
        return "team_job_status".to_string();
    }
    if messages
        .iter()
        .any(|message| message.message.starts_with("Lead autonomy tick:"))
    {
        return "lead_tick".to_string();
    }
    if messages
        .iter()
        .any(|message| message.message.starts_with("Department idle wakeup"))
    {
        return "idle_wakeup".to_string();
    }
    if messages
        .iter()
        .any(|message| message.message.starts_with("Department heartbeat"))
    {
        return "department_heartbeat".to_string();
    }
    if messages
        .iter()
        .any(|message| message.message.starts_with("Task watchdog:"))
    {
        return "task_watchdog".to_string();
    }
    if messages
        .iter()
        .any(|message| message.message.starts_with("Periodic idle outreach"))
    {
        return "idle_outreach".to_string();
    }
    if messages
        .iter()
        .any(|message| message.message.starts_with("Stale active turn attention:"))
    {
        return "stale_active_turn".to_string();
    }
    if messages.iter().any(|message| message.from != "system") {
        return usage_category_for_team_messages(messages);
    }
    default.to_string()
}

fn usage_category_for_team_messages(messages: &[MailMessage]) -> String {
    let mut saw_non_system = false;
    let mut saw_noop_stay = false;
    let mut saw_wait_status = false;
    let mut saw_job_status = false;
    let mut saw_lead_proposal = false;
    let mut saw_debate_request = false;
    let mut saw_debate_response = false;
    let mut saw_decision_record = false;
    let mut saw_review_request = false;
    let mut saw_review_response = false;
    let mut saw_handoff = false;
    let mut saw_final_handoff = false;
    let mut saw_artifact_handoff = false;
    let mut saw_artifact_plan = false;
    let mut saw_blocker = false;
    let mut saw_failure_blocker = false;
    let mut saw_dependency_gate = false;
    let mut saw_review = false;
    let mut saw_audit_review = false;
    let mut saw_status = false;
    for message in messages.iter().filter(|message| message.from != "system") {
        saw_non_system = true;
        let text = message.message.trim();
        let has_marker = |marker: &str| {
            text.lines().any(|line| {
                line.trim_start()
                    .to_ascii_uppercase()
                    .starts_with(marker)
            })
        };
        if is_stay_message(text) {
            saw_noop_stay = true;
            continue;
        }
        if text.starts_with("WAIT_STATUS:") {
            saw_wait_status = true;
        }
        if text.starts_with("JOB_STATUS:")
            || text.starts_with("JOB_UPDATE:")
            || text.starts_with("AUX_JOB_STATUS:")
        {
            saw_job_status = true;
        }
        if has_marker("LEAD_PROPOSAL:") {
            saw_lead_proposal = true;
        }
        if has_marker("DEBATE_REQUEST:") {
            saw_debate_request = true;
        }
        if has_marker("DEBATE_RESPONSE:") {
            saw_debate_response = true;
        }
        if has_marker("DECISION_RECORD:") {
            saw_decision_record = true;
        }
        if has_marker("REVIEW_REQUEST") {
            saw_review_request = true;
        }
        if has_marker("REVIEW_RESPONSE") {
            saw_review_response = true;
        }
        if has_marker("ARTIFACT_PLAN:") {
            saw_artifact_plan = true;
        }
        if has_marker("TEAM_COMPLETION_CHECKLIST")
            || has_marker("FINAL_HANDOFF")
            || has_marker("PLANNER_FINAL_HANDOFF")
        {
            saw_final_handoff = true;
        }
        if has_marker("ARTIFACT_HANDOFF:") {
            saw_artifact_handoff = true;
        }
        if has_marker("TEAM_COMPLETION_CHECKLIST")
            || has_marker("FINAL_HANDOFF")
            || has_marker("PLANNER_FINAL_HANDOFF")
            || has_marker("ARTIFACT_HANDOFF:")
        {
            saw_handoff = true;
        }
        if has_marker("FAILURE:")
            || has_marker("TERMINAL_FAILURE:")
            || has_marker("JOB_FAILED:")
        {
            saw_failure_blocker = true;
        }
        if has_marker("DEPENDENCY_GATE:")
            || has_marker("CREDENTIAL_GATE:")
            || has_marker("AUTH_GATE:")
        {
            saw_dependency_gate = true;
        }
        if has_marker("BLOCKER:") {
            saw_blocker = true;
        }
        if has_marker("REVIEW_REQUEST") || has_marker("REVIEW_RESPONSE") {
            saw_review = true;
        }
        if has_marker("AUDIT_REVIEW:") {
            saw_audit_review = true;
        }
        if has_marker("STATUS:") || has_marker("STAY:") {
            saw_status = true;
        }
    }
    if !saw_non_system {
        return "team_message".to_string();
    }
    if saw_lead_proposal {
        "team_lead_proposal".to_string()
    } else if saw_debate_request {
        "team_debate_request".to_string()
    } else if saw_debate_response {
        "team_debate_response".to_string()
    } else if saw_decision_record {
        "team_decision_record".to_string()
    } else if saw_review_request {
        "team_review_request".to_string()
    } else if saw_review_response {
        "team_review_response".to_string()
    } else if saw_wait_status {
        "team_wait_status".to_string()
    } else if saw_job_status {
        "team_job_status".to_string()
    } else if saw_final_handoff {
        "team_final_handoff".to_string()
    } else if saw_artifact_plan {
        "team_artifact_plan".to_string()
    } else if saw_handoff && saw_artifact_handoff {
        "team_artifact_handoff".to_string()
    } else if saw_failure_blocker {
        "team_failure_blocker".to_string()
    } else if saw_dependency_gate {
        "team_dependency_gate".to_string()
    } else if saw_blocker {
        "team_blocker".to_string()
    } else if saw_audit_review {
        "team_audit_review".to_string()
    } else if saw_review {
        "team_review_request".to_string()
    } else if saw_handoff {
        "team_handoff".to_string()
    } else if saw_noop_stay && !saw_status {
        "team_noop_stay".to_string()
    } else if saw_status || saw_noop_stay {
        "team_status".to_string()
    } else {
        "team_message".to_string()
    }
}

fn artifact_plan_delivery_senders(member_name: &str, messages: &[MailMessage]) -> Vec<String> {
    if usage_category_for_messages("team_message", messages) != "team_artifact_plan" {
        return Vec::new();
    }
    if messages.iter().any(|message| {
        message.from == "system" || artifact_plan_message_has_action_marker(&message.message)
    }) {
        return Vec::new();
    }
    let mut senders = messages
        .iter()
        .filter(|message| message.to == member_name || message.to == "all")
        .filter(|message| !message.from.is_empty())
        .map(|message| message.from.clone())
        .collect::<Vec<_>>();
    senders.sort();
    senders.dedup();
    senders
}

fn artifact_plan_message_has_action_marker(message: &str) -> bool {
    message.lines().any(|line| {
        let upper = line.trim_start().to_ascii_uppercase();
        upper.starts_with("ACTION_REQUIRED:")
            || upper.starts_with("BLOCKER:")
            || upper.starts_with("LEAD_PROPOSAL:")
            || upper.starts_with("DECISION_RECORD:")
            || upper.starts_with("DEBATE_REQUEST:")
            || upper.starts_with("DEBATE_RESPONSE:")
            || upper.starts_with("REVIEW_REQUEST")
            || upper.starts_with("REVIEW_RESPONSE")
            || upper.starts_with("JOB_STATUS:")
            || upper.starts_with("JOB_UPDATE:")
            || upper.starts_with("WAIT_STATUS:")
            || upper.starts_with("WAIT_STILL_OPEN:")
            || upper.starts_with("TEAM_COMPLETION_CHECKLIST")
            || upper.starts_with("FINAL_HANDOFF")
    })
}

fn artifact_plan_delivery_fingerprint(member_name: &str, sender: &str) -> String {
    format!("member={member_name};sender={sender}")
}

fn repeated_artifact_plan_delivery(team_dir: &Path, member_name: &str, messages: &[MailMessage]) -> bool {
    let senders = artifact_plan_delivery_senders(member_name, messages);
    !senders.is_empty()
        && senders.iter().all(|sender| {
            attention_fingerprint_recently_sent(
                team_dir,
                "artifact_plan_delivery",
                &artifact_plan_delivery_fingerprint(member_name, sender),
            )
        })
}

fn record_artifact_plan_delivery(team_dir: &Path, member_name: &str, messages: &[MailMessage]) -> Result<()> {
    for sender in artifact_plan_delivery_senders(member_name, messages) {
        record_attention_fingerprint(
            team_dir,
            "artifact_plan_delivery",
            &artifact_plan_delivery_fingerprint(member_name, &sender),
            serde_json::json!({
                "member": member_name,
                "sender": sender,
                "messages": messages.len(),
            }),
        )?;
    }
    Ok(())
}

fn is_stay_message(message: &str) -> bool {
    let trimmed = message.trim();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("stay:")
        || lower == "stay"
        || lower.starts_with("staying:")
        || lower.starts_with("no action")
        || lower.starts_with("no-op")
        || lower.starts_with("noop")
}

fn update_active_turn_usage_category(
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    member_name: &str,
    category: String,
    source_event: &str,
) -> Result<()> {
    let Some(run) = active.get_mut(member_name) else {
        return Ok(());
    };
    run.usage_category = category;
    record_turn_usage_index(
        team_dir,
        &run.member,
        &run.node_id,
        &run.thread_id,
        &run.turn_id,
        &run.usage_category,
        source_event,
    )
}

fn team_turn_usage_index_path(team_dir: &Path) -> PathBuf {
    team_dir.join("turn_usage_index.jsonl")
}

fn team_token_usage_path(team_dir: &Path) -> PathBuf {
    team_dir.join("token_usage.jsonl")
}

fn record_turn_usage_index(
    team_dir: &Path,
    member: &TeamMember,
    node: &str,
    thread: &str,
    turn: &str,
    category: &str,
    source_event: &str,
) -> Result<()> {
    append_jsonl(
        &team_turn_usage_index_path(team_dir),
        &TeamTurnUsageIndexRecord {
            timestamp: now(),
            member: member.name.clone(),
            role: member.role.clone(),
            node: node.to_string(),
            thread: thread.to_string(),
            turn: turn.to_string(),
            category: category.to_string(),
            source_event: source_event.to_string(),
        },
    )
}

fn lookup_turn_usage_index(
    team_dir: &Path,
    node: &str,
    thread: &str,
    turn: &str,
) -> Result<Option<TeamTurnUsageIndexRecord>> {
    Ok(
        read_jsonl::<TeamTurnUsageIndexRecord>(&team_turn_usage_index_path(team_dir))?
            .into_iter()
            .rev()
            .find(|record| record.node == node && record.thread == thread && record.turn == turn),
    )
}

fn record_token_usage_update(
    team_dir: &Path,
    node: &str,
    notification: ThreadTokenUsageUpdatedNotification,
    active: &HashMap<String, AppServerMemberRun>,
    side_replies: &HashMap<String, AppServerSideReply>,
    thread_to_member: &HashMap<String, String>,
) -> Result<()> {
    let key = thread_key(node, &notification.thread_id);
    let indexed = lookup_turn_usage_index(
        team_dir,
        node,
        &notification.thread_id,
        &notification.turn_id,
    )?;
    let (member, role, category, source) = if let Some(reply) = side_replies.get(&key) {
        (
            reply.member.name.clone(),
            reply.member.role.clone(),
            reply.usage_category.clone(),
            "side_channel_live".to_string(),
        )
    } else if let Some(member_name) = thread_to_member.get(&key)
        && let Some(run) = active.get(member_name)
    {
        (
            run.member.name.clone(),
            run.member.role.clone(),
            run.usage_category.clone(),
            "active_turn".to_string(),
        )
    } else if let Some(indexed) = indexed {
        (
            indexed.member,
            indexed.role,
            indexed.category,
            "turn_index".to_string(),
        )
    } else {
        (
            "unknown".to_string(),
            "unknown".to_string(),
            "unknown".to_string(),
            "unmapped".to_string(),
        )
    };
    append_jsonl(
        &team_token_usage_path(team_dir),
        &TeamTokenUsageRecord {
            timestamp: now(),
            member,
            role,
            node: node.to_string(),
            thread: notification.thread_id,
            turn: notification.turn_id,
            category,
            source,
            total: notification.token_usage.total.into(),
            last: notification.token_usage.last.into(),
            model_context_window: notification.token_usage.model_context_window,
        },
    )?;
    Ok(())
}

fn latest_thread_token_usage(
    team_dir: &Path,
    node: &str,
    thread: &str,
) -> Result<Option<TeamTokenUsageRecord>> {
    Ok(
        read_jsonl::<TeamTokenUsageRecord>(&team_token_usage_path(team_dir))?
            .into_iter()
            .rev()
            .find(|record| record.node == node && record.thread == thread),
    )
}

fn thread_usage_exceeds_rotation_limit(record: &TeamTokenUsageRecord) -> bool {
    if record.total.total_tokens >= MAX_APP_SERVER_THREAD_TOTAL_TOKENS {
        return true;
    }
    let Some(context_window) = record.model_context_window else {
        return false;
    };
    if context_window <= 0 {
        return false;
    }
    record.total.total_tokens.saturating_mul(100)
        >= context_window.saturating_mul(MAX_APP_SERVER_THREAD_CONTEXT_RATIO_PERCENT)
}

async fn maybe_rotate_app_server_thread_before_turn(
    node_client: &mut TeamAppServerNodeClient,
    team_dir: &Path,
    run: &mut AppServerMemberRun,
    thread_to_member: &mut HashMap<String, String>,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    approval_policy: Option<AskForApproval>,
    language: TeamPromptLanguage,
) -> Result<()> {
    if !run.completed {
        return Ok(());
    }
    let Some(usage) = latest_thread_token_usage(team_dir, &run.node_id, &run.thread_id)? else {
        return Ok(());
    };
    if !thread_usage_exceeds_rotation_limit(&usage) {
        return Ok(());
    }
    let old_thread = run.thread_id.clone();
    let new_thread: ThreadStartResponse = start_team_app_server_thread(
        node_client,
        team_dir,
        &run.node_id,
        &run.member.name,
        "context_rotation_thread",
        ThreadStartParams {
            model,
            cwd: Some(run.cwd.display().to_string()),
            sandbox,
            approval_policy,
            ephemeral: Some(false),
            ..ThreadStartParams::default()
        },
        language,
    )
    .await?;
    thread_to_member.remove(&thread_key(&run.node_id, &old_thread));
    thread_to_member.insert(
        thread_key(&run.node_id, &new_thread.thread.id),
        run.member.name.clone(),
    );
    run.thread_id = new_thread.thread.id.clone();
    run.turn_id.clear();
    run.usage_category = "thread_rotated".to_string();
    run.team_message_scan_offset = 0;
    run.side_context_ids.clear();
    set_member_thread(team_dir, &run.member.name, &run.thread_id)?;
    append_event(
        team_dir,
        "app_server_thread_rotated",
        serde_json::json!({
            "member": run.member.name,
            "role": run.member.role,
            "node": run.node_id,
            "old_thread": old_thread,
            "new_thread": run.thread_id,
            "total_tokens": usage.total.total_tokens,
            "model_context_window": usage.model_context_window,
            "reason": "thread token usage exceeded rotation limit before starting next turn",
        }),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn start_app_server_member_turn(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    nodes: &[TeamNode],
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    thread_to_member: &mut HashMap<String, String>,
    member_name: &str,
    prompt: String,
    _cwd: &Path,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    relay_port: u16,
    event_name: &str,
) -> Result<bool> {
    let language = load_config(team_dir)?.language.unwrap_or_default();
    let mut recovered_once = false;

    loop {
        let (node_id, thread_id) = {
            let Some(run) = active.get_mut(member_name) else {
                bail!("member `{member_name}` has no app-server thread");
            };
            if let Some(remaining) = app_server_retry_remaining(run) {
                append_event(
                    team_dir,
                    "app_server_member_turn_start_deferred",
                    serde_json::json!({
                        "member": member_name,
                        "node": run.node_id.clone(),
                        "thread": run.thread_id.clone(),
                        "reason": "temporary app-server/model usage-limit cooldown",
                        "retry_after_sec": remaining.as_secs(),
                        "event": event_name,
                    }),
                )?;
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                return Ok(false);
            }
            (run.node_id.clone(), run.thread_id.clone())
        };

        if !node_clients.contains_key(&node_id) {
            if node_id != "local" && !recovered_once {
                match recover_member_thread_on_node(
                    node_clients,
                    node_processes,
                    nodes,
                    team_dir,
                    active,
                    thread_to_member,
                    member_name,
                    model.clone(),
                    sandbox.clone(),
                    approval_policy.clone(),
                    relay_port,
                    language,
                    "node client missing before turn start",
                )
                .await
                {
                    Ok(()) => {
                        recovered_once = true;
                        append_event(
                            team_dir,
                            "app_server_member_turn_start_recovered",
                            serde_json::json!({
                                "member": member_name,
                                "node": node_id,
                                "old_thread": thread_id,
                                "event": event_name,
                                "reason": "node client was missing; recovered node and member thread",
                            }),
                        )?;
                        continue;
                    }
                    Err(err) => {
                        mark_app_server_member_turn_start_failed(
                            team_dir,
                            active,
                            member_name,
                            &node_id,
                            &thread_id,
                            "node client missing and recovery failed",
                            event_name,
                            &err.to_string(),
                        )?;
                        return Ok(false);
                    }
                }
            }
            append_event(
                team_dir,
                "app_server_member_turn_start_skipped",
                serde_json::json!({
                    "member": member_name,
                    "node": node_id,
                    "thread": thread_id,
                    "reason": "node client missing",
                    "event": event_name,
                }),
            )?;
            block_member_tasks_if_active(
                team_dir,
                member_name,
                "Member could not be resumed because its app-server node client is missing.",
            )?;
            if let Some(run) = active.get_mut(member_name) {
                run.completed = true;
                run.failed = false;
                run.standby_after_turn = false;
            }
            set_member_status(team_dir, member_name, MemberStatus::Standby)?;
            return Ok(false);
        }

        let rotation_result = {
            let run = active
                .get_mut(member_name)
                .with_context(|| format!("member `{member_name}` has no app-server thread"))?;
            let node_client = node_clients
                .get_mut(&node_id)
                .with_context(|| format!("app-server client missing for node `{node_id}`"))?;
            maybe_rotate_app_server_thread_before_turn(
                node_client,
                team_dir,
                run,
                thread_to_member,
                model.clone(),
                sandbox.clone(),
                approval_policy.clone(),
                language,
            )
            .await
        };
        if let Err(err) = rotation_result {
            if node_id != "local" && !recovered_once {
                match recover_member_thread_on_node(
                    node_clients,
                    node_processes,
                    nodes,
                    team_dir,
                    active,
                    thread_to_member,
                    member_name,
                    model.clone(),
                    sandbox.clone(),
                    approval_policy.clone(),
                    relay_port,
                    language,
                    &format!("thread rotation failed before turn start: {err}"),
                )
                .await
                {
                    Ok(()) => {
                        recovered_once = true;
                        append_event(
                            team_dir,
                            "app_server_member_turn_start_recovered",
                            serde_json::json!({
                                "member": member_name,
                                "node": node_id,
                                "old_thread": thread_id,
                                "event": event_name,
                                "reason": "thread rotation failure recovered before turn start",
                            }),
                        )?;
                        continue;
                    }
                    Err(recovery_err) => {
                        append_event(
                            team_dir,
                            "app_server_node_recovery_failed",
                            serde_json::json!({
                                "member": member_name,
                                "node": node_id,
                                "thread": thread_id,
                                "event": event_name,
                                "original_error": err.to_string(),
                                "error": recovery_err.to_string(),
                            }),
                        )?;
                    }
                }
            }
            mark_app_server_member_turn_start_failed(
                team_dir,
                active,
                member_name,
                &node_id,
                &thread_id,
                "thread rotation failed before turn start",
                event_name,
                &err.to_string(),
            )?;
            return Ok(false);
        }

        let (turn_cwd, turn_thread_id) = {
            let run = active
                .get(member_name)
                .with_context(|| format!("member `{member_name}` has no app-server thread"))?;
            (run.cwd.clone(), run.thread_id.clone())
        };
        let (turn_prompt, side_context_ids) =
            append_side_channel_context_prompt(team_dir, member_name, "", prompt.clone(), language)?;
        let turn_result = {
            let node_client = node_clients
                .get_mut(&node_id)
                .with_context(|| format!("app-server client missing for node `{node_id}`"))?;
            node_client
                .client
                .request_typed(ClientRequest::TurnStart {
                    request_id: next_request_id(&mut node_client.request_counter),
                    params: TurnStartParams {
                        thread_id: turn_thread_id.clone(),
                        input: vec![text_input(turn_prompt)],
                        cwd: Some(turn_cwd.clone()),
                        model: model.clone(),
                        approval_policy: approval_policy.clone(),
                        sandbox_policy: if dangerously_bypass_approvals_and_sandbox {
                            Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess)
                        } else {
                            None
                        },
                        ..TurnStartParams::default()
                    },
                })
                .await
        };

        let turn: TurnStartResponse = match turn_result {
            Ok(turn) => turn,
            Err(err) => {
                let err = anyhow!(err);
                if node_id != "local" && !recovered_once {
                    match recover_member_thread_on_node(
                        node_clients,
                        node_processes,
                        nodes,
                        team_dir,
                        active,
                        thread_to_member,
                        member_name,
                        model.clone(),
                        sandbox.clone(),
                        approval_policy.clone(),
                        relay_port,
                        language,
                        &format!("turn start failed: {err}"),
                    )
                    .await
                    {
                        Ok(()) => {
                            recovered_once = true;
                            append_event(
                                team_dir,
                                "app_server_member_turn_start_recovered",
                                serde_json::json!({
                                    "member": member_name,
                                    "node": node_id,
                                    "old_thread": turn_thread_id,
                                    "event": event_name,
                                    "reason": "turn start failure recovered; retrying once on fresh node thread",
                                }),
                            )?;
                            continue;
                        }
                        Err(recovery_err) => {
                            append_event(
                                team_dir,
                                "app_server_node_recovery_failed",
                                serde_json::json!({
                                    "member": member_name,
                                    "node": node_id,
                                    "thread": turn_thread_id,
                                    "event": event_name,
                                    "original_error": err.to_string(),
                                    "error": recovery_err.to_string(),
                                }),
                            )?;
                        }
                    }
                }
                mark_app_server_member_turn_start_failed(
                    team_dir,
                    active,
                    member_name,
                    &node_id,
                    &turn_thread_id,
                    "turn start failed",
                    event_name,
                    &err.to_string(),
                )?;
                return Ok(false);
            }
        };

        let (member, usage_category, event_node_id, event_thread_id) = {
            let run = active
                .get_mut(member_name)
                .with_context(|| format!("member `{member_name}` has no app-server thread"))?;
            run.turn_id = turn.turn.id.clone();
            run.completed = false;
            run.failed = false;
            run.standby_after_turn = false;
            run.usage_category = usage_category_for_event(event_name).to_string();
            run.retry_not_before = None;
            run.last_activity_at = Instant::now();
            run.last_activity_kind = "turn_started".to_string();
            run.last_stale_notice_at = None;
            run.side_context_ids = side_context_ids.clone();
            (
                run.member.clone(),
                run.usage_category.clone(),
                run.node_id.clone(),
                run.thread_id.clone(),
            )
        };
        reset_member_live_message_for_new_turn(team_dir, member_name, &turn.turn.id)?;
        set_member_status(team_dir, member_name, MemberStatus::Running)?;
        mark_side_channel_contexts_injected(team_dir, member_name, &side_context_ids, &turn.turn.id)?;
        append_event(
            team_dir,
            event_name,
            serde_json::json!({
                "member": member_name,
                "node": event_node_id,
                "thread": event_thread_id,
                "turn": turn.turn.id,
                "cwd": turn_cwd,
            }),
        )?;
        record_turn_usage_index(
            team_dir,
            &member,
            &event_node_id,
            &event_thread_id,
            &turn.turn.id,
            &usage_category,
            event_name,
        )?;
        return Ok(true);
    }
}

fn mark_app_server_member_turn_start_failed(
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    member_name: &str,
    node_id: &str,
    thread_id: &str,
    reason: &str,
    event_name: &str,
    error: &str,
) -> Result<()> {
    append_event(
        team_dir,
        "app_server_member_turn_start_failed",
        serde_json::json!({
            "member": member_name,
            "node": node_id,
            "thread": thread_id,
            "reason": reason,
            "event": event_name,
            "error": error,
        }),
    )?;
    if node_id != "local" {
        let _ = set_node_connection(team_dir, node_id, TeamNodeStatus::Failed, None);
    }
    block_member_tasks_if_active(
        team_dir,
        member_name,
        &format!("Member could not be resumed because {reason}: {error}"),
    )?;
    if let Some(run) = active.get_mut(member_name) {
        run.completed = true;
        run.failed = false;
        run.standby_after_turn = false;
    }
    set_member_status(team_dir, member_name, MemberStatus::Standby)?;
    Ok(())
}

async fn connect_team_app_server(url: &str) -> Result<RemoteAppServerClient> {
    connect_team_app_server_with_attempts(url, 50).await
}

async fn recover_app_server_node_client(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    nodes: &[TeamNode],
    team_dir: &Path,
    node_id: &str,
    relay_port: u16,
    reason: &str,
) -> Result<()> {
    if node_id == "local" {
        bail!("local app-server node recovery is not supported inside the team runtime");
    }
    append_event(
        team_dir,
        "app_server_node_recovery_started",
        serde_json::json!({
            "node": node_id,
            "reason": reason,
        }),
    )?;
    if let Some(client) = node_clients.remove(node_id) {
        let _ = client.client.shutdown().await;
    }
    let mut idx = 0;
    while idx < node_processes.len() {
        if node_processes[idx].node_id == node_id {
            let process = node_processes.remove(idx);
            process.stop();
        } else {
            idx += 1;
        }
    }
    let node = nodes
        .iter()
        .find(|node| node.id == node_id)
        .cloned()
        .with_context(|| format!("node `{node_id}` is not registered"))?;
    let (url, process) = resolve_or_spawn_node_app_server(team_dir, &node, relay_port)
        .with_context(|| format!("recover app-server node `{node_id}`"))?;
    if let Some(process) = process {
        node_processes.push(process);
    }
    let connected_client = connect_team_app_server(&url)
        .await
        .with_context(|| format!("connect recovered app-server node `{node_id}` at `{url}`"))?;
    append_event(
        team_dir,
        "app_server_node_recovered",
        serde_json::json!({
            "node": node_id,
            "kind": node.kind,
            "url": url,
            "reason": reason,
        }),
    )?;
    set_node_connection(team_dir, node_id, TeamNodeStatus::Online, Some(url.clone()))?;
    node_clients.insert(
        node_id.to_string(),
        TeamAppServerNodeClient {
            client: connected_client,
            request_counter: 1,
        },
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn recover_member_thread_on_node(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    nodes: &[TeamNode],
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    thread_to_member: &mut HashMap<String, String>,
    member_name: &str,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    approval_policy: Option<AskForApproval>,
    relay_port: u16,
    language: TeamPromptLanguage,
    reason: &str,
) -> Result<()> {
    let (node_id, old_thread, cwd, member) = {
        let run = active
            .get(member_name)
            .with_context(|| format!("member `{member_name}` has no app-server thread"))?;
        (
            run.node_id.clone(),
            run.thread_id.clone(),
            run.cwd.clone(),
            run.member.clone(),
        )
    };
    recover_app_server_node_client(
        node_clients,
        node_processes,
        nodes,
        team_dir,
        &node_id,
        relay_port,
        reason,
    )
    .await?;
    let node_client = node_clients
        .get_mut(&node_id)
        .with_context(|| format!("recovered app-server client missing for node `{node_id}`"))?;
    let thread: ThreadStartResponse = start_team_app_server_thread(
        node_client,
        team_dir,
        &node_id,
        member_name,
        "node_recovery_thread",
        ThreadStartParams {
            model,
            cwd: Some(cwd.display().to_string()),
            sandbox,
            approval_policy,
            ephemeral: Some(false),
            ..ThreadStartParams::default()
        },
        language,
    )
    .await?;
    thread_to_member.remove(&thread_key(&node_id, &old_thread));
    thread_to_member.insert(thread_key(&node_id, &thread.thread.id), member_name.to_string());
    set_member_thread(team_dir, member_name, &thread.thread.id)?;
    if let Some(run) = active.get_mut(member_name) {
        run.thread_id = thread.thread.id.clone();
        run.turn_id.clear();
        run.completed = true;
        run.failed = false;
        run.standby_after_turn = false;
        run.last_activity_at = Instant::now();
        run.last_activity_kind = "node_recovered".to_string();
        run.last_stale_notice_at = None;
        run.retry_not_before = None;
    }
    append_event(
        team_dir,
        "app_server_member_thread_recovered",
        serde_json::json!({
            "member": member.name,
            "node": node_id,
            "old_thread": old_thread,
            "new_thread": thread.thread.id,
            "reason": reason,
        }),
    )?;
    Ok(())
}

async fn connect_team_app_server_with_attempts(
    url: &str,
    attempts: usize,
) -> Result<RemoteAppServerClient> {
    let mut last_error = None;
    for _ in 0..attempts.max(1) {
        match RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
            websocket_url: url.to_string(),
            auth_token: None,
            client_name: "codex_team".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            experimental_api: true,
            opt_out_notification_methods: vec![
                "command/exec/outputDelta".to_string(),
                "item/commandExecution/outputDelta".to_string(),
                "item/fileChange/outputDelta".to_string(),
                "item/reasoning/summaryTextDelta".to_string(),
                "item/reasoning/textDelta".to_string(),
            ],
            channel_capacity: 256,
        })
        .await
        {
            Ok(client) => return Ok(client),
            Err(err) => {
                last_error = Some(err);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    Err(anyhow!(
        "failed to connect to app-server at `{}`: {}",
        url,
        last_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    ))
}

fn next_request_id(counter: &mut i64) -> RequestId {
    let request_id = *counter;
    *counter += 1;
    RequestId::Integer(request_id)
}

fn app_server_usage_limit_cooldown(error: Option<&str>) -> Option<Duration> {
    let error = error?;
    let normalized = error.to_ascii_lowercase();
    if !(normalized.contains("usage limit")
        || normalized.contains("purchase more credits")
        || normalized.contains("try again at"))
    {
        return None;
    }
    Some(usage_limit_cooldown_from_error(
        error,
        &[
            Local::now().time().num_seconds_from_midnight(),
            Utc::now().time().num_seconds_from_midnight(),
        ],
    ))
}

fn usage_limit_cooldown_from_error(error: &str, now_secs_candidates: &[u32]) -> Duration {
    usage_limit_cooldown_from_error_at(error, Local::now(), now_secs_candidates)
}

fn usage_limit_cooldown_from_error_at(
    error: &str,
    now_local: DateTime<Local>,
    now_secs_candidates: &[u32],
) -> Duration {
    const DEFAULT_USAGE_LIMIT_COOLDOWN_SEC: u64 = 45 * 60;
    const RETRY_TIME_JUST_PASSED_GRACE_SEC: u32 = 10 * 60;
    const RETRY_TIME_JUST_PASSED_BACKOFF_SEC: u32 = 5 * 60;
    if let Some(retry_at) = parse_usage_limit_retry_datetime(error) {
        let delta = retry_at.signed_duration_since(now_local);
        if delta.num_seconds() > 0 {
            return Duration::from_secs(delta.num_seconds().max(60) as u64);
        }
        if (-delta).num_seconds() <= i64::from(RETRY_TIME_JUST_PASSED_GRACE_SEC) {
            return Duration::from_secs(u64::from(RETRY_TIME_JUST_PASSED_BACKOFF_SEC));
        }
    }
    match parse_usage_limit_retry_time_secs(error) {
        Some(retry_secs) => {
            let delta = now_secs_candidates
                .iter()
                .map(|now_secs| {
                    let now_secs = now_secs % (24 * 60 * 60);
                    if retry_secs >= now_secs {
                        retry_secs - now_secs
                    } else if now_secs - retry_secs <= RETRY_TIME_JUST_PASSED_GRACE_SEC {
                        RETRY_TIME_JUST_PASSED_BACKOFF_SEC
                    } else {
                        24 * 60 * 60 - now_secs + retry_secs
                    }
                })
                .min()
                .unwrap_or(DEFAULT_USAGE_LIMIT_COOLDOWN_SEC as u32);
            Duration::from_secs(u64::from(delta.max(60)))
        }
        None => Duration::from_secs(DEFAULT_USAGE_LIMIT_COOLDOWN_SEC),
    }
}

fn parse_usage_limit_retry_datetime(error: &str) -> Option<DateTime<Local>> {
    let lower = error.to_ascii_lowercase();
    let marker = "try again at";
    let start = lower.find(marker)? + marker.len();
    let original = error.get(start..)?.trim_start_matches(|ch: char| {
        ch.is_ascii_whitespace() || matches!(ch, ':' | '-' | '.' | ',')
    });
    let cleaned = clean_usage_limit_datetime_text(original);
    let naive = NaiveDateTime::parse_from_str(&cleaned, "%B %d, %Y %I:%M %p")
        .or_else(|_| NaiveDateTime::parse_from_str(&cleaned, "%b %d, %Y %I:%M %p"))
        .ok()?;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value),
        LocalResult::Ambiguous(a, b) => Some(a.min(b)),
        LocalResult::None => None,
    }
}

fn clean_usage_limit_datetime_text(value: &str) -> String {
    let mut cleaned = value
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(|ch: char| matches!(ch, '.' | ';'))
        .to_string();
    for suffix in ["st", "nd", "rd", "th"] {
        cleaned = remove_ordinal_suffix(&cleaned, suffix);
    }
    cleaned
}

fn remove_ordinal_suffix(value: &str, suffix: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.to_ascii_lowercase().find(suffix) {
        let (before, after_suffix) = rest.split_at(index);
        let after = &after_suffix[suffix.len()..];
        let previous_is_digit = before
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_ascii_digit());
        let next_is_boundary = after
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric());
        out.push_str(before);
        if !(previous_is_digit && next_is_boundary) {
            out.push_str(&after_suffix[..suffix.len()]);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn parse_usage_limit_retry_time_secs(error: &str) -> Option<u32> {
    let lower = error.to_ascii_lowercase();
    let marker = "try again at";
    let start = lower.find(marker)? + marker.len();
    let mut rest = lower[start..].trim_start_matches(|ch: char| {
        ch.is_ascii_whitespace() || matches!(ch, ':' | '-' | '.' | ',')
    });
    if rest.is_empty() {
        return None;
    }

    let hour_len = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if hour_len == 0 {
        return None;
    }
    let hour = rest[..hour_len].parse::<u32>().ok()?;
    rest = &rest[hour_len..];

    let mut minute = 0;
    if let Some(after_colon) = rest.strip_prefix(':') {
        let minute_len = after_colon
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .map(char::len_utf8)
            .sum::<usize>();
        if minute_len == 0 {
            return None;
        }
        minute = after_colon[..minute_len].parse::<u32>().ok()?;
        rest = &after_colon[minute_len..];
    }
    if minute >= 60 {
        None
    } else {
        let suffix =
            rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '.' | ','));
        let hour_24 = if suffix.starts_with("am") {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour % 12
        } else if suffix.starts_with("pm") {
            if !(1..=12).contains(&hour) {
                return None;
            }
            (hour % 12) + 12
        } else {
            if hour >= 24 {
                return None;
            }
            hour
        };
        Some(hour_24 * 60 * 60 + minute * 60)
    }
}

fn app_server_retry_remaining(run: &AppServerMemberRun) -> Option<Duration> {
    let retry_not_before = run.retry_not_before?;
    retry_not_before.checked_duration_since(Instant::now())
}

fn active_run_usage_limit_remaining(
    active: &HashMap<String, AppServerMemberRun>,
    member_name: &str,
) -> Option<Duration> {
    active.get(member_name).and_then(app_server_retry_remaining)
}

fn should_suppress_empty_department_ping_during_cooldown(
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    member_name: &str,
    has_open_tasks: bool,
    has_active_turn: bool,
) -> Option<Duration> {
    if has_open_tasks || has_active_turn {
        return None;
    }
    active_run_usage_limit_remaining(active, member_name)
        .or_else(|| active_run_usage_limit_remaining(active, &config.lead))
}

fn recent_usage_limit_retry_not_before(
    team_dir: &Path,
    member_name: &str,
) -> Result<Option<Instant>> {
    Ok(recent_usage_limit_retry_remaining(team_dir, member_name)?
        .map(|remaining| Instant::now() + remaining))
}

fn recent_usage_limit_retry_remaining(
    team_dir: &Path,
    member_name: &str,
) -> Result<Option<Duration>> {
    let auth_json = codex_core::config::find_codex_home()
        .ok()
        .map(|home| home.join("auth.json"));
    recent_usage_limit_retry_remaining_with_auth(team_dir, member_name, auth_json.as_deref())
}

fn recent_usage_limit_retry_remaining_with_auth(
    team_dir: &Path,
    member_name: &str,
    auth_json: Option<&Path>,
) -> Result<Option<Duration>> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    let member_node = usage_limit_member_node_id(team_dir, member_name)?;
    let now_utc = Utc::now();
    for event in events.into_iter().rev().take(300) {
        if event.event != "app_server_member_usage_limited" {
            continue;
        }
        if event.data.get("member").and_then(|value| value.as_str()) != Some(member_name) {
            continue;
        }
        let event_time = match DateTime::parse_from_rfc3339(&event.timestamp) {
            Ok(value) => value.with_timezone(&Utc),
            Err(_) => continue,
        };
        if auth_json_was_modified_after(auth_json, event_time)? {
            return Ok(None);
        }
        if node_device_auth_completed_after(team_dir, member_node.as_deref(), event_time)? {
            return Ok(None);
        }
        let elapsed = now_utc.signed_duration_since(event_time);
        let elapsed_secs = elapsed.num_seconds().max(0) as u64;
        let mut cooldown = event
            .data
            .get("retry_after_sec")
            .and_then(|value| value.as_u64())
            .map(Duration::from_secs);
        if let Some(error) = event.data.get("error").and_then(|value| value.as_str()) {
            let event_local = event_time.with_timezone(&Local);
            let parsed = usage_limit_cooldown_from_error_at(
                error,
                event_local,
                &[
                    event_local.time().num_seconds_from_midnight(),
                    event_time.time().num_seconds_from_midnight(),
                ],
            );
            cooldown = Some(cooldown.map_or(parsed, |existing| existing.max(parsed)));
        }
        let Some(cooldown) = cooldown else {
            continue;
        };
        if cooldown.as_secs() > elapsed_secs {
            return Ok(Some(Duration::from_secs(cooldown.as_secs() - elapsed_secs)));
        }
        return Ok(None);
    }
    Ok(None)
}

fn usage_limit_member_node_id(team_dir: &Path, member_name: &str) -> Result<Option<String>> {
    let config_path = team_dir.join("config.json");
    let Ok(config) = read_json::<TeamConfig>(&config_path) else {
        return Ok(None);
    };
    Ok(config
        .members
        .iter()
        .find(|member| member.name == member_name)
        .map(member_node_id))
}

fn node_device_auth_completed_after(
    team_dir: &Path,
    node_id: Option<&str>,
    event_time: DateTime<Utc>,
) -> Result<bool> {
    let Some(node_id) = node_id else {
        return Ok(false);
    };
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    for event in events.into_iter().rev().take(300) {
        if event.event != "node_direct_device_auth_completed"
            && event.event != "node_auth_copy_fallback_synced"
        {
            continue;
        }
        if event.data.get("node").and_then(|value| value.as_str()) != Some(node_id) {
            continue;
        }
        let event_time_auth = match DateTime::parse_from_rfc3339(&event.timestamp) {
            Ok(value) => value.with_timezone(&Utc),
            Err(_) => continue,
        };
        if event_time_auth > event_time {
            return Ok(true);
        }
        return Ok(false);
    }
    Ok(false)
}

fn auth_json_was_modified_after(
    auth_json: Option<&Path>,
    event_time: DateTime<Utc>,
) -> Result<bool> {
    let Some(auth_json) = auth_json else {
        return Ok(false);
    };
    let Ok(metadata) = fs::metadata(auth_json) else {
        return Ok(false);
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(false);
    };
    let modified_utc: DateTime<Utc> = modified.into();
    Ok(modified_utc > event_time)
}

fn member_node_id(member: &TeamMember) -> String {
    member
        .node
        .clone()
        .filter(|node| !node.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
}

fn app_server_member_cwd(node_id: &str, nodes: &[TeamNode], local_cwd: &Path) -> PathBuf {
    if node_id == "local" {
        return local_cwd.to_path_buf();
    }
    nodes
        .iter()
        .find(|node| node.id == node_id)
        .and_then(|node| node.cwd.as_deref())
        .filter(|cwd| !cwd.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn thread_key(node_id: &str, thread_id: &str) -> String {
    format!("{node_id}:{thread_id}")
}

fn text_input(text: String) -> AppServerUserInput {
    AppServerUserInput::Text {
        text,
        text_elements: Vec::new(),
    }
}

fn app_server_sandbox(
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<Option<SandboxMode>> {
    if dangerously_bypass_approvals_and_sandbox {
        return Ok(Some(SandboxMode::DangerFullAccess));
    }
    match sandbox {
        None => Ok(None),
        Some("read-only" | "readonly" | "read_only") => Ok(Some(SandboxMode::ReadOnly)),
        Some("workspace-write" | "workspace_write") => Ok(Some(SandboxMode::WorkspaceWrite)),
        Some("danger-full-access" | "danger_full_access") => {
            Ok(Some(SandboxMode::DangerFullAccess))
        }
        Some(value) => bail!("unsupported app-server sandbox mode `{value}`"),
    }
}

async fn drain_app_server_events(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    thread_to_member: &HashMap<String, String>,
    assistant_buffers: &mut HashMap<String, String>,
) -> Result<()> {
    let node_ids = node_clients.keys().cloned().collect::<Vec<_>>();
    for node_id in node_ids {
        loop {
            let Some(node_client) = node_clients.get_mut(&node_id) else {
                break;
            };
            let event = match tokio::time::timeout(
                Duration::from_millis(1),
                node_client.client.next_event(),
            )
            .await
            {
                Ok(Some(event)) => event,
                Ok(None) => {
                    if node_id == "local" {
                        bail!("app-server node `{node_id}` disconnected");
                    }
                    append_event(
                        team_dir,
                        "app_server_node_disconnected",
                        serde_json::json!({
                            "node": node_id,
                            "reason": "event stream closed",
                        }),
                    )?;
                    node_clients.remove(&node_id);
                    requeue_app_server_node_members(
                        team_dir,
                        active,
                        &node_id,
                        "app-server event stream closed; restarting node session",
                    )?;
                    remove_side_replies_for_node(team_dir, side_replies, &node_id)?;
                    break;
                }
                Err(_) => break,
            };
            if let AppServerEvent::Disconnected { message } = &event {
                if node_id == "local" {
                    bail!("app-server disconnected: {message}");
                }
                append_event(
                    team_dir,
                    "app_server_node_disconnected",
                    serde_json::json!({
                        "node": node_id,
                        "reason": message,
                    }),
                )?;
                node_clients.remove(&node_id);
                requeue_app_server_node_members(
                    team_dir,
                    active,
                    &node_id,
                    &format!("app-server disconnected: {message}; restarting node session"),
                )?;
                remove_side_replies_for_node(team_dir, side_replies, &node_id)?;
                break;
            }
            handle_app_server_event(
                &mut node_client.client,
                &node_id,
                event,
                team_dir,
                active,
                side_replies,
                thread_to_member,
                assistant_buffers,
            )
            .await?;
        }
    }
    Ok(())
}

fn requeue_app_server_node_members(
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    node_id: &str,
    reason: &str,
) -> Result<()> {
    let member_names = active
        .iter()
        .filter(|(_, run)| run.node_id == node_id && run.member.role != "lead")
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for member_name in member_names {
        active.remove(&member_name);
        set_member_status(team_dir, &member_name, MemberStatus::Online)?;
        append_event(
            team_dir,
            "app_server_member_requeued",
            serde_json::json!({
                "member": member_name,
                "node": node_id,
                "reason": reason,
            }),
        )?;
    }
    Ok(())
}

fn remove_side_replies_for_node(
    team_dir: &Path,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    node_id: &str,
) -> Result<()> {
    let removed = side_replies
        .iter()
        .filter(|(_, reply)| reply.node_id == node_id)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in removed {
        if let Some(reply) = side_replies.remove(&key) {
            append_event(
                team_dir,
                "app_server_side_channel_reply_dropped",
                serde_json::json!({
                    "member": reply.member.name,
                    "node": reply.node_id,
                    "side_thread": reply.side_thread_id,
                    "turn": reply.turn_id,
                    "reason": "node disconnected",
                }),
            )?;
        }
    }
    Ok(())
}

async fn handle_app_server_event(
    client: &mut RemoteAppServerClient,
    node_id: &str,
    event: AppServerEvent,
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    thread_to_member: &HashMap<String, String>,
    assistant_buffers: &mut HashMap<String, String>,
) -> Result<()> {
    match event {
        AppServerEvent::ServerNotification(ServerNotification::AgentMessageDelta(delta)) => {
            let key = thread_key(node_id, &delta.thread_id);
            if let Some(reply) = side_replies.get_mut(&key) {
                reply.buffer.push_str(&delta.delta);
                append_text(
                    &team_dir
                        .join("live_messages")
                        .join(format!("{}.side.md", sanitize_id(&reply.member.name))),
                    &delta.delta,
                )?;
            } else if let Some(member) = thread_to_member.get(&key) {
                if let Some(run) = active.get_mut(member) {
                    run.last_activity_at = Instant::now();
                    run.last_activity_kind = "agent_message_delta".to_string();
                }
                assistant_buffers
                    .entry(member.clone())
                    .or_default()
                    .push_str(&delta.delta);
                ingest_team_signal_lines(team_dir, member, active, assistant_buffers, false)?;
                append_text(
                    &team_dir
                        .join("live_messages")
                        .join(format!("{}.md", sanitize_id(member))),
                    &delta.delta,
                )?;
            }
        }
        AppServerEvent::ServerNotification(ServerNotification::TurnStarted(started)) => {
            let key = thread_key(node_id, &started.thread_id);
            if let Some(member) = thread_to_member.get(&key)
                && let Some(run) = active.get_mut(member)
            {
                let new_untracked_turn = reset_member_turn_buffer_if_new(
                    run,
                    assistant_buffers,
                    member,
                    &started.turn.id,
                );
                if new_untracked_turn {
                    reset_member_live_message_for_new_turn(team_dir, member, &started.turn.id)?;
                    run.usage_category = "external_turn".to_string();
                }
                run.turn_id = started.turn.id.clone();
                run.completed = false;
                run.failed = false;
                run.retry_not_before = None;
                run.last_activity_at = Instant::now();
                run.last_activity_kind = "external_turn_started".to_string();
                run.last_stale_notice_at = None;
                set_member_status(team_dir, member, MemberStatus::Running)?;
                append_event(
                    team_dir,
                    "app_server_member_external_turn_started",
                    serde_json::json!({
                        "member": member,
                        "node": node_id,
                        "thread": started.thread_id,
                        "turn": started.turn.id,
                    }),
                )?;
                if new_untracked_turn {
                    record_turn_usage_index(
                        team_dir,
                        &run.member,
                        node_id,
                        &started.thread_id,
                        &started.turn.id,
                        &run.usage_category,
                        "app_server_member_external_turn_started",
                    )?;
                }
            }
        }
        AppServerEvent::ServerNotification(ServerNotification::ThreadTokenUsageUpdated(
            notification,
        )) => {
            record_token_usage_update(
                team_dir,
                node_id,
                notification,
                active,
                side_replies,
                thread_to_member,
            )?;
        }
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(completed)) => {
            let key = thread_key(node_id, &completed.thread_id);
            if side_replies.contains_key(&key) {
                handle_app_server_side_reply_completed(team_dir, side_replies, node_id, completed)?;
            } else {
                handle_app_server_turn_completed(
                    team_dir,
                    active,
                    thread_to_member,
                    assistant_buffers,
                    node_id,
                    completed,
                )?;
            }
        }
        AppServerEvent::ServerRequest(request) => {
            reject_app_server_request(client, request).await?;
        }
        AppServerEvent::Disconnected { message } => {
            bail!("app-server disconnected: {message}");
        }
        AppServerEvent::Lagged { skipped } => {
            append_event(
                team_dir,
                "app_server_events_lagged",
                serde_json::json!({ "skipped": skipped }),
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn reset_member_turn_buffer_if_new(
    run: &mut AppServerMemberRun,
    assistant_buffers: &mut HashMap<String, String>,
    member_name: &str,
    new_turn_id: &str,
) -> bool {
    if run.turn_id == new_turn_id {
        return false;
    }
    assistant_buffers.insert(member_name.to_string(), String::new());
    run.team_message_scan_offset = 0;
    true
}

fn reset_member_live_message_for_new_turn(
    team_dir: &Path,
    member_name: &str,
    new_turn_id: &str,
) -> Result<()> {
    let live_path = team_dir
        .join("live_messages")
        .join(format!("{}.md", sanitize_id(member_name)));
    if let Ok(previous_live) = fs::read_to_string(&live_path)
        && !previous_live.trim().is_empty()
    {
        let last_path = team_dir
            .join("last_messages")
            .join(format!("{}.md", sanitize_id(member_name)));
        write_text_atomic(&last_path, &previous_live)?;
    }
    write_text_atomic(
        &live_path,
        &format!("## Turn {turn}\n\n", turn = new_turn_id),
    )?;
    Ok(())
}

fn handle_app_server_turn_completed(
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    thread_to_member: &HashMap<String, String>,
    assistant_buffers: &HashMap<String, String>,
    node_id: &str,
    completed: TurnCompletedNotification,
) -> Result<()> {
    let Some(member_name) = thread_to_member.get(&thread_key(node_id, &completed.thread_id)) else {
        return Ok(());
    };
    let Some(run) = active.get_mut(member_name) else {
        return Ok(());
    };
    run.completed = true;
    run.last_activity_at = Instant::now();
    run.last_activity_kind = "turn_completed".to_string();
    run.last_stale_notice_at = None;
    match completed.turn.status {
        TurnStatus::Completed => {
            run.retry_not_before = None;
            if run.member.role == "lead" {
                set_member_status(team_dir, member_name, MemberStatus::Online)?;
            } else if member_turn_reports_blocked(assistant_buffers, member_name)
                && member_has_active_tasks(team_dir, member_name)?
            {
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                block_member_tasks_if_active(
                    team_dir,
                    member_name,
                    "Worker turn ended while waiting on a team gate or handoff.",
                )?;
                append_event(
                    team_dir,
                    "app_server_member_blocked",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "reason": "turn output reported blocked/waiting",
                    }),
                )?;
            } else if run.standby_after_turn
                || member_status(team_dir, member_name)? == Some(MemberStatus::Standby)
            {
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                if member_has_active_tasks(team_dir, member_name)? {
                    block_member_tasks_if_active(
                        team_dir,
                        member_name,
                        "Member was moved to standby before this mission was completed.",
                    )?;
                    append_event(
                        team_dir,
                        "app_server_member_standby_blocked",
                        serde_json::json!({
                            "member": member_name,
                            "node": node_id,
                            "thread": completed.thread_id,
                            "turn": completed.turn.id,
                        }),
                    )?;
                }
                run.standby_after_turn = false;
            } else if member_has_active_tasks(team_dir, member_name)?
                && let Some(checklist_issue) = member_turn_active_task_completion_issue(
                    team_dir,
                    assistant_buffers,
                    member_name,
                )?
            {
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                block_member_tasks_if_active(
                    team_dir,
                    member_name,
                    &format!(
                        "Worker turn ended without acceptable TEAM_COMPLETION_CHECKLIST handoff evidence: {checklist_issue}."
                    ),
                )?;
                append_event(
                    team_dir,
                    "app_server_member_completion_checklist_missing",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "issue": checklist_issue,
                    }),
                )?;
            } else {
                set_member_status(team_dir, member_name, MemberStatus::Completed)?;
                complete_member_tasks_if_active(team_dir, member_name)?;
            }
            append_event(
                team_dir,
                if run.member.role == "lead" {
                    "app_server_lead_completed"
                } else {
                    "app_server_member_completed"
                },
                serde_json::json!({
                    "member": member_name,
                    "node": node_id,
                    "thread": completed.thread_id,
                    "turn": completed.turn.id,
                }),
            )?;
            acknowledge_side_channel_contexts(
                team_dir,
                member_name,
                &run.side_context_ids,
                &completed.turn.id,
            )?;
            run.side_context_ids.clear();
        }
        _ => {
            let status = format!("{:?}", completed.turn.status);
            let error = completed.turn.error.map(|err| err.message);
            if let Some(cooldown) = app_server_usage_limit_cooldown(error.as_deref()) {
                run.failed = false;
                run.retry_not_before = Some(Instant::now() + cooldown);
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                append_event(
                    team_dir,
                    "app_server_member_usage_limited",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "status": status,
                        "error": error,
                        "retry_after_sec": cooldown.as_secs(),
                    }),
                )?;
            } else {
                run.failed = true;
                run.retry_not_before = None;
                set_member_status(team_dir, member_name, MemberStatus::Failed)?;
                append_event(
                    team_dir,
                    "app_server_member_failed",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "status": status,
                        "error": error,
                    }),
                )?;
            }
        }
    }
    ingest_team_signal_lines(team_dir, member_name, active, assistant_buffers, true)?;
    Ok(())
}

fn handle_app_server_side_reply_completed(
    team_dir: &Path,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    node_id: &str,
    completed: TurnCompletedNotification,
) -> Result<()> {
    let key = thread_key(node_id, &completed.thread_id);
    let Some(reply) = side_replies.remove(&key) else {
        return Ok(());
    };
    let elapsed = reply.started_at.elapsed().as_secs();
    match completed.turn.status {
        TurnStatus::Completed => {
            let language = load_config(team_dir)?.language.unwrap_or_default();
            let body = side_reply_message_body(&reply, language);
            if body.trim().is_empty() {
                append_event(
                    team_dir,
                    "app_server_side_channel_reply_empty",
                    serde_json::json!({
                        "member": reply.member.name,
                        "node": reply.node_id,
                        "source_thread": reply.source_thread_id,
                        "side_thread": reply.side_thread_id,
                        "turn": completed.turn.id,
                        "elapsed_sec": elapsed,
                    }),
                )?;
                return Ok(());
            }
            for recipient in &reply.recipients {
                send_team_message_to_dir(team_dir, &reply.member.name, recipient, &body)?;
            }
            let handoff = if language.is_ja() {
                format!(
                    "Side-channel reply: あなたの main turn が busy の間に短い返信を送りました。\n\n宛先: {}\n\n処理した受信 message:\n{}\n\n送信した返信:\n{}\n\nこの返信で生じた約束や制約を、実務上可能なタイミングで main work に取り込んでください。action が必要な場合以外は重複 chat を避けてください。",
                    reply.recipients.join(", "),
                    summarize_side_reply_messages(&reply.messages, language),
                    body
                )
            } else {
                format!(
                    "Side-channel reply sent while your main turn was busy.\n\nRecipients: {}\n\nIncoming messages handled:\n{}\n\nReply sent:\n{}\n\nIncorporate any resulting commitments or constraints into your main work when practical. Avoid duplicate chat unless action is needed.",
                    reply.recipients.join(", "),
                    summarize_side_reply_messages(&reply.messages, language),
                    body
                )
            };
            record_side_channel_context(
                team_dir,
                &reply,
                completed.turn.id.clone(),
                &body,
                language,
            )?;
            send_team_message_to_dir(team_dir, "system", &reply.member.name, &handoff)?;
            append_event(
                team_dir,
                "app_server_side_channel_reply_completed",
                serde_json::json!({
                    "member": reply.member.name,
                    "node": reply.node_id,
                    "source_thread": reply.source_thread_id,
                    "side_thread": reply.side_thread_id,
                    "turn": completed.turn.id,
                    "recipients": reply.recipients,
                    "messages": reply.messages.len(),
                    "elapsed_sec": elapsed,
                }),
            )?;
        }
        _ => {
            append_event(
                team_dir,
                "app_server_side_channel_reply_failed",
                serde_json::json!({
                    "member": reply.member.name,
                    "node": reply.node_id,
                    "source_thread": reply.source_thread_id,
                    "side_thread": reply.side_thread_id,
                    "turn": completed.turn.id,
                    "status": format!("{:?}", completed.turn.status),
                    "error": completed.turn.error.map(|err| err.message),
                    "elapsed_sec": elapsed,
                }),
            )?;
        }
    }
    Ok(())
}

fn side_channel_context_path(team_dir: &Path, member_name: &str) -> PathBuf {
    team_dir
        .join("side_channel_contexts")
        .join(format!("{}.jsonl", sanitize_id(member_name)))
}

fn record_side_channel_context(
    team_dir: &Path,
    reply: &AppServerSideReply,
    side_turn: String,
    body: &str,
    language: TeamPromptLanguage,
) -> Result<()> {
    let id = sanitize_id(&format!(
        "sidectx-{}-{}-{}",
        reply.member.name, reply.side_thread_id, side_turn
    ));
    let record = SideChannelContextRecord {
        id: id.clone(),
        member: reply.member.name.clone(),
        node: reply.node_id.clone(),
        source_thread: reply.source_thread_id.clone(),
        side_thread: reply.side_thread_id.clone(),
        side_turn,
        recipients: reply.recipients.clone(),
        incoming_summary: summarize_side_reply_messages(&reply.messages, language),
        reply: body.to_string(),
        created_at: now(),
        status: SideChannelContextStatus::Pending,
        injected_turns: Vec::new(),
        injected_at: None,
        acknowledged_at: None,
    };
    append_jsonl(
        &side_channel_context_path(team_dir, &reply.member.name),
        &record,
    )?;
    append_event(
        team_dir,
        "side_channel_context_pending",
        serde_json::json!({
            "member": reply.member.name,
            "node": reply.node_id,
            "source_thread": reply.source_thread_id,
            "side_thread": reply.side_thread_id,
            "side_turn": record.side_turn.clone(),
            "context_id": id,
            "recipients": reply.recipients.clone(),
        }),
    )?;
    Ok(())
}

fn pending_side_channel_contexts_for_turn(
    team_dir: &Path,
    member_name: &str,
    turn_id: &str,
) -> Result<Vec<SideChannelContextRecord>> {
    Ok(
        read_jsonl::<SideChannelContextRecord>(&side_channel_context_path(team_dir, member_name))?
            .into_iter()
            .filter(|record| {
                record.status != SideChannelContextStatus::Acknowledged
                    && !record.injected_turns.iter().any(|id| id == turn_id)
            })
            .collect(),
    )
}

fn append_side_channel_context_prompt(
    team_dir: &Path,
    member_name: &str,
    turn_id: &str,
    prompt: String,
    language: TeamPromptLanguage,
) -> Result<(String, Vec<String>)> {
    let mut contexts = pending_side_channel_contexts_for_turn(team_dir, member_name, turn_id)?;
    if contexts.is_empty() {
        return Ok((prompt, Vec::new()));
    }
    let omitted_contexts = contexts
        .len()
        .saturating_sub(MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT);
    if omitted_contexts > 0 {
        contexts = contexts
            .into_iter()
            .rev()
            .take(MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT)
            .collect::<Vec<_>>();
        contexts.reverse();
    }
    let mut out = prompt;
    if language.is_ja() {
        out.push_str("\n\nあなたの main turn に未反映の side-channel context があります:\n");
        out.push_str(
            "この main thread が busy の間に、あなた名義で短い side-channel 返信が送信されました。team message で明示的に訂正しない限り、team に見える約束や制約として扱ってください。\n",
        );
        out.push_str(
            "続行する前に、現在の plan と artifact をこれらの side-channel commitment と突き合わせてください。停止、fail closed、claim scope 変更、evidence 保持、handoff 更新などを約束している場合は、その更新を実施して機械可読 artifact/manifest を検証するか、理由付きで撤回/訂正する team message を直ちに送ってください。side-channel commitment と矛盾する古い artifact に依存したり、handoff/task 完了をしないでください。\n",
        );
        if omitted_contexts > 0 {
            out.push_str(&format!(
                "トークン節約のため、古い side-channel context {} 件はこの prompt では本文を省略しています。必要なら team state の side_channel_contexts/{}.jsonl を参照してください。\n",
                omitted_contexts,
                sanitize_id(member_name)
            ));
        }
    } else {
        out.push_str("\n\nPending side-channel context for your main turn:\n");
        out.push_str(
            "The following fast side-channel replies were sent as you while this main thread was busy. Treat them as team-visible commitments or constraints unless you explicitly correct them with a team message.\n",
        );
        out.push_str(
            "Before you continue, reconcile your current plan and artifacts against these side-channel commitments. If a side-channel reply promised to stop, fail closed, change claim scope, preserve evidence, or update a handoff, you must either perform that update and verify the resulting machine-readable artifacts/manifests, or immediately send a team message explicitly retracting/correcting the side-channel reply with the reason. Do not hand off, complete a task, or rely on stale artifacts that contradict a side-channel commitment.\n",
        );
        if omitted_contexts > 0 {
            out.push_str(&format!(
                "To keep this turn compact, {} older side-channel context record(s) are omitted from this prompt body. Inspect side_channel_contexts/{}.jsonl in the team state only if those older commitments are relevant.\n",
                omitted_contexts,
                sanitize_id(member_name)
            ));
        }
    }
    for context in &contexts {
        if language.is_ja() {
            out.push_str(&format!(
                "\n[{}]\n宛先: {}\n処理した受信 message:\n{}\nすでに送信済みの返信:\n{}\n",
                context.id,
                context.recipients.join(", "),
                context.incoming_summary,
                context.reply
            ));
        } else {
            out.push_str(&format!(
                "\n[{}]\nRecipients: {}\nIncoming handled:\n{}\nReply already sent:\n{}\n",
                context.id,
                context.recipients.join(", "),
                context.incoming_summary,
                context.reply
            ));
        }
    }
    if language.is_ja() {
        out.push_str("\n続行前に、これらの制約を取り込み、検証してください。\n");
    } else {
        out.push_str("\nIncorporate and verify these constraints before continuing.\n");
    }
    Ok((
        out,
        contexts.into_iter().map(|context| context.id).collect(),
    ))
}

fn mark_side_channel_contexts_injected(
    team_dir: &Path,
    member_name: &str,
    context_ids: &[String],
    turn_id: &str,
) -> Result<()> {
    if context_ids.is_empty() {
        return Ok(());
    }
    let path = side_channel_context_path(team_dir, member_name);
    let mut records = read_jsonl::<SideChannelContextRecord>(&path)?;
    let mut changed = false;
    for record in &mut records {
        if context_ids.iter().any(|id| id == &record.id) {
            record.status = SideChannelContextStatus::Injected;
            if !record.injected_turns.iter().any(|id| id == turn_id) {
                record.injected_turns.push(turn_id.to_string());
            }
            record.injected_at = Some(now());
            changed = true;
        }
    }
    if changed {
        write_jsonl_atomic(&path, &records)?;
        append_event(
            team_dir,
            "side_channel_context_injected",
            serde_json::json!({
                "member": member_name,
                "turn": turn_id,
                "context_ids": context_ids,
            }),
        )?;
    }
    Ok(())
}

fn acknowledge_side_channel_contexts(
    team_dir: &Path,
    member_name: &str,
    context_ids: &[String],
    turn_id: &str,
) -> Result<()> {
    if context_ids.is_empty() {
        return Ok(());
    }
    let path = side_channel_context_path(team_dir, member_name);
    let mut records = read_jsonl::<SideChannelContextRecord>(&path)?;
    let mut acknowledged = Vec::new();
    for record in &mut records {
        if context_ids.iter().any(|id| id == &record.id)
            && record.status != SideChannelContextStatus::Acknowledged
        {
            record.status = SideChannelContextStatus::Acknowledged;
            record.acknowledged_at = Some(now());
            acknowledged.push(record.id.clone());
        }
    }
    if !acknowledged.is_empty() {
        write_jsonl_atomic(&path, &records)?;
        append_event(
            team_dir,
            "side_channel_context_acknowledged",
            serde_json::json!({
                "member": member_name,
                "turn": turn_id,
                "context_ids": acknowledged,
            }),
        )?;
    }
    Ok(())
}

fn merge_side_context_ids(run: &mut AppServerMemberRun, context_ids: &[String]) {
    for context_id in context_ids {
        if !run.side_context_ids.iter().any(|id| id == context_id) {
            run.side_context_ids.push(context_id.clone());
        }
    }
}

fn side_reply_message_body(reply: &AppServerSideReply, language: TeamPromptLanguage) -> String {
    let cleaned = strip_side_channel_completion_checklist(&reply.buffer);
    let body = cleaned.trim();
    if body.is_empty() {
        return String::new();
    }
    if language.is_ja() {
        format!(
            "@{} からの side-channel 速報返信です。main turn は継続中です:\n\n{}",
            reply.member.name, body
        )
    } else {
        format!(
            "Quick side-channel reply from @{} while my main turn continues:\n\n{}",
            reply.member.name, body
        )
    }
}

fn strip_side_channel_completion_checklist(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let Some(idx) = lower.find("team_completion_checklist:") else {
        return message.to_string();
    };
    message[..idx].trim_end().to_string()
}

fn summarize_side_reply_messages(messages: &[MailMessage], language: TeamPromptLanguage) -> String {
    let budget = reactive_prompt_message_budget(messages);
    let omitted = messages.len().saturating_sub(budget.max_messages);
    let mut lines = Vec::new();
    if omitted > 0 {
        lines.push(if language.is_ja() {
            format!("(古い side-channel message {} 件を省略)", omitted)
        } else {
            format!("({} older side-channel message(s) omitted)", omitted)
        });
    }
    messages
        .iter()
        .skip(omitted)
        .map(|message| {
            if language.is_ja() {
                format!(
                    "- @{} から {}: {}",
                    message.from,
                    message.timestamp,
                    compact_prompt_message(&message.message, budget.max_chars.min(420))
                )
            } else {
                format!(
                    "- from @{} at {}: {}",
                    message.from,
                    message.timestamp,
                    compact_prompt_message(&message.message, budget.max_chars.min(420))
                )
            }
        })
        .for_each(|line| lines.push(line));
    lines.join("\n")
}

fn compact_one_line(value: &str, max_chars: usize) -> String {
    let mut compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn compact_prompt_message(value: &str, max_chars: usize) -> String {
    let compact = compact_one_line(value, max_chars);
    if !compact.ends_with("...") {
        return compact;
    }
    let refs = extract_prompt_message_refs(value);
    if refs.is_empty() {
        return compact;
    }
    format!("{compact} refs: {}", refs.join(", "))
}

fn extract_prompt_message_refs(value: &str) -> Vec<String> {
    let mut refs = Vec::<String>::new();
    for raw in value.split_whitespace() {
        let token = raw.trim_matches(|ch: char| {
            matches!(
                ch,
                ',' | ';'
                    | ':'
                    | '"'
                    | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '<'
                    | '>'
                    | '`'
            )
        });
        let lower = token.to_ascii_lowercase();
        let keep = token.starts_with('/')
            || lower.starts_with("wait-")
            || lower.starts_with("job-")
            || lower.starts_with("task-")
            || lower.starts_with("task:")
            || lower.starts_with("rc=")
            || lower.starts_with("exit=")
            || lower.ends_with(".md")
            || lower.ends_with(".json")
            || lower.ends_with(".jsonl")
            || lower.ends_with(".log")
            || lower.ends_with(".txt")
            || lower.contains("manifest")
            || lower.contains("evidence");
        if keep && !refs.iter().any(|existing| existing == token) {
            refs.push(token.to_string());
        }
        if refs.len() >= 8 {
            break;
        }
    }
    refs
}

#[derive(Clone, Copy)]
struct ReactivePromptMessageBudget {
    max_messages: usize,
    max_chars: usize,
}

fn reactive_prompt_message_budget(messages: &[MailMessage]) -> ReactivePromptMessageBudget {
    let category = usage_category_for_messages("team_message", messages);
    match category.as_str() {
        "team_review_request" | "team_review_response" | "team_audit_review" => {
            ReactivePromptMessageBudget {
                max_messages: 6,
                max_chars: 520,
            }
        }
        "team_failure_blocker" | "team_dependency_gate" | "team_blocker" => {
            ReactivePromptMessageBudget {
                max_messages: 6,
                max_chars: 560,
            }
        }
        "team_wait_status" | "team_job_status" | "team_noop_stay" | "team_status" => {
            ReactivePromptMessageBudget {
                max_messages: 5,
                max_chars: 360,
            }
        }
        "team_artifact_plan" => ReactivePromptMessageBudget {
            max_messages: 5,
            max_chars: 420,
        },
        "team_lead_proposal"
        | "team_debate_request"
        | "team_debate_response"
        | "team_decision_record" => ReactivePromptMessageBudget {
            max_messages: 8,
            max_chars: 620,
        },
        "team_final_handoff" | "team_artifact_handoff" | "team_handoff" => {
            ReactivePromptMessageBudget {
                max_messages: 8,
                max_chars: 640,
            }
        }
        _ => ReactivePromptMessageBudget {
            max_messages: MAX_REACTIVE_PROMPT_MESSAGES,
            max_chars: MAX_REACTIVE_PROMPT_MESSAGE_CHARS,
        },
    }
}

fn format_mail_messages_for_reactive_prompt(
    messages: &[MailMessage],
    language: TeamPromptLanguage,
) -> String {
    format_mail_messages_for_prompt_with_budget(
        messages,
        language,
        reactive_prompt_message_budget(messages),
    )
}

fn format_mail_messages_for_prompt_with_budget(
    messages: &[MailMessage],
    language: TeamPromptLanguage,
    budget: ReactivePromptMessageBudget,
) -> String {
    let omitted = messages.len().saturating_sub(budget.max_messages);
    let selected = messages
        .iter()
        .skip(omitted)
        .map(|message| {
            format!(
                "- [{}] {} -> {}: {}",
                message.timestamp,
                message.from,
                message.to,
                compact_prompt_message(&message.message, budget.max_chars)
            )
        })
        .collect::<Vec<_>>();
    let mut out = String::new();
    if omitted > 0 {
        if language.is_ja() {
            out.push_str(&format!(
                "(トークン節約のため、古い message {} 件は省略しています。必要なら `team inbox` や mailbox jsonl を参照してください。)\n",
                omitted
            ));
        } else {
            out.push_str(&format!(
                "(To keep this turn compact, {} older message(s) are omitted. Inspect `team inbox` or the mailbox jsonl if needed.)\n",
                omitted
            ));
        }
    }
    out.push_str(&selected.join("\n"));
    out
}

fn member_turn_reports_blocked(
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> bool {
    let Some(text) = assistant_buffers.get(member_name) else {
        return false;
    };
    let tail = text
        .chars()
        .rev()
        .take(5000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    let blocked_markers = [
        "blocked on",
        "blocked by",
        "waiting on",
        "waiting for",
        "wait for",
        "blocked until",
        "blocked pending",
        "pending lead clearance",
        "pending explicit lead",
        "until explicit lead",
        "requires lead clearance",
        "require lead clearance",
        "awaiting lead clearance",
        "holding until",
        "hold until",
        "paused until",
        "gate wait",
        "gate remains",
        "remains gated",
        "not started",
        "no model-specific",
        "handoff待ち",
        "結果待ち",
        "研究待ち",
        "ゲート待ち",
        "未着",
        "待機",
    ];
    blocked_markers.iter().any(|marker| tail.contains(marker))
}

#[cfg(test)]
fn member_turn_has_completion_checklist(
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> bool {
    member_turn_completion_checklist_issue(assistant_buffers, member_name).is_none()
}

fn member_turn_completion_checklist_issue(
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> Option<String> {
    let Some(text) = assistant_buffers.get(member_name) else {
        return Some("no assistant output was captured".to_string());
    };
    let tail = text
        .chars()
        .rev()
        .take(8000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    if !tail.contains("team_completion_checklist:") {
        return Some("missing TEAM_COMPLETION_CHECKLIST marker".to_string());
    }
    for field in [
        "artifacts:",
        "verification:",
        "messages_sent:",
        "consumers_notified:",
        "blockers_or_limits:",
    ] {
        if !tail.contains(field) {
            return Some(format!("missing `{field}` field"));
        }
    }
    let messages_sent = checklist_field_value(&tail, "messages_sent:");
    if checklist_value_is_empty_or_unknown(messages_sent.as_deref()) {
        return Some(
            "messages_sent is empty/unknown; final handoff message was not evidenced".to_string(),
        );
    }
    if checklist_value_has_unresolved_marker(messages_sent.as_deref()) {
        return Some(
            "messages_sent still contains pending/unresolved language; final handoff message was not completed".to_string(),
        );
    }
    let consumers_notified = checklist_field_value(&tail, "consumers_notified:");
    if checklist_value_is_empty_or_unknown(consumers_notified.as_deref()) {
        return Some(
            "consumers_notified is empty/unknown; artifact consumers were not evidenced"
                .to_string(),
        );
    }
    if checklist_value_has_unresolved_marker(consumers_notified.as_deref()) {
        return Some(
            "consumers_notified still contains pending/unresolved language; artifact consumers were not actually notified"
                .to_string(),
        );
    }
    None
}

fn member_turn_active_task_completion_issue(
    team_dir: &Path,
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> Result<Option<String>> {
    if let Some(issue) = member_turn_completion_checklist_issue(assistant_buffers, member_name) {
        return Ok(Some(issue));
    }
    let Some(text) = assistant_buffers.get(member_name) else {
        return Ok(Some("no assistant output was captured".to_string()));
    };
    let tail = text
        .chars()
        .rev()
        .take(12000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    let checklist = tail
        .rfind("team_completion_checklist:")
        .map(|idx| &tail[idx..])
        .unwrap_or(&tail);
    if checklist_value_is_empty_or_unknown(
        checklist_field_value(checklist, "artifacts:").as_deref(),
    ) {
        return Ok(Some(
            "artifacts is empty/unknown; final handoff artifacts were not evidenced".to_string(),
        ));
    }
    let verification = checklist_field_value(checklist, "verification:");
    if checklist_value_is_empty_or_unknown(verification.as_deref()) {
        return Ok(Some(
            "verification is empty/unknown; final handoff verification was not evidenced"
                .to_string(),
        ));
    }
    if checklist_value_has_unresolved_marker(verification.as_deref()) {
        return Ok(Some(
            "verification still contains pending/unresolved language; final verification is not complete"
                .to_string(),
        ));
    }

    let tasks = load_tasks(team_dir)?;
    for task in tasks.into_iter().filter(|task| {
        task.owner.as_deref() == Some(member_name)
            && matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Review
            )
    }) {
        let required = task_required_declared_non_local_output_paths(team_dir, &task)?;
        if required.is_empty() {
            continue;
        }
        let missing = required
            .iter()
            .filter(|path| !checklist.contains(&path.to_ascii_lowercase()))
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Ok(Some(format!(
                "final checklist does not cite required non-local task output path(s): {}",
                missing.join(", ")
            )));
        }
    }
    Ok(None)
}

fn checklist_field_value(text: &str, field: &str) -> Option<String> {
    let start = text.find(field)? + field.len();
    let rest = &text[start..];
    let mut values = Vec::new();
    let mut lines = rest.lines();
    if let Some(first) = lines.next() {
        let first = normalize_checklist_field_line(first);
        if !first.is_empty() {
            values.push(first);
        }
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if values.is_empty() {
                continue;
            }
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        let normalized = lower.strip_prefix("- ").unwrap_or(&lower);
        if [
            "artifacts:",
            "verification:",
            "messages_sent:",
            "consumers_notified:",
            "blockers_or_limits:",
        ]
        .iter()
        .any(|known| normalized.starts_with(known))
        {
            break;
        }
        if line.starts_with(char::is_whitespace) || trimmed.starts_with('-') {
            values.push(normalize_checklist_field_line(line));
        } else {
            break;
        }
    }
    Some(
        values
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn normalize_checklist_field_line(line: &str) -> String {
    line.trim().trim_start_matches('-').trim().to_string()
}

fn checklist_value_is_empty_or_unknown(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let value = value.to_ascii_lowercase();
    matches!(
        value.as_str(),
        "none" | "n/a" | "na" | "unknown" | "missing" | "tbd" | "not sure"
    ) || ["none", "n/a", "na", "unknown", "missing", "tbd", "not sure"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn checklist_value_has_unresolved_marker(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let value = value.to_ascii_lowercase();
    [
        "pending",
        "not yet",
        "not sent",
        "not notified",
        "to be sent",
        "to be generated",
        "after manifest generation",
        "awaiting",
        "未送信",
        "未通知",
        "未完了",
        "未生成",
        "保留",
        "待ち",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

fn member_has_active_tasks(team_dir: &Path, member_name: &str) -> Result<bool> {
    Ok(load_tasks(team_dir)?.iter().any(|task| {
        task.owner.as_deref() == Some(member_name)
            && matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)
    }))
}

fn ingest_team_signal_lines(
    team_dir: &Path,
    member_name: &str,
    active: &mut HashMap<String, AppServerMemberRun>,
    assistant_buffers: &HashMap<String, String>,
    final_flush: bool,
) -> Result<()> {
    let Some(run) = active.get_mut(member_name) else {
        return Ok(());
    };
    let Some(buffer) = assistant_buffers.get(member_name) else {
        return Ok(());
    };
    let offset = run.team_message_scan_offset.min(buffer.len());
    let new_text = &buffer[offset..];
    let scan_end = if final_flush {
        buffer.len()
    } else {
        let complete_len = new_text.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
        offset + complete_len
    };
    if scan_end <= offset {
        return Ok(());
    }
    let new_text = &buffer[offset..scan_end];
    run.team_message_scan_offset = scan_end;
    let config = load_config(team_dir)?;
    for line in new_text.lines() {
        let Some((to, message)) = parse_team_message_line(line) else {
            continue;
        };
        let recipients = resolve_message_recipients(&config, member_name, &to)?;
        for recipient in &recipients {
            let msg = MailMessage {
                from: member_name.to_string(),
                to: recipient.clone(),
                message: message.clone(),
                timestamp: now(),
                read: false,
            };
            append_jsonl(&mailbox_path(team_dir, &msg.to), &msg)?;
        }
        append_event(
            team_dir,
            "team_message_ingested",
            serde_json::json!({
                "from": member_name,
                "to": recipients,
                "message": message,
                "source": "assistant_text",
            }),
        )?;
    }
    for line in new_text.lines() {
        let task_update = match parse_team_task_line(line) {
            Ok(Some(task_update)) => task_update,
            Ok(None) => continue,
            Err(err) => {
                append_event(
                    team_dir,
                    "team_task_parse_failed",
                    serde_json::json!({
                        "from": member_name,
                        "line": line.trim().chars().take(500).collect::<String>(),
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
                continue;
            }
        };
        let changed = set_task_status_if_open(
            team_dir,
            &task_update.id,
            task_update.status,
            task_update.result.as_deref(),
        )?;
        append_event(
            team_dir,
            "team_task_ingested",
            serde_json::json!({
                "from": member_name,
                "task": task_update.id,
                "status": task_update.status.to_string(),
                "result": task_update.result,
                "changed": changed,
                "source": "assistant_text",
            }),
        )?;
    }
    for line in new_text.lines() {
        let wait_update = match parse_team_wait_line(line) {
            Ok(Some(wait_update)) => wait_update,
            Ok(None) => continue,
            Err(err) => {
                append_event(
                    team_dir,
                    "team_wait_parse_failed",
                    serde_json::json!({
                        "from": member_name,
                        "line": line.trim().chars().take(500).collect::<String>(),
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
                continue;
            }
        };
        match ingest_team_wait_fallback(team_dir, member_name, wait_update) {
            Ok(wait_id) => append_event(
                team_dir,
                "team_wait_ingested",
                serde_json::json!({
                    "from": member_name,
                    "wait": wait_id,
                    "source": "assistant_text",
                }),
            )?,
            Err(err) => append_event(
                team_dir,
                "team_wait_ingest_failed",
                serde_json::json!({
                    "from": member_name,
                    "line": line.trim().chars().take(500).collect::<String>(),
                    "error": err.to_string(),
                    "source": "assistant_text",
                }),
            )?,
        }
    }
    for line in new_text.lines() {
        let node_args = match parse_team_node_line(line) {
            Ok(Some(node_args)) => node_args,
            Ok(None) => continue,
            Err(err) => {
                append_event(
                    team_dir,
                    "team_node_parse_failed",
                    serde_json::json!({
                        "from": member_name,
                        "line": line.trim().chars().take(500).collect::<String>(),
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
                continue;
            }
        };
        let node_id = node_args.id.clone();
        match add_team_node(team_dir, node_args) {
            Ok(()) => {
                ensure_container_node_departments(team_dir)?;
                append_event(
                    team_dir,
                    "team_node_ingested",
                    serde_json::json!({
                        "from": member_name,
                        "node": node_id,
                        "source": "assistant_text",
                    }),
                )?;
            }
            Err(err) => {
                append_event(
                    team_dir,
                    "team_node_ingest_failed",
                    serde_json::json!({
                        "from": member_name,
                        "node": node_id,
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
            }
        }
    }
    Ok(())
}

struct TeamTaskFallback {
    id: String,
    status: TaskStatus,
    result: Option<String>,
}

struct TeamWaitFallback {
    id: Option<String>,
    title: String,
    status: TeamWaitStatus,
    task_id: Option<String>,
    condition: String,
    progress: String,
    evidence: Option<String>,
}

fn parse_team_message_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let marker = line.find("TEAM_MESSAGE ")?;
    let rest = &line[marker + "TEAM_MESSAGE ".len()..];
    let rest = rest.strip_prefix("to=")?;
    let (to, message) = rest.split_once(':')?;
    let to = to.trim().to_string();
    let message = message.trim();
    if to.is_empty() || message.is_empty() {
        return None;
    }
    Some((to, message.to_string()))
}

fn parse_team_task_line(line: &str) -> Result<Option<TeamTaskFallback>> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("TEAM_TASK ") else {
        return Ok(None);
    };
    let (fields_text, result) = match rest.split_once(" result=") {
        Some((fields, result)) => (
            fields,
            Some(
                result
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            ),
        ),
        None => (rest, None),
    };
    let mut fields = HashMap::<String, String>::new();
    for token in fields_text.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let id = fields
        .remove("id")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_TASK needs id=<task-id>")?;
    if id.contains('<') || id.contains('>') {
        bail!("TEAM_TASK id must be concrete, not a placeholder");
    }
    let status = fields
        .remove("status")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_TASK needs status=<status>")?;
    Ok(Some(TeamTaskFallback {
        id,
        status: parse_task_status(&status)?,
        result: result.filter(|value| !value.trim().is_empty()),
    }))
}

fn parse_team_wait_line(line: &str) -> Result<Option<TeamWaitFallback>> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("TEAM_WAIT ") else {
        return Ok(None);
    };
    let parts = rest.split(" | ").collect::<Vec<_>>();
    let head = parts.first().copied().unwrap_or_default();
    let mut fields = HashMap::<String, String>::new();
    for token in head.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    for part in parts.into_iter().skip(1) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let title = fields
        .remove("title")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "external wait".to_string());
    let status = fields
        .remove("status")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "waiting".to_string());
    let id = fields.remove("id").filter(|value| !value.trim().is_empty());
    if id
        .as_deref()
        .is_some_and(|id| id.contains('<') || id.contains('>'))
    {
        bail!("TEAM_WAIT id must be concrete, not a placeholder");
    }
    let task_id = fields
        .remove("task")
        .filter(|value| !value.trim().is_empty());
    if task_id
        .as_deref()
        .is_some_and(|task| task.contains('<') || task.contains('>'))
    {
        bail!("TEAM_WAIT task must be concrete, not a placeholder");
    }
    Ok(Some(TeamWaitFallback {
        id,
        title,
        status: parse_wait_status(&status)?,
        task_id,
        condition: fields.remove("condition").unwrap_or_default(),
        progress: fields.remove("progress").unwrap_or_default(),
        evidence: fields
            .remove("evidence")
            .filter(|value| !value.trim().is_empty()),
    }))
}

fn ingest_team_wait_fallback(
    team_dir: &Path,
    member_name: &str,
    wait_update: TeamWaitFallback,
) -> Result<String> {
    let config = load_config(team_dir)?;
    ensure_member_exists(&config, member_name)?;
    if let Some(wait_id) = wait_update.id.as_deref()
        && wait_path(team_dir, wait_id).exists()
    {
        set_team_wait(
            team_dir,
            WaitSetArgs {
                id: wait_id.to_string(),
                status: Some(wait_update.status),
                progress: Some(wait_update.progress),
                evidence: wait_update.evidence,
                clear_evidence: false,
            },
        )?;
        return Ok(wait_id.to_string());
    }

    let id = wait_update
        .id
        .unwrap_or_else(|| allocate_wait_id(team_dir).unwrap_or_else(|_| "wait-1".to_string()));
    if wait_path(team_dir, &id).exists() {
        bail!("wait `{id}` already exists");
    }
    if let Some(task_id) = wait_update.task_id.as_deref() {
        let tasks = load_tasks(team_dir)?;
        if !tasks.iter().any(|task| task.id == task_id) {
            bail!("task `{task_id}` does not exist");
        }
        record_task_wait_registration(team_dir, task_id, &id, &wait_update.title)?;
    }
    let now = now();
    let wait = TeamWait {
        id: id.clone(),
        title: wait_update.title,
        owner: Some(member_name.to_string()),
        task_id: wait_update.task_id,
        node: None,
        condition: wait_update.condition,
        status: wait_update.status,
        progress: wait_update.progress,
        evidence: wait_update.evidence,
        created_at: now.clone(),
        updated_at: now,
    };
    fs::create_dir_all(waits_dir(team_dir))?;
    write_json_atomic(&wait_path(team_dir, &id), &wait)?;
    Ok(id)
}

fn parse_team_node_line(line: &str) -> Result<Option<NodeAddArgs>> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("TEAM_NODE ") else {
        return Ok(None);
    };
    let mut fields = HashMap::<String, String>::new();
    for token in rest.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let id = fields
        .remove("id")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_NODE needs id=<node-id>")?;
    if id.contains('<') || id.contains('>') {
        bail!("TEAM_NODE id must be concrete, not a placeholder");
    }
    let kind = match fields
        .remove("kind")
        .unwrap_or_else(|| "docker".to_string())
        .replace('_', "-")
        .as_str()
    {
        "docker" => TeamNodeKind::Docker,
        "ssh-docker" => TeamNodeKind::SshDocker,
        other => bail!("TEAM_NODE unsupported kind `{other}`"),
    };
    let host = fields
        .remove("host")
        .filter(|value| !value.is_empty() && value != "-");
    if matches!(kind, TeamNodeKind::SshDocker) && host.is_none() {
        bail!("TEAM_NODE kind=ssh-docker needs host=<ssh-host>");
    }
    let container = fields
        .remove("container")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_NODE needs container=<container-name>")?;
    if container.contains('<') || container.contains('>') {
        bail!("TEAM_NODE container must be concrete, not a placeholder");
    }
    let cwd = fields
        .remove("cwd")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some("/workspace".to_string()));
    let note = fields
        .remove("note")
        .unwrap_or_else(|| "Docker node reported by a team department.".to_string())
        .replace('_', " ");
    Ok(Some(NodeAddArgs {
        id,
        kind,
        url: None,
        host,
        container: Some(container),
        cwd,
        note,
    }))
}

async fn reject_app_server_request(
    client: &mut RemoteAppServerClient,
    request: ServerRequest,
) -> Result<()> {
    let request_id = request.id().clone();
    client
        .reject_server_request(
            request_id,
            JSONRPCErrorError {
                code: -32000,
                message: "codex team app-server mode does not handle interactive approvals; rerun with --dangerously-bypass-approvals-and-sandbox or a non-interactive permission profile".to_string(),
                data: None,
            },
        )
        .await
        .context("reject app-server server request")
}

#[allow(clippy::too_many_arguments)]
async fn sync_dynamic_app_server_members(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    nodes: &[TeamNode],
    team_dir: &Path,
    config: &mut TeamConfig,
    active: &mut HashMap<String, AppServerMemberRun>,
    thread_to_member: &mut HashMap<String, String>,
    assistant_buffers: &mut HashMap<String, String>,
    mailbox_counts: &mut HashMap<String, usize>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    cwd: &Path,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    codex_exe: &Path,
    relay_port: u16,
    language: TeamPromptLanguage,
) -> Result<()> {
    let latest = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    'member_loop: for member in latest.members.iter().filter(|member| member.role != "lead") {
        if !matches!(member.status, MemberStatus::Online | MemberStatus::Running) {
            continue;
        }
        let has_active_task = tasks.iter().any(|task| {
            task.owner.as_deref() == Some(member.name.as_str())
                && task_status_can_start_turn(task.status)
        });
        if !has_active_task {
            continue;
        }
        if let Some(remaining) = recent_usage_limit_retry_remaining(team_dir, &member.name)? {
            append_event(
                team_dir,
                "app_server_dynamic_member_start_deferred",
                serde_json::json!({
                    "member": member.name,
                    "node": member_node_id(member),
                    "reason": "recent app-server/model usage-limit cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            set_member_status(team_dir, &member.name, MemberStatus::Standby)?;
            continue;
        }
        if let Some(existing) = active.get(&member.name) {
            if !existing.completed {
                continue;
            }
            let old_node_id = existing.node_id.clone();
            let old_thread_id = existing.thread_id.clone();
            let old_turn_id = existing.turn_id.clone();
            thread_to_member.remove(&thread_key(&old_node_id, &old_thread_id));
            assistant_buffers.remove(&member.name);
            active.remove(&member.name);
            append_event(
                team_dir,
                "app_server_completed_member_restarting",
                serde_json::json!({
                    "member": member.name,
                    "old_node": old_node_id,
                    "old_thread": old_thread_id,
                    "old_turn": old_turn_id,
                    "reason": "member is online/running with unfinished assigned task",
                }),
            )?;
        }

        set_member_status(team_dir, &member.name, MemberStatus::Running)?;
        mark_member_tasks(team_dir, &member.name, TaskStatus::InProgress)?;
        let node_id = member_node_id(member);
        if !node_clients.contains_key(&node_id) {
            let node = nodes
                .iter()
                .find(|node| node.id == node_id)
                .cloned()
                .with_context(|| format!("node `{node_id}` is not registered"))?;
            let (url, process) = match resolve_or_spawn_node_app_server(team_dir, &node, relay_port)
            {
                Ok(result) => result,
                Err(err) => {
                    append_event(
                        team_dir,
                        "app_server_node_reconnect_failed",
                        serde_json::json!({
                            "node": node_id,
                            "member": member.name,
                            "error": err.to_string(),
                        }),
                    )?;
                    set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                    continue;
                }
            };
            if let Some(process) = process {
                node_processes.push(process);
            }
            let connected_client = match connect_team_app_server(&url).await {
                Ok(client) => client,
                Err(err) => {
                    append_event(
                        team_dir,
                        "app_server_node_reconnect_failed",
                        serde_json::json!({
                            "node": node_id,
                            "member": member.name,
                            "url": url,
                            "error": err.to_string(),
                        }),
                    )?;
                    set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                    continue;
                }
            };
            append_event(
                team_dir,
                "app_server_node_connected",
                serde_json::json!({
                    "node": node_id,
                    "kind": node.kind,
                    "url": url,
                    "source": "dynamic_member",
                }),
            )?;
            set_node_connection(
                team_dir,
                &node_id,
                TeamNodeStatus::Online,
                Some(url.clone()),
            )?;
            node_clients.insert(
                node_id.clone(),
                TeamAppServerNodeClient {
                    client: connected_client,
                    request_counter: 1,
                },
            );
        }
        let member_cwd = app_server_member_cwd(&node_id, nodes, cwd);
        let mut recovered_once = false;
        let (thread, turn): (ThreadStartResponse, TurnStartResponse) = loop {
            let thread_result = {
                let node_client = node_clients
                    .get_mut(&node_id)
                    .with_context(|| format!("app-server client missing for node `{node_id}`"))?;
                start_team_app_server_thread(
                    node_client,
                    team_dir,
                    &node_id,
                    &member.name,
                    "dynamic_department_thread",
                    ThreadStartParams {
                        model: model.clone(),
                        cwd: Some(member_cwd.display().to_string()),
                        sandbox: sandbox.clone(),
                        approval_policy: approval_policy.clone(),
                        ephemeral: Some(false),
                        ..ThreadStartParams::default()
                    },
                    language,
                )
                .await
            };
            let thread = match thread_result {
                Ok(thread) => thread,
                Err(err) if node_id != "local" && !recovered_once => {
                    recovered_once = true;
                    append_event(
                        team_dir,
                        "app_server_dynamic_member_thread_start_recovering",
                        serde_json::json!({
                            "member": member.name,
                            "node": node_id,
                            "error": err.to_string(),
                        }),
                    )?;
                    if let Err(recovery_err) = recover_app_server_node_client(
                        node_clients,
                        node_processes,
                        nodes,
                        team_dir,
                        &node_id,
                        relay_port,
                        &format!("dynamic member thread start failed: {err}"),
                    )
                    .await
                    {
                        append_event(
                            team_dir,
                            "app_server_dynamic_member_start_failed",
                            serde_json::json!({
                                "member": member.name,
                                "node": node_id,
                                "stage": "thread_start_recovery",
                                "error": recovery_err.to_string(),
                            }),
                        )?;
                        set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                        continue 'member_loop;
                    }
                    continue;
                }
                Err(err) => {
                    append_event(
                        team_dir,
                        "app_server_dynamic_member_start_failed",
                        serde_json::json!({
                            "member": member.name,
                            "node": node_id,
                            "stage": "thread_start",
                            "error": err.to_string(),
                        }),
                    )?;
                    set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                    continue 'member_loop;
                }
            };
            set_member_thread(team_dir, &member.name, &thread.thread.id)?;
            set_member_workspace(team_dir, &member.name, &member_cwd)?;

            let current_config = load_config(team_dir)?;
            let current_tasks = load_tasks(team_dir)?;
            let prompt = build_app_server_worker_prompt(
                &current_config,
                &current_tasks,
                member,
                codex_exe,
                nodes,
                language,
            );
            let turn_result = {
                let node_client = node_clients.get_mut(&node_id).with_context(|| {
                    format!("app-server client missing for node `{node_id}`")
                })?;
                node_client
                    .client
                    .request_typed(ClientRequest::TurnStart {
                        request_id: next_request_id(&mut node_client.request_counter),
                        params: TurnStartParams {
                            thread_id: thread.thread.id.clone(),
                            input: vec![text_input(prompt)],
                            cwd: Some(member_cwd.clone()),
                            model: model.clone(),
                            approval_policy: approval_policy.clone(),
                            sandbox_policy: if dangerously_bypass_approvals_and_sandbox {
                                Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess)
                            } else {
                                None
                            },
                            ..TurnStartParams::default()
                        },
                    })
                    .await
            };
            let turn = match turn_result {
                Ok(turn) => turn,
                Err(err) if node_id != "local" && !recovered_once => {
                    let err = anyhow!(err);
                    recovered_once = true;
                    append_event(
                        team_dir,
                        "app_server_dynamic_member_turn_start_recovering",
                        serde_json::json!({
                            "member": member.name,
                            "node": node_id,
                            "thread": thread.thread.id,
                            "error": err.to_string(),
                        }),
                    )?;
                    if let Err(recovery_err) = recover_app_server_node_client(
                        node_clients,
                        node_processes,
                        nodes,
                        team_dir,
                        &node_id,
                        relay_port,
                        &format!("dynamic member turn start failed: {err}"),
                    )
                    .await
                    {
                        append_event(
                            team_dir,
                            "app_server_dynamic_member_start_failed",
                            serde_json::json!({
                                "member": member.name,
                                "node": node_id,
                                "stage": "turn_start_recovery",
                                "error": recovery_err.to_string(),
                            }),
                        )?;
                        set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                        continue 'member_loop;
                    }
                    continue;
                }
                Err(err) => {
                    append_event(
                        team_dir,
                        "app_server_dynamic_member_start_failed",
                        serde_json::json!({
                            "member": member.name,
                            "node": node_id,
                            "thread": thread.thread.id,
                            "stage": "turn_start",
                            "error": err.to_string(),
                        }),
                    )?;
                    set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                    continue 'member_loop;
                }
            };
            break (thread, turn);
        };

        thread_to_member.insert(thread_key(&node_id, &thread.thread.id), member.name.clone());
        assistant_buffers.insert(member.name.clone(), String::new());
        mailbox_counts
            .entry(member.name.clone())
            .or_insert(mailbox_seen_count(&read_jsonl::<MailMessage>(
                &mailbox_path(team_dir, &member.name),
            )?));
        active.insert(
            member.name.clone(),
            AppServerMemberRun {
                member: member.clone(),
                node_id: node_id.clone(),
                cwd: member_cwd.clone(),
                thread_id: thread.thread.id.clone(),
                turn_id: turn.turn.id.clone(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "dynamic_member_start".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );
        println!(
            "Started dynamic {} ({}) thread={} turn={}",
            member.name, member.role, thread.thread.id, turn.turn.id
        );
        append_event(
            team_dir,
            "app_server_dynamic_member_started",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "thread": thread.thread.id,
                "turn": turn.turn.id,
                "node": node_id,
                "cwd": member_cwd,
            }),
        )?;
        record_turn_usage_index(
            team_dir,
            member,
            &node_id,
            &thread.thread.id,
            &turn.turn.id,
            "dynamic_member_start",
            "app_server_dynamic_member_started",
        )?;
    }
    *config = load_config(team_dir)?;
    Ok(())
}

async fn sync_removed_app_server_nodes(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    nodes: &[TeamNode],
    team_dir: &Path,
    active: &HashMap<String, AppServerMemberRun>,
) -> Result<()> {
    let known = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir).unwrap_or_default();
    let connected = node_clients.keys().cloned().collect::<Vec<_>>();
    for node_id in connected {
        if node_id == "local" || known.contains(&node_id) {
            continue;
        }
        let active_member = active.values().any(|run| {
            run.node_id == node_id
                && !run.completed
                && config
                    .members
                    .iter()
                    .find(|member| member.name == run.member.name)
                    .map(|member| {
                        !matches!(
                            member.status,
                            MemberStatus::Standby
                                | MemberStatus::Completed
                                | MemberStatus::Failed
                                | MemberStatus::Offline
                        )
                            && tasks.iter().any(|task| {
                                task.owner.as_deref() == Some(member.name.as_str())
                                    && !matches!(
                                        task.status,
                                        TaskStatus::Completed
                                            | TaskStatus::Cancelled
                                            | TaskStatus::Failed
                                    )
                            })
                    })
                    .unwrap_or(false)
        });
        if active_member {
            append_event(
                team_dir,
                "app_server_node_remove_deferred",
                serde_json::json!({
                    "node": node_id,
                    "reason": "node still has an active member",
                }),
            )?;
            continue;
        }
        if let Some(client) = node_clients.remove(&node_id) {
            client
                .client
                .shutdown()
                .await
                .with_context(|| format!("shutdown removed node `{node_id}` client"))?;
        }
        let mut idx = 0;
        while idx < node_processes.len() {
            if node_processes[idx].node_id == node_id {
                let process = node_processes.remove(idx);
                process.stop();
            } else {
                idx += 1;
            }
        }
        append_event(
            team_dir,
            "app_server_node_disconnected",
            serde_json::json!({ "node": node_id, "reason": "node removed" }),
        )?;
    }
    Ok(())
}

fn has_unstarted_app_server_members(
    team_dir: &Path,
    active: &HashMap<String, AppServerMemberRun>,
) -> Result<bool> {
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    Ok(config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .any(|member| {
            !active.contains_key(&member.name)
                && matches!(member.status, MemberStatus::Online | MemberStatus::Running)
                && tasks.iter().any(|task| {
                    task.owner.as_deref() == Some(member.name.as_str())
                        && task_status_can_start_turn(task.status)
                })
        }))
}

fn current_mailbox_counts(
    team_dir: &Path,
    members: &[TeamMember],
    tasks: &[TeamTask],
) -> Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    let language = load_config(team_dir)
        .ok()
        .and_then(|config| config.language)
        .unwrap_or_default();
    for member in members {
        let path = mailbox_path(team_dir, &member.name);
        let mut messages = read_jsonl::<MailMessage>(&path)?;
        let has_open_task = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()) && task_is_open(task));
        let count = if member.role == "lead" || has_open_task {
            let seen = mailbox_seen_count(&messages);
            compact_runtime_start_mailbox_backlog(
                team_dir,
                &member.name,
                &path,
                &mut messages,
                seen,
                language,
            )?
        } else {
            messages.len()
        };
        counts.insert(member.name.clone(), count);
    }
    Ok(counts)
}

fn mailbox_seen_count(messages: &[MailMessage]) -> usize {
    messages
        .iter()
        .position(|message| !message.read)
        .unwrap_or(messages.len())
}

fn compact_runtime_start_mailbox_backlog(
    team_dir: &Path,
    member_name: &str,
    path: &Path,
    messages: &mut Vec<MailMessage>,
    first_unread: usize,
    language: TeamPromptLanguage,
) -> Result<usize> {
    let unread_count = messages.len().saturating_sub(first_unread);
    if unread_count <= MAX_RUNTIME_START_UNREAD_MAILBOX_MESSAGES {
        return Ok(first_unread);
    }
    let old_len = messages.len();
    let tail_start = old_len
        .saturating_sub(MAX_RUNTIME_START_UNREAD_MAILBOX_TAIL_MESSAGES)
        .max(first_unread);
    let compacted_count = tail_start.saturating_sub(first_unread);
    if compacted_count == 0 {
        return Ok(first_unread);
    }
    for message in messages.iter_mut().take(tail_start).skip(first_unread) {
        message.read = true;
    }
    let summary_start =
        tail_start.saturating_sub(MAX_RUNTIME_START_UNREAD_MAILBOX_SUMMARY_MESSAGES);
    let summary_messages = messages[summary_start..tail_start].to_vec();
    let summarized = summarize_side_reply_messages(&summary_messages, language);
    let summary = if language.is_ja() {
        format!(
            "Mailbox resume compaction: runtime 起動時点で @{member_name} に unread message が {unread_count} 件ありました。トークン節約と stale context 混入防止のため、古い {compacted_count} 件を既読化し、直近 {retained} 件とこの要約だけを実行 turn へ渡します。全履歴は team state の mailbox jsonl に残っています。\n\n古い既読化対象の末尾要約:\n{summarized}",
            retained = old_len.saturating_sub(tail_start)
        )
    } else {
        format!(
            "Mailbox resume compaction: @{member_name} had {unread_count} unread messages when the runtime started. To reduce token pressure and stale-context injection, {compacted_count} older message(s) were marked read; only the latest {retained} message(s) plus this summary will be delivered into the next turn. The full history remains in the team state's mailbox jsonl.\n\nTail summary of compacted messages:\n{summarized}",
            retained = old_len.saturating_sub(tail_start)
        )
    };
    messages.push(MailMessage {
        from: "system".to_string(),
        to: member_name.to_string(),
        message: summary,
        timestamp: now(),
        read: false,
    });
    write_jsonl_atomic(path, messages)?;
    append_event(
        team_dir,
        "mailbox_resume_backlog_compacted",
        serde_json::json!({
            "member": member_name,
            "first_unread": first_unread,
            "unread_before": unread_count,
            "compacted": compacted_count,
            "retained_tail": old_len.saturating_sub(tail_start),
            "summary_messages": summary_messages.len(),
        }),
    )?;
    Ok(tail_start)
}

#[cfg(test)]
fn mark_mailbox_messages_read(team_dir: &Path, member_name: &str, from_index: usize) -> Result<()> {
    let path = mailbox_path(team_dir, member_name);
    let mut messages = read_jsonl::<MailMessage>(&path)?;
    mark_mailbox_messages_read_range_inner(
        team_dir,
        member_name,
        &path,
        &mut messages,
        from_index,
        None,
    )
}

fn mark_mailbox_messages_read_range(
    team_dir: &Path,
    member_name: &str,
    from_index: usize,
    to_index: usize,
) -> Result<()> {
    let path = mailbox_path(team_dir, member_name);
    let mut messages = read_jsonl::<MailMessage>(&path)?;
    mark_mailbox_messages_read_range_inner(
        team_dir,
        member_name,
        &path,
        &mut messages,
        from_index,
        Some(to_index),
    )
}

fn mark_mailbox_messages_read_range_inner(
    team_dir: &Path,
    member_name: &str,
    path: &Path,
    messages: &mut [MailMessage],
    from_index: usize,
    to_index: Option<usize>,
) -> Result<()> {
    if from_index >= messages.len() {
        return Ok(());
    }
    let end = to_index.unwrap_or(messages.len()).min(messages.len());
    if from_index >= end {
        return Ok(());
    }
    let mut changed = false;
    for message in messages.iter_mut().take(end).skip(from_index) {
        if !message.read {
            message.read = true;
            changed = true;
        }
    }
    if changed {
        write_jsonl_atomic(&path, &messages)?;
        append_event(
            team_dir,
            "mailbox_messages_marked_read",
            serde_json::json!({
                "member": member_name,
                "from_index": from_index,
                "count": end.saturating_sub(from_index),
            }),
        )?;
    }
    Ok(())
}

fn maybe_send_idle_department_outreach(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    last_outreach: &mut Instant,
    cursor: &mut usize,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_outreach) < interval {
        return Ok(());
    }
    *last_outreach = now_instant;

    let tasks = load_tasks(team_dir)?;
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let mut helpers = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| {
            matches!(
                member.status,
                MemberStatus::Standby | MemberStatus::Completed
            )
        })
        .filter(|member| active.get(&member.name).is_none_or(|run| run.completed))
        .filter(|member| member_node_unavailable_from_nodes(member, &nodes).is_none())
        .filter(|member| {
            !tasks.iter().any(|task| {
                task.owner.as_deref() == Some(member.name.as_str())
                    && matches!(
                        task.status,
                        TaskStatus::InProgress
                            | TaskStatus::Review
                            | TaskStatus::Pending
                            | TaskStatus::Ready
                            | TaskStatus::Blocked
                    )
            })
        })
        .map(|member| member.name.clone())
        .collect::<Vec<_>>();
    helpers.sort();
    helpers.dedup();
    if helpers.is_empty() {
        append_event(
            team_dir,
            "idle_outreach_skipped",
            serde_json::json!({ "reason": "no_idle_departments" }),
        )?;
        return Ok(());
    }

    let mut targets = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| !matches!(member.status, MemberStatus::Failed | MemberStatus::Offline))
        .filter(|member| {
            active.get(&member.name).is_some_and(|run| !run.completed)
                || member_node_unavailable_from_nodes(member, &nodes).is_none()
        })
        .filter(|member| {
            tasks.iter().any(|task| {
                task.owner.as_deref() == Some(member.name.as_str())
                    && matches!(
                        task.status,
                        TaskStatus::InProgress
                            | TaskStatus::Ready
                            | TaskStatus::Waiting
                            | TaskStatus::Blocked
                            | TaskStatus::Review
                    )
            })
        })
        .map(|member| member.name.clone())
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();

    let helper = helpers[*cursor % helpers.len()].clone();
    *cursor = cursor.wrapping_add(1);
    targets.retain(|target| target != &helper);
    if targets.is_empty() {
        append_event(
            team_dir,
            "idle_outreach_skipped",
            serde_json::json!({ "helper": helper, "reason": "no_active_or_blocked_targets" }),
        )?;
        return Ok(());
    }

    let selected_targets = targets.into_iter().take(3).collect::<Vec<_>>();
    let message = if language.is_ja() {
        format!(
            "REPLY_REQUEST: @{helper} からの定期アイドル声かけです。私はいま free/standby です。blocker、レビュー依頼、artifact 解釈、schema/runtime の懸念、handoff の整理など、手伝えることがあれば具体的に返してください。必要なら lead に、具体的な mission 付きで私を resume するよう依頼してください。不要なら `STAY:` で返してください。"
        )
    } else {
        format!(
            "REPLY_REQUEST: Periodic idle outreach from @{helper}. I am currently free/standby. If you have a blocker, review need, artifact interpretation question, schema/runtime concern, or handoff cleanup I can help with, reply with the concrete need. If useful, ask lead to resume me with a concrete mission. Reply with `STAY:` if no help is needed."
        )
    };
    for target in &selected_targets {
        send_team_message_to_dir(team_dir, &helper, target, &message)?;
    }
    append_event(
        team_dir,
        "idle_outreach_sent",
        serde_json::json!({
            "from": helper,
            "to": selected_targets,
            "interval_sec": interval.as_secs(),
        }),
    )?;
    Ok(())
}

fn record_runtime_loop_error(team_dir: &Path, phase: &str, err: anyhow::Error) -> Result<()> {
    append_event(
        team_dir,
        "app_server_runtime_loop_nonfatal_error",
        serde_json::json!({
            "phase": phase,
            "error": format!("{err:#}"),
        }),
    )
}

fn stable_short_hash(value: &str) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn attention_fingerprint_recently_sent(team_dir: &Path, kind: &str, fingerprint: &str) -> bool {
    read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))
        .unwrap_or_default()
        .into_iter()
        .rev()
        .take(400)
        .any(|event| {
            event.event == "system_attention_fingerprint"
                && event.data.get("kind").and_then(|value| value.as_str()) == Some(kind)
                && event
                    .data
                    .get("fingerprint")
                    .and_then(|value| value.as_str())
                    == Some(fingerprint)
        })
}

fn record_attention_fingerprint(
    team_dir: &Path,
    kind: &str,
    fingerprint: &str,
    scope: serde_json::Value,
) -> Result<()> {
    append_event(
        team_dir,
        "system_attention_fingerprint",
        serde_json::json!({
            "kind": kind,
            "fingerprint": fingerprint,
            "scope": scope,
        }),
    )
}

fn refresh_running_team_jobs(team_dir: &Path) -> Result<()> {
    let jobs = load_jobs(team_dir)?;
    for job in jobs {
        if matches!(job.status, TeamJobStatus::Running | TeamJobStatus::Unknown) {
            refresh_job_status(team_dir, &job.id)?;
        }
    }
    Ok(())
}

fn maybe_warn_unattended_tasks(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    last_watchdog: &mut Instant,
    warned: &mut HashSet<String>,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_watchdog) < interval {
        return Ok(());
    }
    *last_watchdog = now_instant;

    let tasks = load_tasks(team_dir)?;
    let jobs = load_jobs(team_dir)?;
    let waits = load_waits(team_dir)?;
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let running_job_tasks = jobs
        .iter()
        .filter(|job| matches!(job.status, TeamJobStatus::Running))
        .filter_map(|job| job.task_id.as_deref())
        .collect::<HashSet<_>>();
    let open_wait_tasks = waits
        .iter()
        .filter(|wait| wait.status.is_open())
        .filter_map(|wait| wait.task_id.as_deref())
        .collect::<HashSet<_>>();
    let mut config = config.clone();
    let mut config_changed = false;
    let member_status = config
        .members
        .iter()
        .map(|member| (member.name.clone(), member.status.clone()))
        .collect::<HashMap<_, _>>();

    for task in tasks.iter().filter(|task| {
        matches!(
            task.status,
            TaskStatus::Pending
                | TaskStatus::Ready
                | TaskStatus::Waiting
                | TaskStatus::InProgress
                | TaskStatus::Review
                | TaskStatus::Blocked
        )
    }) {
        let Some(owner) = task.owner.as_deref() else {
            continue;
        };
        let active_owner_turn = active.get(owner).is_some_and(|run| !run.completed);
        let tracked_running_job = running_job_tasks.contains(task.id.as_str());
        let tracked_open_wait = open_wait_tasks.contains(task.id.as_str());
        if active_owner_turn || tracked_running_job || tracked_open_wait {
            continue;
        }
        if !task.depends_on.is_empty() && !task_dependencies_completed(task, &tasks) {
            continue;
        }
        if task_age_secs(task).is_some_and(|age| age < 90) {
            continue;
        }
        if let Some(job) = jobs.iter().find(|job| {
            matches!(job.status, TeamJobStatus::Completed)
                && !job.artifacts.is_empty()
                && job.task_id.as_deref() == Some(task.id.as_str())
                && job.owner.as_deref() == Some(owner)
        }) {
            let changed = set_task_status_if_open(
                team_dir,
                &task.id,
                TaskStatus::InProgress,
                Some(&format!(
                    "Watchdog found completed job `{}` with registered artifact(s); owner must inspect them and publish the task's final handoff/checklist or a concrete blocker before review.",
                    job.id
                )),
            )?;
            if changed {
                resume_job_owner_after_job_status_change(
                    team_dir,
                    job,
                    &task.id,
                    TaskStatus::InProgress,
                )?;
                append_event(
                    team_dir,
                    "task_watchdog_completed_artifact_revival",
                    serde_json::json!({
                        "task": task.id,
                        "owner": owner,
                        "job": job.id,
                        "artifacts": job.artifacts,
                    }),
                )?;
            }
            continue;
        }
        let status = member_status
            .get(owner)
            .map(|status| format!("{status:?}"))
            .unwrap_or_else(|| "unknown".to_string());
        let warning_key = format!("{}:{}:{}", task.id, task.status, task.updated_at);
        if !warned.insert(warning_key) {
            continue;
        }
        let fingerprint = stable_short_hash(&format!(
            "task-watchdog|{}|{}|{}|{}|{}",
            task.id,
            owner,
            task.status,
            task.depends_on.join(","),
            task.result.as_deref().unwrap_or("")
        ));
        if attention_fingerprint_recently_sent(team_dir, "task_watchdog", &fingerprint) {
            append_event(
                team_dir,
                "task_watchdog_attention_skipped",
                serde_json::json!({
                    "task": task.id,
                    "owner": owner,
                    "reason": "same_fingerprint_recently_sent",
                    "fingerprint": fingerprint,
                }),
            )?;
            continue;
        }
        let proposal_lines =
            collect_recent_lead_proposals_for_task(team_dir, &config.lead, &task.id, 3)?;
        let proposal_note = if proposal_lines.is_empty() {
            String::new()
        } else if language.is_ja() {
            format!(
                "\n\nこの task に言及している最近の LEAD_PROPOSAL signal:\n{}",
                proposal_lines.join("\n")
            )
        } else {
            format!(
                "\n\nRecent LEAD_PROPOSAL signal(s) mentioning this task:\n{}",
                proposal_lines.join("\n")
            )
        };
        let owner_cooldown = recent_usage_limit_retry_remaining(team_dir, owner)?;
        let cooldown_note = if let Some(remaining) = owner_cooldown {
            if language.is_ja() {
                format!(
                    " owner は usage-limit cooldown 中です retry_in={}。owner へ直接 wakeup を積まず、lead が待機・正式な再割当て・安全な代替 owner のいずれかを判断してください。",
                    format_compact_duration(remaining.as_secs())
                )
            } else {
                format!(
                    " The owner is in usage-limit cooldown retry_in={}; do not queue direct owner wakeups. Lead should explicitly wait, reassign, or choose a safe alternate owner.",
                    format_compact_duration(remaining.as_secs())
                )
            }
        } else {
            String::new()
        };
        let message = if language.is_ja() {
            format!(
                "Task watchdog: task {} は @{owner} が owner で、状態は `{}` ですが、owner の live turn も tracked running job もありません。Owner status は {status} です。lead は @{owner} を resume するか、`team job --owner {owner} --task {}` を attach/start するか、具体的 blocker 付きで blocked にするか、evidence 付きで completed にしてください。{cooldown_note}{proposal_note}",
                task.id, task.status, task.id
            )
        } else {
            format!(
                "Task watchdog: task {} owned by @{owner} is `{}` but has no live owner turn and no tracked running job. Owner status is {status}. Resume @{owner}, attach/start a `team job --owner {owner} --task {}`, mark it blocked with a concrete blocker, or complete it with evidence.{cooldown_note}{proposal_note}",
                task.id, task.status, task.id
            )
        };
        send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
        if owner_cooldown.is_none() && config.members.iter().any(|member| member.name == owner) {
            send_team_message_to_dir(team_dir, "system", owner, &message)?;
        }
        record_attention_fingerprint(
            team_dir,
            "task_watchdog",
            &fingerprint,
            serde_json::json!({
                "task": task.id,
                "owner": owner,
                "status": task.status,
            }),
        )?;
        let mut reactivated_owner = false;
        if owner_cooldown.is_none()
            && task_status_can_start_turn(task.status)
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
            config.updated_at = now();
            config_changed = true;
            reactivated_owner = true;
        }
        append_event(
            team_dir,
            "task_watchdog_attention",
            serde_json::json!({
                "task": task.id,
                "owner": owner,
                "status": task.status,
                "owner_status": status,
                "reason": "no live owner turn and no tracked running job",
                "owner_reactivated": reactivated_owner,
                "owner_usage_limit_cooldown_sec": owner_cooldown.map(|remaining| remaining.as_secs()),
            }),
        )?;
    }
    warn_open_waits_on_unavailable_nodes(team_dir, &config, &waits, &nodes, warned, language)?;
    warn_review_tasks_missing_local_handoff_artifacts(team_dir, &config, &tasks, warned)?;
    if config_changed {
        write_json_atomic(&team_dir.join("config.json"), &config)?;
        touch_config(team_dir)?;
    }
    Ok(())
}

fn warn_open_waits_on_unavailable_nodes(
    team_dir: &Path,
    config: &TeamConfig,
    waits: &[TeamWait],
    nodes: &[TeamNode],
    warned: &mut HashSet<String>,
    language: TeamPromptLanguage,
) -> Result<()> {
    for wait in waits.iter().filter(|wait| wait.status.is_open()) {
        let Some(node_id) = wait.node.as_deref() else {
            continue;
        };
        let Some(unavailable) = node_unavailable_from_nodes(node_id, nodes) else {
            continue;
        };
        if wait_age_secs(wait).is_some_and(|age| age < 90) {
            continue;
        }
        let warning_key = format!(
            "wait-node:{}:{}:{}:{}",
            wait.id, wait.status, wait.updated_at, unavailable.reason
        );
        if !warned.insert(warning_key) {
            continue;
        }
        let fingerprint = stable_short_hash(&format!(
            "wait-node|{}|{}|{}|{}|{}|{}",
            wait.id, wait.status, node_id, unavailable.reason, wait.condition, wait.progress
        ));
        if attention_fingerprint_recently_sent(team_dir, "wait_node_unavailable", &fingerprint) {
            append_event(
                team_dir,
                "wait_node_unavailable_attention_skipped",
                serde_json::json!({
                    "wait": wait.id,
                    "node": node_id,
                    "reason": "same_fingerprint_recently_sent",
                    "fingerprint": fingerprint,
                }),
            )?;
            continue;
        }
        let owner = wait.owner.as_deref().unwrap_or("unassigned");
        let task = wait.task_id.as_deref().unwrap_or("-");
        let message = if language.is_ja() {
            format!(
                "Wait node watchdog: wait {id} は `{status}` のままですが、node `{node}` が利用できません reason={reason} raw_status={raw_status} age={age}。condition={condition} progress={progress}\n\nlead は node を復旧/再接続するか、wait を concrete blocker 付きで blocked/failed にするか、owner を再割当てしてください。node が復旧するまで、この wait に依存する task {task} を完了扱いにしないでください。",
                id = wait.id,
                status = wait.status,
                node = node_id,
                reason = unavailable.reason,
                raw_status = unavailable.status,
                age = unavailable.age,
                condition = compact_one_line(&wait.condition, 500),
                progress = compact_one_line(&wait.progress, 500),
            )
        } else {
            format!(
                "Wait node watchdog: wait {id} is still `{status}`, but node `{node}` is unavailable reason={reason} raw_status={raw_status} age={age}. condition={condition} progress={progress}\n\nLead should restore/reconnect the node, mark the wait blocked/failed with a concrete blocker, or reassign the owner. Do not complete dependent task {task} while this wait's node is unavailable.",
                id = wait.id,
                status = wait.status,
                node = node_id,
                reason = unavailable.reason,
                raw_status = unavailable.status,
                age = unavailable.age,
                condition = compact_one_line(&wait.condition, 500),
                progress = compact_one_line(&wait.progress, 500),
            )
        };
        send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
        if config.members.iter().any(|member| member.name == owner) {
            send_team_message_to_dir(team_dir, "system", owner, &message)?;
        }
        record_attention_fingerprint(
            team_dir,
            "wait_node_unavailable",
            &fingerprint,
            serde_json::json!({
                "wait": wait.id,
                "node": node_id,
                "status": wait.status,
            }),
        )?;
        append_event(
            team_dir,
            "wait_node_unavailable_attention",
            serde_json::json!({
                "wait": wait.id,
                "owner": owner,
                "task": task,
                "node": node_id,
                "status": wait.status,
                "reason": unavailable.reason,
                "node_status": unavailable.status,
                "node_age": unavailable.age,
            }),
        )?;
    }
    Ok(())
}

#[derive(Debug)]
struct ReviewArtifactIssue {
    path: String,
    issue: String,
}

fn warn_review_tasks_missing_local_handoff_artifacts(
    team_dir: &Path,
    config: &TeamConfig,
    tasks: &[TeamTask],
    warned: &mut HashSet<String>,
) -> Result<()> {
    let ownerships = load_ownerships(team_dir)?;
    if ownerships.is_empty() {
        return Ok(());
    }

    for task in tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Review)
    {
        let Some(owner) = task.owner.as_deref() else {
            continue;
        };
        if task_age_secs(task).is_some_and(|age| age < 60) {
            continue;
        }
        let issues = review_task_local_artifact_issues(team_dir, task, owner, &ownerships)?;
        if issues.is_empty() {
            continue;
        }
        let issue_key = issues
            .iter()
            .map(|issue| format!("{}={}", issue.path, issue.issue))
            .collect::<Vec<_>>()
            .join("|");
        let warning_key = format!(
            "review-handoff-artifacts:{}:{}:{}",
            task.id, task.updated_at, issue_key
        );
        if !warned.insert(warning_key) {
            continue;
        }

        let issue_lines = issues
            .iter()
            .take(6)
            .map(|issue| format!("- {}: {}", issue.path, issue.issue))
            .collect::<Vec<_>>()
            .join("\n");
        let message = format!(
            "Review handoff watchdog: task {} owned by @{owner} is in `review`, but task-specific local artifact ownership path(s) do not yet contain a complete formal handoff package.\n\nIssues:\n{issue_lines}\n\nA clean review handoff needs owner artifacts such as report/JSON ledger, `sha256_manifest.txt`, and `TEAM_COMPLETION_CHECKLIST.md`, plus a final message to lead/consumers. Collection or semantic verification jobs are not enough by themselves. Lead should steer @{owner} to publish the package, report the exact blocker, or move the task back to `in_progress`/`blocked` with a concrete next checkpoint.",
            task.id
        );
        send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
        if config.members.iter().any(|member| member.name == owner) {
            send_team_message_to_dir(team_dir, "system", owner, &message)?;
        }
        append_event(
            team_dir,
            "review_handoff_artifact_attention",
            serde_json::json!({
                "task": task.id,
                "owner": owner,
                "issues": issues.iter().map(|issue| {
                    serde_json::json!({
                        "path": issue.path,
                        "issue": issue.issue,
                    })
                }).collect::<Vec<_>>(),
            }),
        )?;
    }
    Ok(())
}

fn review_task_local_artifact_issues(
    team_dir: &Path,
    task: &TeamTask,
    owner: &str,
    ownerships: &[FileOwnership],
) -> Result<Vec<ReviewArtifactIssue>> {
    let mut issues = Vec::new();
    let mut complete_handoff_seen = false;
    let mut non_local_handoff_paths = Vec::new();
    for ownership in ownerships
        .iter()
        .filter(|ownership| ownership.owner == owner)
        .filter(|ownership| ownership_mentions_task(ownership, task))
    {
        if !ownership_path_is_probably_local(team_dir, &ownership.path) {
            non_local_handoff_paths.push(ownership.path.clone());
            continue;
        }
        let path = PathBuf::from(&ownership.path);
        match inspect_local_handoff_path(
            &path,
            owner_recent_completion_checklist_message(team_dir, owner)?,
        )? {
            Some(issue) => issues.push(ReviewArtifactIssue {
                path: ownership.path.clone(),
                issue,
            }),
            None => complete_handoff_seen = true,
        }
    }
    if complete_handoff_seen {
        return Ok(Vec::new());
    }
    if issues.is_empty() && !non_local_handoff_paths.is_empty() {
        for path in non_local_handoff_paths.into_iter().take(3) {
            issues.push(ReviewArtifactIssue {
                path,
                issue: "owned handoff path is not locally inspectable; require a node-side manifest/checklist verification job or explicit blocker before accepting review".to_string(),
            });
        }
    }
    Ok(issues)
}

fn ownership_mentions_task(ownership: &FileOwnership, task: &TeamTask) -> bool {
    let haystack = format!("{} {}", ownership.path, ownership.note).to_ascii_lowercase();
    [
        format!("task {}", task.id),
        format!("task{}", task.id),
        format!("task-{}", task.id),
        format!("#{}", task.id),
    ]
    .iter()
    .any(|needle| haystack.contains(&needle.to_ascii_lowercase()))
}

fn ownership_path_is_probably_local(team_dir: &Path, raw: &str) -> bool {
    let path = Path::new(raw);
    if path.exists() || path.starts_with(team_dir) {
        return true;
    }
    if let Ok(home) = std::env::var("HOME")
        && path.starts_with(home)
    {
        return true;
    }
    false
}

fn owner_recent_completion_checklist_message(team_dir: &Path, owner: &str) -> Result<bool> {
    for mailbox_owner in [owner, "lead"] {
        let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, mailbox_owner))?;
        if messages.into_iter().rev().take(200).any(|message| {
            message.from == owner && message_has_substantive_completion_checklist(&message.message)
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn message_has_substantive_completion_checklist(message: &str) -> bool {
    if !message.contains("TEAM_COMPLETION_CHECKLIST") {
        return false;
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("artifacts: none")
        || lower.contains("artifacts: なし")
        || lower.contains("verification: none")
        || lower.contains("side-channel のため")
        || lower.contains("side-channel")
        || lower.contains("pending")
        || lower.contains("not yet")
        || lower.contains("未送信")
        || lower.contains("未通知")
        || lower.contains("未完了")
    {
        return false;
    }
    lower.contains("artifacts:") && lower.contains("verification:")
}

fn inspect_local_handoff_path(
    path: &Path,
    owner_has_completion_checklist_message: bool,
) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(Some("path does not exist".to_string()));
    }
    if path.is_file() {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if handoff_file_kind(name).is_some() {
            return Ok(None);
        }
        return Ok(Some(
            "owned file is not a recognizable handoff artifact".to_string(),
        ));
    }
    if !path.is_dir() {
        return Ok(Some(
            "path exists but is not a file or directory".to_string(),
        ));
    }

    let mut stats = HandoffArtifactStats::default();
    collect_handoff_artifact_stats(path, 0, &mut stats)?;
    if stats.files == 0 {
        return Ok(Some("directory exists but contains no files".to_string()));
    }

    let mut missing = Vec::new();
    if !stats.has_checklist && !owner_has_completion_checklist_message {
        missing.push("TEAM_COMPLETION_CHECKLIST.md");
    }
    if !stats.has_manifest {
        missing.push("sha256_manifest.txt or manifest/checksums.sha256");
    }
    if !stats.has_report {
        missing.push("report markdown/text");
    }
    if !stats.has_structured {
        missing.push("structured ledger/report");
    }
    if missing.is_empty() {
        if let Some(issue) = inspect_handoff_checklists(&stats)? {
            Ok(Some(issue))
        } else {
            verify_handoff_manifests(&stats)
        }
    } else {
        Ok(Some(format!("missing {}", missing.join(", "))))
    }
}

#[derive(Default)]
struct HandoffArtifactStats {
    files: usize,
    has_checklist: bool,
    has_manifest: bool,
    has_report: bool,
    has_structured: bool,
    checklist_paths: Vec<PathBuf>,
    manifest_paths: Vec<PathBuf>,
}

fn inspect_handoff_checklists(stats: &HandoffArtifactStats) -> Result<Option<String>> {
    for checklist_path in &stats.checklist_paths {
        let content = fs::read_to_string(checklist_path)
            .with_context(|| format!("read {}", checklist_path.display()))?;
        let lower = content.to_ascii_lowercase();
        if !lower.contains("team_completion_checklist") {
            return Ok(Some(format!(
                "{} does not contain TEAM_COMPLETION_CHECKLIST",
                checklist_path.display()
            )));
        }
        for field in [
            "artifacts:",
            "verification:",
            "messages_sent:",
            "consumers_notified:",
            "blockers_or_limits:",
        ] {
            let value = checklist_field_value(&lower, field);
            if field != "blockers_or_limits:"
                && checklist_value_is_empty_or_unknown(value.as_deref())
            {
                return Ok(Some(format!(
                    "{} has empty/unknown `{field}` field",
                    checklist_path.display()
                )));
            }
            if field != "blockers_or_limits:"
                && checklist_value_has_unresolved_marker(value.as_deref())
            {
                if field == "verification:" && !stats.manifest_paths.is_empty() {
                    continue;
                }
                return Ok(Some(format!(
                    "{} has pending/unresolved `{field}` field",
                    checklist_path.display()
                )));
            }
        }
    }
    Ok(None)
}

fn verify_handoff_manifests(stats: &HandoffArtifactStats) -> Result<Option<String>> {
    for manifest_path in &stats.manifest_paths {
        let metadata = fs::metadata(manifest_path)
            .with_context(|| format!("stat {}", manifest_path.display()))?;
        if metadata.len() == 0 {
            return Ok(Some(format!(
                "{} is empty",
                manifest_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("sha256_manifest.txt")
            )));
        }
        if let Some(issue) = inspect_handoff_manifest_entries(manifest_path)? {
            return Ok(Some(issue));
        }
        let Some((cwd, manifest_arg)) = sha256_manifest_check_context(manifest_path) else {
            continue;
        };
        let output = Command::new("sha256sum")
            .arg("-c")
            .arg(&manifest_arg)
            .current_dir(&cwd)
            .output()
            .with_context(|| {
                format!(
                    "run sha256sum -c {} from {}",
                    manifest_arg.display(),
                    cwd.display()
                )
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .take(2)
                .collect::<Vec<_>>()
                .join("; ");
            return Ok(Some(format!(
                "{} failed sha256 verification{}{}",
                manifest_path.display(),
                if detail.is_empty() { "" } else { ": " },
                detail
            )));
        }
    }
    Ok(None)
}

fn sha256_manifest_check_context(manifest_path: &Path) -> Option<(PathBuf, PathBuf)> {
    let parent = manifest_path.parent()?;
    let file_name = manifest_path.file_name()?;
    let parent_name = parent.file_name().and_then(|name| name.to_str());
    if matches!(parent_name, Some("manifest" | "manifests"))
        && let (Some(root), Some(parent_file_name)) = (parent.parent(), parent.file_name())
    {
        return Some((
            root.to_path_buf(),
            PathBuf::from(parent_file_name).join(file_name),
        ));
    }
    Some((parent.to_path_buf(), PathBuf::from(file_name)))
}

fn inspect_handoff_manifest_entries(manifest_path: &Path) -> Result<Option<String>> {
    let content = fs::read_to_string(manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest_name = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sha256_manifest.txt");
    for (idx, line) in content.lines().enumerate() {
        let Some(entry_path) = parse_sha256_manifest_entry_path(line) else {
            continue;
        };
        if manifest_entry_points_to_self(&entry_path, manifest_path) {
            return Ok(Some(format!(
                "{manifest_name} includes itself on line {}; generate the manifest after all final files and exclude the manifest file",
                idx + 1
            )));
        }
        if let Some(reason) = volatile_handoff_manifest_entry_reason(&entry_path) {
            return Ok(Some(format!(
                "{manifest_name} includes volatile entry `{entry_path}` on line {} ({reason}); exclude active transcripts/logs from the final manifest or freeze them before hashing and do not append afterward",
                idx + 1
            )));
        }
    }
    Ok(None)
}

fn parse_sha256_manifest_entry_path(line: &str) -> Option<String> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let hash = trimmed.get(..64)?;
    if !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let rest = trimmed.get(64..)?.trim_start();
    let path = rest.strip_prefix('*').unwrap_or(rest).trim_start();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn manifest_entry_points_to_self(entry_path: &str, manifest_path: &Path) -> bool {
    let entry = Path::new(entry_path);
    if entry.is_absolute() {
        return entry == manifest_path;
    }
    if manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| entry_path == name)
    {
        return true;
    }
    let Some(parent) = manifest_path.parent() else {
        return false;
    };
    if !matches!(
        parent.file_name().and_then(|name| name.to_str()),
        Some("manifest" | "manifests")
    ) {
        return false;
    }
    let Some(file_name) = manifest_path.file_name() else {
        return false;
    };
    parent
        .file_name()
        .is_some_and(|parent_name| entry == Path::new(parent_name).join(file_name))
}

fn volatile_handoff_manifest_entry_reason(entry_path: &str) -> Option<&'static str> {
    let normalized = entry_path.replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    match name {
        "command_transcript.log" => {
            Some("command transcripts are often appended after hash generation")
        }
        "manifest_verification.log" => {
            Some("manifest verification logs are commonly written after hash generation")
        }
        "sha256_manifest.verify.log" => {
            Some("manifest verification logs are commonly written after hash generation")
        }
        "job.log" | "job_stdout.log" | "job_stderr.log" => {
            Some("job logs may still be live when final handoff manifests are generated")
        }
        _ => None,
    }
}

fn collect_handoff_artifact_stats(
    dir: &Path,
    depth: usize,
    stats: &mut HandoffArtifactStats,
) -> Result<()> {
    if depth > 2 {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_handoff_artifact_stats(&path, depth + 1, stats)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        stats.files += 1;
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            let lower_name = name.to_ascii_lowercase();
            if lower_name.ends_with(".md") || lower_name.ends_with(".txt") {
                stats.has_report = true;
            }
            if handoff_text_file_contains_completion_checklist(&path)? {
                stats.has_checklist = true;
                stats.checklist_paths.push(path.clone());
            }
            let Some(kind) = handoff_file_kind(name) else {
                continue;
            };
            match kind {
                HandoffFileKind::Checklist => {
                    stats.has_checklist = true;
                    if !stats.checklist_paths.iter().any(|seen| seen == &path) {
                        stats.checklist_paths.push(path.clone());
                    }
                }
                HandoffFileKind::Manifest => {
                    stats.has_manifest = true;
                    stats.manifest_paths.push(path.clone());
                }
                HandoffFileKind::Report => stats.has_report = true,
                HandoffFileKind::Structured => stats.has_structured = true,
            }
        }
    }
    Ok(())
}

fn handoff_text_file_contains_completion_checklist(path: &Path) -> Result<bool> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    let lower = name.to_ascii_lowercase();
    if lower.contains("transcript") || lower.contains("log") {
        return Ok(false);
    }
    if !(lower.ends_with(".md") || lower.ends_with(".txt")) {
        return Ok(false);
    }
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if metadata.len() > 2_000_000 {
        return Ok(false);
    }
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(message_has_substantive_completion_checklist(&content))
}

enum HandoffFileKind {
    Checklist,
    Manifest,
    Report,
    Structured,
}

fn handoff_file_kind(name: &str) -> Option<HandoffFileKind> {
    let lower = name.to_ascii_lowercase();
    if lower == "team_completion_checklist.md" || lower.contains("completion_checklist") {
        return Some(HandoffFileKind::Checklist);
    }
    if lower == "sha256_manifest.txt"
        || lower == "checksums.sha256"
        || lower == "checksum.sha256"
        || lower.ends_with("_manifest.sha256")
        || lower.ends_with("manifest.sha256")
    {
        return Some(HandoffFileKind::Manifest);
    }
    if lower.ends_with(".json") || lower.ends_with(".yaml") || lower.ends_with(".yml") {
        return Some(HandoffFileKind::Structured);
    }
    if lower.ends_with(".csv") {
        return Some(HandoffFileKind::Structured);
    }
    if lower.ends_with(".log")
        && (lower.contains("manifest_check") || lower.contains("sha256_manifest_check"))
    {
        return Some(HandoffFileKind::Report);
    }
    if handoff_markdown_name_looks_structured(&lower) {
        return Some(HandoffFileKind::Structured);
    }
    if lower.ends_with(".md") || lower.ends_with(".txt") {
        return Some(HandoffFileKind::Report);
    }
    None
}

fn handoff_markdown_name_looks_structured(lower_name: &str) -> bool {
    if !(lower_name.ends_with(".md") || lower_name.ends_with(".txt")) {
        return false;
    }
    [
        "audit_status",
        "claim_evidence",
        "evidence_review",
        "gate_matrix",
        "ledger",
        "manifest_validation",
        "metrics",
        "status",
        "validation_report",
    ]
    .iter()
    .any(|marker| lower_name.contains(marker))
}

fn maybe_send_lead_autonomy_tick(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    last_tick: &mut Instant,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_tick) < interval {
        return Ok(());
    }
    *last_tick = now_instant;
    if interactive_lead_attached(team_dir)? {
        append_event(
            team_dir,
            "lead_autonomy_tick_deferred_for_interactive_tui",
            serde_json::json!({
                "lead": config.lead,
                "reason": "interactive lead TUI is attached; do not steal the user's prompt",
            }),
        )?;
        return Ok(());
    }

    let tasks = load_tasks(team_dir)?;
    let waits = load_waits(team_dir)?;
    let open_tasks = tasks
        .iter()
        .filter(|task| task_is_open(task))
        .collect::<Vec<_>>();
    let open_waits = waits
        .iter()
        .filter(|wait| wait.status.is_open())
        .collect::<Vec<_>>();
    let next_action_lines = collect_recent_next_action_signals(team_dir, 4)?;
    let goal_requests_continuation = team_goal_requests_continuation(&config.goal);
    if open_tasks.is_empty()
        && open_waits.is_empty()
        && active.values().all(|run| run.completed)
        && (!goal_requests_continuation || next_action_lines.is_empty())
    {
        return Ok(());
    }
    if let Some(lead_run) = active.get(&config.lead)
        && let Some(remaining) = app_server_retry_remaining(lead_run)
    {
        append_event(
            team_dir,
            "lead_autonomy_tick_suppressed",
            serde_json::json!({
                "lead": config.lead,
                "reason": "temporary app-server/model usage-limit cooldown",
                "retry_after_sec": remaining.as_secs(),
                "open_tasks": open_tasks.len(),
                "active_turns": active.values().filter(|run| !run.completed).count(),
            }),
        )?;
        return Ok(());
    }
    if let Some(lead_run) = active.get(&config.lead)
        && !lead_run.completed
    {
        append_event(
            team_dir,
            "lead_autonomy_tick_skipped",
            serde_json::json!({
                "lead": config.lead,
                "reason": "lead_turn_active",
                "open_tasks": open_tasks.len(),
                "open_waits": open_waits.len(),
                "quiet_for_sec": now_instant.duration_since(lead_run.last_activity_at).as_secs(),
                "last_activity": lead_run.last_activity_kind,
            }),
        )?;
        return Ok(());
    }

    let mut active_lines = active
        .values()
        .map(|run| {
            let state = if run.completed { "idle" } else { "active" };
            let quiet_for = now_instant.duration_since(run.last_activity_at).as_secs();
            format!(
                "- @{name} role={role} node={node} state={state} quiet_for={quiet_for}s last={last}",
                name = run.member.name,
                role = run.member.role,
                node = run.node_id,
                last = compact_one_line(&run.last_activity_kind, 80)
            )
        })
        .collect::<Vec<_>>();
    active_lines.sort();

    let open_task_lines = open_tasks
        .iter()
        .take(8)
        .map(|task| {
            let owner = task.owner.as_deref().unwrap_or("unassigned");
            let age = task_age_secs(task)
                .map(|age| format!("{age}s"))
                .unwrap_or_else(|| "unknown".to_string());
            let subject = compact_one_line(&task.subject, 160);
            format!(
                "- task {id} [{status}] @{owner} age={age}: {subject}",
                id = task.id,
                status = task.status,
                subject = subject
            )
        })
        .collect::<Vec<_>>();
    let omitted = open_tasks.len().saturating_sub(open_task_lines.len());
    let omitted_line = if omitted > 0 {
        format!("\n- ... {omitted} more open tasks")
    } else {
        String::new()
    };
    let mut task_owner_cooldown_lines = Vec::new();
    let mut seen_cooldown_owners = HashSet::new();
    for task in &open_tasks {
        let Some(owner) = task.owner.as_deref() else {
            continue;
        };
        if !seen_cooldown_owners.insert(owner.to_string()) {
            continue;
        }
        if let Some(remaining) = recent_usage_limit_retry_remaining(team_dir, owner)? {
            let owner_open_tasks = open_tasks
                .iter()
                .filter(|candidate| candidate.owner.as_deref() == Some(owner))
                .map(|candidate| format!("#{}", candidate.id))
                .collect::<Vec<_>>()
                .join(", ");
            task_owner_cooldown_lines.push(format!(
                "- @{owner} owns open task(s) {owner_open_tasks} but is in usage-limit cooldown for {duration}; lead must either wait explicitly, reassign, or create a safe alternate task owner.",
                duration = format_compact_duration(remaining.as_secs())
            ));
        }
    }
    let task_owner_cooldown_block = if task_owner_cooldown_lines.is_empty() {
        "- none".to_string()
    } else {
        task_owner_cooldown_lines.join("\n")
    };
    let open_wait_lines = open_waits
        .iter()
        .take(6)
        .map(|wait| {
            let condition = compact_one_line(&wait.condition, 120);
            let progress = compact_one_line(&wait.progress, 120);
            format!(
                "- wait {id} [{status}] owner=@{owner} task={task} node={node} condition={condition} progress={progress}",
                id = wait.id,
                status = wait.status,
                owner = wait.owner.as_deref().unwrap_or("unassigned"),
                task = wait.task_id.as_deref().unwrap_or("-"),
                node = wait.node.as_deref().unwrap_or("-"),
                condition = condition,
                progress = progress
            )
        })
        .collect::<Vec<_>>();
    let omitted_waits = open_waits.len().saturating_sub(open_wait_lines.len());
    let omitted_waits_line = if omitted_waits > 0 {
        format!("\n- ... {omitted_waits} more open waits")
    } else {
        String::new()
    };
    let proposal_lines = collect_recent_lead_proposals(team_dir, &config.lead, 3)?;
    let proposal_block = if proposal_lines.is_empty() {
        "- none".to_string()
    } else {
        proposal_lines.join("\n")
    };
    let next_action_block = if next_action_lines.is_empty() {
        "- none".to_string()
    } else {
        next_action_lines.join("\n")
    };
    let continuation_policy = if goal_requests_continuation {
        if language.is_ja() {
            "この team の goal は継続・反復を明示しています。open task/open wait がなく、audit/evaluation/handoff に `NEXT_ACTION:` / `RECOMMENDED_NEXT_ACTION:` / `FOLLOW_UP:` がある場合、idle とみなさず、lead が次の許可済み task/owner/wait/job を作るか、ユーザー入力が必要な blocker を明示してください。domain 固有の cycle 粒度や監査条件は、goal・skill・外部仕様が明示した場合だけ使ってください。".to_string()
        } else {
            "This team's goal explicitly requests continuation/iteration. If there are no open tasks or waits but audit/evaluation/handoff artifacts contain `NEXT_ACTION:`, `RECOMMENDED_NEXT_ACTION:`, or `FOLLOW_UP:`, do not treat the team as idle; lead must either create the next authorized tasks/owners/waits/jobs or record the concrete blocker requiring user input. Use domain-specific cycle size and audit gates only when the goal, skill, or external spec explicitly defines them.".to_string()
        }
    } else if language.is_ja() {
        "この team の goal は明示的な継続 loop を要求していません。next action signal は参考情報として扱い、勝手に新しい改善 loop を作らず、必要ならユーザー入力待ちを明示してください。".to_string()
    } else {
        "This team's goal does not explicitly request a continuation loop. Treat next-action signals as advisory context; do not invent a new improvement loop unless the user's instructions require it, and record user-input wait when needed.".to_string()
    };
    let message = if language.is_ja() {
        format!(
            "Lead autonomy tick: あなたがこの team の意思決定オーケストレーターです。runtime は状態を届けているだけです。\n\nAction checklist: open task/wait/mailbox/job/artifact を見て、必要な steer/resume/reassign/review/standby を1つ以上具体化してください。open wait がある task は完了扱いにせず、completed/failed/blocked wait は owner を resume して実結果を確認させてください。`LEAD_PROPOSAL:` は採用/却下を明示してください。判断が分かれる未解決点、部署間 interface、runtime/環境選択、QA 境界、handoff 解釈、弱い evidence がある場合は、status 収集だけで終わらず、関係部署に具体的な質問を投げて短い対話を発生させてください。{continuation_policy}\n\nOpen tasks:\n{}{omitted_line}\n\nOpen task owner cooldowns:\n{task_owner_cooldown_block}\n\nOpen waits:\n{}{omitted_waits_line}\n\nRecent LEAD_PROPOSAL signals:\n{proposal_block}\n\nRecent artifact next-action signals:\n{next_action_block}\n\nActive turns:\n{}",
            if open_task_lines.is_empty() {
                "- none".to_string()
            } else {
                open_task_lines.join("\n")
            },
            if open_wait_lines.is_empty() {
                "- none".to_string()
            } else {
                open_wait_lines.join("\n")
            },
            if active_lines.is_empty() {
                "- none".to_string()
            } else {
                active_lines.join("\n")
            }
        )
    } else {
        format!(
            "Lead autonomy tick: you are the decision-making orchestrator for this team. The runtime is only delivering state.\n\nAction checklist: inspect open tasks/waits/mailboxes/jobs/artifacts, then make any concrete steer/resume/reassign/review/standby decision needed. Never complete a task with an open wait; when a wait is completed/failed/blocked, resume its owner to inspect the real result. Explicitly accept or reject each `LEAD_PROPOSAL:`. If there is an unresolved judgment call, cross-department interface, runtime/environment choice, QA boundary, handoff interpretation, or weak evidence, do not stop at status collection; ask the relevant departments concrete questions and make a short discussion happen. {continuation_policy}\n\nOpen tasks:\n{}{omitted_line}\n\nOpen task owner cooldowns:\n{task_owner_cooldown_block}\n\nOpen waits:\n{}{omitted_waits_line}\n\nRecent LEAD_PROPOSAL signals:\n{proposal_block}\n\nRecent artifact next-action signals:\n{next_action_block}\n\nActive turns:\n{}",
            if open_task_lines.is_empty() {
                "- none".to_string()
            } else {
                open_task_lines.join("\n")
            },
            if open_wait_lines.is_empty() {
                "- none".to_string()
            } else {
                open_wait_lines.join("\n")
            },
            if active_lines.is_empty() {
                "- none".to_string()
            } else {
                active_lines.join("\n")
            }
        )
    };
    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
    append_event(
        team_dir,
        "lead_autonomy_tick_sent",
        serde_json::json!({
            "lead": config.lead,
            "open_tasks": open_tasks.len(),
            "open_waits": open_waits.len(),
            "next_action_signals": next_action_lines.len(),
            "goal_requests_continuation": goal_requests_continuation,
            "active_turns": active.values().filter(|run| !run.completed).count(),
        }),
    )?;
    Ok(())
}

fn team_goal_requests_continuation(goal: &str) -> bool {
    explicit_team_control_marker(goal, "CODEX_TEAM_CONTINUATION")
}

fn team_goal_requests_autoresearch_loop(goal: &str) -> bool {
    explicit_team_control_marker(goal, "CODEX_TEAM_AUTORESEARCH_LOOP")
}

fn explicit_team_control_marker(text: &str, marker: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == marker
            || trimmed == format!("{marker}=1")
            || trimmed == format!("{marker}: true")
            || trimmed == format!("{marker}: yes")
    })
}

fn collect_recent_lead_proposals(team_dir: &Path, lead: &str, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, lead))?;
    let resolved_after = latest_lead_proposal_resolution_timestamp(team_dir, lead)?;
    let mut proposals = messages
        .iter()
        .rev()
        .filter(|message| is_real_lead_proposal_message(message))
        .filter(|message| {
            resolved_after
                .as_deref()
                .is_none_or(|cutoff| message.timestamp.as_str() > cutoff)
        })
        .take(limit)
        .map(format_lead_proposal_summary)
        .collect::<Vec<_>>();
    proposals.reverse();
    Ok(proposals)
}

fn collect_recent_lead_proposals_for_task(
    team_dir: &Path,
    lead: &str,
    task_id: &str,
    limit: usize,
) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let task_token = format!("task {task_id}");
    let task_hash_token = format!("task-{task_id}");
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, lead))?;
    let resolved_after = latest_lead_proposal_resolution_timestamp(team_dir, lead)?;
    let mut proposals = messages
        .iter()
        .rev()
        .filter(|message| is_real_lead_proposal_message(message))
        .filter(|message| {
            resolved_after
                .as_deref()
                .is_none_or(|cutoff| message.timestamp.as_str() > cutoff)
        })
        .filter(|message| {
            message
                .message
                .to_ascii_lowercase()
                .contains(&task_token.to_ascii_lowercase())
                || message
                    .message
                    .to_ascii_lowercase()
                    .contains(&task_hash_token.to_ascii_lowercase())
        })
        .take(limit)
        .map(format_lead_proposal_summary)
        .collect::<Vec<_>>();
    proposals.reverse();
    Ok(proposals)
}

fn latest_lead_proposal_resolution_timestamp(
    team_dir: &Path,
    lead: &str,
) -> Result<Option<String>> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    Ok(events
        .iter()
        .rev()
        .find(|event| {
            if event.event != "message_sent" {
                return false;
            }
            let from = event
                .data
                .get("from")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if from != lead {
                return false;
            }
            let message = event
                .data
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            message_mentions_lead_proposal_resolution(message)
        })
        .map(|event| event.timestamp.clone()))
}

fn message_mentions_lead_proposal_resolution(message: &str) -> bool {
    message.lines().any(|line| {
        let upper = line.trim_start().to_ascii_uppercase();
        upper.starts_with("LEAD_PROPOSAL_RESOLUTION:")
            || upper.starts_with("LEAD_PROPOSAL_ACCEPTED:")
            || upper.starts_with("LEAD_PROPOSAL_REJECTED:")
    })
}

fn is_real_lead_proposal_message(message: &MailMessage) -> bool {
    let from = message.from.trim().trim_start_matches('@');
    if from.eq_ignore_ascii_case("system") {
        return false;
    }
    let text = message.message.trim_start();
    if text.starts_with("Lead autonomy tick:")
        || text.starts_with("Department heartbeat")
        || text.starts_with("Department idle wakeup")
        || text.starts_with("TASK_COMPLETION_FREEZE:")
        || text.starts_with("JOB_STATUS:")
        || text.starts_with("AUX_JOB_STATUS:")
    {
        return false;
    }
    text.contains("LEAD_PROPOSAL:")
}

fn format_lead_proposal_summary(message: &MailMessage) -> String {
    format!(
        "- [{}] @{}: {}",
        message.timestamp,
        message.from,
        compact_one_line(&message.message, 320)
    )
}

fn collect_recent_next_action_signals(team_dir: &Path, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut dirs = vec![team_dir.to_path_buf()];
    let mut candidates = Vec::new();
    for ownership in load_ownerships(team_dir)? {
        let path = PathBuf::from(&ownership.path);
        if path.is_file() {
            candidates.push(path);
        } else if path.is_dir() {
            dirs.push(path);
        }
    }
    for dir in dirs {
        collect_next_action_candidate_files(&dir, 0, &mut candidates);
    }

    candidates.sort_by(|left, right| {
        let left_mtime = left
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok();
        let right_mtime = right
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok();
        right_mtime.cmp(&left_mtime)
    });

    let mut seen_paths = HashSet::new();
    let mut seen_signals = HashSet::new();
    let mut signals = Vec::new();
    for path in candidates {
        if signals.len() >= limit {
            break;
        }
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        for line in extract_next_action_lines(&path)? {
            let normalized = compact_one_line(&line, 160).to_ascii_lowercase();
            if normalized.is_empty() || !seen_signals.insert(normalized) {
                continue;
            }
            signals.push(format!(
                "- {}: {}",
                path.display(),
                compact_one_line(&line, 180)
            ));
            if signals.len() >= limit {
                break;
            }
        }
    }
    Ok(signals)
}

fn collect_next_action_candidate_files(dir: &Path, depth: usize, candidates: &mut Vec<PathBuf>) {
    if depth > 3 || candidates.len() > 200 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_next_action_candidate_files(&path, depth + 1, candidates);
        } else if file_type.is_file() && is_next_action_candidate_file(&path) {
            candidates.push(path);
        }
        if candidates.len() > 200 {
            break;
        }
    }
}

fn is_next_action_candidate_file(path: &Path) -> bool {
    if path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("live_messages" | "last_messages" | "mailboxes" | "job_status_notifications")
        )
    }) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let has_report_name = [
        "audit", "report", "summary", "handoff", "status", "outcome", "progress",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let has_supported_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "json" | "txt"
            )
        })
        .unwrap_or(false);
    has_report_name && has_supported_extension
}

fn extract_next_action_lines(path: &Path) -> Result<Vec<String>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat next-action candidate {}", path.display()))?;
    if metadata.len() > 512 * 1024 {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read next-action candidate {}", path.display()))?;
    let mut lines = Vec::new();
    let all_lines = text.lines().collect::<Vec<_>>();
    for (idx, line) in all_lines.iter().enumerate() {
        if !line_mentions_next_action(line) {
            continue;
        }
        let mut signal = line.trim().to_string();
        if signal.ends_with(':')
            && let Some(next_line) = all_lines.iter().skip(idx + 1).find(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
        {
            signal.push(' ');
            signal.push_str(next_line.trim());
        }
        lines.push(signal);
        if lines.len() >= 4 {
            break;
        }
    }
    Ok(lines)
}

fn line_mentions_next_action(line: &str) -> bool {
    let upper = line.trim_start().to_ascii_uppercase();
    upper.starts_with("NEXT_ACTION:")
        || upper.starts_with("RECOMMENDED_NEXT_ACTION:")
        || upper.starts_with("NEXT_CYCLE:")
        || upper.starts_with("FOLLOW_UP:")
}

fn maybe_send_department_idle_wakeups(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    idle_since: &mut HashMap<String, Instant>,
    last_wakeup: &mut HashMap<String, Instant>,
    last_batch: &mut Instant,
    cursor: &mut usize,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    const MAX_IDLE_WAKEUPS_PER_BATCH: usize = 2;

    let now_instant = Instant::now();
    let tasks = load_tasks(team_dir)?;
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let members = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| !matches!(member.status, MemberStatus::Failed | MemberStatus::Offline))
        .collect::<Vec<_>>();
    if members.is_empty() {
        return Ok(());
    }
    if *cursor >= members.len() {
        *cursor = 0;
    }

    let mut eligible = Vec::new();
    for (idx, member) in members.iter().enumerate() {
        let active_run = active.get(&member.name).is_some_and(|run| !run.completed);
        if active_run {
            idle_since.remove(&member.name);
            last_wakeup.remove(&member.name);
            continue;
        }
        let member_open_task_count = tasks
            .iter()
            .filter(|task| {
                task.owner.as_deref() == Some(member.name.as_str()) && task_is_open(task)
            })
            .count();
        let member_has_open_tasks = member_open_task_count > 0;
        let unread = mailbox_unread_counts(team_dir, &member.name).unwrap_or_default();
        if !member_has_open_tasks
            && unread.direct_unread == 0
            && let Some(backoff) = recent_stay_backoff_remaining(team_dir, &member.name, interval)?
        {
            if last_wakeup
                .get(&member.name)
                .is_some_and(|last| now_instant.duration_since(*last) < interval)
            {
                continue;
            }
            last_wakeup.insert(member.name.clone(), now_instant);
            idle_since.remove(&member.name);
            append_event(
                team_dir,
                "department_idle_wakeup_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "recent_stay_backoff",
                    "backoff_remaining_sec": backoff.as_secs(),
                    "owned_open_tasks": member_open_task_count,
                    "direct_unread": unread.direct_unread,
                }),
            )?;
            continue;
        }
        if let Some(unavailable) = member_node_unavailable_from_nodes(member, &nodes) {
            if last_wakeup
                .get(&member.name)
                .is_some_and(|last| now_instant.duration_since(*last) < interval)
            {
                continue;
            }
            last_wakeup.insert(member.name.clone(), now_instant);
            idle_since.remove(&member.name);
            append_event(
                team_dir,
                "department_idle_wakeup_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": unavailable.reason,
                    "node_status": unavailable.status,
                    "node_age": unavailable.age,
                    "owned_open_tasks": member_open_task_count,
                }),
            )?;
            continue;
        }
        if let Some(remaining) = recent_usage_limit_retry_remaining(team_dir, &member.name)? {
            if last_wakeup
                .get(&member.name)
                .is_some_and(|last| now_instant.duration_since(*last) < interval)
            {
                continue;
            }
            last_wakeup.insert(member.name.clone(), now_instant);
            idle_since.remove(&member.name);
            append_event(
                team_dir,
                "department_idle_wakeup_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "usage_limit_cooldown",
                    "retry_after_sec": remaining.as_secs(),
                    "owned_open_tasks": member_open_task_count,
                    "cooldown_source": "member",
                }),
            )?;
            continue;
        }
        if let Some(remaining) = should_suppress_empty_department_ping_during_cooldown(
            config,
            active,
            &member.name,
            member_has_open_tasks,
            active_run,
        ) {
            if last_wakeup
                .get(&member.name)
                .is_some_and(|last| now_instant.duration_since(*last) < interval)
            {
                continue;
            }
            last_wakeup.insert(member.name.clone(), now_instant);
            idle_since.remove(&member.name);
            append_event(
                team_dir,
                "department_idle_wakeup_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "usage_limit_cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            continue;
        }

        let since = idle_since.entry(member.name.clone()).or_insert(now_instant);
        let idle_for = now_instant.duration_since(*since);
        if idle_for < interval {
            continue;
        }
        if last_wakeup
            .get(&member.name)
            .is_some_and(|last| now_instant.duration_since(*last) < interval)
        {
            continue;
        }
        eligible.push(idx);
    }
    if eligible.is_empty() || now_instant.duration_since(*last_batch) < interval {
        return Ok(());
    }

    let mut sent = 0_usize;
    let mut last_sent_idx = None;
    for offset in 0..members.len() {
        if sent >= MAX_IDLE_WAKEUPS_PER_BATCH {
            break;
        }
        let idx = (*cursor + offset) % members.len();
        if !eligible.contains(&idx) {
            continue;
        }
        let member = members[idx];
        let idle_for = idle_since
            .get(&member.name)
            .map(|since| now_instant.duration_since(*since))
            .unwrap_or_default();
        last_wakeup.insert(member.name.clone(), now_instant);
        sent += 1;
        last_sent_idx = Some(idx);

        let member_tasks = tasks
            .iter()
            .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
            .filter(|task| task_is_open(task))
            .take(5)
            .map(|task| {
                format!(
                    "- task {} [{}]: {}",
                    task.id,
                    task.status,
                    compact_one_line(&task.subject, 180)
                )
            })
            .collect::<Vec<_>>();
        let team_open_tasks = tasks
            .iter()
            .filter(|task| task_is_open(task))
            .take(8)
            .map(|task| {
                let owner = task.owner.as_deref().unwrap_or("-");
                format!(
                    "- task {} [{}] @{}: {}",
                    task.id,
                    task.status,
                    owner,
                    compact_one_line(&task.subject, 180)
                )
            })
            .collect::<Vec<_>>();
        let message = format!(
            "{}",
            if language.is_ja() {
                format!(
                    "Department idle wakeup for @{name}: {idle}s active turn がありません。自分宛て未読/担当task/明確なready gateだけ確認してください。再開・誤割当・重複・支援提案がある時は `LEAD_PROPOSAL:` を evidence 付きで lead へ。さらに、設計/実行/検証/引き継ぎで自分の判断が他部署の判断に依存する点を見つけたら、status ではなく具体的な質問をその部署へ送ってください。不要なら `STAY:` 一行だけで終了してください。\n\nYour open tasks:\n{member_tasks}\n\nTeam open tasks snapshot:\n{team_tasks}",
                    name = member.name,
                    idle = idle_for.as_secs(),
                    member_tasks = if member_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        member_tasks.join("\n")
                    },
                    team_tasks = if team_open_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        team_open_tasks.join("\n")
                    }
                )
            } else {
                format!(
                    "Department idle wakeup for @{name}: no active app-server turn for {idle}s. Check only direct unread mail, your open tasks, and obvious ready-gate/help proposals. Send lead `LEAD_PROPOSAL:` with evidence only if action is useful. Also, if you find a design/execution/verification/handoff decision where your judgment depends on another department, send that department a concrete question instead of a status update. Otherwise send one-line `STAY:` and stop.\n\nYour open tasks:\n{member_tasks}\n\nTeam open tasks snapshot:\n{team_tasks}",
                    name = member.name,
                    idle = idle_for.as_secs(),
                    member_tasks = if member_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        member_tasks.join("\n")
                    },
                    team_tasks = if team_open_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        team_open_tasks.join("\n")
                    }
                )
            },
        );
        send_team_message_to_dir(team_dir, "system", &member.name, &message)?;
        append_event(
            team_dir,
            "department_idle_wakeup_sent",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "node": member_node_id(member),
                "idle_for_sec": idle_for.as_secs(),
                "owned_open_tasks": member_tasks.len(),
                "batch_limit": MAX_IDLE_WAKEUPS_PER_BATCH,
            }),
        )?;
    }
    if sent > 0 {
        *last_batch = now_instant;
        if let Some(idx) = last_sent_idx {
            *cursor = (idx + 1) % members.len();
        }
    }
    Ok(())
}

fn recent_stay_backoff_remaining(
    team_dir: &Path,
    member_name: &str,
    base_interval: Duration,
) -> Result<Option<Duration>> {
    let events_path = team_dir.join("events.jsonl");
    if !events_path.exists() {
        return Ok(None);
    }
    let events = read_jsonl::<TeamEventRecord>(&events_path)?;
    let mut stay_count = 0_u32;
    let mut latest_stay_timestamp = None::<chrono::DateTime<Utc>>;
    for event in events.into_iter().rev().take(400) {
        if event.event != "message_sent" {
            continue;
        }
        let Some(from) = event.data.get("from").and_then(|value| value.as_str()) else {
            continue;
        };
        if from != member_name {
            continue;
        }
        let message = event
            .data
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if is_stay_message(message) {
            stay_count += 1;
            if latest_stay_timestamp.is_none()
                && let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
            {
                latest_stay_timestamp = Some(timestamp.with_timezone(&Utc));
            }
            continue;
        }
        break;
    }
    if stay_count == 0 {
        return Ok(None);
    }
    let Some(latest_stay_timestamp) = latest_stay_timestamp else {
        return Ok(None);
    };
    let elapsed = Utc::now() - latest_stay_timestamp;
    let Ok(elapsed) = elapsed.to_std() else {
        return Ok(None);
    };
    let multiplier = 2_u32.saturating_pow(stay_count.min(3));
    let backoff = base_interval.saturating_mul(multiplier);
    if elapsed >= backoff {
        Ok(None)
    } else {
        Ok(Some(backoff - elapsed))
    }
}

fn seed_department_idle_wakeup_cooldowns(
    team_dir: &Path,
    last_wakeup: &mut HashMap<String, Instant>,
    last_batch: &mut Instant,
    interval: Duration,
) -> Result<()> {
    let events_path = team_dir.join("events.jsonl");
    if !events_path.exists() {
        return Ok(());
    }

    let events = read_jsonl::<TeamEventRecord>(&events_path)?;
    let now_utc = Utc::now();
    let now_instant = Instant::now();
    let mut newest_elapsed = None::<Duration>;

    for event in events
        .into_iter()
        .filter(|event| event.event == "department_idle_wakeup_sent")
    {
        let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&event.timestamp) else {
            continue;
        };
        let elapsed = now_utc - timestamp.with_timezone(&Utc);
        if elapsed < chrono::Duration::zero() {
            continue;
        }
        let Ok(elapsed_std) = elapsed.to_std() else {
            continue;
        };
        if elapsed_std >= interval {
            continue;
        }
        let Some(member) = event
            .data
            .get("member")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        last_wakeup.insert(member.to_string(), now_instant - elapsed_std);
        newest_elapsed = Some(newest_elapsed.map_or(elapsed_std, |current| {
            if elapsed_std < current {
                elapsed_std
            } else {
                current
            }
        }));
    }

    if let Some(elapsed) = newest_elapsed {
        *last_batch = now_instant - elapsed;
    }
    Ok(())
}

fn maybe_send_department_heartbeats(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    heartbeats: &mut HashMap<String, Instant>,
    recent_idle_wakeups: &HashMap<String, Instant>,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    let tasks = load_tasks(team_dir)?;
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    for member in config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| !matches!(member.status, MemberStatus::Failed | MemberStatus::Offline))
    {
        let member_tasks = tasks
            .iter()
            .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
            .filter(|task| task_is_open(task))
            .collect::<Vec<_>>();
        let active_run = active.get(&member.name).is_some_and(|run| !run.completed);
        if recent_idle_wakeups
            .get(&member.name)
            .is_some_and(|last| now_instant.duration_since(*last) < interval)
        {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "recent_idle_wakeup",
                }),
            )?;
            continue;
        }
        if member_tasks.is_empty() && matches!(member.status, MemberStatus::Completed) {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "completed_no_open_tasks",
                    "active_turn": active_run,
                }),
            )?;
            continue;
        }
        if active_run {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "active_turn_in_progress",
                    "owned_open_tasks": member_tasks.len(),
                }),
            )?;
            continue;
        }
        if !active_run && let Some(unavailable) = member_node_unavailable_from_nodes(member, &nodes)
        {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": unavailable.reason,
                    "node_status": unavailable.status,
                    "node_age": unavailable.age,
                    "owned_open_tasks": member_tasks.len(),
                }),
            )?;
            continue;
        }
        if !active_run
            && let Some(remaining) = recent_usage_limit_retry_remaining(team_dir, &member.name)?
        {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "usage_limit_cooldown",
                    "retry_after_sec": remaining.as_secs(),
                    "owned_open_tasks": member_tasks.len(),
                    "cooldown_source": "member",
                }),
            )?;
            continue;
        }
        if let Some(remaining) = should_suppress_empty_department_ping_during_cooldown(
            config,
            active,
            &member.name,
            !member_tasks.is_empty(),
            active_run,
        ) {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "usage_limit_cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            continue;
        }
        if member_tasks.is_empty() && !active_run {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "no_open_tasks",
                    "status": format!("{:?}", member.status),
                }),
            )?;
            continue;
        }
        if !active_run
            && mailbox_unread_counts(team_dir, &member.name)
                .unwrap_or_default()
                .direct_unread
                == 0
            && let Some(backoff) = recent_stay_backoff_remaining(team_dir, &member.name, interval)?
        {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "recent_stay_backoff",
                    "backoff_remaining_sec": backoff.as_secs(),
                    "open_tasks": member_tasks.len(),
                }),
            )?;
            continue;
        }
        if recent_idle_wakeups
            .get(&member.name)
            .is_some_and(|last| now_instant.duration_since(*last) < interval)
        {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "recent_idle_wakeup",
                }),
            )?;
            continue;
        }
        let entry = heartbeats
            .entry(member.name.clone())
            .or_insert(now_instant - interval);
        if now_instant.duration_since(*entry) < interval {
            continue;
        }
        *entry = now_instant;

        let task_lines = member_tasks
            .iter()
            .take(5)
            .map(|task| {
                format!(
                    "- task {} [{}]: {}",
                    task.id,
                    task.status,
                    compact_one_line(&task.subject, 180)
                )
            })
            .collect::<Vec<_>>();
        let node = member_node_id(member);
        let owned_tasks = if task_lines.is_empty() {
            "- none currently recorded, but your department is still active/standby".to_string()
        } else {
            task_lines.join("\n")
        };
        let message = if language.is_ja() {
            format!(
                "Department heartbeat for @{name}: 未完了 mission/task がある場合だけ、lead と consumer に短い status を送ってください。artifact/log/job/wait/request path、blocker、next checkpoint、必要な `LEAD_PROPOSAL:` を含めます。ただし判断が分かれる設計・実行・検証・handoff・interface・環境選択が残っている場合は、status だけで終わらず、関係部署へ具体的な質問または選択肢付き相談を送ってください。重い処理は team job/wait 登録を確認してください。manifest/checksum package は最終追記後に再検証してください。完了なら TEAM_COMPLETION_CHECKLIST と具体 artifact を出してください。\n\nOwned open tasks:\n{owned_tasks}",
                name = member.name
            )
        } else {
            format!(
                "Department heartbeat for @{name}: if your mission/task is still incomplete, send lead/consumers a short status with artifact/log/job/wait/request paths, blocker, next checkpoint, and any needed `LEAD_PROPOSAL:`. However, if an open design/execution/verification/handoff/interface/environment decision remains, do not stop at status; send the relevant department a concrete question or options-based consultation. Ensure heavy work is tracked as team job/wait. Recheck manifests/checksums after final writes. If complete, provide TEAM_COMPLETION_CHECKLIST with concrete artifacts.\n\nOwned open tasks:\n{owned_tasks}",
                name = member.name
            )
        };
        send_team_message_to_dir(team_dir, "system", &member.name, &message)?;
        append_event(
            team_dir,
            "department_heartbeat_sent",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "node": node,
                "open_tasks": member_tasks.len(),
                "active_turn": active_run,
            }),
        )?;
    }
    Ok(())
}

fn maybe_warn_stale_active_turns(
    team_dir: &Path,
    config: &TeamConfig,
    active: &mut HashMap<String, AppServerMemberRun>,
    last_check: &mut Instant,
    interval: Duration,
    stale_timeout: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_check) < interval {
        return Ok(());
    }
    *last_check = now_instant;

    let tasks = load_tasks(team_dir)?;
    for (member_name, run) in active.iter_mut() {
        if run.completed {
            continue;
        }
        let quiet_for = now_instant.duration_since(run.last_activity_at);
        if quiet_for < stale_timeout {
            continue;
        }
        if run
            .last_stale_notice_at
            .is_some_and(|last| now_instant.duration_since(last) < stale_timeout)
        {
            continue;
        }
        let repeated_stale = run.last_stale_notice_at.is_some();
        run.last_stale_notice_at = Some(now_instant);
        let member_tasks = tasks
            .iter()
            .filter(|task| task.owner.as_deref() == Some(member_name.as_str()))
            .filter(|task| task_is_open(task))
            .take(8)
            .map(|task| format!("- task {} [{}]: {}", task.id, task.status, task.subject))
            .collect::<Vec<_>>();
        let task_summary = if member_tasks.is_empty() {
            "- no open owned task recorded".to_string()
        } else {
            member_tasks.join("\n")
        };
        let escalation = if repeated_stale {
            "\n\nEscalation: this member has already received at least one stale-turn notice in this active turn. If the previous notice did not produce concrete status, artifact growth, a tracked job id, or a real blocker, do not keep repeating generic check-ins. Inspect the owned artifact path(s), jobs, and mailbox, then either steer a very specific next checkpoint, cancel/reassign/recover the task to a recovery owner, or mark it blocked with evidence. Preserve any partial files as draft-only until the recovery owner verifies and re-manifests them."
        } else {
            ""
        };
        let lead_message = if language.is_ja() {
            format!(
                "Stale active turn attention: @{member} は active な app-server turn を持っていますが、team runtime は {quiet}s の間 assistant output を観測していません。Last observed activity: {last}。これは通常の長い MCP/tool call、遅いが妥当な部署ペース、blocked remote/container operation、または wedged turn の可能性があります。\n\nあなたは lead として、部署に低品質な partial work を急がせず recovery action を決めてください。まず observability を求めてください: current subtask、tool/job/MCP/remote/container operation が実行中か、関連 request/job/log/artifact path、risks、next planned checkpoint。evidence が stuck / mis-scoped / waiting on another actor を示す場合だけ、exact next step で steer、reassign、または task を blocked にしてください。artifact、handoff message、required verification evidence が揃うまで owned task を complete とみなさないでください。{escalation}\n\nOwned open tasks:\n{tasks}",
                member = member_name,
                quiet = quiet_for.as_secs(),
                last = run.last_activity_kind,
                escalation = escalation,
                tasks = task_summary
            )
        } else {
            format!(
                "Stale active turn attention: @{member} has an app-server turn marked active, but the team runtime has observed no assistant output for {quiet}s. Last observed activity: {last}. This may be a normal long MCP/tool call, a slow but valid department pace, a blocked remote/container operation, or a wedged turn.\n\nYou are the lead and must decide the recovery action without pressuring the department to ship low-quality partial work. First ask for observability: current subtask, whether a tool/job/MCP/remote/container operation is running, relevant request/job/log/artifact paths, risks, and the next planned checkpoint. Only steer with an exact next step, reassign work, or mark the task blocked when evidence shows the work is stuck, mis-scoped, or waiting on another actor. Do not assume the owned task is complete until artifacts, handoff messages, and required verification evidence exist.{escalation}\n\nOwned open tasks:\n{tasks}",
                member = member_name,
                quiet = quiet_for.as_secs(),
                last = run.last_activity_kind,
                escalation = escalation,
                tasks = task_summary
            )
        };
        send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
        if member_name != &config.lead {
            let member_message = if language.is_ja() {
                format!(
                    "Automated lead status check: あなたの現在の app-server turn では {quiet}s の間 assistant output が観測されていません。これは急げ、または品質を下げろという要求ではありません。今すぐ lead に current status を報告してください: current subtask、MCP/tool/job/remote/container work を待っているか、関連 request/job/log/artifact path、risks、next checkpoint。work が広い/重い場合、subagent/agent tools、skills、MCP servers、internal decomposition が使えるなら積極的に使い、その使い方を報告してください。blocked なら concrete evidence 付きで owned task を blocked にしてください。",
                    quiet = quiet_for.as_secs()
                )
            } else {
                format!(
                    "Automated lead status check: your current app-server turn has had no observed assistant output for {quiet}s. This is not a demand to rush or lower quality. Report current status to lead now: current subtask, whether you are waiting on MCP/tool/job/remote/container work, relevant request/job/log/artifact paths, risks, and next checkpoint. If work is broad or heavy and subagent/agent tools, skills, MCP servers, or internal decomposition are available, use them proactively and mention how. If you are blocked, mark the owned task blocked with concrete evidence.",
                    quiet = quiet_for.as_secs()
                )
            };
            send_team_message_to_dir(team_dir, &config.lead, member_name, &member_message)?;
        }
        append_event(
            team_dir,
            "stale_active_turn_attention",
            serde_json::json!({
                "member": member_name,
                "node": run.node_id.clone(),
                "thread": run.thread_id.clone(),
                "turn": run.turn_id.clone(),
                "quiet_for_sec": quiet_for.as_secs(),
                "last_activity": run.last_activity_kind.clone(),
            }),
        )?;
    }
    Ok(())
}

fn task_is_open(task: &TeamTask) -> bool {
    !matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
    )
}

fn detect_team_wait_idle_state(
    team_dir: &Path,
    active: &HashMap<String, AppServerMemberRun>,
    quiet_active_threshold: Option<Duration>,
) -> Result<Option<TeamWaitIdleState>> {
    let tasks = load_tasks(team_dir)?;
    let open_tasks = tasks
        .iter()
        .filter(|task| task_is_open(task))
        .cloned()
        .collect::<Vec<_>>();
    let waits = load_waits(team_dir)?;
    let jobs = load_jobs(team_dir)?;
    let now_instant = Instant::now();

    let mut non_waiting_active_members = Vec::new();
    let mut long_active_members = Vec::new();
    for run in active.values().filter(|run| !run.completed) {
        let quiet_for = now_instant.duration_since(run.last_activity_at);
        if quiet_active_threshold.is_some_and(|threshold| quiet_for >= threshold) {
            long_active_members.push(run.member.name.clone());
            continue;
        }
        non_waiting_active_members.push(run.member.name.clone());
    }
    if !non_waiting_active_members.is_empty() {
        return Ok(None);
    }

    let mut wait_ids = waits
        .iter()
        .filter(|wait| wait_is_wait_idle_blocker(wait))
        .map(|wait| wait.id.clone())
        .collect::<Vec<_>>();
    wait_ids.sort();
    wait_ids.dedup();

    let mut job_ids = jobs
        .iter()
        .filter(|job| matches!(job.status, TeamJobStatus::Running | TeamJobStatus::Unknown))
        .map(|job| job.id.clone())
        .collect::<Vec<_>>();
    job_ids.sort();
    job_ids.dedup();

    let mut blocker_task_ids = waits
        .iter()
        .filter(|wait| wait_is_wait_idle_blocker(wait))
        .filter_map(|wait| wait.task_id.clone())
        .collect::<HashSet<_>>();
    blocker_task_ids.extend(
        jobs.iter()
            .filter(|job| matches!(job.status, TeamJobStatus::Running | TeamJobStatus::Unknown))
            .filter_map(|job| job.task_id.clone()),
    );
    blocker_task_ids.extend(open_tasks.iter().filter_map(|task| {
        task.owner.as_deref().and_then(|owner| {
            long_active_members
                .iter()
                .any(|member| member == owner)
                .then(|| task.id.clone())
        })
    }));

    if wait_ids.is_empty() && job_ids.is_empty() && long_active_members.is_empty() {
        return Ok(None);
    }
    if open_tasks.is_empty() {
        let mut state = TeamWaitIdleState {
            wait_ids,
            job_ids,
            task_ids: Vec::new(),
            active_members: long_active_members,
        };
        state.active_members.sort();
        state.active_members.dedup();
        return Ok((!state.is_empty()).then_some(state));
    }
    if blocker_task_ids.is_empty() {
        return Ok(None);
    }

    let task_by_id = open_tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let mut quiescent_task_ids = Vec::new();
    for task in &open_tasks {
        if task_waits_on_any_blocker(task, &task_by_id, &blocker_task_ids, &mut HashSet::new()) {
            quiescent_task_ids.push(task.id.clone());
        } else {
            return Ok(None);
        }
    }
    quiescent_task_ids.sort();
    quiescent_task_ids.dedup();
    long_active_members.sort();
    long_active_members.dedup();

    Ok(Some(TeamWaitIdleState {
        wait_ids,
        job_ids,
        task_ids: quiescent_task_ids,
        active_members: long_active_members,
    }))
}

fn wait_is_wait_idle_blocker(wait: &TeamWait) -> bool {
    if !matches!(
        wait.status,
        TeamWaitStatus::Waiting | TeamWaitStatus::Running | TeamWaitStatus::Polling
    ) {
        return false;
    }
    wait_looks_like_external_long_wait(wait) || !parse_wait_auto_checks(wait).is_empty()
}

fn task_waits_on_any_blocker(
    task: &TeamTask,
    task_by_id: &HashMap<&str, &TeamTask>,
    blocker_task_ids: &HashSet<String>,
    visiting: &mut HashSet<String>,
) -> bool {
    if blocker_task_ids.contains(&task.id) {
        return true;
    }
    if !visiting.insert(task.id.clone()) {
        return false;
    }
    for dep in &task.depends_on {
        if blocker_task_ids.contains(dep) {
            return true;
        }
        if let Some(dep_task) = task_by_id.get(dep.as_str())
            && task_waits_on_any_blocker(dep_task, task_by_id, blocker_task_ids, visiting)
        {
            return true;
        }
    }
    false
}

fn team_wait_idle_event_active(team_dir: &Path) -> bool {
    let Ok(events) = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")) else {
        return false;
    };
    for event in events.into_iter().rev().take(80) {
        match event.event.as_str() {
            "team_wait_idle_entered" => return true,
            "team_wait_idle_exited"
            | "team_runtime_paused"
            | "app_server_keep_alive_stopped"
            | "app_server_keep_alive_idle_timeout" => return false,
            _ => {}
        }
    }
    false
}

fn suppress_wait_idle_mailbox_chatter(
    team_dir: &Path,
    members: &[TeamMember],
    mailbox_counts: &mut HashMap<String, usize>,
) -> Result<bool> {
    let mut user_message_pending = false;
    for member in members {
        let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member.name))?;
        let seen = mailbox_counts
            .get(&member.name)
            .copied()
            .unwrap_or_default()
            .min(messages.len());
        let new_messages = messages.iter().skip(seen).collect::<Vec<_>>();
        if new_messages.is_empty() {
            continue;
        }
        let user_messages = new_messages
            .iter()
            .filter(|message| message.from == "user")
            .count();
        if user_messages > 0 {
            user_message_pending = true;
            append_event(
                team_dir,
                "team_wait_idle_user_message_pending",
                serde_json::json!({
                    "member": member.name,
                    "messages": new_messages.len(),
                    "user_messages": user_messages,
                    "reason": "explicit user input may override long-task wait idle",
                }),
            )?;
            continue;
        }
        acknowledge_mailbox_delivery(
            team_dir,
            mailbox_counts,
            &member.name,
            seen,
            new_messages.len(),
        )?;
        append_event(
            team_dir,
            "team_wait_idle_mailbox_suppressed",
            serde_json::json!({
                "member": member.name,
                "messages": new_messages.len(),
                "reason": "non-user mailbox chatter suppressed while all open work waits on long-running work",
            }),
        )?;
    }
    Ok(user_message_pending)
}

fn task_status_can_start_turn(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Pending
            | TaskStatus::Waiting
            | TaskStatus::Ready
            | TaskStatus::InProgress
            | TaskStatus::Review
    )
}

fn task_age_secs(task: &TeamTask) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(&task.updated_at)
        .ok()
        .map(|updated| (Utc::now() - updated.with_timezone(&Utc)).num_seconds())
}

fn wait_age_secs(wait: &TeamWait) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(&wait.updated_at)
        .ok()
        .map(|updated| (Utc::now() - updated.with_timezone(&Utc)).num_seconds())
}

fn active_external_wait_ids_for_member(team_dir: &Path, member_name: &str) -> Result<Vec<String>> {
    Ok(load_waits(team_dir)?
        .into_iter()
        .filter(|wait| wait.owner.as_deref() == Some(member_name))
        .filter(|wait| {
            matches!(
                wait.status,
                TeamWaitStatus::Running | TeamWaitStatus::Polling
            )
        })
        .filter(wait_looks_like_external_long_wait)
        .map(|wait| wait.id)
        .collect())
}

fn active_turn_token_pressure(team_dir: &Path, run: &AppServerMemberRun) -> Result<Option<i64>> {
    let Some(usage) = latest_thread_token_usage(team_dir, &run.node_id, &run.thread_id)? else {
        return Ok(None);
    };
    if thread_usage_exceeds_rotation_limit(&usage) {
        Ok(Some(usage.total.total_tokens))
    } else {
        Ok(None)
    }
}

fn record_deferred_active_turn_context(
    team_dir: &Path,
    run: &AppServerMemberRun,
    messages: &[MailMessage],
    wait_ids: &[String],
    language: TeamPromptLanguage,
) -> Result<Option<String>> {
    if messages.is_empty() {
        return Ok(None);
    }
    let path = side_channel_context_path(team_dir, &run.member.name);
    let sequence = read_jsonl::<SideChannelContextRecord>(&path)?.len() + 1;
    let id = sanitize_id(&format!(
        "deferredctx-{}-{}-{}",
        run.member.name, run.turn_id, sequence
    ));
    let wait_summary = if wait_ids.is_empty() {
        "-".to_string()
    } else {
        wait_ids.join(", ")
    };
    let reply = if language.is_ja() {
        format!(
            "この message 群は main turn が外部長期待ち ({wait_summary}) の間に届いたため、実行中 turn へ直接 steer せず保留されました。次に @{name} の main turn が再開・新規開始されたら、通常の team message と同じ制約/相談として取り込んでください。",
            name = run.member.name
        )
    } else {
        format!(
            "These messages arrived while the main turn was waiting on external long-running work ({wait_summary}), so they were not steered into the active turn. When @{name}'s main turn resumes or starts again, incorporate them as normal team-message constraints or questions.",
            name = run.member.name
        )
    };
    let record = SideChannelContextRecord {
        id: id.clone(),
        member: run.member.name.clone(),
        node: run.node_id.clone(),
        source_thread: run.thread_id.clone(),
        side_thread: String::new(),
        side_turn: String::new(),
        recipients: vec![run.member.name.clone()],
        incoming_summary: summarize_side_reply_messages(messages, language),
        reply,
        created_at: now(),
        status: SideChannelContextStatus::Pending,
        injected_turns: Vec::new(),
        injected_at: None,
        acknowledged_at: None,
    };
    append_jsonl(&path, &record)?;
    append_event(
        team_dir,
        "active_turn_mailbox_context_deferred",
        serde_json::json!({
            "member": run.member.name,
            "node": run.node_id,
            "thread": run.thread_id,
            "turn": run.turn_id,
            "context_id": id,
            "messages": messages.len(),
            "waits": wait_ids,
        }),
    )?;
    Ok(Some(id))
}

fn active_turn_messages_are_deferrable_system_nudges(messages: &[MailMessage]) -> bool {
    !messages.is_empty() && messages.iter().all(|message| message.from == "system")
}

fn active_turn_recently_steered(
    team_dir: &Path,
    run: &AppServerMemberRun,
    min_interval: Duration,
) -> Result<Option<Duration>> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    let now_utc = Utc::now();
    for event in events
        .into_iter()
        .rev()
        .filter(|event| event.event == "app_server_turn_steered")
    {
        let Some(member) = event.data.get("member").and_then(|value| value.as_str()) else {
            continue;
        };
        if member != run.member.name {
            continue;
        }
        let Some(thread) = event.data.get("thread").and_then(|value| value.as_str()) else {
            continue;
        };
        if thread != run.thread_id {
            continue;
        }
        let Some(turn) = event.data.get("turn").and_then(|value| value.as_str()) else {
            continue;
        };
        if turn != run.turn_id {
            continue;
        }
        let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&event.timestamp) else {
            continue;
        };
        let elapsed = now_utc - timestamp.with_timezone(&Utc);
        if elapsed < chrono::Duration::zero() {
            return Ok(Some(Duration::ZERO));
        }
        let Ok(elapsed) = elapsed.to_std() else {
            continue;
        };
        if elapsed < min_interval {
            return Ok(Some(min_interval.saturating_sub(elapsed)));
        }
        return Ok(None);
    }
    Ok(None)
}

async fn steer_new_team_messages(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    nodes: &[TeamNode],
    team_dir: &Path,
    members: &[TeamMember],
    active: &mut HashMap<String, AppServerMemberRun>,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    thread_to_member: &mut HashMap<String, String>,
    mailbox_counts: &mut HashMap<String, usize>,
    cwd: &Path,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    codex_exe: &Path,
    side_channel_replies: bool,
    relay_port: u16,
    language: TeamPromptLanguage,
) -> Result<()> {
    let mut by_recipient = HashMap::<String, PendingMailboxDelivery>::new();
    for member in members {
        if active
            .get(&member.name)
            .and_then(app_server_retry_remaining)
            .is_some()
        {
            continue;
        }
        let Some(pending) = collect_new_active_mailbox_messages(
            team_dir,
            member,
            active.contains_key(&member.name) && !matches!(member.status, MemberStatus::Offline),
            mailbox_counts,
        )?
        else {
            continue;
        };
        if !pending.messages.is_empty() {
            by_recipient.insert(member.name.clone(), pending);
        }
    }

    for (member_name, pending) in by_recipient {
        let messages = pending.messages;
        let Some(run) = active.get(&member_name).cloned() else {
            continue;
        };
        if repeated_artifact_plan_delivery(team_dir, &member_name, &messages) {
            append_event(
                team_dir,
                "app_server_artifact_plan_delivery_suppressed",
                serde_json::json!({
                    "member": member_name,
                    "messages": messages.len(),
                    "reason": "same sender's artifact-plan context was already delivered; actionable updates must use decision/review/job/wait/blocker markers",
                }),
            )?;
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
            continue;
        }
        record_artifact_plan_delivery(team_dir, &member_name, &messages)?;
        if run.member.role == "lead" && interactive_lead_attached(team_dir)? {
            append_event(
                team_dir,
                "interactive_lead_mailbox_delivery_deferred",
                serde_json::json!({
                    "member": member_name,
                    "messages": messages.len(),
                    "seen": pending.seen,
                    "reason": "interactive lead TUI is attached; mailbox remains inspectable but no hidden runtime prompt is submitted to the visible lead thread",
                }),
            )?;
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
            continue;
        }
        if run.completed {
            if run.member.role == "lead" {
                let config = load_config(team_dir)?;
                let prompt = build_reactive_lead_turn_prompt(
                    &run.member,
                    &messages,
                    codex_exe,
                    &config.id,
                    team_dir,
                    language,
                );
                let started = start_app_server_member_turn(
                    node_clients,
                    node_processes,
                    nodes,
                    team_dir,
                    active,
                    thread_to_member,
                    &member_name,
                    prompt,
                    cwd,
                    model.clone(),
                    sandbox.clone(),
                    approval_policy,
                    dangerously_bypass_approvals_and_sandbox,
                    relay_port,
                    "app_server_lead_reactive_started",
                )
                .await?;
                if started {
                    let category = usage_category_for_messages("lead_reactive", &messages);
                    update_active_turn_usage_category(
                        team_dir,
                        active,
                        &member_name,
                        category,
                        "app_server_lead_reactive_classified",
                    )?;
                    acknowledge_mailbox_delivery(
                        team_dir,
                        mailbox_counts,
                        &member_name,
                        pending.seen,
                        messages.len(),
                    )?;
                }
            } else {
                let config = load_config(team_dir)?;
                let status =
                    member_status(team_dir, &member_name)?.unwrap_or(MemberStatus::Completed);
                let prompt = build_reactive_member_turn_prompt(
                    &run.member,
                    &messages,
                    codex_exe,
                    &config.id,
                    matches!(status, MemberStatus::Standby),
                    language,
                );
                let started = start_app_server_member_turn(
                    node_clients,
                    node_processes,
                    nodes,
                    team_dir,
                    active,
                    thread_to_member,
                    &member_name,
                    prompt,
                    cwd,
                    model.clone(),
                    sandbox.clone(),
                    approval_policy,
                    dangerously_bypass_approvals_and_sandbox,
                    relay_port,
                    "app_server_member_reactive_started",
                )
                .await?;
                if let Some(run) = active.get_mut(&member_name) {
                    run.standby_after_turn = matches!(status, MemberStatus::Standby);
                }
                if started {
                    let category = usage_category_for_messages("member_reactive", &messages);
                    update_active_turn_usage_category(
                        team_dir,
                        active,
                        &member_name,
                        category,
                        "app_server_member_reactive_classified",
                    )?;
                    acknowledge_mailbox_delivery(
                        team_dir,
                        mailbox_counts,
                        &member_name,
                        pending.seen,
                        messages.len(),
                    )?;
                }
            }
            continue;
        }
        let active_external_waits = active_external_wait_ids_for_member(team_dir, &member_name)?;
        if !active_external_waits.is_empty() {
            let mut side_started = false;
            if side_channel_replies {
                let side_messages = messages
                    .iter()
                    .filter(|message| side_channel_message_needs_fast_reply(&member_name, message))
                    .cloned()
                    .collect::<Vec<_>>();
                if !side_messages.is_empty() {
                    side_started = start_app_server_side_channel_reply(
                        node_clients,
                        team_dir,
                        side_replies,
                        &run,
                        side_messages,
                        model.clone(),
                        approval_policy.clone(),
                        dangerously_bypass_approvals_and_sandbox,
                        language,
                        false,
                    )
                    .await?;
                }
            }
            let context_id = record_deferred_active_turn_context(
                team_dir,
                &run,
                &messages,
                &active_external_waits,
                language,
            )?;
            append_event(
                team_dir,
                "app_server_turn_steer_deferred_external_wait",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id,
                    "turn": run.turn_id,
                    "messages": messages.len(),
                    "waits": active_external_waits,
                    "side_channel_reply_started": side_started,
                    "deferred_context": context_id,
                }),
            )?;
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
            continue;
        }
        if active_turn_messages_are_deferrable_system_nudges(&messages) {
            let context_id =
                record_deferred_active_turn_context(team_dir, &run, &messages, &[], language)?;
            append_event(
                team_dir,
                "app_server_turn_steer_deferred_system_nudge",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id,
                    "turn": run.turn_id,
                    "messages": messages.len(),
                    "deferred_context": context_id,
                }),
            )?;
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
            continue;
        }
        if let Some(wait_remaining) = active_turn_recently_steered(
            team_dir,
            &run,
            Duration::from_secs(MIN_ACTIVE_TURN_STEER_INTERVAL_SECS),
        )? {
            let mut side_started = false;
            if side_channel_replies {
                let side_messages = messages
                    .iter()
                    .filter(|message| side_channel_message_needs_fast_reply(&member_name, message))
                    .cloned()
                    .collect::<Vec<_>>();
                if !side_messages.is_empty() {
                    side_started = start_app_server_side_channel_reply(
                        node_clients,
                        team_dir,
                        side_replies,
                        &run,
                        side_messages,
                        model.clone(),
                        approval_policy.clone(),
                        dangerously_bypass_approvals_and_sandbox,
                        language,
                        false,
                    )
                    .await?;
                }
            }
            let context_id =
                record_deferred_active_turn_context(team_dir, &run, &messages, &[], language)?;
            append_event(
                team_dir,
                "app_server_turn_steer_deferred_rate_limit",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id,
                    "turn": run.turn_id,
                    "messages": messages.len(),
                    "min_interval_sec": MIN_ACTIVE_TURN_STEER_INTERVAL_SECS,
                    "retry_after_sec": wait_remaining.as_secs(),
                    "side_channel_reply_started": side_started,
                    "deferred_context": context_id,
                }),
            )?;
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
            continue;
        }
        if let Some(total_tokens) = active_turn_token_pressure(team_dir, &run)? {
            let mut side_started = false;
            if side_channel_replies {
                let side_messages = messages
                    .iter()
                    .filter(|message| side_channel_message_needs_fast_reply(&member_name, message))
                    .cloned()
                    .collect::<Vec<_>>();
                if !side_messages.is_empty() {
                    side_started = start_app_server_side_channel_reply(
                        node_clients,
                        team_dir,
                        side_replies,
                        &run,
                        side_messages,
                        model.clone(),
                        approval_policy.clone(),
                        dangerously_bypass_approvals_and_sandbox,
                        language,
                        false,
                    )
                    .await?;
                }
            }
            let context_id =
                record_deferred_active_turn_context(team_dir, &run, &messages, &[], language)?;
            append_event(
                team_dir,
                "app_server_turn_steer_deferred_token_pressure",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id,
                    "turn": run.turn_id,
                    "messages": messages.len(),
                    "total_tokens": total_tokens,
                    "side_channel_reply_started": side_started,
                    "deferred_context": context_id,
                }),
            )?;
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
            continue;
        }
        let mut delivered = false;
        if side_channel_replies {
            let side_messages = messages
                .iter()
                .filter(|message| side_channel_message_needs_fast_reply(&member_name, message))
                .cloned()
                .collect::<Vec<_>>();
            if !side_messages.is_empty() {
                let side_started = start_app_server_side_channel_reply(
                    node_clients,
                    team_dir,
                    side_replies,
                    &run,
                    side_messages,
                    model.clone(),
                    approval_policy,
                    dangerously_bypass_approvals_and_sandbox,
                    language,
                    true,
                )
                .await?;
                if side_started {
                    let system_messages = messages
                        .iter()
                        .filter(|message| message.from == "system")
                        .cloned()
                        .collect::<Vec<_>>();
                    if system_messages.is_empty() {
                        acknowledge_mailbox_delivery(
                            team_dir,
                            mailbox_counts,
                            &member_name,
                            pending.seen,
                            messages.len(),
                        )?;
                        continue;
                    }
                    let steer_text =
                        build_reactive_steer_prompt(&run.member, &system_messages, language);
                    let (steer_text, side_context_ids) = append_side_channel_context_prompt(
                        team_dir,
                        &member_name,
                        &run.turn_id,
                        steer_text,
                        language,
                    )?;
                    let Some(node_client) = node_clients.get_mut(&run.node_id) else {
                        append_event(
                            team_dir,
                            "app_server_turn_steer_skipped",
                            serde_json::json!({
                                "member": member_name,
                                "node": run.node_id,
                                "thread": run.thread_id.clone(),
                                "turn": run.turn_id.clone(),
                                "messages": system_messages.len(),
                                "error": "node client missing",
                            }),
                        )?;
                        continue;
                    };
                    let steer_result = node_client
                        .client
                        .request_typed::<TurnSteerResponse>(ClientRequest::TurnSteer {
                            request_id: next_request_id(&mut node_client.request_counter),
                            params: TurnSteerParams {
                                thread_id: run.thread_id.clone(),
                                input: vec![text_input(steer_text)],
                                responsesapi_client_metadata: None,
                                expected_turn_id: run.turn_id.clone(),
                            },
                        })
                        .await;
                    let steer_succeeded = steer_result.is_ok();
                    match steer_result {
                        Ok(response) => {
                            let response_turn_id = response.turn_id.clone();
                            append_turn_steer_result(
                                team_dir,
                                &member_name,
                                &run,
                                system_messages.len(),
                                Ok::<TurnSteerResponse, String>(response),
                            )?;
                            let category =
                                usage_category_for_messages(&run.usage_category, &system_messages);
                            update_active_turn_usage_category(
                                team_dir,
                                active,
                                &member_name,
                                category,
                                "app_server_turn_steer_classified",
                            )?;
                            mark_side_channel_contexts_injected(
                                team_dir,
                                &member_name,
                                &side_context_ids,
                                &response_turn_id,
                            )?;
                            if let Some(run) = active.get_mut(&member_name) {
                                merge_side_context_ids(run, &side_context_ids);
                            }
                        }
                        Err(err) => append_turn_steer_result(
                            team_dir,
                            &member_name,
                            &run,
                            system_messages.len(),
                            Err(err),
                        )?,
                    }
                    if steer_succeeded {
                        acknowledge_mailbox_delivery(
                            team_dir,
                            mailbox_counts,
                            &member_name,
                            pending.seen,
                            messages.len(),
                        )?;
                    }
                    continue;
                }
            }
        }
        let steer_text = build_reactive_steer_prompt(&run.member, &messages, language);
        let (steer_text, side_context_ids) = append_side_channel_context_prompt(
            team_dir,
            &member_name,
            &run.turn_id,
            steer_text,
            language,
        )?;
        let Some(node_client) = node_clients.get_mut(&run.node_id) else {
            append_event(
                team_dir,
                "app_server_turn_steer_skipped",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id.clone(),
                    "turn": run.turn_id.clone(),
                    "messages": messages.len(),
                    "error": "node client missing",
                }),
            )?;
            continue;
        };
        let steer_result = node_client
            .client
            .request_typed::<TurnSteerResponse>(ClientRequest::TurnSteer {
                request_id: next_request_id(&mut node_client.request_counter),
                params: TurnSteerParams {
                    thread_id: run.thread_id.clone(),
                    input: vec![text_input(steer_text)],
                    responsesapi_client_metadata: None,
                    expected_turn_id: run.turn_id.clone(),
                },
            })
            .await;
        match steer_result {
            Ok(response) => {
                let response_turn_id = response.turn_id.clone();
                append_turn_steer_result(
                    team_dir,
                    &member_name,
                    &run,
                    messages.len(),
                    Ok::<TurnSteerResponse, String>(response),
                )?;
                let category = usage_category_for_messages(&run.usage_category, &messages);
                update_active_turn_usage_category(
                    team_dir,
                    active,
                    &member_name,
                    category,
                    "app_server_turn_steer_classified",
                )?;
                mark_side_channel_contexts_injected(
                    team_dir,
                    &member_name,
                    &side_context_ids,
                    &response_turn_id,
                )?;
                if let Some(run) = active.get_mut(&member_name) {
                    merge_side_context_ids(run, &side_context_ids);
                }
                delivered = true;
            }
            Err(err) => {
                append_turn_steer_result(team_dir, &member_name, &run, messages.len(), Err(err))?
            }
        }
        if delivered {
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
        }
    }
    Ok(())
}

struct PendingMailboxDelivery {
    seen: usize,
    messages: Vec<MailMessage>,
}

fn collect_new_active_mailbox_messages(
    team_dir: &Path,
    member: &TeamMember,
    active: bool,
    mailbox_counts: &mut HashMap<String, usize>,
) -> Result<Option<PendingMailboxDelivery>> {
    if !active {
        return Ok(None);
    }
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member.name))?;
    let seen = mailbox_counts
        .get(&member.name)
        .copied()
        .unwrap_or_default()
        .min(messages.len());
    let new_messages = messages.into_iter().skip(seen).collect::<Vec<_>>();
    Ok(Some(PendingMailboxDelivery {
        seen,
        messages: new_messages,
    }))
}

fn acknowledge_mailbox_delivery(
    team_dir: &Path,
    mailbox_counts: &mut HashMap<String, usize>,
    member_name: &str,
    seen: usize,
    delivered_count: usize,
) -> Result<()> {
    if delivered_count == 0 {
        return Ok(());
    }
    let delivered_until = seen.saturating_add(delivered_count);
    mark_mailbox_messages_read_range(team_dir, member_name, seen, delivered_until)?;
    mailbox_counts.insert(member_name.to_string(), delivered_until);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn start_app_server_side_channel_reply(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    team_dir: &Path,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    run: &AppServerMemberRun,
    messages: Vec<MailMessage>,
    model: Option<String>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    language: TeamPromptLanguage,
    fork_source_thread: bool,
) -> Result<bool> {
    let recipients = side_channel_reply_recipients(&run.member.name, &messages);
    if recipients.is_empty() {
        return Ok(false);
    }
    if side_replies.values().any(|reply| {
        reply.member.name == run.member.name
            && reply.node_id == run.node_id
            && reply.source_thread_id == run.thread_id
    }) {
        append_event(
            team_dir,
            "app_server_side_channel_reply_skipped",
            serde_json::json!({
                "member": run.member.name,
                "node": run.node_id,
                "thread": run.thread_id,
                "reason": "side_channel_reply_already_running",
                "messages": messages.len(),
                "recipients": recipients,
            }),
        )?;
        return Ok(false);
    }
    let Some(node_client) = node_clients.get_mut(&run.node_id) else {
        append_event(
            team_dir,
            "app_server_side_channel_reply_skipped",
            serde_json::json!({
                "member": run.member.name,
                "node": run.node_id,
                "thread": run.thread_id,
                "reason": "node client missing",
                "messages": messages.len(),
            }),
        )?;
        return Ok(false);
    };
    let side_thread_id = if fork_source_thread {
        let fork: ThreadForkResponse = match fork_team_app_server_thread(
            node_client,
            team_dir,
            &run.node_id,
            &run.member.name,
            "side_channel_reply_fork",
            ThreadForkParams {
                thread_id: run.thread_id.clone(),
                model: model.clone(),
                cwd: Some(run.cwd.display().to_string()),
                approval_policy: approval_policy.clone(),
                sandbox: if dangerously_bypass_approvals_and_sandbox {
                    Some(SandboxMode::DangerFullAccess)
                } else {
                    None
                },
                ephemeral: true,
                exclude_turns: true,
                ..ThreadForkParams::default()
            },
            language,
        )
        .await
        {
            Ok(fork) => fork,
            Err(err) => {
                append_event(
                    team_dir,
                    "app_server_side_channel_reply_skipped",
                    serde_json::json!({
                        "member": run.member.name,
                        "node": run.node_id,
                        "thread": run.thread_id,
                        "reason": "fork failed",
                        "messages": messages.len(),
                        "error": err.to_string(),
                    }),
                )?;
                return Ok(false);
            }
        };
        fork.thread.id.clone()
    } else {
        let thread: ThreadStartResponse = match start_team_app_server_thread(
            node_client,
            team_dir,
            &run.node_id,
            &run.member.name,
            "side_channel_reply_thread",
            ThreadStartParams {
                model: model.clone(),
                cwd: Some(run.cwd.display().to_string()),
                sandbox: if dangerously_bypass_approvals_and_sandbox {
                    Some(SandboxMode::DangerFullAccess)
                } else {
                    None
                },
                approval_policy: approval_policy.clone(),
                ephemeral: Some(true),
                ..ThreadStartParams::default()
            },
            language,
        )
        .await
        {
            Ok(thread) => thread,
            Err(err) => {
                append_event(
                    team_dir,
                    "app_server_side_channel_reply_skipped",
                    serde_json::json!({
                        "member": run.member.name,
                        "node": run.node_id,
                        "thread": run.thread_id,
                        "reason": "thread start failed",
                        "messages": messages.len(),
                        "error": err.to_string(),
                    }),
                )?;
                return Ok(false);
            }
        };
        thread.thread.id.clone()
    };
    let prompt = build_side_channel_reply_prompt(&run.member, &messages, language);
    let turn: TurnStartResponse = match node_client
        .client
        .request_typed(ClientRequest::TurnStart {
            request_id: next_request_id(&mut node_client.request_counter),
            params: TurnStartParams {
                thread_id: side_thread_id.clone(),
                input: vec![text_input(prompt)],
                cwd: Some(run.cwd.clone()),
                model,
                approval_policy,
                sandbox_policy: if dangerously_bypass_approvals_and_sandbox {
                    Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess)
                } else {
                    None
                },
                ..TurnStartParams::default()
            },
        })
        .await
    {
        Ok(turn) => turn,
        Err(err) => {
            append_event(
                team_dir,
                "app_server_side_channel_reply_skipped",
                serde_json::json!({
                    "member": run.member.name,
                    "node": run.node_id,
                    "thread": run.thread_id,
                    "side_thread": side_thread_id,
                    "reason": "turn start failed",
                    "messages": messages.len(),
                    "error": err.to_string(),
                }),
            )?;
            return Ok(false);
        }
    };
    side_replies.insert(
        thread_key(&run.node_id, &side_thread_id),
        AppServerSideReply {
            member: run.member.clone(),
            node_id: run.node_id.clone(),
            source_thread_id: run.thread_id.clone(),
            side_thread_id: side_thread_id.clone(),
            turn_id: turn.turn.id.clone(),
            usage_category: "side_channel_reply".to_string(),
            recipients: recipients.clone(),
            messages: messages.clone(),
            buffer: String::new(),
            started_at: Instant::now(),
        },
    );
    append_event(
        team_dir,
        "app_server_side_channel_reply_started",
        serde_json::json!({
            "member": run.member.name,
            "node": run.node_id,
            "source_thread": run.thread_id,
            "side_thread": side_thread_id,
            "turn": turn.turn.id,
            "recipients": recipients,
            "messages": messages.len(),
            "mode": if fork_source_thread { "fork" } else { "independent" },
        }),
    )?;
    record_turn_usage_index(
        team_dir,
        &run.member,
        &run.node_id,
        &side_thread_id,
        &turn.turn.id,
        "side_channel_reply",
        "app_server_side_channel_reply_started",
    )?;
    Ok(true)
}

fn side_channel_message_needs_fast_reply(member_name: &str, message: &MailMessage) -> bool {
    message.from != "system"
        && message.from != member_name
        && !is_side_channel_generated_message(&message.message)
        && message_requests_fast_reply(&message.from, &message.message)
}

fn is_side_channel_generated_message(message: &str) -> bool {
    let trimmed = message.trim_start();
    trimmed.starts_with("Quick side-channel reply from @")
        || (trimmed.starts_with('@') && trimmed.contains(" からの side-channel 速報返信です"))
        || trimmed.starts_with("Side-channel reply sent while your main turn was busy.")
        || trimmed.starts_with(
            "Side-channel reply: あなたの main turn が busy の間に短い返信を送りました。",
        )
}

fn message_requests_fast_reply(from: &str, message: &str) -> bool {
    if from == "user" {
        return true;
    }
    message_has_explicit_question_or_reply_request(message)
}

fn message_has_explicit_question_or_reply_request(message: &str) -> bool {
    message.lines().any(|line| {
        let upper = line.trim_start().to_ascii_uppercase();
        upper.starts_with("REPLY_REQUEST:")
            || upper.starts_with("QUESTION:")
            || upper.starts_with("DEBATE_REQUEST:")
            || upper.starts_with("REVIEW_REQUEST:")
            || upper.starts_with("BLOCKER:")
            || upper.starts_with("LEAD_PROPOSAL:")
    })
}

fn side_channel_reply_recipients(member_name: &str, messages: &[MailMessage]) -> Vec<String> {
    let mut recipients = messages
        .iter()
        .filter(|message| side_channel_message_needs_fast_reply(member_name, message))
        .map(|message| message.from.clone())
        .collect::<Vec<_>>();
    recipients.sort();
    recipients.dedup();
    recipients
}

fn build_side_channel_reply_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    language: TeamPromptLanguage,
) -> String {
    if language.is_ja() {
        format!(
            "あなたは Codex team における @{name} の fast side-channel responder です。\n\n部署 role: {role}\n\nmain @{name} turn はまだ実行中です。止めないでください。long job を始めないでください。この side channel で広範な実装作業をしないでください。必要なら軽い local state の確認は可能ですが、主目的は部署間の対話を滑らかに保つことです。\n\n以下の incoming team messages に対して、@{name} としてすぐに簡潔に返信してください。返信は requester に直接送られるため、「返信した」というメタ要約ではなく、実質的な答えそのものを書いてください。status を聞かれた場合は current mode、blocker 有無、request/job id、command/log path、next checkpoint、expected artifact filenames、verification gate を具体的に含めてください。質問・相談・レビュー依頼なら、判断、理由、推奨案、必要な次アクションを返してください。main turn の作業変更が必要なら、main turn が取り込むべき commitment/constraint を明記してください。不明なら具体的な clarifying question を 1 つだけ聞くか blocker を述べてください。side-channel は task 完了・artifact handoff ではないため、TEAM_COMPLETION_CHECKLIST を絶対に書かないでください。必要がなければ markdown code fence は使わないでください。自然文は日本語で書いてください。\n\nIncoming messages:\n{}",
            summarize_side_reply_messages(messages, language),
            name = member.name,
            role = member.role,
        )
    } else {
        format!(
            "You are @{name}'s fast side-channel responder for a Codex team.\n\nYour department role: {role}\n\nThe main @{name} turn is still running. Do not stop it, do not start long jobs, and do not perform broad implementation work in this side channel. You may inspect lightweight local state if needed, but the primary purpose is to keep inter-department discussion fluid.\n\nReply immediately and concisely as @{name} to the incoming team messages below. Your reply is sent directly to the requester, so it must be the substantive answer itself, not a meta-summary of what you did. Do not write phrases like \"I replied\", \"handed back\", \"will tell lead\", or \"status was provided\" unless you also include the actual requested facts in the same message. If the incoming message asks for status, include concrete status fields directly: current mode, blocker or none, request/job id or none, command/log path if any, next checkpoint, expected artifact filenames, and any verification gate. If the message is a question, consultation, or review request, provide judgment, reasoning, recommendation, and the needed next action. If the request requires the main turn to change its work, state the exact commitment or constraint that the main turn must incorporate. If you are unsure, ask one concrete clarifying question or state the blocker. A side-channel reply is not task completion or artifact handoff, so never include TEAM_COMPLETION_CHECKLIST. Do not include markdown code fences unless necessary.\n\nIncoming messages:\n{}",
            summarize_side_reply_messages(messages, language),
            name = member.name,
            role = member.role,
        )
    }
}

fn append_turn_steer_result<E: std::fmt::Display>(
    team_dir: &Path,
    member_name: &str,
    run: &AppServerMemberRun,
    message_count: usize,
    result: std::result::Result<TurnSteerResponse, E>,
) -> Result<()> {
    match result {
        Ok(response) => {
            append_event(
                team_dir,
                "app_server_turn_steered",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id.clone(),
                    "turn": response.turn_id,
                    "messages": message_count,
                }),
            )?;
        }
        Err(err) => {
            append_event(
                team_dir,
                "app_server_turn_steer_skipped",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id.clone(),
                    "turn": run.turn_id.clone(),
                    "messages": message_count,
                    "error": err.to_string(),
                }),
            )?;
        }
    }
    Ok(())
}

fn discuss_team(root: &Path, args: DiscussArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    let cwd = args
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;
    if args.dry_run {
        print_discussion_dry_run(&team_dir, args.rounds, &cwd, &codex_exe)?;
        return Ok(());
    }
    run_discussion_rounds(
        &team_dir,
        &config.id,
        &cwd,
        &codex_exe,
        args.rounds,
        args.model.as_deref(),
        args.profile.as_deref(),
        args.sandbox.as_deref(),
        args.dangerously_bypass_approvals_and_sandbox,
    )
}

fn print_discussion_dry_run(
    team_dir: &Path,
    rounds: u32,
    cwd: &Path,
    codex_exe: &Path,
) -> Result<()> {
    if rounds == 0 {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let workers = team_workers(&config);
    for round in 1..=rounds {
        for member in &workers {
            println!("--- discuss round {round}/{rounds}: {} ---", member.name);
            println!("{} exec -C {} <prompt>", codex_exe.display(), cwd.display());
            println!(
                "{}",
                build_discussion_prompt(&config, &tasks, member, round, rounds)
            );
        }
    }
    Ok(())
}

fn run_discussion_rounds(
    team_dir: &Path,
    team_id: &str,
    cwd: &Path,
    codex_exe: &Path,
    rounds: u32,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<()> {
    if rounds == 0 {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    let workers = team_workers(&config);
    if workers.is_empty() {
        bail!("team `{}` has no worker members to discuss", config.id);
    }

    append_event(
        team_dir,
        "discussion_started",
        serde_json::json!({ "rounds": rounds }),
    )?;
    send_system_message_to_members(
        team_dir,
        &config,
        "lead",
        &workers,
        &format!(
            "Discussion starting for team goal: {}. Read your inbox, share assumptions, blockers, handoffs, and review concerns.",
            config.goal
        ),
    )?;

    for round in 1..=rounds {
        let tasks = load_tasks(team_dir)?;
        for member in &workers {
            let log_path = team_dir
                .join("logs")
                .join(format!("discuss-round{round}-{}.log", member.name));
            let last_message_path = team_dir
                .join("last_messages")
                .join(format!("discuss-round{round}-{}.md", member.name));
            let prompt = build_discussion_prompt(&config, &tasks, member, round, rounds);
            append_event(
                team_dir,
                "discussion_member_started",
                serde_json::json!({ "round": round, "member": member.name }),
            )?;
            let status = run_codex_exec(
                codex_exe,
                cwd,
                team_id,
                &member.name,
                &member.role,
                &prompt,
                &log_path,
                &last_message_path,
                model,
                profile,
                sandbox,
                dangerously_bypass_approvals_and_sandbox,
            )?;
            append_event(
                team_dir,
                if status.success() {
                    "discussion_member_completed"
                } else {
                    "discussion_member_failed"
                },
                serde_json::json!({ "round": round, "member": member.name, "status": status.code() }),
            )?;
            if !status.success() {
                bail!(
                    "discussion round {round} failed for member `{}`",
                    member.name
                );
            }
        }
    }
    append_event(
        team_dir,
        "discussion_completed",
        serde_json::json!({ "rounds": rounds }),
    )?;
    Ok(())
}

fn run_lead_synthesis(
    team_dir: &Path,
    team_id: &str,
    cwd: &Path,
    codex_exe: &Path,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<()> {
    set_member_status(team_dir, "lead", MemberStatus::Running)?;
    let log_path = team_dir.join("logs").join("lead.log");
    let summary_path = team_dir.join("summary.md");
    let stdout =
        fs::File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let prompt = build_lead_synthesis_prompt(team_dir)?;

    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("-o")
        .arg(&summary_path)
        .env("CODEX_TEAM_ID", team_id)
        .env("CODEX_TEAM_MEMBER", "lead")
        .env("CODEX_TEAM_ROLE", "lead")
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

    append_event(
        team_dir,
        "lead_synthesis_started",
        serde_json::json!({ "log": log_path, "summary": summary_path }),
    )?;
    let status = command.spawn()?.wait()?;
    if status.success() {
        set_member_status(team_dir, "lead", MemberStatus::Completed)?;
        append_event(
            team_dir,
            "lead_synthesis_completed",
            serde_json::json!({ "status": status.code(), "summary": summary_path }),
        )?;
        println!("Summary: {}", summary_path.display());
        Ok(())
    } else {
        set_member_status(team_dir, "lead", MemberStatus::Failed)?;
        append_event(
            team_dir,
            "lead_synthesis_failed",
            serde_json::json!({ "status": status.code() }),
        )?;
        bail!("lead synthesis failed");
    }
}
