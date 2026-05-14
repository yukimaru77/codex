fn run_autoresearch_audit(root: &Path, args: AutoresearchAuditArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let report = build_autoresearch_audit_report(&team_dir)?;
    print!("{report}");
    if args.write || args.output.is_some() {
        let output = args.output.unwrap_or_else(|| {
            team_dir.join(format!(
                "autoresearch_audit_{}.md",
                sanitize_id(&now()).replace('-', "_")
            ))
        });
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&output, &report)
            .with_context(|| format!("failed to write {}", output.display()))?;
        println!("\nWrote audit report: {}", output.display());
    }
    Ok(())
}

fn maybe_write_autoresearch_runtime_audit(
    team_dir: &Path,
    last_audit: &mut Instant,
    interval: Duration,
) -> Result<()> {
    if last_audit.elapsed() < interval {
        return Ok(());
    }
    *last_audit = Instant::now();

    let config = load_config(team_dir)?;
    if !team_goal_requests_autoresearch_loop(&config.goal) {
        return Ok(());
    }

    let report = build_autoresearch_audit_report(team_dir)?;
    let output_dir = team_dir.join("autoresearch_audits");
    fs::create_dir_all(&output_dir)?;
    let output = output_dir.join(format!(
        "runtime_audit_{}.md",
        sanitize_id(&now()).replace('-', "_")
    ));
    write_text_atomic(&output, &report)?;
    let pass_count = report.matches("| PASS |").count();
    let warn_count = report.matches("| WARN |").count();
    let fail_count = report.matches("| FAIL |").count();
    append_event(
        team_dir,
        "autoresearch_runtime_audit_written",
        serde_json::json!({
            "path": output,
            "pass": pass_count,
            "warn": warn_count,
            "fail": fail_count,
            "note": "external god-view audit only; no team mailbox steer was sent",
        }),
    )?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutoresearchAuditStatus {
    Pass,
    Warn,
    Fail,
}

impl AutoresearchAuditStatus {
    fn as_str(self) -> &'static str {
        match self {
            AutoresearchAuditStatus::Pass => "PASS",
            AutoresearchAuditStatus::Warn => "WARN",
            AutoresearchAuditStatus::Fail => "FAIL",
        }
    }
}

struct AutoresearchAuditItem {
    id: &'static str,
    requirement: &'static str,
    status: AutoresearchAuditStatus,
    evidence: String,
    gap: String,
}

