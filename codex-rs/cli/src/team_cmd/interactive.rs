fn runtime_args_to_run_args(args: RuntimeArgs, root: &Path) -> Result<RunArgs> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    if !args.no_replace_existing
        && let Some(pid) = read_team_run_pid(&team_dir)
        && pid != std::process::id()
    {
        stop_process(pid);
    }
    Ok(RunArgs {
        start: StartArgs {
            goal: format!("Reattach runtime for {}", config.id),
            id: None,
            members: Vec::new(),
            nodes: Vec::new(),
            tasks: Vec::new(),
            language: args.language.or(config.language),
        },
        model: args.model,
        profile: args.profile,
        sandbox: args.sandbox,
        cwd: args.cwd,
        worktree: false,
        dangerously_bypass_approvals_and_sandbox: args.dangerously_bypass_approvals_and_sandbox,
        dry_run: false,
        prepare_only: false,
        no_synthesis: true,
        discuss_rounds: 0,
        app_server: true,
        reactive_poll_ms: args.reactive_poll_ms,
        node_sync_interval_sec: args.node_sync_interval_sec,
        idle_outreach_interval_sec: args.idle_outreach_interval_sec,
        task_watchdog_interval_sec: args.task_watchdog_interval_sec,
        lead_tick_interval_sec: args.lead_tick_interval_sec,
        idle_wakeup_interval_sec: args.idle_wakeup_interval_sec,
        department_heartbeat_interval_sec: args.department_heartbeat_interval_sec,
        stale_active_turn_timeout_sec: args.stale_active_turn_timeout_sec,
        team_wait_idle_active_quiet_sec: args.team_wait_idle_active_quiet_sec,
        autoresearch_audit_interval_sec: args.autoresearch_audit_interval_sec,
        side_channel_replies: args.side_channel_replies,
        interactive_lead: args.interactive_lead,
        no_keep_alive: args.no_keep_alive,
        idle_exit_after_sec: args.idle_exit_after_sec,
        app_server_url: args.app_server_url,
        no_app_server_registry: args.no_app_server_registry,
        resume_team: Some(config.id),
    })
}

fn resume_runtime_base_cwd(config: &TeamConfig, fallback: &Path) -> PathBuf {
    config
        .members
        .iter()
        .find(|member| member.role == "lead")
        .and_then(|member| member.workspace_path.as_deref())
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.to_path_buf())
}

