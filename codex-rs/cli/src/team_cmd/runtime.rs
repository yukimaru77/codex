fn run_team(root: &Path, mut args: RunArgs) -> Result<()> {
    if args.app_server_url.is_some() {
        bail!("--app-server-url is only supported with --app-server");
    }
    apply_natural_language_defaults(&mut args.start);
    let (team_id, team_dir) = create_team(root, args.start)?;
    println!("Created team `{team_id}`");
    println!("State: {}", team_dir.display());
    write_team_run_pid(&team_dir, std::process::id())?;

    assign_unowned_tasks_round_robin(&team_dir)?;
    let config = load_config(&team_dir)?;
    let tasks = load_tasks(&team_dir)?;
    let workers: Vec<TeamMember> = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .cloned()
        .collect();
    if workers.is_empty() {
        bail!("team `{team_id}` has no worker members; add --member NAME[:ROLE]");
    }

    let cwd = args
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    bind_parent_codex_session_to_team(root, &team_id, &team_dir, &cwd)?;
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;

    if args.prepare_only {
        if args.worktree {
            for member in &workers {
                let assigned = tasks
                    .iter()
                    .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
                if assigned {
                    let _ = prepare_member_worktree(&team_dir, &cwd, &team_id, member)?;
                }
            }
        }
        print_status(&team_dir)?;
        return Ok(());
    }

    if args.dry_run {
        print_discussion_dry_run(&team_dir, args.discuss_rounds, &cwd, &codex_exe)?;
        for member in &workers {
            let prompt = build_worker_prompt(&config, &tasks, member);
            println!("--- {} ({}) ---", member.name, member.role);
            println!("{} exec -C {} <prompt>", codex_exe.display(), cwd.display());
            println!("{prompt}");
        }
        return Ok(());
    }

    run_discussion_rounds(
        &team_dir,
        &team_id,
        &cwd,
        &codex_exe,
        args.discuss_rounds,
        args.model.as_deref(),
        args.profile.as_deref(),
        args.sandbox.as_deref(),
        args.dangerously_bypass_approvals_and_sandbox,
    )?;

    let mut children = Vec::new();
    for member in &workers {
        let assigned = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
        if !assigned {
            continue;
        }

        set_member_status(&team_dir, &member.name, MemberStatus::Running)?;
        mark_member_tasks(&team_dir, &member.name, TaskStatus::InProgress)?;

        let worker_cwd = if args.worktree {
            prepare_member_worktree(&team_dir, &cwd, &team_id, member)?
        } else {
            cwd.clone()
        };

        let log_path = team_dir.join("logs").join(format!("{}.log", member.name));
        let last_message_path = team_dir
            .join("last_messages")
            .join(format!("{}.md", member.name));
        let stdout = fs::File::create(&log_path)
            .with_context(|| format!("create {}", log_path.display()))?;
        let stderr = stdout.try_clone()?;
        let prompt = build_worker_prompt(&config, &tasks, member);

        let mut command = Command::new(&codex_exe);
        command
            .arg("exec")
            .arg("-C")
            .arg(&worker_cwd)
            .arg("-o")
            .arg(&last_message_path)
            .env("CODEX_TEAM_ID", &team_id)
            .env("CODEX_TEAM_MEMBER", &member.name)
            .env("CODEX_TEAM_ROLE", &member.role)
            .env("CODEX_TEAM_CLI", &codex_exe)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        if let Some(model) = args.model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(profile) = args.profile.as_deref() {
            command.arg("--profile").arg(profile);
        }
        if let Some(sandbox) = args.sandbox.as_deref() {
            command.arg("--sandbox").arg(sandbox);
        }
        if args.dangerously_bypass_approvals_and_sandbox {
            command.arg("--dangerously-bypass-approvals-and-sandbox");
        }
        command.arg(prompt);

        append_event(
            &team_dir,
            "member_started",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "cwd": worker_cwd,
                "log": log_path,
                "lastMessage": last_message_path,
            }),
        )?;

        let child = command
            .spawn()
            .with_context(|| format!("spawn Codex worker `{}`", member.name))?;
        children.push((member.name.clone(), child));
    }

    if children.is_empty() {
        bail!("no workers had assigned tasks");
    }

    let mut failed = false;
    for (member_name, mut child) in children {
        let status = child
            .wait()
            .with_context(|| format!("wait for Codex worker `{member_name}`"))?;
        if status.success() {
            set_member_status(&team_dir, &member_name, MemberStatus::Completed)?;
            complete_member_tasks_if_active(&team_dir, &member_name)?;
            append_event(
                &team_dir,
                "member_completed",
                serde_json::json!({ "member": member_name, "status": status.code() }),
            )?;
        } else {
            failed = true;
            set_member_status(&team_dir, &member_name, MemberStatus::Failed)?;
            append_event(
                &team_dir,
                "member_failed",
                serde_json::json!({ "member": member_name, "status": status.code() }),
            )?;
        }
    }

    print_status(&team_dir)?;
    if failed {
        bail!("one or more team members failed");
    }
    if !args.no_synthesis {
        run_lead_synthesis(
            &team_dir,
            &team_id,
            &cwd,
            &codex_exe,
            args.model.as_deref(),
            args.profile.as_deref(),
            args.sandbox.as_deref(),
            args.dangerously_bypass_approvals_and_sandbox,
        )?;
    }
    Ok(())
}