fn build_autoresearch_audit_report(team_dir: &Path) -> Result<String> {
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let waits = load_waits(team_dir)?;
    let jobs = load_jobs(team_dir)?;
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let ownerships = load_ownerships(team_dir)?;
    let roots = autoresearch_candidate_roots(team_dir, &config, &ownerships);
    let files = collect_audit_files(&roots, 5, 1200)?;
    let mut items = Vec::new();

    let goal_lower = config.goal.to_ascii_lowercase();
    let theme_present = config.goal.chars().count() > 120
        && (goal_lower.contains("digital twin")
            || config.goal.contains("デジタルツイン")
            || config.goal.contains("実験装置"));
    items.push(AutoresearchAuditItem {
        id: "theme",
        requirement: "研究テーマが提示され、質問だけで停止せず phase0 に進める状態である",
        status: if theme_present {
            AutoresearchAuditStatus::Pass
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!("goal_chars={}", config.goal.chars().count()),
        gap: if theme_present {
            "-".to_string()
        } else {
            "goal から研究テーマを十分に確認できません".to_string()
        },
    });

    let phase0_contract = Path::new("/home/yukimaru/research_prompt/phase0.md");
    let phase1_contract = Path::new("/home/yukimaru/research_prompt/phase1.md");
    items.push(AutoresearchAuditItem {
        id: "contracts",
        requirement: "phase0.md と phase1.md を外部契約として参照できる",
        status: if phase0_contract.exists() && phase1_contract.exists() {
            AutoresearchAuditStatus::Pass
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "phase0_exists={} phase1_exists={}",
            phase0_contract.exists(),
            phase1_contract.exists()
        ),
        gap: if phase0_contract.exists() && phase1_contract.exists() {
            "-".to_string()
        } else {
            "research_prompt の契約ファイルが不足しています".to_string()
        },
    });

    let scan_dirs = files
        .iter()
        .filter_map(|path| path.parent())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("scan"))
        })
        .collect::<std::collections::BTreeSet<_>>();
    let phase0_manifest = find_audit_file(&files, &["phase0", "manifest"]);
    let phase0_checklist = find_audit_file(&files, &["phase0", "team_completion_checklist"]);
    let structurally_complete_scan_dirs = scan_dirs
        .iter()
        .filter(|dir| {
            [
                "prompt.md",
                "result.md",
                "source_evidence.md",
                "confidence.md",
                "integration_target.md",
                "command_or_mcp_record.md",
                "manifest.sha256",
            ]
            .iter()
            .all(|name| dir.join(name).exists())
        })
        .count();
    let strong_scan_dirs = scan_dirs
        .iter()
        .filter(|dir| autoresearch_scan_dir_has_strong_content(dir))
        .count();
    let scan_dirs_with_wait_or_command_records = scan_dirs
        .iter()
        .filter(|dir| autoresearch_scan_dir_has_wait_or_command_record(dir))
        .count();
    items.push(AutoresearchAuditItem {
        id: "phase0_scans",
        requirement:
            "Fixed-4 + Flexible-2 の 6 scan が実行され、prompt/result/source/confidence/integration/wait-or-command/manifest を持つ",
        status: if strong_scan_dirs >= 6 && phase0_manifest.is_some() && phase0_checklist.is_some()
        {
            AutoresearchAuditStatus::Pass
        } else if structurally_complete_scan_dirs > 0 || strong_scan_dirs > 0 {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "strong_scan_dirs={} structural_scan_dirs={} phase0_manifest={} phase0_checklist={}",
            strong_scan_dirs,
            structurally_complete_scan_dirs,
            phase0_manifest
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
            phase0_checklist
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        gap: if strong_scan_dirs >= 6 && phase0_manifest.is_some() && phase0_checklist.is_some()
        {
            "-".to_string()
        } else {
            "6本すべての scan package、phase0 manifest/checklist、または source/confidence/MCP記録などの中身が不足しています".to_string()
        },
    });

    let phase1_manifest = find_audit_file(&files, &["phase1", "manifest"]);
    let phase1_text_hit = files.iter().any(|path| {
        path.to_string_lossy()
            .to_ascii_lowercase()
            .contains("phase1")
            && file_contains_any(
                path,
                &[
                    "killer experiment",
                    "bounded experiment",
                    "environment_handoff",
                    "最初に潰すべき",
                    "最初の killer",
                ],
            )
    });
    items.push(AutoresearchAuditItem {
        id: "phase1_synthesis",
        requirement:
            "phase0 の結果を統合し、phase1 synthesis で bounded/killer experiment と環境 handoff を決める",
        status: if phase1_manifest.is_some() && phase1_text_hit {
            AutoresearchAuditStatus::Pass
        } else if phase1_manifest.is_some() || phase1_text_hit {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "phase1_manifest={} synthesis_terms_found={}",
            phase1_manifest
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
            phase1_text_hit
        ),
        gap: if phase1_manifest.is_some() && phase1_text_hit {
            "-".to_string()
        } else {
            "phase1 の manifest または killer/bounded experiment/environment handoff 証拠が弱いです"
                .to_string()
        },
    });

    let deep_waits = waits
        .iter()
        .filter(|wait| {
            let text =
                format!("{} {} {}", wait.title, wait.condition, wait.progress).to_ascii_lowercase();
            text.contains("deep_thinker") || text.contains("deep_research")
        })
        .collect::<Vec<_>>();
    let completed_deep_waits = deep_waits
        .iter()
        .filter(|wait| matches!(wait.status, TeamWaitStatus::Completed))
        .count();
    let open_deep_waits = deep_waits
        .iter()
        .filter(|wait| wait.status.is_open())
        .count();
    let deep_wait_gate_passes = open_deep_waits == 0
        && (completed_deep_waits >= 6
            || (completed_deep_waits > 0
                && strong_scan_dirs >= 6
                && scan_dirs_with_wait_or_command_records >= 6));
    items.push(AutoresearchAuditItem {
        id: "mcp_waits",
        requirement:
            "deep_thinker/deep_researcher など長時間外部思考を wait として登録し、結果を待つ",
        status: if deep_wait_gate_passes {
            AutoresearchAuditStatus::Pass
        } else if completed_deep_waits > 0 {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "deep_waits={} completed={} open={} scan_wait_or_command_records={}",
            deep_waits.len(),
            completed_deep_waits,
            open_deep_waits,
            scan_dirs_with_wait_or_command_records
        ),
        gap: if deep_wait_gate_passes {
            "-".to_string()
        } else if completed_deep_waits > 0 {
            "一部の deep_thinker/deep_researcher wait は完了していますが、6 scan の wait/command 証跡または deep wait coverage が不足しています".to_string()
        } else {
            "完了済み deep_thinker/deep_researcher wait が見つかりません".to_string()
        },
    });

    let saitou_node = nodes
        .iter()
        .any(|node| node.id == "saitou" || node.host.as_deref() == Some("saitou"));
    let docker_jobs = jobs
        .iter()
        .filter(|job| {
            job.node == "saitou"
                && format!("{} {}", job.id, job.command)
                    .to_ascii_lowercase()
                    .contains("docker")
        })
        .collect::<Vec<_>>();
    let completed_docker_jobs = docker_jobs
        .iter()
        .filter(|job| matches!(job.status, TeamJobStatus::Completed))
        .count();
    items.push(AutoresearchAuditItem {
        id: "saitou_docker_build",
        requirement:
            "ssh saitou 上で Dockerfile/image/container 作成を行い、Docker build を team job として追跡する",
        status: if saitou_node && completed_docker_jobs > 0 {
            AutoresearchAuditStatus::Pass
        } else if saitou_node || !docker_jobs.is_empty() {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "saitou_node={} docker_jobs={} completed={}",
            saitou_node,
            docker_jobs.len(),
            completed_docker_jobs
        ),
        gap: if saitou_node && completed_docker_jobs > 0 {
            "-".to_string()
        } else {
            "saitou node または完了済み Docker job が不足しています".to_string()
        },
    });

    let container_nodes = nodes
        .iter()
        .filter(|node| matches!(node.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker))
        .collect::<Vec<_>>();
    let container_members = config
        .members
        .iter()
        .filter(|member| {
            member
                .node
                .as_deref()
                .is_some_and(|node_id| container_nodes.iter().any(|node| node.id == node_id))
        })
        .collect::<Vec<_>>();
    items.push(AutoresearchAuditItem {
        id: "container_node_department",
        requirement:
            "container 作成後に long-lived Docker/ssh-docker node を登録し、container 内部署を立てる",
        status: if !container_nodes.is_empty() && !container_members.is_empty() {
            AutoresearchAuditStatus::Pass
        } else if !container_nodes.is_empty() {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "container_nodes={} container_members={}",
            container_nodes
                .iter()
                .map(|node| format!(
                    "{}:{:?}:{:?}:{}",
                    node.id,
                    node.kind,
                    node.status,
                    node.container.as_deref().unwrap_or("-")
                ))
                .collect::<Vec<_>>()
                .join(", "),
            container_members
                .iter()
                .map(|member| member.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        gap: if !container_nodes.is_empty() && !container_members.is_empty() {
            "-".to_string()
        } else {
            "container node または container 内部署が不足しています".to_string()
        },
    });

    let runtime_wait = waits.iter().find(|wait| {
        let text =
            format!("{} {} {}", wait.title, wait.condition, wait.progress).to_ascii_lowercase();
        text.contains("runtime package") || text.contains("container runtime")
    });
    let runtime_package_local = files.iter().any(|path| {
        let lower = path.to_string_lossy().to_ascii_lowercase();
        (lower.contains("runtime_container") || lower.contains("runtime"))
            && (lower.ends_with("manifest.sha256")
                || lower.contains("runtime_report")
                || lower.contains("claim_evidence")
                || lower.contains("visualization"))
    });
    items.push(AutoresearchAuditItem {
        id: "container_runtime_evidence",
        requirement:
            "container 内で実験を行い、commands/logs/metrics/visualizations/claim-evidence/日本語report/manifest を保存する",
        status: if runtime_wait.is_some_and(|wait| matches!(wait.status, TeamWaitStatus::Completed))
            && runtime_package_local
        {
            AutoresearchAuditStatus::Pass
        } else if runtime_wait.is_some() || runtime_package_local {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "runtime_wait={} runtime_wait_status={} local_runtime_package_terms={}",
            runtime_wait.map(|wait| wait.id.as_str()).unwrap_or("-"),
            runtime_wait
                .map(|wait| wait.status.to_string())
                .unwrap_or_else(|| "-".to_string()),
            runtime_package_local
        ),
        gap: if runtime_wait.is_some_and(|wait| matches!(wait.status, TeamWaitStatus::Completed))
            && runtime_package_local
        {
            "-".to_string()
        } else {
            "runtime package は未完了またはローカルに pull/検証された成果物がありません".to_string()
        },
    });

    let evaluation_task_done = tasks.iter().any(|task| {
        task.owner.as_deref() == Some("evaluation") && matches!(task.status, TaskStatus::Completed)
    });
    let audit_task_done = tasks.iter().any(|task| {
        task.owner.as_deref() == Some("audit") && matches!(task.status, TaskStatus::Completed)
    });
    let eval_audit_files = files.iter().any(|path| {
        let lower = path.to_string_lossy().to_ascii_lowercase();
        (lower.contains("/evaluation/") || lower.contains("/audit/"))
            && (lower.ends_with("manifest.sha256")
                || lower.contains("team_completion_checklist")
                || lower.contains("claim"))
    });
    items.push(AutoresearchAuditItem {
        id: "evaluation_audit",
        requirement:
            "runtime 結果を evaluation/audit が独立評価し、allowed/blocked claims と次アクションを出す",
        status: if evaluation_task_done && audit_task_done && eval_audit_files {
            AutoresearchAuditStatus::Pass
        } else if eval_audit_files || evaluation_task_done || audit_task_done {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "evaluation_task_done={} audit_task_done={} eval_or_audit_files={}",
            evaluation_task_done, audit_task_done, eval_audit_files
        ),
        gap: if evaluation_task_done && audit_task_done && eval_audit_files {
            "-".to_string()
        } else {
            "evaluation/audit の完了または claim boundary 証拠が不足しています".to_string()
        },
    });

    let open_blocked_tasks = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Blocked)
        .collect::<Vec<_>>();
    let failed_tasks = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Failed)
        .collect::<Vec<_>>();
    let open_task_count = tasks.iter().filter(|task| task_is_open(task)).count();
    let blocked_summary = summarize_audit_tasks(&open_blocked_tasks, 6);
    let failed_summary = summarize_audit_tasks(&failed_tasks, 6);
    items.push(AutoresearchAuditItem {
        id: "latest_iteration_blockers",
        requirement:
            "最新 iteration に blocked/failed task が残っていないかを明示し、履歴上の PASS と現在の停止を混同しない",
        status: if !failed_tasks.is_empty() {
            AutoresearchAuditStatus::Fail
        } else if !open_blocked_tasks.is_empty() {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Pass
        },
        evidence: format!(
            "open_tasks={} blocked_tasks={} failed_tasks={}",
            open_task_count,
            if blocked_summary.is_empty() {
                "-".to_string()
            } else {
                blocked_summary
            },
            if failed_summary.is_empty() {
                "-".to_string()
            } else {
                failed_summary
            }
        ),
        gap: if !failed_tasks.is_empty() {
            "failed task が残っています。履歴上の成果物 PASS だけでは現在の研究ループを clean と判定できません".to_string()
        } else if !open_blocked_tasks.is_empty() {
            "blocked task が残っています。最新 iteration は完了ではなく、blocker 解消または明示的な user-input blocker 判定が必要です".to_string()
        } else {
            "-".to_string()
        },
    });

    let next_action_files = files.iter().any(|path| {
        file_contains_any(
            path,
            &[
                "recommended next action",
                "next bounded",
                "next experiment",
                "次の bounded",
                "次実験",
                "次アクション",
            ],
        )
    });
    let open_non_blocked_tasks = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Waiting
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Review
            )
        })
        .count();
    items.push(AutoresearchAuditItem {
        id: "loop_continuation",
        requirement:
            "1 iteration 後に final audit/recommended next action を出し、次 bounded task へ進む",
        status: if next_action_files && open_non_blocked_tasks > 0 {
            AutoresearchAuditStatus::Pass
        } else if next_action_files || open_non_blocked_tasks > 0 {
            AutoresearchAuditStatus::Warn
        } else {
            AutoresearchAuditStatus::Fail
        },
        evidence: format!(
            "next_action_terms_in_files={} open_non_blocked_tasks={}",
            next_action_files, open_non_blocked_tasks
        ),
        gap: if next_action_files && open_non_blocked_tasks > 0 {
            "-".to_string()
        } else {
            "次サイクル判断または次 bounded task が確認できません".to_string()
        },
    });

    Ok(format_autoresearch_audit_report(
        &config, team_dir, &roots, &items, &tasks, &waits, &jobs, &nodes,
    ))
}