pub(crate) struct TeamInteractiveLeadLaunch {
    pub(crate) team_id: String,
    pub(crate) team_dir: PathBuf,
    pub(crate) app_server_url: String,
    pub(crate) lead_thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InteractiveLeadAttachment {
    pid: u32,
    thread: String,
    app_server_url: String,
    cwd: String,
    attached_at: String,
}

fn interactive_lead_attachment_path(team_dir: &Path) -> PathBuf {
    team_dir.join("interactive-lead-attached.json")
}

fn write_interactive_lead_attachment(
    team_dir: &Path,
    thread: &str,
    app_server_url: &str,
    cwd: &Path,
) -> Result<()> {
    let attachment = InteractiveLeadAttachment {
        pid: std::process::id(),
        thread: thread.to_string(),
        app_server_url: app_server_url.to_string(),
        cwd: cwd.display().to_string(),
        attached_at: now(),
    };
    let path = interactive_lead_attachment_path(team_dir);
    write_json_atomic(&path, &attachment).with_context(|| format!("write {}", path.display()))?;
    append_event(
        team_dir,
        "interactive_lead_attached",
        serde_json::json!({
            "pid": attachment.pid,
            "thread": attachment.thread,
            "app_server_url": attachment.app_server_url,
            "cwd": attachment.cwd,
        }),
    )?;
    Ok(())
}

pub(crate) fn detach_interactive_lead_team(team_dir: &Path) -> Result<()> {
    let path = interactive_lead_attachment_path(team_dir);
    match fs::remove_file(&path) {
        Ok(()) => {
            append_event(
                team_dir,
                "interactive_lead_detached",
                serde_json::json!({ "reason": "tui_exit" }),
            )?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn interactive_lead_attached(team_dir: &Path) -> Result<bool> {
    let path = interactive_lead_attachment_path(team_dir);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let attachment = match serde_json::from_str::<InteractiveLeadAttachment>(&raw) {
        Ok(attachment) => attachment,
        Err(_) => {
            let _ = fs::remove_file(&path);
            append_event(
                team_dir,
                "interactive_lead_attachment_cleared",
                serde_json::json!({ "reason": "invalid_marker" }),
            )?;
            return Ok(false);
        }
    };
    if process_alive(attachment.pid) {
        return Ok(true);
    }
    let _ = fs::remove_file(&path);
    append_event(
        team_dir,
        "interactive_lead_attachment_cleared",
        serde_json::json!({
            "reason": "stale_pid",
            "pid": attachment.pid,
            "thread": attachment.thread,
        }),
    )?;
    Ok(false)
}

pub(crate) fn launch_interactive_lead_team(
    shared: &SharedCliOptions,
    team: Option<&str>,
    language: Option<TeamPromptLanguage>,
    idle_exit_after_sec: u64,
) -> Result<TeamInteractiveLeadLaunch> {
    let codex_home =
        codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
    let root = codex_home.join("teams");
    fs::create_dir_all(&root)?;
    let _app_server_child = ensure_team_ui_app_server(&root)?;
    let app_server_url = read_registered_app_server_url()?
        .filter(|url| app_server_readyz(url))
        .context("shared app-server did not become ready for interactive team lead")?;
    if let Some(team) = team {
        return launch_existing_interactive_lead_team(
            &root,
            shared,
            team,
            &app_server_url,
            language,
            idle_exit_after_sec,
        );
    }
    let team_id = format!("team-{}", tokyo_now().format("%Y%m%d%H%M%S"));
    let team_dir = root.join(&team_id);
    let cwd = shared
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let goal = "Interactive Codex team lead session. Wait for the user's first substantive team request. You are the live lead; create departments, tasks, SSH/Docker nodes, and coordination flow only after the user asks for work that needs them.";

    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("team")
        .arg("swarm")
        .arg("--id")
        .arg(&team_id)
        .arg("--app-server")
        .arg("--app-server-url")
        .arg(&app_server_url)
        .arg("--discuss-rounds")
        .arg("0")
        .arg("--interactive-lead")
        .arg("--cd")
        .arg(&cwd);
    if idle_exit_after_sec > 0 {
        command
            .arg("--idle-exit-after-sec")
            .arg(idle_exit_after_sec.to_string());
    }
    if let Some(language) = language {
        command.arg("--language").arg(language.cli_value());
    }
    if shared.dangerously_bypass_approvals_and_sandbox {
        command.arg("--dangerously-bypass-approvals-and-sandbox");
    }
    if let Some(model) = shared.model.as_deref() {
        command.arg("--model").arg(model);
    }
    if let Some(profile) = shared.config_profile.as_deref() {
        command.arg("--profile").arg(profile);
    }
    if let Some(sandbox) = shared.sandbox_mode {
        command
            .arg("--sandbox")
            .arg(sandbox_mode_cli_arg_name(sandbox));
    }
    command.arg(goal).stdin(Stdio::null());

    let log_path = root.join("interactive-lead-runs.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let stderr = log.try_clone()?;
    let mut child = command
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("spawn interactive team lead runtime")?;

    let started_at = Instant::now();
    loop {
        if let Some(lead_thread_id) = read_team_lead_thread_id(&team_dir)? {
            write_interactive_lead_attachment(&team_dir, &lead_thread_id, &app_server_url, &cwd)?;
            append_event(
                &team_dir,
                "interactive_lead_tui_attached",
                serde_json::json!({
                    "thread": lead_thread_id,
                    "app_server_url": app_server_url,
                    "cwd": cwd,
                }),
            )?;
            return Ok(TeamInteractiveLeadLaunch {
                team_id,
                team_dir: team_dir.to_path_buf(),
                app_server_url,
                lead_thread_id,
            });
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "interactive team lead runtime exited before lead thread was ready: {status}. See {}",
                log_path.display()
            );
        }
        if started_at.elapsed() > Duration::from_secs(30) {
            bail!(
                "timed out waiting for interactive team lead thread. See {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn launch_existing_interactive_lead_team(
    root: &Path,
    shared: &SharedCliOptions,
    team: &str,
    app_server_url: &str,
    language: Option<TeamPromptLanguage>,
    idle_exit_after_sec: u64,
) -> Result<TeamInteractiveLeadLaunch> {
    let team_dir = resolve_team_dir(root, Some(team))?;
    let config = load_config(&team_dir)?;
    let cwd = shared
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let old_lead_thread_id = read_team_lead_thread_id(&team_dir)?;
    let runtime_refresh_required =
        interactive_runtime_refresh_required(shared, language, idle_exit_after_sec);
    let runtime_process_alive = read_team_run_pid(&team_dir)
        .map(|pid| process_alive(pid) && process_looks_like_codex_team(pid))
        .unwrap_or(false);
    let runtime_alive = runtime_process_alive;

    let mut child = if runtime_alive {
        if runtime_refresh_required {
            append_event(
                &team_dir,
                "interactive_lead_runtime_refresh_deferred",
                serde_json::json!({
                    "reason": "existing live runtime is preserved for direct lead attach",
                    "dangerously_bypass_approvals_and_sandbox": shared.dangerously_bypass_approvals_and_sandbox,
                    "sandbox": shared
                        .sandbox_mode
                        .map(sandbox_mode_cli_arg_name),
                    "model": shared.model.as_deref(),
                    "profile": shared.config_profile.as_deref(),
                    "language": language.map(|language| language.cli_value()),
                    "idle_exit_after_sec": idle_exit_after_sec,
                }),
            )?;
        }
        None
    } else {
        if runtime_refresh_required {
            append_event(
                &team_dir,
                "interactive_lead_runtime_refresh_requested",
                serde_json::json!({
                    "dangerously_bypass_approvals_and_sandbox": shared.dangerously_bypass_approvals_and_sandbox,
                    "sandbox": shared
                        .sandbox_mode
                        .map(sandbox_mode_cli_arg_name),
                    "model": shared.model.as_deref(),
                    "profile": shared.config_profile.as_deref(),
                    "language": language.map(|language| language.cli_value()),
                    "idle_exit_after_sec": idle_exit_after_sec,
                }),
            )?;
        }
        let _ = fs::remove_file(interactive_lead_attachment_path(&team_dir));
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("team")
            .arg("resume")
            .arg("--team")
            .arg(&config.id)
            .arg("--cd")
            .arg(&cwd);
        if !runtime_refresh_required {
            command.arg("--app-server-url").arg(app_server_url);
        }
        command.arg("--interactive-lead");
        if idle_exit_after_sec > 0 {
            command
                .arg("--idle-exit-after-sec")
                .arg(idle_exit_after_sec.to_string());
        }
        if shared.dangerously_bypass_approvals_and_sandbox {
            command.arg("--dangerously-bypass-approvals-and-sandbox");
        }
        if let Some(model) = shared.model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(profile) = shared.config_profile.as_deref() {
            command.arg("--profile").arg(profile);
        }
        if let Some(sandbox) = shared.sandbox_mode {
            command
                .arg("--sandbox")
                .arg(sandbox_mode_cli_arg_name(sandbox));
        }
        if let Some(language) = language {
            command.arg("--language").arg(language.cli_value());
        }
        command.stdin(Stdio::null());

        let log_path = root.join("interactive-lead-runs.log");
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("open {}", log_path.display()))?;
        let stderr = log.try_clone()?;
        let child = command
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .context("spawn existing team lead runtime")?;
        Some((child, log_path))
    };

    let started_at = Instant::now();
    loop {
        if let Some(lead_thread_id) = read_team_lead_thread_id(&team_dir)?
            && (runtime_alive
                || old_lead_thread_id.as_deref() != Some(lead_thread_id.as_str())
                || interactive_lead_runtime_ready(&team_dir, &lead_thread_id)?)
        {
            let active_app_server_url =
                read_local_node_app_server_url(&team_dir)?.unwrap_or_else(|| app_server_url.to_string());
            write_interactive_lead_attachment(&team_dir, &lead_thread_id, &active_app_server_url, &cwd)?;
            append_event(
                &team_dir,
                "interactive_lead_tui_attached",
                serde_json::json!({
                    "thread": lead_thread_id,
                    "app_server_url": active_app_server_url,
                    "cwd": cwd,
                    "resumed_existing_team": !runtime_alive,
                }),
            )?;
            return Ok(TeamInteractiveLeadLaunch {
                team_id: config.id,
                team_dir,
                app_server_url: active_app_server_url,
                lead_thread_id,
            });
        }
        if let Some((child, log_path)) = child.as_mut()
            && let Some(status) = child.try_wait()?
        {
            bail!(
                "existing team lead runtime exited before lead thread was ready: {status}. See {}",
                log_path.display()
            );
        }
        if started_at.elapsed() > Duration::from_secs(30) {
            if runtime_alive {
                bail!("team `{}` has no lead thread to attach", config.id);
            }
            let log_path = root.join("interactive-lead-runs.log");
            bail!(
                "timed out waiting for team `{}` lead thread after resume. See {}",
                config.id,
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn interactive_lead_runtime_ready(team_dir: &Path, thread_id: &str) -> Result<bool> {
    let path = interactive_lead_attachment_path(team_dir);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let attachment = match serde_json::from_str::<InteractiveLeadAttachment>(&raw) {
        Ok(attachment) => attachment,
        Err(_) => return Ok(false),
    };
    if attachment.thread != thread_id {
        return Ok(false);
    }
    Ok(process_alive(attachment.pid) && process_looks_like_codex_team(attachment.pid))
}

fn read_local_node_app_server_url(team_dir: &Path) -> Result<Option<String>> {
    Ok(load_nodes(team_dir)?
        .into_iter()
        .find(|node| node.id == "local")
        .and_then(|node| node.url)
        .filter(|url| !url.trim().is_empty()))
}

fn interactive_runtime_refresh_required(
    shared: &SharedCliOptions,
    language: Option<TeamPromptLanguage>,
    idle_exit_after_sec: u64,
) -> bool {
    shared.dangerously_bypass_approvals_and_sandbox
        || shared.sandbox_mode.is_some()
        || shared.model.is_some()
        || shared.config_profile.is_some()
        || shared.cwd.is_some()
        || language.is_some()
        || idle_exit_after_sec > 0
}

fn read_team_lead_thread_id(team_dir: &Path) -> Result<Option<String>> {
    if !team_dir.join("config.json").exists() {
        return Ok(None);
    }
    let config = load_config(team_dir)?;
    Ok(config
        .members
        .iter()
        .find(|member| member.role == "lead")
        .and_then(|member| member.thread_id.clone())
        .filter(|thread| !thread.trim().is_empty()))
}

fn sandbox_mode_cli_arg_name(mode: codex_utils_cli::SandboxModeCliArg) -> &'static str {
    match mode {
        codex_utils_cli::SandboxModeCliArg::ReadOnly => "read-only",
        codex_utils_cli::SandboxModeCliArg::WorkspaceWrite => "workspace-write",
        codex_utils_cli::SandboxModeCliArg::DangerFullAccess => "danger-full-access",
    }
}

fn app_server_registry_path() -> Result<PathBuf> {
    let codex_home =
        codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
    Ok(codex_home.join("app-server.json").to_path_buf())
}

#[cfg(test)]
mod interactive_tests {
    use super::*;
    use codex_utils_cli::SandboxModeCliArg;

    #[test]
    fn interactive_attach_refreshes_runtime_for_yolo() {
        let shared = SharedCliOptions {
            dangerously_bypass_approvals_and_sandbox: true,
            ..SharedCliOptions::default()
        };

        assert!(interactive_runtime_refresh_required(&shared, None, 0));
    }

    #[test]
    fn interactive_attach_refreshes_runtime_for_sandbox_override() {
        let shared = SharedCliOptions {
            sandbox_mode: Some(SandboxModeCliArg::DangerFullAccess),
            ..SharedCliOptions::default()
        };

        assert!(interactive_runtime_refresh_required(&shared, None, 0));
    }

    #[test]
    fn interactive_attach_reuses_runtime_without_runtime_overrides() {
        let shared = SharedCliOptions::default();

        assert!(!interactive_runtime_refresh_required(&shared, None, 0));
    }
}

pub(crate) fn register_app_server_transport(
    transport: &codex_app_server::AppServerTransport,
) -> Result<Option<String>> {
    let codex_app_server::AppServerTransport::WebSocket { bind_address } = transport else {
        return Ok(None);
    };
    if bind_address.port() == 0 {
        return Ok(None);
    }
    let url = format!("ws://{bind_address}");
    let path = app_server_registry_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let registry = AppServerRegistry {
        url: url.clone(),
        pid: std::process::id(),
        updated_at: now(),
    };
    let json = serde_json::to_string_pretty(&registry)?;
    fs::write(&path, format!("{json}\n")).with_context(|| format!("write {}", path.display()))?;
    Ok(Some(url))
}

pub(crate) fn clear_app_server_registry_if_matches(url: &str) -> Result<()> {
    let path = app_server_registry_path()?;
    let Ok(raw) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(registry) = serde_json::from_str::<AppServerRegistry>(&raw) else {
        return Ok(());
    };
    if registry.url == url {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn read_registered_app_server_url() -> Result<Option<String>> {
    Ok(read_app_server_registry()?.map(|registry| registry.url))
}

fn read_app_server_registry() -> Result<Option<AppServerRegistry>> {
    let path = app_server_registry_path()?;
    let Ok(raw) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    let registry: AppServerRegistry = match serde_json::from_str(&raw) {
        Ok(registry) => registry,
        Err(_) => {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
    };
    let url = registry.url.trim();
    if url.is_empty() {
        return Ok(None);
    }
    Ok(Some(registry))
}

fn remove_app_server_registry() -> Result<()> {
    let path = app_server_registry_path()?;
    let _ = fs::remove_file(path);
    Ok(())
}