async fn run_team_app_server(root: &Path, mut args: RunArgs) -> Result<()> {
    let resume_team = args.resume_team.clone();
    let use_lead_department_design = resume_team.is_none()
        && !args.interactive_lead
        && should_use_lead_department_design(&args.start);
    let explicit_cwd = args.cwd.is_some();
    let mut cwd = args
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;
    let lead_department_design = if use_lead_department_design
        && !args.dry_run
        && !args.prepare_only
    {
        let design = run_lead_department_design(
            &codex_exe,
            &cwd,
            &args.start.goal,
            &lead_placement_candidates_from_start(&args.start)?,
            args.start.language.unwrap_or_default(),
            args.model.as_deref(),
            args.profile.as_deref(),
            args.sandbox.as_deref(),
            args.dangerously_bypass_approvals_and_sandbox,
        )
        .with_context(|| "lead failed to design departments")?;
        apply_department_design(&mut args.start, design.clone());
        Some(design)
    } else {
        if use_lead_department_design && args.dry_run {
            println!("Dry run: lead would design departments from the goal before team creation.");
        }
        if !args.interactive_lead {
            apply_natural_language_defaults(&mut args.start);
        }
        None
    };

    let requested_language = args.start.language;
    let (team_id, team_dir) = if let Some(team) = resume_team.as_deref() {
        let team_dir = resolve_team_dir(root, Some(team))?;
        let config = load_config(&team_dir)?;
        if !explicit_cwd {
            cwd = resume_runtime_base_cwd(&config, &cwd);
        }
        println!("Reattached app-server runtime to team `{}`", config.id);
        println!("State: {}", team_dir.display());
        append_event(
            &team_dir,
            "app_server_runtime_reattached",
            serde_json::json!({
                "pid": std::process::id(),
            }),
        )?;
        (config.id, team_dir)
    } else {
        let (team_id, team_dir) = create_team(root, args.start)?;
        println!("Created app-server team `{team_id}`");
        println!("State: {}", team_dir.display());
        (team_id, team_dir)
    };
    let prompt_language = ensure_team_prompt_language(&team_dir, requested_language)?;
    write_team_run_pid(&team_dir, std::process::id())?;
    bind_parent_codex_session_to_team(root, &team_id, &team_dir, &cwd)?;
    if let Some(design) = lead_department_design.as_ref() {
        merge_lead_node_metadata(&team_dir, &design.nodes)?;
        append_event(
            &team_dir,
            "lead_department_design",
            serde_json::json!({ "nodes": &design.nodes, "departments": &design.departments }),
        )?;
    }

    assign_unowned_tasks_round_robin(&team_dir)?;
    ensure_container_node_departments(&team_dir)?;
    let mut config = load_config(&team_dir)?;
    let tasks = load_tasks(&team_dir)?;
    let workers = team_workers(&config);
    if workers.is_empty() && !args.interactive_lead {
        bail!("team `{team_id}` has no worker members; add --member NAME[:ROLE]");
    }
    if args.prepare_only {
        if args.worktree {
            for member in &workers {
                let assigned = tasks
                    .iter()
                    .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
                if assigned {
                    let _ = prepare_member_worktree(&team_dir, &cwd, &team_id, member)?;
                }
            }
        }
        print_status(&team_dir)?;
        return Ok(());
    }

    if args.dry_run {
        println!("App-server mode dry run.");
        println!(
            "{} app-server --listen ws://127.0.0.1:<port>",
            codex_exe.display()
        );
        print_discussion_dry_run(&team_dir, args.discuss_rounds, &cwd, &codex_exe)?;
        if let Some(lead_member) = config.members.iter().find(|member| member.role == "lead") {
            let prompt = build_app_server_lead_prompt(
                &config,
                &tasks,
                lead_member,
                &codex_exe,
                prompt_language,
            );
            println!(
                "--- app-server lead thread: {} ({}) ---",
                lead_member.name, lead_member.role
            );
            println!("{prompt}");
        }
        for member in &workers {
            let mut dry_nodes = load_nodes(&team_dir)?;
            ensure_local_node(&mut dry_nodes);
            let prompt = build_app_server_worker_prompt(
                &config,
                &tasks,
                member,
                &codex_exe,
                &dry_nodes,
                prompt_language,
            );
            println!("--- app-server turn: {} ({}) ---", member.name, member.role);
            println!("{prompt}");
        }
        return Ok(());
    }

    if args.discuss_rounds > 0 {
        run_discussion_rounds(
            &team_dir,
            &team_id,
            &cwd,
            &codex_exe,
            args.discuss_rounds,
            args.model.as_deref(),
            args.profile.as_deref(),
            args.sandbox.as_deref(),
            args.dangerously_bypass_approvals_and_sandbox,
        )?;
    }

    let relay = TeamRelayServer::spawn(team_dir.clone())?;
    append_event(
        &team_dir,
        "team_relay_started",
        serde_json::json!({
            "url": relay.local_url(),
        }),
    )?;

    let registered_app_server_url = if args.app_server_url.is_none() && !args.no_app_server_registry
    {
        read_registered_app_server_url()?
    } else {
        None
    };
    let requested_app_server_url = args
        .app_server_url
        .clone()
        .or_else(|| registered_app_server_url.clone());
    let using_registered_app_server =
        args.app_server_url.is_none() && registered_app_server_url.is_some();

    let mut app_server = None;
    let mut node_clients = HashMap::<String, TeamAppServerNodeClient>::new();
    let mut node_processes = Vec::<NodeAppServerProcess>::new();
    let app_server_url;
    let app_server_external;
    let app_server_source;
    if let Some(url) = requested_app_server_url {
        let connect_attempts = if using_registered_app_server { 2 } else { 50 };
        match connect_team_app_server_with_attempts(&url, connect_attempts).await {
            Ok(connected_client) => {
                app_server_url = url;
                app_server_external = true;
                app_server_source = if using_registered_app_server {
                    "registry"
                } else {
                    "explicit"
                };
                node_clients.insert(
                    "local".to_string(),
                    TeamAppServerNodeClient {
                        client: connected_client,
                        request_counter: 1,
                    },
                );
            }
            Err(err) if using_registered_app_server => {
                append_event(
                    &team_dir,
                    "app_server_registry_unavailable",
                    serde_json::json!({
                        "url": url,
                        "error": err.to_string(),
                    }),
                )?;
                let _ = clear_app_server_registry_if_matches(&url);
                eprintln!(
                    "Registered app-server `{url}` is unavailable; starting a private app-server."
                );
                let spawned =
                    BackgroundTeamAppServer::spawn(&codex_exe, &team_dir, args.profile.as_deref())?;
                app_server_url = spawned.url.clone();
                app_server = Some(spawned);
                app_server_external = false;
                app_server_source = "spawned";
                let connected_client = connect_team_app_server(&app_server_url).await?;
                node_clients.insert(
                    "local".to_string(),
                    TeamAppServerNodeClient {
                        client: connected_client,
                        request_counter: 1,
                    },
                );
            }
            Err(err) => return Err(err),
        }
    } else {
        let spawned =
            BackgroundTeamAppServer::spawn(&codex_exe, &team_dir, args.profile.as_deref())?;
        app_server_url = spawned.url.clone();
        app_server = Some(spawned);
        app_server_external = false;
        app_server_source = "spawned";
        let connected_client = connect_team_app_server(&app_server_url).await?;
        node_clients.insert(
            "local".to_string(),
            TeamAppServerNodeClient {
                client: connected_client,
                request_counter: 1,
            },
        );
    }
    append_event(
        &team_dir,
        "app_server_connected",
        serde_json::json!({
            "url": app_server_url,
            "external": app_server_external,
            "source": app_server_source,
        }),
    )?;
    set_node_connection(
        &team_dir,
        "local",
        TeamNodeStatus::Online,
        Some(app_server_url.clone()),
    )?;
    let mut nodes = load_nodes(&team_dir)?;
    ensure_local_node(&mut nodes);
    let mut needed_node_ids = vec!["local".to_string()];
    for member in &workers {
        let assigned = tasks.iter().any(|task| {
            task.owner.as_deref() == Some(member.name.as_str())
                && task_status_can_start_turn(task.status)
        });
        if assigned {
            let node_id = member_node_id(member);
            if !needed_node_ids.contains(&node_id) {
                needed_node_ids.push(node_id);
            }
        }
    }
    for node_id in needed_node_ids {
        if node_id == "local" || node_clients.contains_key(&node_id) {
            continue;
        }
        let node = nodes
            .iter()
            .find(|node| node.id == node_id)
            .cloned()
            .with_context(|| format!("node `{node_id}` is not registered"))?;
        let (url, child) = resolve_or_spawn_node_app_server(&team_dir, &node, relay.port())?;
        if let Some(child) = child {
            node_processes.push(child);
        }
        let connected_client = connect_team_app_server(&url)
            .await
            .with_context(|| format!("connect app-server node `{node_id}` at `{url}`"))?;
        append_event(
            &team_dir,
            "app_server_node_connected",
            serde_json::json!({
                "node": node_id,
                "kind": node.kind,
                "url": url,
                "source": "node",
            }),
        )?;
        set_node_connection(
            &team_dir,
            &node_id,
            TeamNodeStatus::Online,
            Some(url.clone()),
        )?;
        node_clients.insert(
            node_id,
            TeamAppServerNodeClient {
                client: connected_client,
                request_counter: 1,
            },
        );
    }
    let mut active = HashMap::<String, AppServerMemberRun>::new();
    let mut thread_to_member = HashMap::<String, String>::new();
    let mut side_replies = HashMap::<String, AppServerSideReply>::new();
    let mut assistant_buffers = HashMap::<String, String>::new();

    let sandbox = app_server_sandbox(
        args.sandbox.as_deref(),
        args.dangerously_bypass_approvals_and_sandbox,
    )?;
    let approval_policy = if args.dangerously_bypass_approvals_and_sandbox {
        Some(AskForApproval::Never)
    } else {
        None
    };

    let lead_member = config
        .members
        .iter()
        .find(|member| member.role == "lead")
        .cloned()
        .context("team has no lead member")?;
    let lead_node_id = "local".to_string();
    let lead_client = node_clients
        .get_mut(&lead_node_id)
        .context("local app-server client missing for lead")?;
    let lead_thread: ThreadStartResponse = start_team_app_server_thread(
        lead_client,
        &team_dir,
        &lead_node_id,
        &lead_member.name,
        "lead_initial_thread",
        ThreadStartParams {
            model: args.model.clone(),
            cwd: Some(cwd.display().to_string()),
            sandbox,
            approval_policy,
            ephemeral: Some(false),
            ..ThreadStartParams::default()
        },
        prompt_language,
    )
    .await?;
    set_member_thread(&team_dir, &lead_member.name, &lead_thread.thread.id)?;
    set_member_workspace(&team_dir, &lead_member.name, &cwd)?;
    thread_to_member.insert(
        thread_key(&lead_node_id, &lead_thread.thread.id),
        lead_member.name.clone(),
    );
    assistant_buffers.insert(lead_member.name.clone(), String::new());
    active.insert(
        lead_member.name.clone(),
        AppServerMemberRun {
            member: lead_member.clone(),
            node_id: lead_node_id.clone(),
            cwd: cwd.clone(),
            thread_id: lead_thread.thread.id.clone(),
            turn_id: String::new(),
            completed: true,
            failed: false,
            standby_after_turn: false,
            usage_category: "lead_thread".to_string(),
            team_message_scan_offset: 0,
            last_activity_at: Instant::now(),
            last_activity_kind: "thread_started".to_string(),
            last_stale_notice_at: None,
            retry_not_before: recent_usage_limit_retry_not_before(&team_dir, &lead_member.name)?,
            side_context_ids: Vec::new(),
        },
    );
    println!("Started lead thread={}", lead_thread.thread.id);
    append_event(
        &team_dir,
        "app_server_lead_thread_started",
        serde_json::json!({
            "member": lead_member.name,
            "thread": lead_thread.thread.id,
            "cwd": cwd,
        }),
    )?;

    let mut started_workers = 0usize;
    for member in &workers {
        let assigned = tasks.iter().any(|task| {
            task.owner.as_deref() == Some(member.name.as_str())
                && task_status_can_start_turn(task.status)
        });
        if !assigned {
            continue;
        }
        if let Some(remaining) = recent_usage_limit_retry_remaining(&team_dir, &member.name)? {
            append_event(
                &team_dir,
                "app_server_member_start_deferred",
                serde_json::json!({
                    "member": member.name,
                    "node": member_node_id(member),
                    "reason": "recent app-server/model usage-limit cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            set_member_status(&team_dir, &member.name, MemberStatus::Standby)?;
            continue;
        }

        set_member_status(&team_dir, &member.name, MemberStatus::Running)?;
        mark_member_tasks(&team_dir, &member.name, TaskStatus::InProgress)?;

        let node_id = member_node_id(member);
        if node_id != "local" && args.worktree {
            bail!(
                "--worktree is not supported for remote node member `{}` yet",
                member.name
            );
        }
        let worker_cwd = if node_id != "local" {
            app_server_member_cwd(&node_id, &nodes, &cwd)
        } else if args.worktree {
            prepare_member_worktree(&team_dir, &cwd, &team_id, member)?
        } else if resume_team.is_some() && !explicit_cwd {
            member
                .workspace_path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| cwd.clone())
        } else {
            cwd.clone()
        };

        let node_client = node_clients
            .get_mut(&node_id)
            .with_context(|| format!("app-server client missing for node `{node_id}`"))?;
        let thread: ThreadStartResponse = match start_team_app_server_thread(
            node_client,
            &team_dir,
            &node_id,
            &member.name,
            "department_initial_thread",
            ThreadStartParams {
                model: args.model.clone(),
                cwd: Some(worker_cwd.display().to_string()),
                sandbox,
                approval_policy,
                ephemeral: Some(false),
                ..ThreadStartParams::default()
            },
            prompt_language,
        )
        .await
        {
            Ok(thread) => thread,
            Err(err) => {
                append_event(
                    &team_dir,
                    "app_server_member_start_failed",
                    serde_json::json!({
                        "member": member.name,
                        "role": member.role,
                        "node": node_id,
                        "reason": "thread start failed",
                        "error": err.to_string(),
                    }),
                )?;
                if node_id != "local" {
                    let _ = set_node_connection(&team_dir, &node_id, TeamNodeStatus::Failed, None);
                }
                block_member_tasks_if_active(
                    &team_dir,
                    &member.name,
                    &format!("Member initial app-server thread could not start: {err}"),
                )?;
                set_member_status(&team_dir, &member.name, MemberStatus::Standby)?;
                continue;
            }
        };
        set_member_thread(&team_dir, &member.name, &thread.thread.id)?;
        set_member_workspace(&team_dir, &member.name, &worker_cwd)?;

        let prompt = build_app_server_worker_prompt(
            &config,
            &tasks,
            member,
            &codex_exe,
            &nodes,
            prompt_language,
        );
        let turn: TurnStartResponse = match node_client
            .client
            .request_typed(ClientRequest::TurnStart {
                request_id: next_request_id(&mut node_client.request_counter),
                params: TurnStartParams {
                    thread_id: thread.thread.id.clone(),
                    input: vec![text_input(prompt)],
                    cwd: Some(worker_cwd.clone()),
                    model: args.model.clone(),
                    approval_policy,
                    sandbox_policy: if args.dangerously_bypass_approvals_and_sandbox {
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
                let err = anyhow!(err);
                append_event(
                    &team_dir,
                    "app_server_member_start_failed",
                    serde_json::json!({
                        "member": member.name,
                        "role": member.role,
                        "node": node_id,
                        "thread": thread.thread.id,
                        "reason": "turn start failed",
                        "error": err.to_string(),
                    }),
                )?;
                if node_id != "local" {
                    let _ = set_node_connection(&team_dir, &node_id, TeamNodeStatus::Failed, None);
                }
                block_member_tasks_if_active(
                    &team_dir,
                    &member.name,
                    &format!("Member initial app-server turn could not start: {err}"),
                )?;
                set_member_status(&team_dir, &member.name, MemberStatus::Standby)?;
                continue;
            }
        };

        println!(
            "Started {} ({}) thread={} turn={}",
            member.name, member.role, thread.thread.id, turn.turn.id
        );
        append_event(
            &team_dir,
            "app_server_member_started",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "thread": thread.thread.id,
                "turn": turn.turn.id,
                "node": node_id,
                "cwd": worker_cwd,
            }),
        )?;

        thread_to_member.insert(thread_key(&node_id, &thread.thread.id), member.name.clone());
        assistant_buffers.insert(member.name.clone(), String::new());
        active.insert(
            member.name.clone(),
            AppServerMemberRun {
                member: member.clone(),
                node_id: node_id.clone(),
                cwd: worker_cwd,
                thread_id: thread.thread.id.clone(),
                turn_id: turn.turn.id.clone(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "department_start".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );
        record_turn_usage_index(
            &team_dir,
            member,
            &node_id,
            &thread.thread.id,
            &turn.turn.id,
            "department_start",
            "app_server_member_started",
        )?;
        started_workers += 1;
    }

    if started_workers == 0 {
        if args.interactive_lead {
            append_event(
                &team_dir,
                "app_server_interactive_lead_only",
                serde_json::json!({
                    "message": "lead-only interactive team runtime; departments will be added after user instruction"
                }),
            )?;
        } else if args.resume_team.is_some() {
            append_event(
                &team_dir,
                "app_server_runtime_no_startable_workers",
                serde_json::json!({
                    "message": "reattached runtime has no startable worker tasks; keep-alive will wait for messages, dependency changes, or dynamic members"
                }),
            )?;
        } else {
            bail!("no workers had assigned tasks");
        }
    }

    let lead_prompt =
        build_app_server_lead_prompt(&config, &tasks, &lead_member, &codex_exe, prompt_language);
    start_app_server_member_turn(
        &mut node_clients,
        &mut node_processes,
        &nodes,
        &team_dir,
        &mut active,
        &mut thread_to_member,
        &lead_member.name,
        lead_prompt,
        &cwd,
        args.model.clone(),
        sandbox.clone(),
        approval_policy,
        args.dangerously_bypass_approvals_and_sandbox,
        relay.port(),
        "app_server_lead_started",
    )
    .await?;
    normalize_stale_running_members_without_active_turns(&team_dir, &active)?;
    config = load_config(&team_dir)?;

    let mut mailbox_counts = current_mailbox_counts(&team_dir, &config.members, &tasks)?;
    let poll_interval = Duration::from_millis(args.reactive_poll_ms.max(250));
    let node_sync_interval = if args.node_sync_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.node_sync_interval_sec.max(30)))
    };
    let idle_outreach_interval = if args.idle_outreach_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.idle_outreach_interval_sec.max(60)))
    };
    let task_watchdog_interval = if args.task_watchdog_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.task_watchdog_interval_sec.max(30)))
    };
    let lead_tick_interval = if args.lead_tick_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.lead_tick_interval_sec.max(60)))
    };
    let idle_wakeup_interval = if args.idle_wakeup_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.idle_wakeup_interval_sec.max(60)))
    };
    let department_heartbeat_interval = if args.department_heartbeat_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(
            args.department_heartbeat_interval_sec.max(60),
        ))
    };
    let stale_active_turn_timeout = if args.stale_active_turn_timeout_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(
            args.stale_active_turn_timeout_sec.max(120),
        ))
    };
    let team_wait_idle_active_quiet_threshold = if args.team_wait_idle_active_quiet_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(
            args.team_wait_idle_active_quiet_sec.max(30),
        ))
    };
    let autoresearch_audit_interval = if args.autoresearch_audit_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(
            args.autoresearch_audit_interval_sec.max(60),
        ))
    };
    let mut last_node_asset_sync = HashMap::<String, Instant>::new();
    let mut last_member_journal_node_sync = HashMap::<String, Instant>::new();
    let mut last_idle_outreach = Instant::now();
    let mut idle_outreach_cursor = 0_usize;
    let mut last_task_watchdog = Instant::now();
    let mut task_watchdog_warned = HashSet::<String>::new();
    let mut last_lead_tick = lead_tick_interval
        .map(|interval| Instant::now() - interval)
        .unwrap_or_else(Instant::now);
    let mut member_idle_since = HashMap::<String, Instant>::new();
    let mut member_last_idle_wakeup = HashMap::<String, Instant>::new();
    let mut last_idle_wakeup_batch = Instant::now();
    if let Some(wakeup_interval) = idle_wakeup_interval {
        seed_department_idle_wakeup_cooldowns(
            &team_dir,
            &mut member_last_idle_wakeup,
            &mut last_idle_wakeup_batch,
            wakeup_interval,
        )?;
    }
    let mut idle_wakeup_cursor = 0_usize;
    let mut department_heartbeats = HashMap::<String, Instant>::new();
    let mut last_stale_active_turn_check = Instant::now();
    let mut last_autoresearch_audit = autoresearch_audit_interval
        .map(|interval| Instant::now() - interval)
        .unwrap_or_else(Instant::now);
    let mut last_member_journal_update = Instant::now() - Duration::from_secs(60);
    let mut last_job_refresh = Instant::now() - Duration::from_secs(15);
    let mut last_node_connection_heartbeat = Instant::now() - Duration::from_secs(60);
    let mut contract_input_sync_attempts = HashSet::<String>::new();
    let mut keep_alive_idle_reported = false;
    let mut keep_alive_idle_since = None::<Instant>;
    let mut idle_exit_requested = false;
    let mut team_wait_idle_key = team_wait_idle_event_active(&team_dir)
        .then(|| "reattached-previous-wait-idle".to_string());
    #[cfg(unix)]
    let hangup_task = {
        let mut hangup_signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .context("install team runtime SIGHUP handler")?;
        let team_dir = team_dir.clone();
        tokio::spawn(async move {
            while hangup_signal.recv().await.is_some() {
                let _ = append_event(
                    &team_dir,
                    "app_server_keep_alive_hangup_ignored",
                    serde_json::json!({ "reason": "sighup" }),
                );
            }
        })
    };

    loop {
        let has_running_turn = active.values().any(|run| !run.completed);
        let has_unstarted_member = has_unstarted_app_server_members(&team_dir, &active)?;
        let team_is_idle = !has_running_turn && !has_unstarted_member;
        if team_is_idle {
            if !args.no_keep_alive {
                let idle_since = *keep_alive_idle_since.get_or_insert_with(Instant::now);
                if !keep_alive_idle_reported {
                    println!(
                        "Team {} is idle and staying alive. Send messages or member changes; press Ctrl-C to stop.",
                        team_id
                    );
                    append_event(
                        &team_dir,
                        "app_server_keep_alive_idle",
                        serde_json::json!({ "message": "team idle; waiting for messages or dynamic member changes" }),
                    )?;
                    keep_alive_idle_reported = true;
                }
                if args.idle_exit_after_sec > 0
                    && idle_since.elapsed() >= Duration::from_secs(args.idle_exit_after_sec)
                {
                    append_event(
                        &team_dir,
                        "app_server_keep_alive_idle_timeout",
                        serde_json::json!({
                            "idle_for_sec": idle_since.elapsed().as_secs(),
                            "idle_exit_after_sec": args.idle_exit_after_sec,
                            "message": "idle keep-alive runtime paused automatically to avoid token/process waste",
                        }),
                    )?;
                    idle_exit_requested = true;
                    break;
                }
            } else {
                break;
            }
        } else {
            keep_alive_idle_reported = false;
            keep_alive_idle_since = None;
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c(), if !args.no_keep_alive => {
                append_event(
                    &team_dir,
                    "app_server_keep_alive_stopped",
                    serde_json::json!({ "reason": "ctrl_c" }),
                )?;
                break;
            }
            _ = tokio::time::sleep(poll_interval) => {
                drain_app_server_events(
                    &mut node_clients,
                    &team_dir,
                    &mut active,
                    &mut side_replies,
                    &thread_to_member,
                    &mut assistant_buffers,
                ).await?;
                if last_job_refresh.elapsed() >= Duration::from_secs(15) {
                    last_job_refresh = Instant::now();
                    if let Err(err) = refresh_running_team_jobs(&team_dir) {
                        record_runtime_loop_error(&team_dir, "refresh_running_team_jobs", err)?;
                    }
                }
                if let Err(err) = auto_complete_wait_checks(&team_dir) {
                    record_runtime_loop_error(&team_dir, "auto_complete_wait_checks", err)?;
                }
                if let Err(err) = auto_promote_dependency_waits(&team_dir) {
                    record_runtime_loop_error(&team_dir, "auto_promote_dependency_waits", err)?;
                }
                let team_wait_idle = detect_team_wait_idle_state(
                    &team_dir,
                    &active,
                    team_wait_idle_active_quiet_threshold,
                )?;
                if let Some(state) = team_wait_idle {
                    let key = state.key();
                    if team_wait_idle_key.as_deref() != Some(key.as_str()) {
                        println!(
                            "Team {} is waiting on long-running work; suppressing automatic team prompts until the wait/job/active turn completes.",
                            team_id
                        );
                        append_event(
                            &team_dir,
                            "team_wait_idle_entered",
                            serde_json::json!({
                                "message": "automatic team prompts suppressed while all open work is waiting on long-running work",
                                "waits": state.wait_ids,
                                "jobs": state.job_ids,
                                "tasks": state.task_ids,
                                "active_members": state.active_members,
                                "suppressed": [
                                    "lead_tick",
                                    "task_watchdog",
                                    "idle_wakeup",
                                    "department_heartbeat",
                                    "idle_outreach",
                                    "member_digest_journal",
                                    "autoresearch_audit",
                                    "node_asset_sync",
                                    "dynamic_member_sync",
                                    "container_department_discovery",
                                    "contract_input_sync",
                                    "stale_active_turn_warning"
                                ],
                            }),
                        )?;
                        team_wait_idle_key = Some(key);
                    }
                    let user_message_pending = suppress_wait_idle_mailbox_chatter(
                        &team_dir,
                        &config.members,
                        &mut mailbox_counts,
                    )?;
                    if user_message_pending {
                        steer_new_team_messages(
                            &mut node_clients,
                            &mut node_processes,
                            &nodes,
                            &team_dir,
                            &config.members,
                            &mut active,
                            &mut side_replies,
                            &mut thread_to_member,
                            &mut mailbox_counts,
                            &cwd,
                            args.model.clone(),
                            sandbox.clone(),
                            approval_policy.clone(),
                            args.dangerously_bypass_approvals_and_sandbox,
                            &codex_exe,
                            args.side_channel_replies,
                            relay.port(),
                            prompt_language,
                        )
                        .await?;
                    }
                    continue;
                } else if let Some(previous) = team_wait_idle_key.take() {
                    append_event(
                        &team_dir,
                        "team_wait_idle_exited",
                        serde_json::json!({
                            "previous_key": previous,
                            "message": "long-running wait idle condition cleared; automatic team prompts resumed",
                        }),
                    )?;
                }
                nodes = load_nodes(&team_dir)?;
                ensure_local_node(&mut nodes);
                ensure_container_node_departments(&team_dir)?;
                nodes = load_nodes(&team_dir)?;
                ensure_local_node(&mut nodes);
                if last_node_connection_heartbeat.elapsed() >= Duration::from_secs(60) {
                    last_node_connection_heartbeat = Instant::now();
                    if let Err(err) =
                        heartbeat_connected_app_server_nodes(&team_dir, &node_clients)
                    {
                        record_runtime_loop_error(&team_dir, "node_connection_heartbeat", err)?;
                    }
                    nodes = load_nodes(&team_dir)?;
                    ensure_local_node(&mut nodes);
                }
                if let Some(sync_interval) = node_sync_interval {
                    if let Err(err) = maybe_sync_remote_node_assets(
                        &team_dir,
                        &nodes,
                        &node_clients,
                        &mut last_node_asset_sync,
                        sync_interval,
                    ) {
                        record_runtime_loop_error(&team_dir, "node_asset_sync", err)?;
                    }
                }
                sync_removed_app_server_nodes(
                    &mut node_clients,
                    &mut node_processes,
                    &nodes,
                    &team_dir,
                    &active,
                ).await?;
                if last_member_journal_update.elapsed() >= Duration::from_secs(60) {
                    last_member_journal_update = Instant::now();
                    if let Err(err) = update_member_journals(&team_dir, &config) {
                        record_runtime_loop_error(&team_dir, "member_journal_update", err)?;
                    }
                    if let Err(err) = maybe_generate_member_digest_journals(
                        &team_dir,
                        &codex_exe,
                        args.model.as_deref(),
                        args.profile.as_deref(),
                        args.sandbox.as_deref(),
                        args.dangerously_bypass_approvals_and_sandbox,
                    ) {
                        record_runtime_loop_error(&team_dir, "member_digest_journal", err)?;
                    }
                    if let Err(err) = maybe_sync_member_journals_to_nodes(
                        &team_dir,
                        &nodes,
                        &node_clients,
                        &mut last_member_journal_node_sync,
                        Duration::from_secs(60),
                    ) {
                        record_runtime_loop_error(&team_dir, "member_journal_node_sync", err)?;
                    }
                }
                if let Err(err) = assign_unowned_tasks_round_robin(&team_dir) {
                    record_runtime_loop_error(&team_dir, "assign_unowned_tasks_round_robin", err)?;
                }
                if let Err(err) = maybe_sync_contract_declared_inputs(
                    &team_dir,
                    &config,
                    &nodes,
                    &mut contract_input_sync_attempts,
                ) {
                    record_runtime_loop_error(&team_dir, "contract_declared_input_sync", err)?;
                }
                sync_dynamic_app_server_members(
                    &mut node_clients,
                    &nodes,
                    &team_dir,
                    &mut config,
                    &mut active,
                    &mut thread_to_member,
                    &mut assistant_buffers,
                    &mut mailbox_counts,
                    &mut node_processes,
                    &cwd,
                    args.model.clone(),
                    sandbox.clone(),
                    approval_policy.clone(),
                    args.dangerously_bypass_approvals_and_sandbox,
                    &codex_exe,
                    relay.port(),
                    prompt_language,
                ).await?;
                steer_new_team_messages(
                    &mut node_clients,
                    &mut node_processes,
                    &nodes,
                    &team_dir,
                    &config.members,
                    &mut active,
                    &mut side_replies,
                    &mut thread_to_member,
                    &mut mailbox_counts,
                    &cwd,
                    args.model.clone(),
                    sandbox.clone(),
                    approval_policy.clone(),
                    args.dangerously_bypass_approvals_and_sandbox,
                    &codex_exe,
                    args.side_channel_replies,
                    relay.port(),
                    prompt_language,
                ).await?;
                if let Some(outreach_interval) = idle_outreach_interval {
                    if let Err(err) = maybe_send_idle_department_outreach(
                        &team_dir,
                        &config,
                        &active,
                        &mut last_idle_outreach,
                        &mut idle_outreach_cursor,
                        outreach_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "idle_department_outreach", err)?;
                    }
                }
                if let Some(watchdog_interval) = task_watchdog_interval {
                    if let Err(err) = maybe_warn_unattended_tasks(
                        &team_dir,
                        &config,
                        &active,
                        &mut last_task_watchdog,
                        &mut task_watchdog_warned,
                        watchdog_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "task_watchdog", err)?;
                    }
                }
                config = load_config(&team_dir)?;
                if let Some(tick_interval) = lead_tick_interval {
                    if let Err(err) = maybe_send_lead_autonomy_tick(
                        &team_dir,
                        &config,
                        &active,
                        &mut last_lead_tick,
                        tick_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "lead_autonomy_tick", err)?;
                    }
                }
                if let Some(wakeup_interval) = idle_wakeup_interval {
                    if let Err(err) = maybe_send_department_idle_wakeups(
                        &team_dir,
                        &config,
                        &active,
                        &mut member_idle_since,
                        &mut member_last_idle_wakeup,
                        &mut last_idle_wakeup_batch,
                        &mut idle_wakeup_cursor,
                        wakeup_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "department_idle_wakeup", err)?;
                    }
                }
                if let Some(heartbeat_interval) = department_heartbeat_interval {
                    if let Err(err) = maybe_send_department_heartbeats(
                        &team_dir,
                        &config,
                        &active,
                        &mut department_heartbeats,
                        &member_last_idle_wakeup,
                        heartbeat_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "department_heartbeat", err)?;
                    }
                }
                if let Some(stale_timeout) = stale_active_turn_timeout {
                    if let Err(err) = maybe_warn_stale_active_turns(
                        &team_dir,
                        &config,
                        &mut active,
                        &mut last_stale_active_turn_check,
                        Duration::from_secs(30),
                        stale_timeout,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "stale_active_turn_check", err)?;
                    }
                }
                if let Some(audit_interval) = autoresearch_audit_interval {
                    if let Err(err) = maybe_write_autoresearch_runtime_audit(
                        &team_dir,
                        &mut last_autoresearch_audit,
                        audit_interval,
                    ) {
                        record_runtime_loop_error(&team_dir, "autoresearch_runtime_audit", err)?;
                    }
                }
            }
        }
    }

    if !args.no_synthesis
        && !idle_exit_requested
        && let Some(lead_run) = active.get(&lead_member.name)
        && lead_run.completed
    {
        let prompt = build_app_server_lead_final_prompt(&config, &team_dir, prompt_language)?;
        start_app_server_member_turn(
            &mut node_clients,
            &mut node_processes,
            &nodes,
            &team_dir,
            &mut active,
            &mut thread_to_member,
            &lead_member.name,
            prompt,
            &cwd,
            args.model.clone(),
            sandbox.clone(),
            approval_policy,
            args.dangerously_bypass_approvals_and_sandbox,
            relay.port(),
            "app_server_lead_synthesis_started",
        )
        .await?;
        while active
            .get(&lead_member.name)
            .map(|run| !run.completed)
            .unwrap_or(false)
        {
            drain_app_server_events(
                &mut node_clients,
                &team_dir,
                &mut active,
                &mut side_replies,
                &thread_to_member,
                &mut assistant_buffers,
            )
            .await?;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    let failed = active
        .values()
        .any(|run| run.member.role != "lead" && run.failed);
    for run in active.values() {
        let last_message_path = team_dir
            .join("last_messages")
            .join(format!("{}.md", run.member.name));
        let text = assistant_buffers
            .get(&run.member.name)
            .cloned()
            .unwrap_or_default();
        write_text_atomic(&last_message_path, &text)?;
    }
    if let Some(summary) = assistant_buffers.get(&lead_member.name)
        && !summary.trim().is_empty()
    {
        write_text_atomic(&team_dir.join("summary.md"), summary)?;
    }
    if let Err(err) = update_member_journals(&team_dir, &config) {
        record_runtime_loop_error(&team_dir, "member_journal_final_update", err)?;
    }

    print_status(&team_dir)?;
    for (_node_id, node_client) in node_clients {
        node_client
            .client
            .shutdown()
            .await
            .context("shutdown app-server client")?;
    }
    for process in node_processes {
        process.stop();
    }
    #[cfg(unix)]
    hangup_task.abort();
    drop(app_server);

    if failed {
        bail!("one or more app-server team members failed");
    }
    Ok(())
}