fn summarize_audit_tasks(tasks: &[&TeamTask], limit: usize) -> String {
    let mut parts = tasks
        .iter()
        .take(limit)
        .map(|task| {
            format!(
                "#{}:{}:{}",
                task.id,
                task.owner.as_deref().unwrap_or("-"),
                compact_one_line(&task.subject, 120)
            )
        })
        .collect::<Vec<_>>();
    let omitted = tasks.len().saturating_sub(parts.len());
    if omitted > 0 {
        parts.push(format!("+{omitted} more"));
    }
    parts.join("; ")
}

fn format_autoresearch_audit_report(
    config: &TeamConfig,
    team_dir: &Path,
    roots: &[PathBuf],
    items: &[AutoresearchAuditItem],
    tasks: &[TeamTask],
    waits: &[TeamWait],
    jobs: &[TeamJob],
    nodes: &[TeamNode],
) -> String {
    let pass = items
        .iter()
        .filter(|item| item.status == AutoresearchAuditStatus::Pass)
        .count();
    let warn = items
        .iter()
        .filter(|item| item.status == AutoresearchAuditStatus::Warn)
        .count();
    let fail = items
        .iter()
        .filter(|item| item.status == AutoresearchAuditStatus::Fail)
        .count();
    let overall = if fail > 0 {
        "FAIL"
    } else if warn > 0 {
        "WARN"
    } else {
        "PASS"
    };
    let mut out = String::new();
    out.push_str(&format!("# Autoresearch Audit: {}\n\n", config.id));
    out.push_str(&format!("- generated_at: {}\n", now()));
    out.push_str(&format!("- team_state: {}\n", team_dir.display()));
    out.push_str(&format!(
        "- overall: {overall} ({pass} pass / {warn} warn / {fail} fail)\n"
    ));
    out.push_str(&format!(
        "- tasks: {}\n- waits: {} open / {} total\n- jobs: {}\n- nodes: {}\n\n",
        format_task_status_counts(tasks),
        waits.iter().filter(|wait| wait.status.is_open()).count(),
        waits.len(),
        jobs.len(),
        nodes.len()
    ));
    out.push_str("## Inspected Roots\n\n");
    for root in roots {
        out.push_str(&format!("- {}\n", root.display()));
    }
    out.push_str("\n## Prompt-To-Artifact Checklist\n\n");
    out.push_str("| ID | Status | Requirement | Evidence | Gap |\n");
    out.push_str("| --- | --- | --- | --- | --- |\n");
    for item in items {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            item.id,
            item.status.as_str(),
            md_table_cell(item.requirement),
            md_table_cell(&item.evidence),
            md_table_cell(&item.gap)
        ));
    }
    out.push_str("\n## Open Waits\n\n");
    for wait in waits.iter().filter(|wait| wait.status.is_open()) {
        out.push_str(&format!(
            "- {} [{}] owner={} task={} evidence={} title={}\n",
            wait.id,
            wait.status,
            wait.owner.as_deref().unwrap_or("-"),
            wait.task_id.as_deref().unwrap_or("-"),
            wait.evidence.as_deref().unwrap_or("-"),
            wait.title
        ));
    }
    out.push_str("\n## Open Tasks\n\n");
    for task in tasks.iter().filter(|task| task_is_open(task)) {
        out.push_str(&format!(
            "- {} [{}] owner={} deps={} subject={}\n",
            task.id,
            task.status,
            task.owner.as_deref().unwrap_or("-"),
            if task.depends_on.is_empty() {
                "-".to_string()
            } else {
                task.depends_on.join(",")
            },
            task.subject
        ));
    }
    out.push_str("\n## Non-Completed Jobs\n\n");
    for job in jobs
        .iter()
        .filter(|job| !matches!(job.status, TeamJobStatus::Completed))
    {
        out.push_str(&format!(
            "- {} [{:?}] node={} owner={} task={} log={}\n",
            job.id,
            job.status,
            job.node,
            job.owner.as_deref().unwrap_or("-"),
            job.task_id.as_deref().unwrap_or("-"),
            job.log_path
        ));
    }
    out.push_str("\n## Interpretation\n\n");
    out.push_str(
        "This audit reads team state and locally visible artifacts only. Remote/container paths are not treated as verified final evidence unless they have been pulled into an inspected local root or represented by completed wait/job artifacts with final manifests. A PASS here means the required gate has concrete local/team-state evidence; WARN means partial or remote-unverified evidence; FAIL means the gate is missing or still open.\n",
    );
    out
}

fn md_table_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', "<br>")
        .chars()
        .take(900)
        .collect()
}

fn autoresearch_candidate_roots(
    team_dir: &Path,
    config: &TeamConfig,
    ownerships: &[FileOwnership],
) -> Vec<PathBuf> {
    let mut roots = vec![team_dir.to_path_buf()];
    let research_root = PathBuf::from("/home/yukimaru/research").join(&config.id);
    if research_root.exists() {
        roots.push(research_root);
    }
    for ownership in ownerships {
        let path = PathBuf::from(&ownership.path);
        if path.is_absolute() && path.exists() && is_local_autoresearch_path(&path) {
            roots.push(if path.is_file() {
                path.parent().unwrap_or(path.as_path()).to_path_buf()
            } else {
                path
            });
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn is_local_autoresearch_path(path: &Path) -> bool {
    path.starts_with("/home/yukimaru") || path.starts_with("/tmp")
}

fn collect_audit_files(roots: &[PathBuf], max_depth: usize, limit: usize) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for root in roots {
        collect_audit_files_inner(root, 0, max_depth, limit, &mut files)?;
        if files.len() >= limit {
            break;
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn collect_audit_files_inner(
    path: &Path,
    depth: usize,
    max_depth: usize,
    limit: usize,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    if files.len() >= limit || depth > max_depth || !path.exists() {
        return Ok(());
    }
    if path.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(path) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let child = entry.path();
        let name = child
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        if child.is_dir() {
            collect_audit_files_inner(&child, depth + 1, max_depth, limit, files)?;
        } else if child.is_file() {
            files.push(child);
        }
        if files.len() >= limit {
            break;
        }
    }
    Ok(())
}

fn find_audit_file(files: &[PathBuf], needles: &[&str]) -> Option<PathBuf> {
    files.iter().find_map(|path| {
        let lower = path.to_string_lossy().to_ascii_lowercase();
        if needles.iter().all(|needle| lower.contains(needle)) {
            Some(path.clone())
        } else {
            None
        }
    })
}

fn file_contains_any(path: &Path, needles: &[&str]) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

fn read_small_audit_text(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() == 0 || metadata.len() > max_bytes {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn audit_text_contains_any(text: &str, needles: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

fn audit_text_has_min_words(text: &str, min_words: usize) -> bool {
    text.split_whitespace().take(min_words).count() >= min_words
}

fn autoresearch_scan_dir_has_strong_content(dir: &Path) -> bool {
    const REQUIRED: [&str; 7] = [
        "prompt.md",
        "result.md",
        "source_evidence.md",
        "confidence.md",
        "integration_target.md",
        "command_or_mcp_record.md",
        "manifest.sha256",
    ];
    if !REQUIRED.iter().all(|name| dir.join(name).exists()) {
        return false;
    }

    let Some(prompt) = read_small_audit_text(&dir.join("prompt.md"), 256 * 1024) else {
        return false;
    };
    let Some(result) = read_small_audit_text(&dir.join("result.md"), 1024 * 1024) else {
        return false;
    };
    let Some(source) = read_small_audit_text(&dir.join("source_evidence.md"), 512 * 1024) else {
        return false;
    };
    let Some(confidence) = read_small_audit_text(&dir.join("confidence.md"), 128 * 1024) else {
        return false;
    };
    let Some(integration) = read_small_audit_text(&dir.join("integration_target.md"), 128 * 1024)
    else {
        return false;
    };
    let Some(record) = read_small_audit_text(&dir.join("command_or_mcp_record.md"), 256 * 1024)
    else {
        return false;
    };
    let Some(manifest) = read_small_audit_text(&dir.join("manifest.sha256"), 256 * 1024) else {
        return false;
    };

    audit_text_has_min_words(&prompt, 20)
        && audit_text_has_min_words(&result, 80)
        && audit_text_has_min_words(&source, 20)
        && audit_text_contains_any(
            &source,
            &[
                "http://",
                "https://",
                "doi:",
                "arxiv",
                "request",
                "source",
                "provenance",
                "fetched",
                "timestamp",
                "url",
            ],
        )
        && audit_text_contains_any(
            &confidence,
            &[
                "confirmed",
                "likely",
                "speculative",
                "unknown",
                "確認",
                "高",
                "中",
                "低",
            ],
        )
        && audit_text_contains_any(
            &integration,
            &[
                "phase1",
                "synthesis",
                "environment",
                "handoff",
                "experiment",
                "runtime",
                "build",
                "run",
                "gating",
                "gate",
                "claim",
                "paper",
                "poc",
                "fallback",
                "dataset",
                "license",
                "access",
                "次",
                "統合",
                "実験",
            ],
        )
        && audit_text_contains_any(
            &record,
            &[
                "deep_thinker",
                "deep_research",
                "deepresearch",
                "mcp",
                "team wait",
                "request",
                "job",
                "command",
                "rc=",
                "exit=",
                "completed",
            ],
        )
        && audit_text_has_min_words(&record, 12)
        && audit_text_has_min_words(&manifest, 2)
}

fn autoresearch_scan_dir_has_wait_or_command_record(dir: &Path) -> bool {
    let Some(record) = read_small_audit_text(&dir.join("command_or_mcp_record.md"), 256 * 1024)
    else {
        return false;
    };
    audit_text_has_min_words(&record, 12)
        && audit_text_contains_any(
            &record,
            &[
                "deep_thinker",
                "deep_research",
                "deepresearch",
                "mcp",
                "team wait",
                "request",
                "job",
                "command",
                "rc=",
                "exit=",
                "completed",
            ],
        )
}

fn format_status_text(team_dir: &Path) -> Result<String> {
    auto_complete_wait_checks(team_dir)?;
    auto_promote_dependency_waits(team_dir)?;
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let waits = load_waits(team_dir)?;
    let mut out = String::new();
    out.push_str(&format!("Team: {}\n", config.id));
    out.push_str(&format!("Goal: {}\n", compact_one_line(&config.goal, 500)));
    out.push_str(&format_runtime_status_text(
        team_dir, &config, &tasks, &waits,
    ));
    out.push_str(&format!("Members: {}\n", config.members.len()));
    for member in &config.members {
        let task_status = member_task_status_summary(&tasks, &member.name);
        let mail = mailbox_unread_counts(team_dir, &member.name)?;
        out.push_str(&format!(
            "  {} ({}) session={:?} tasks={} node={} unread={} direct={}\n",
            member.name,
            member.role,
            member.status,
            task_status,
            member.node.as_deref().unwrap_or("local"),
            mail.unread,
            mail.direct_unread
        ));
    }
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    out.push_str(&format!("Nodes: {}\n", nodes.len()));
    for node in nodes {
        out.push_str(&format!("{}\n", format_node_status_line(&node)));
    }
    let cooldowns = format_usage_limit_cooldowns(team_dir, &config)?;
    if !cooldowns.is_empty() {
        out.push_str(&cooldowns);
    }
    let open_waits = waits.iter().filter(|wait| wait.status.is_open()).count();
    if open_waits > 0 {
        out.push_str(&format!(
            "Waits: {open_waits} open, {} total\n",
            waits.len()
        ));
        for wait in waits.iter().filter(|wait| wait.status.is_open()).take(12) {
            out.push_str(&format_wait_line(wait));
            out.push('\n');
        }
    }
    out.push_str(&format!("Tasks: {}\n", format_task_status_counts(&tasks)));
    out.push_str(&format_tasks_text(team_dir)?);
    let ownerships = format_ownerships_text(team_dir)?;
    if !ownerships.trim().is_empty() && !ownerships.starts_with("No ownership") {
        out.push_str(&format!("Ownerships:\n{ownerships}"));
    }
    Ok(out)
}

fn format_runtime_status_text(
    team_dir: &Path,
    config: &TeamConfig,
    tasks: &[TeamTask],
    waits: &[TeamWait],
) -> String {
    let status = team_run_status_for_dir(team_dir, &config.id);
    let run_pid = read_team_run_pid(team_dir)
        .map(|pid| {
            let state = if process_alive(pid) { "alive" } else { "dead" };
            format!(" run_pid={pid}({state})")
        })
        .unwrap_or_default();
    let ui_pid = team_dir
        .parent()
        .and_then(|root| read_ui_team_pid(root, &config.id))
        .map(|pid| {
            let state = if process_alive(pid) { "alive" } else { "dead" };
            format!(" ui_pid={pid}({state})")
        })
        .unwrap_or_default();
    let open_tasks = open_task_count(tasks);
    let open_waits = open_wait_count(waits);
    let mut out = format!(
        "Runtime: {}{}{} open_tasks={} open_waits={}\n",
        status.label(),
        run_pid,
        ui_pid,
        open_tasks,
        open_waits
    );
    if matches!(status, UiTeamRunStatus::Exiting) && (open_tasks > 0 || open_waits > 0) {
        out.push_str(&format!(
            "Runtime warning: open work remains but the team runtime is stopped. Resume with: codex team resume --team {} --dangerously-bypass-approvals-and-sandbox\n",
            config.id
        ));
    }
    out
}

fn open_task_count(tasks: &[TeamTask]) -> usize {
    tasks.iter().filter(|task| task_is_open(task)).count()
}

fn open_wait_count(waits: &[TeamWait]) -> usize {
    waits.iter().filter(|wait| wait.status.is_open()).count()
}

fn format_tasks_text(team_dir: &Path) -> Result<String> {
    auto_complete_wait_checks(team_dir)?;
    auto_promote_dependency_waits(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    if tasks.is_empty() {
        return Ok("No tasks found.\n".to_string());
    }
    let mut out = String::new();
    for task in tasks {
        out.push_str(&format_task_line(&task));
        out.push('\n');
    }
    Ok(out)
}

fn format_task_status_counts(tasks: &[TeamTask]) -> String {
    let completed = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Completed))
        .count();
    let open = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Waiting
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Blocked
                    | TaskStatus::Review
            )
        })
        .count();
    let cancelled = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Cancelled))
        .count();
    let failed = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Failed))
        .count();
    format!(
        "{completed} completed, {open} open, {cancelled} cancelled, {failed} failed, {} total",
        tasks.len()
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MailboxUnreadCounts {
    unread: usize,
    direct_unread: usize,
}

fn mailbox_unread_counts(team_dir: &Path, member_name: &str) -> Result<MailboxUnreadCounts> {
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, member_name))?;
    let unread = messages.iter().filter(|message| !message.read).count();
    let direct_unread = messages
        .iter()
        .filter(|message| !message.read && message.from != "system")
        .count();
    Ok(MailboxUnreadCounts {
        unread,
        direct_unread,
    })
}

fn format_node_status_line(node: &TeamNode) -> String {
    let (age, stale) = format_node_last_seen_age(&node.updated_at);
    let status = format_node_display_status(&node.status, stale);
    format!(
        "  {} {:?} {} url={} last_seen={} age={}{}",
        node.id,
        node.kind,
        status,
        node.url.as_deref().unwrap_or(""),
        node.updated_at,
        age,
        if stale { " stale" } else { "" }
    )
}

fn format_node_display_status(status: &TeamNodeStatus, stale: bool) -> String {
    if stale {
        match status {
            TeamNodeStatus::Online => "Stale(raw=Online)".to_string(),
            TeamNodeStatus::Pending => "Pending(stale)".to_string(),
            TeamNodeStatus::Offline => "Offline(stale)".to_string(),
            TeamNodeStatus::Failed => "Failed(stale)".to_string(),
        }
    } else {
        format!("{status:?}")
    }
}

#[derive(Clone, Debug)]
struct NodeUnavailableReason {
    reason: &'static str,
    status: String,
    age: String,
}

fn member_node_unavailable_from_nodes(
    member: &TeamMember,
    nodes: &[TeamNode],
) -> Option<NodeUnavailableReason> {
    node_unavailable_from_nodes(&member_node_id(member), nodes)
}

fn node_unavailable_from_nodes(node_id: &str, nodes: &[TeamNode]) -> Option<NodeUnavailableReason> {
    if node_id == "local" {
        return None;
    }
    let Some(node) = nodes.iter().find(|node| node.id == node_id) else {
        return Some(NodeUnavailableReason {
            reason: "node_missing",
            status: "missing".to_string(),
            age: "unknown".to_string(),
        });
    };
    let (age, stale) = format_node_last_seen_age(&node.updated_at);
    if stale {
        return Some(NodeUnavailableReason {
            reason: "node_stale",
            status: format!("{:?}", node.status),
            age,
        });
    }
    if node.status != TeamNodeStatus::Online {
        return Some(NodeUnavailableReason {
            reason: "node_unavailable",
            status: format!("{:?}", node.status),
            age,
        });
    }
    None
}

fn format_usage_limit_cooldowns(team_dir: &Path, config: &TeamConfig) -> Result<String> {
    let mut lines = Vec::new();
    for member in &config.members {
        if let Some(remaining) = recent_usage_limit_retry_remaining(team_dir, &member.name)? {
            lines.push(format!(
                "  {} usage_limit retry_in={}",
                member.name,
                format_compact_duration(remaining.as_secs())
            ));
        }
    }
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("Cooldowns:\n{}\n", lines.join("\n")))
    }
}

fn format_node_last_seen_age(updated_at: &str) -> (String, bool) {
    const STALE_AFTER_SEC: i64 = 10 * 60;
    let Ok(timestamp) = DateTime::parse_from_rfc3339(updated_at) else {
        return ("unknown".to_string(), true);
    };
    let age = Utc::now()
        .signed_duration_since(timestamp.with_timezone(&Utc))
        .num_seconds()
        .max(0);
    (format_compact_duration(age as u64), age >= STALE_AFTER_SEC)
}

fn format_compact_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

fn format_tokens(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (idx, ch) in digits.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    let formatted = out.chars().rev().collect::<String>();
    format!("{sign}{formatted}")
}

fn member_task_status_summary(tasks: &[TeamTask], member_name: &str) -> String {
    let mut owned = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member_name))
        .filter(|task| {
            !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        })
        .collect::<Vec<_>>();
    owned.sort_by(|a, b| a.id.cmp(&b.id));
    if owned.is_empty() {
        return "no_open_tasks".to_string();
    }
    owned
        .into_iter()
        .take(4)
        .map(|task| format!("#{}:{}", task.id, task.status))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_task_line(task: &TeamTask) -> String {
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
    format!(
        "  {:>3} {:<11} {:<16} {}{}",
        task.id, task.status, owner, subject, deps
    )
}

fn format_wait_line(wait: &TeamWait) -> String {
    format!(
        "  {:<8} {:<9} owner={:<12} task={:<6} {}",
        wait.id,
        wait.status,
        wait.owner.as_deref().unwrap_or("-"),
        wait.task_id.as_deref().unwrap_or("-"),
        wait.title
    )
}

fn format_inbox_text(team_dir: &Path, member: &str) -> Result<String> {
    let config = load_config(team_dir)?;
    let member = sanitize_id(member);
    ensure_member_exists(&config, &member)?;
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member))?;
    if messages.is_empty() {
        return Ok(format!("Inbox for `{member}` is empty.\n"));
    }
    let mut out = String::new();
    for msg in messages {
        out.push_str(&format!(
            "[{}] {} -> {}: {}\n",
            msg.timestamp, msg.from, msg.to, msg.message
        ));
    }
    Ok(out)
}

fn format_ownerships_text(team_dir: &Path) -> Result<String> {
    let ownerships = load_ownerships(team_dir)?;
    if ownerships.is_empty() {
        return Ok("No ownership claims.\n".to_string());
    }
    let mut out = String::new();
    for ownership in ownerships {
        let note = if ownership.note.trim().is_empty() {
            String::new()
        } else {
            format!("  {}", ownership.note)
        };
        out.push_str(&format!(
            "  {:<24} {}{}\n",
            format!("@{}", ownership.owner),
            ownership.path,
            note
        ));
    }
    Ok(out)
}

fn parse_task_status(value: &str) -> Result<TaskStatus> {
    match value.trim().replace('-', "_").as_str() {
        "pending" => Ok(TaskStatus::Pending),
        "waiting" | "wait" => Ok(TaskStatus::Waiting),
        "ready" => Ok(TaskStatus::Ready),
        "in_progress" => Ok(TaskStatus::InProgress),
        "blocked" => Ok(TaskStatus::Blocked),
        "review" => Ok(TaskStatus::Review),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        "cancelled" | "canceled" => Ok(TaskStatus::Cancelled),
        other => bail!("unsupported task status `{other}`"),
    }
}

fn parse_job_status(value: &str) -> Result<TeamJobStatus> {
    match value.trim().replace('-', "_").as_str() {
        "running" => Ok(TeamJobStatus::Running),
        "completed" => Ok(TeamJobStatus::Completed),
        "failed" => Ok(TeamJobStatus::Failed),
        "stopped" => Ok(TeamJobStatus::Stopped),
        "unknown" => Ok(TeamJobStatus::Unknown),
        other => bail!("unsupported job status `{other}`"),
    }
}

fn parse_wait_status(value: &str) -> Result<TeamWaitStatus> {
    match value.trim().replace('-', "_").as_str() {
        "waiting" | "wait" => Ok(TeamWaitStatus::Waiting),
        "running" => Ok(TeamWaitStatus::Running),
        "polling" | "pending" => Ok(TeamWaitStatus::Polling),
        "blocked" => Ok(TeamWaitStatus::Blocked),
        "completed" | "complete" | "done" => Ok(TeamWaitStatus::Completed),
        "failed" | "failure" => Ok(TeamWaitStatus::Failed),
        "cancelled" | "canceled" => Ok(TeamWaitStatus::Cancelled),
        other => bail!("unsupported wait status `{other}`"),
    }
}

