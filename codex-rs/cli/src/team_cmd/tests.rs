#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn write_test_config(team_dir: &Path) {
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-task-test".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "engineering".to_string(),
                    role: "engineering".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "quality".to_string(),
                    role: "quality".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
    }

    fn write_test_task(
        team_dir: &Path,
        id: &str,
        owner: Option<&str>,
        status: TaskStatus,
        depends_on: Vec<&str>,
        result: Option<&str>,
    ) {
        let now = now();
        write_json_atomic(
            &task_path(team_dir, id),
            &TeamTask {
                id: id.to_string(),
                subject: format!("task {id}"),
                description: String::new(),
                owner: owner.map(str::to_string),
                status,
                depends_on: depends_on.into_iter().map(str::to_string).collect(),
                result: result.map(str::to_string),
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
    }

    #[test]
    fn container_cleanup_recognizes_team_managed_runtime_names() {
        let script = container_team_cleanup_shell(
            "team-20260511115343",
            "team-20260511115343-runtime",
            false,
        );
        assert!(script.contains("[c]odex app-server"));
    }

    #[test]
    fn container_cleanup_does_not_broadly_kill_shared_containers_by_default() {
        let script =
            container_team_cleanup_shell("team-20260511115343", "shared-runtime-container", false);
        assert!(!script.contains("[c]odex app-server"));
        assert!(script.contains("[C]ODEX_TEAM_ID"));
    }

    #[test]
    fn embedded_local_path_extraction_handles_japanese_punctuation_and_prefixes() {
        assert_eq!(
            clean_embedded_path_token(
                "出力先=/home/yukimaru/research/team/audit/protocol_freeze_review。必須成果物:"
            ),
            Some("/home/yukimaru/research/team/audit/protocol_freeze_review")
        );
        assert_eq!(
            clean_embedded_path_token(
                "root=/home/yukimaru/research/team/evaluation/protocol_freeze_review、manifest"
            ),
            Some("/home/yukimaru/research/team/evaluation/protocol_freeze_review")
        );
        assert_eq!(
            clean_embedded_absolute_path_token(
                "container_root=/workspace/team/runtime/cycle1。次の確認"
            )
            .as_deref(),
            Some("/workspace/team/runtime/cycle1")
        );
    }

    #[test]
    fn completion_path_extraction_prefers_declared_output_over_input_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let input_root = team_dir
            .join("evaluation")
            .join("cycle1_rbo_source_dev_review");
        let output_root = team_dir
            .join("research_planning")
            .join("source_dev_schema_warning_triage");
        fs::create_dir_all(&input_root).expect("input root");
        fs::create_dir_all(&output_root).expect("output root");
        let task = TeamTask {
            id: "29".to_string(),
            subject: "source/dev schema warning triage".to_string(),
            description: format!(
                "Authoritative inputs: task11 source/dev review root={}; task25 protocol_freeze root={}. Produce {} with warning_triage.md, parser_field_contract.yaml, TEAM_COMPLETION_CHECKLIST.md, manifest.sha256, manifest_check.log.",
                input_root.display(),
                team_dir
                    .join("research_planning")
                    .join("protocol_freeze")
                    .display(),
                output_root.display(),
            ),
            owner: Some("research_planning".to_string()),
            status: TaskStatus::InProgress,
            depends_on: Vec::new(),
            result: None,
            created_at: now(),
            updated_at: now(),
        };

        let paths = extract_probable_local_output_paths_from_task_text(team_dir, &task);

        assert_eq!(paths, vec![output_root]);
    }

    #[test]
    fn completion_checker_does_not_reject_complete_output_due_to_incomplete_input_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let input_root = team_dir
            .join("evaluation")
            .join("cycle1_rbo_source_dev_review");
        let output_root = team_dir
            .join("research_planning")
            .join("source_dev_schema_warning_triage");
        fs::create_dir_all(&input_root).expect("input root");
        fs::create_dir_all(&output_root).expect("output root");
        fs::write(output_root.join("warning_triage.md"), "# triage\n").expect("report");
        fs::write(
            output_root.join("parser_field_contract.yaml"),
            "team: test\n",
        )
        .expect("yaml");
        fs::write(
            output_root.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: source_dev_schema_warning_triage\n- verification: sha256sum -c manifest.sha256 rc=0\n- messages_sent: lead/evaluation/audit\n- consumers_notified: lead/evaluation/audit\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "warning_triage.md",
                "parser_field_contract.yaml",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&output_root)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(output_root.join("manifest.sha256"), manifest.stdout).expect("manifest");
        let task = TeamTask {
            id: "29".to_string(),
            subject: "source/dev schema warning triage".to_string(),
            description: format!(
                "Authoritative inputs: task11 source/dev review root={}. Produce {} with warning_triage.md, parser_field_contract.yaml, TEAM_COMPLETION_CHECKLIST.md, manifest.sha256.",
                input_root.display(),
                output_root.display(),
            ),
            owner: Some("research_planning".to_string()),
            status: TaskStatus::InProgress,
            depends_on: Vec::new(),
            result: Some("handoff complete".to_string()),
            created_at: now(),
            updated_at: now(),
        };

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_checker_rejects_stale_reported_manifest_hash() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let output_root = team_dir.join("research_planning").join("task32_gate");
        fs::create_dir_all(&output_root).expect("output root");
        fs::write(output_root.join("reconciliation_report.md"), "# report\n").expect("report");
        fs::write(output_root.join("gate.yaml"), "verdict: pass\n").expect("yaml");
        fs::write(
            output_root.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: task32_gate\n- verification: sha256sum -c manifest.sha256 rc=0\n- messages_sent: lead/evaluation/audit\n- consumers_notified: lead/evaluation/audit\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        let first_manifest = Command::new("sha256sum")
            .args([
                "reconciliation_report.md",
                "gate.yaml",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&output_root)
            .output()
            .expect("sha256sum");
        assert!(first_manifest.status.success());
        fs::write(output_root.join("manifest.sha256"), first_manifest.stdout).expect("manifest");
        let stale_manifest_hash =
            sha256sum_file(&output_root.join("manifest.sha256")).expect("manifest hash");

        let second_manifest = Command::new("sha256sum")
            .args([
                "TEAM_COMPLETION_CHECKLIST.md",
                "gate.yaml",
                "reconciliation_report.md",
            ])
            .current_dir(&output_root)
            .output()
            .expect("sha256sum");
        assert!(second_manifest.status.success());
        fs::write(output_root.join("manifest.sha256"), second_manifest.stdout).expect("manifest");
        assert_ne!(
            stale_manifest_hash,
            sha256sum_file(&output_root.join("manifest.sha256")).expect("current manifest hash")
        );
        let task = TeamTask {
            id: "32".to_string(),
            subject: "task32 gate".to_string(),
            description: format!(
                "Produce {} with reconciliation_report.md, gate.yaml, TEAM_COMPLETION_CHECKLIST.md, manifest.sha256.",
                output_root.display(),
            ),
            owner: Some("research_planning".to_string()),
            status: TaskStatus::InProgress,
            depends_on: Vec::new(),
            result: Some(format!(
                "corrected package complete: manifest hash={stale_manifest_hash}"
            )),
            created_at: now(),
            updated_at: now(),
        };

        let issue = task_completion_blocker(team_dir, &task).expect("completion blocker");

        assert!(
            issue
                .as_deref()
                .unwrap_or("")
                .contains("reported handoff hash for `manifest.sha256` is stale")
        );
    }

    #[test]
    fn side_channel_reply_strips_completion_checklist_noise() {
        let member = TeamMember {
            name: "runtime_container".to_string(),
            role: "container".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let reply = AppServerSideReply {
            member,
            node_id: "runtime_container".to_string(),
            source_thread_id: "main".to_string(),
            side_thread_id: "side".to_string(),
            turn_id: "turn".to_string(),
            usage_category: "side_channel_reply".to_string(),
            recipients: vec!["lead".to_string()],
            messages: Vec::new(),
            buffer: "現状はpreflight中です。\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: none\n- verification: message sent rc=0\n"
                .to_string(),
            started_at: Instant::now(),
        };

        let body = side_reply_message_body(&reply, TeamPromptLanguage::Ja);

        assert!(body.contains("現状はpreflight中です。"));
        assert!(!body.contains("TEAM_COMPLETION_CHECKLIST"));
        assert!(!body.contains("artifacts: none"));
    }

    #[test]
    fn side_channel_prompt_forbids_completion_checklist() {
        let member = TeamMember {
            name: "worker".to_string(),
            role: "runtime".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let messages = vec![MailMessage {
            from: "lead".to_string(),
            to: "worker".to_string(),
            message: "status?".to_string(),
            timestamp: now(),
            read: false,
        }];

        let en = build_side_channel_reply_prompt(&member, &messages, TeamPromptLanguage::En);
        let ja = build_side_channel_reply_prompt(&member, &messages, TeamPromptLanguage::Ja);

        assert!(en.contains("never include TEAM_COMPLETION_CHECKLIST"));
        assert!(ja.contains("TEAM_COMPLETION_CHECKLIST を絶対に書かない"));
    }

    #[test]
    fn side_channel_ignores_status_handoff_updates() {
        let message = MailMessage {
            from: "evaluation".to_string(),
            to: "runtime".to_string(),
            message: "受領しました。current mode は task 3 blocked / wait-2 waiting(open) 継続です。next checkpoint は runtime package 到着です。"
                .to_string(),
            timestamp: now(),
            read: false,
        };

        assert!(!side_channel_message_needs_fast_reply("runtime", &message));
    }

    #[test]
    fn side_channel_ignores_japanese_no_reply_outreach() {
        let message = MailMessage {
            from: "phase0_scan_recovery".to_string(),
            to: "runtime".to_string(),
            message: "@phase0_scan_recovery からの定期アイドル声かけ: 私はいま free/standby です。blocker、レビュー依頼、artifact 解釈など、手伝えることはありますか？問題なく進んでいるなら返信不要です。"
                .to_string(),
            timestamp: now(),
            read: false,
        };

        assert!(!side_channel_message_needs_fast_reply("runtime", &message));
    }

    #[test]
    fn side_channel_still_replies_to_real_questions() {
        let message = MailMessage {
            from: "evaluation".to_string(),
            to: "runtime".to_string(),
            message: "runtime package の正式 path はどれですか？".to_string(),
            timestamp: now(),
            read: false,
        };

        assert!(side_channel_message_needs_fast_reply("runtime", &message));
    }

    #[test]
    fn autoresearch_audit_maps_required_gates_to_artifacts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-autoresearch-audit".to_string(),
            goal: "Autoresearch loop for 操作可能デジタルツイン / laboratory instrument digital twin using phase0.md phase1.md, deep_thinker, ssh saitou Docker, container runtime experiments, evaluation, audit, and next bounded experiment."
                .to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "runtime_container-container".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: Some("runtime_container".to_string()),
                },
            ],
            language: Some(TeamPromptLanguage::Ja),
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("config");
        let phase0 = team_dir.join("research_planning").join("phase0_scans");
        for i in 1..=6 {
            let dir = phase0.join(format!("scan{i:02}"));
            fs::create_dir_all(&dir).expect("scan dir");
            fs::write(
                dir.join("prompt.md"),
                format!(
                    "Deep research scan {i}: investigate prior work, datasets, models, evaluation, and runtime constraints for the articulated laboratory digital twin loop with concrete source requirements and integration targets for phase1 synthesis.\n"
                ),
            )
            .expect("scan prompt");
            fs::write(
                dir.join("result.md"),
                format!(
                    "Result for scan {i}. This scan records substantive findings about articulated objects, data availability, open source methods, evaluation protocols, runtime risks, and how the evidence should influence the next bounded experiment. The package intentionally contains enough detail to distinguish actual research output from a placeholder list. It also records limitations, alternatives, rejected options, and the handoff needed by phase1 synthesis, environment planning, and runtime validation teams. These repeated words ensure the audit sees a meaningful result artifact rather than an empty stub. {}\n",
                    "evidence ".repeat(50)
                ),
            )
            .expect("scan result");
            fs::write(
                dir.join("source_evidence.md"),
                format!(
                    "Source evidence for scan {i}: https://example.com/paper-{i} timestamp=2026-05-13 request_id=req-{i} provenance=fetched-html source=public web metadata. Additional source notes preserve URL, access status, confidence reason, and exact material consumed by the scan owner.\n"
                ),
            )
            .expect("scan source");
            fs::write(
                dir.join("confidence.md"),
                "confirmed: public source available\nlikely: runtime can be reproduced\nspeculative: dataset quality may vary\nunknown: final benchmark coverage\n",
            )
            .expect("scan confidence");
            fs::write(
                dir.join("integration_target.md"),
                "phase1 synthesis target: feed this scan into bounded experiment selection, environment_handoff, evaluation design, and next runtime experiment planning.\n",
            )
            .expect("scan integration");
            fs::write(
                dir.join("command_or_mcp_record.md"),
                format!(
                    "team wait wait-deep-{i} completed deep_thinker request req-{i} command=mcp/deep_research status=completed rc=0 evidence_path=source_evidence.md saved_result=result.md\n"
                ),
            )
            .expect("scan command record");
            fs::write(
                dir.join("manifest.sha256"),
                "0123456789abcdef  result.md\nfedcba9876543210  source_evidence.md\n",
            )
            .expect("scan manifest");
        }
        fs::write(phase0.join("MANIFEST.sha256"), "manifest\n").expect("phase0 manifest");
        fs::write(
            phase0.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST\n",
        )
        .expect("phase0 checklist");
        let phase1 = team_dir.join("research_planning").join("phase1");
        fs::create_dir_all(&phase1).expect("phase1 dir");
        fs::write(phase1.join("MANIFEST.sha256"), "manifest\n").expect("phase1 manifest");
        fs::write(
            phase1.join("synthesis.md"),
            "killer experiment and bounded experiment with environment_handoff\n",
        )
        .expect("phase1 synthesis");
        let runtime = team_dir.join("runtime_container").join("reports");
        fs::create_dir_all(&runtime).expect("runtime dir");
        fs::write(
            team_dir.join("runtime_container").join("MANIFEST.sha256"),
            "manifest\n",
        )
        .expect("runtime manifest");
        fs::write(runtime.join("runtime_report_ja.md"), "日本語 report\n").expect("runtime report");
        fs::write(runtime.join("claim_evidence_table.md"), "claim table\n").expect("claim table");
        let evaluation = team_dir.join("evaluation");
        let audit = team_dir.join("audit");
        fs::create_dir_all(&evaluation).expect("evaluation dir");
        fs::create_dir_all(&audit).expect("audit dir");
        fs::write(evaluation.join("MANIFEST.sha256"), "manifest\n").expect("eval manifest");
        fs::write(
            audit.join("TEAM_COMPLETION_CHECKLIST.md"),
            "recommended next action\n",
        )
        .expect("audit checklist");
        write_test_task(
            team_dir,
            "1",
            Some("evaluation"),
            TaskStatus::Completed,
            Vec::new(),
            Some("evaluation done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("audit"),
            TaskStatus::Completed,
            Vec::new(),
            Some("audit done"),
        );
        write_test_task(
            team_dir,
            "3",
            Some("lead"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("next bounded task"),
        );
        fs::create_dir_all(waits_dir(team_dir)).expect("waits dir");
        for i in 1..=6 {
            write_json_atomic(
                &wait_path(team_dir, &format!("wait-deep-{i}")),
                &TeamWait {
                    id: format!("wait-deep-{i}"),
                    title: format!("deep_thinker phase0 scan {i}"),
                    owner: Some("research_planning".to_string()),
                    task_id: Some("1".to_string()),
                    node: None,
                    condition: "deep_thinker/deep_research returns with source-backed result"
                        .to_string(),
                    status: TeamWaitStatus::Completed,
                    progress: format!("saved result for scan {i}"),
                    evidence: Some(format!(
                        "research_planning/phase0_scans/scan{i:02}/source_evidence.md"
                    )),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                },
            )
            .expect("wait");
        }
        write_json_atomic(
            &wait_path(team_dir, "wait-runtime"),
            &TeamWait {
                id: "wait-runtime".to_string(),
                title: "container runtime package".to_string(),
                owner: Some("runtime_container-container".to_string()),
                task_id: Some("3".to_string()),
                node: Some("runtime_container".to_string()),
                condition: "runtime package complete".to_string(),
                status: TeamWaitStatus::Completed,
                progress: "runtime package saved".to_string(),
                evidence: Some("runtime_container/MANIFEST.sha256".to_string()),
                created_at: now.clone(),
                updated_at: now.clone(),
            },
        )
        .expect("runtime wait");
        fs::create_dir_all(jobs_dir(team_dir)).expect("jobs dir");
        write_json_atomic(
            &job_path(team_dir, "docker-build"),
            &TeamJob {
                id: "docker-build".to_string(),
                node: "saitou".to_string(),
                command: "docker build .".to_string(),
                cwd: "/data2/nonaka/team".to_string(),
                owner: Some("remote_build_ops".to_string()),
                task_id: Some("2".to_string()),
                status: TeamJobStatus::Completed,
                pid: None,
                log_path: "/tmp/build.log".to_string(),
                exit_path: "/tmp/exit.code".to_string(),
                exit_code: Some(0),
                note: String::new(),
                artifacts: Vec::new(),
                created_at: now.clone(),
                updated_at: now.clone(),
            },
        )
        .expect("job");
        write_nodes(
            team_dir,
            &[
                TeamNode {
                    id: "saitou".to_string(),
                    kind: TeamNodeKind::Ssh,
                    url: None,
                    host: Some("saitou".to_string()),
                    container: None,
                    cwd: Some("/data2/nonaka".to_string()),
                    status: TeamNodeStatus::Online,
                    note: String::new(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                },
                TeamNode {
                    id: "runtime_container".to_string(),
                    kind: TeamNodeKind::SshDocker,
                    url: None,
                    host: Some("saitou".to_string()),
                    container: Some("runtime".to_string()),
                    cwd: Some("/workspace/run".to_string()),
                    status: TeamNodeStatus::Online,
                    note: String::new(),
                    created_at: now.clone(),
                    updated_at: now,
                },
            ],
        )
        .expect("nodes");

        let report = build_autoresearch_audit_report(team_dir).expect("audit");

        assert!(report.contains("overall: PASS"));
        assert!(report.contains("| phase0_scans | PASS |"));
        assert!(report.contains("| container_runtime_evidence | PASS |"));
        assert!(report.contains("| loop_continuation | PASS |"));
    }

    #[test]
    fn autoresearch_audit_accepts_scan_command_records_without_six_deep_waits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-autoresearch-scan-records".to_string(),
            goal: "Autoresearch loop for laboratory instrument digital twin / 実験装置デジタルツイン using phase0.md phase1.md, deep_thinker, ssh saitou Docker, container runtime experiments, evaluation, audit, and next bounded experiment."
                .to_string(),
            lead: "lead".to_string(),
            members: vec![TeamMember {
                name: "lead".to_string(),
                role: "lead".to_string(),
                status: MemberStatus::Standby,
                joined_at: now.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            }],
            language: Some(TeamPromptLanguage::Ja),
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("config");

        let phase0 = team_dir.join("research_planning").join("phase0_scans");
        for i in 1..=6 {
            let dir = phase0.join(format!("scan{i:02}"));
            fs::create_dir_all(&dir).expect("scan dir");
            fs::write(
                dir.join("prompt.md"),
                format!(
                    "Research scan {i}: investigate evidence, datasets, models, evaluation, implementation, deployment constraints, licensing, failure modes, integration targets, and validation requirements for an auditable articulated digital twin research loop with source-backed outputs.\n"
                ),
            )
            .expect("prompt");
            fs::write(
                dir.join("result.md"),
                format!(
                    "Substantive scan {i} result. The scan records concrete findings, source-backed limits, runtime assumptions, research risks, alternatives, and how the evidence affects the next bounded experiment and validation plan. It distinguishes confirmed, likely, speculative, and unknown items and preserves enough detail to avoid being a placeholder. {}\n",
                    "evidence ".repeat(50)
                ),
            )
            .expect("result");
            fs::write(
                dir.join("source_evidence.md"),
                format!(
                    "Source evidence scan {i}: https://example.com/source-{i} timestamp=2026-05-13 request_id=req-{i} provenance=fetched source=url metadata saved with title, access status, retrieval notes, license notes, and cross-check summary for the downstream phase1 synthesis.\n"
                ),
            )
            .expect("source");
            fs::write(
                dir.join("confidence.md"),
                "confirmed: source snapshot exists\nlikely: useful for first PoC\nspeculative: transfer to real lab remains unknown\nunknown: final benchmark strength\n",
            )
            .expect("confidence");
            let integration = if i == 5 {
                "Use this scan as build/run gating. Dockerfile may install code packages but must not embed gated assets. Runtime should preserve access logs. If a dataset returns 401/403, mark it blocked and use fallback. Paper/PoC claims must state limitations.\n"
            } else {
                "phase1 synthesis target: use this scan for environment handoff, bounded experiment choice, runtime validation, and next task planning.\n"
            };
            fs::write(dir.join("integration_target.md"), integration).expect("integration");
            fs::write(
                dir.join("command_or_mcp_record.md"),
                format!(
                    "scan {i} used saved MCP/deep_thinker command records and team wait evidence. request=req-{i} status=completed rc=0 evidence=result.md source=source_evidence.md\n"
                ),
            )
            .expect("record");
            fs::write(
                dir.join("manifest.sha256"),
                "0123456789abcdef  result.md\nfedcba9876543210  source_evidence.md\n",
            )
            .expect("manifest");
        }
        fs::write(phase0.join("MANIFEST.sha256"), "manifest\n").expect("phase0 manifest");
        fs::write(
            phase0.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST\n",
        )
        .expect("phase0 checklist");

        fs::create_dir_all(waits_dir(team_dir)).expect("waits dir");
        for i in 1..=3 {
            write_json_atomic(
                &wait_path(team_dir, &format!("wait-deep-{i}")),
                &TeamWait {
                    id: format!("wait-deep-{i}"),
                    title: format!("deep_thinker research bundle {i}"),
                    owner: Some("research_planning".to_string()),
                    task_id: Some("1".to_string()),
                    node: None,
                    condition: "deep_thinker/deep_research returns with source-backed result"
                        .to_string(),
                    status: TeamWaitStatus::Completed,
                    progress: format!("saved bundled result {i} covering multiple scans"),
                    evidence: Some(format!(
                        "research_planning/phase0_scans/scan{i:02}/source_evidence.md"
                    )),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                },
            )
            .expect("wait");
        }

        let report = build_autoresearch_audit_report(team_dir).expect("audit");

        assert!(report.contains("| phase0_scans | PASS |"));
        assert!(report.contains("| mcp_waits | PASS |"));
        assert!(report.contains("scan_wait_or_command_records=6"));
    }

    #[test]
    fn autoresearch_audit_warns_on_blocked_latest_iteration_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-autoresearch-blocked".to_string(),
            goal: "Autoresearch loop for laboratory instrument digital twin / 実験装置デジタルツイン using phase0.md phase1.md, deep_thinker, ssh saitou Docker, container runtime experiments, evaluation, audit, and next bounded experiment."
                .to_string(),
            lead: "lead".to_string(),
            members: vec![TeamMember {
                name: "lead".to_string(),
                role: "lead".to_string(),
                status: MemberStatus::Standby,
                joined_at: now.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            }],
            language: Some(TeamPromptLanguage::Ja),
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("config");
        write_test_task(
            team_dir,
            "20",
            Some("audit"),
            TaskStatus::Blocked,
            Vec::new(),
            Some("iteration5 final audit"),
        );

        let report = build_autoresearch_audit_report(team_dir).expect("audit");

        assert!(report.contains("| latest_iteration_blockers | WARN |"));
        assert!(report.contains("blocked_tasks=#20:audit:task 20"));
        assert!(report.contains("## Open Tasks"));
        assert!(report.contains("- 20 [blocked] owner=audit"));
    }

    #[test]
    fn autoresearch_runtime_audit_writes_snapshot_without_team_steer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let created = now();
        let config = TeamConfig {
            version: 1,
            id: "team-autoresearch-runtime-audit".to_string(),
            goal: "自動研究として phase0.md と phase1.md を使い、deep_thinker/deep_researcher、ssh saitou Dockerfile build、container 実験、結果を見て次実験を永遠に繰り返す。実験装置デジタルツイン研究。".to_string(),
            lead: "lead".to_string(),
            members: vec![TeamMember {
                name: "lead".to_string(),
                role: "lead".to_string(),
                status: MemberStatus::Standby,
                joined_at: created.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            }],
            language: Some(TeamPromptLanguage::Ja),
            created_at: created.clone(),
            updated_at: created,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("config");

        let mut last_audit = Instant::now() - Duration::from_secs(601);
        maybe_write_autoresearch_runtime_audit(team_dir, &mut last_audit, Duration::from_secs(600))
            .expect("runtime audit");

        let audit_dir = team_dir.join("autoresearch_audits");
        let entries = fs::read_dir(&audit_dir)
            .expect("audit dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("entries");
        assert_eq!(entries.len(), 1);
        let report = fs::read_to_string(entries[0].path()).expect("report");
        assert!(report.contains("# Autoresearch Audit"));
        assert!(report.contains("| theme |"));

        let lead_mailbox =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_mailbox.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "autoresearch_runtime_audit_written"
                && event
                    .data
                    .get("note")
                    .and_then(|value| value.as_str())
                    .is_some_and(|note| note.contains("no team mailbox steer"))
        }));
    }

    #[test]
    fn team_prompts_require_external_template_compliance() {
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Online,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let research = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Online,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-template-policy".to_string(),
            goal: "Use /home/yukimaru/research_prompt/phase0.md before phase1.md.".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), research.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let task = TeamTask {
            id: "1".to_string(),
            subject: "phase0 scan".to_string(),
            description: "Run Fixed-4 + Flexible-2 scans from phase0.md.".to_string(),
            owner: Some("research".to_string()),
            status: TaskStatus::InProgress,
            depends_on: Vec::new(),
            result: None,
            created_at: now.clone(),
            updated_at: now,
        };

        let worker_prompt = build_worker_prompt(&config, std::slice::from_ref(&task), &research);
        assert!(worker_prompt.contains("External prompt/template compliance policy"));
        assert!(worker_prompt.contains("planning document that lists the prompts is not the same"));
        assert!(worker_prompt.contains("compact checklist mapping each template requirement"));
        assert!(worker_prompt.contains("URLs and remembered summaries are not enough"));
        assert!(
            worker_prompt.contains("do not work around that by switching the task to `review`")
        );

        let lead_prompt = build_app_server_lead_prompt(
            &config,
            &[task],
            &lead,
            Path::new("/tmp/codex"),
            TeamPromptLanguage::En,
        );
        assert!(lead_prompt.contains("External prompt/template compliance policy"));
        assert!(lead_prompt.contains("meta-plan describing those prompts is not a substitute"));
        assert!(lead_prompt.contains("create a compliance matrix"));
        assert!(lead_prompt.contains("require more than a URL list"));
        assert!(lead_prompt.contains("Completion rejection policy"));
        assert!(lead_prompt.contains("team autoresearch-audit --team"));
        assert!(lead_prompt.contains("Treat WARN/FAIL rows as real repair inputs"));
    }

    fn write_test_job(
        team_dir: &Path,
        id: &str,
        owner: Option<&str>,
        task_id: Option<&str>,
        status: TeamJobStatus,
        created_at: &str,
    ) {
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_json_atomic(
            &job_path(team_dir, id),
            &TeamJob {
                id: id.to_string(),
                node: "local".to_string(),
                command: "true".to_string(),
                cwd: team_dir.display().to_string(),
                owner: owner.map(str::to_string),
                task_id: task_id.map(str::to_string),
                status,
                pid: None,
                log_path: team_dir.join(format!("{id}.log")).display().to_string(),
                exit_path: team_dir.join(format!("{id}.exit")).display().to_string(),
                exit_code: None,
                note: String::new(),
                artifacts: Vec::new(),
                created_at: created_at.to_string(),
                updated_at: created_at.to_string(),
            },
        )
        .expect("write job");
    }

    fn write_test_wait(
        team_dir: &Path,
        id: &str,
        owner: Option<&str>,
        task_id: Option<&str>,
        status: TeamWaitStatus,
    ) {
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        let now = now();
        write_json_atomic(
            &wait_path(team_dir, id),
            &TeamWait {
                id: id.to_string(),
                title: format!("wait {id}"),
                owner: owner.map(str::to_string),
                task_id: task_id.map(str::to_string),
                node: None,
                condition: "condition becomes true".to_string(),
                status,
                progress: "still waiting".to_string(),
                evidence: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write wait");
    }

    #[test]
    fn completed_job_artifact_revives_blocked_owner_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("config");
        config.language = Some(TeamPromptLanguage::Ja);
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            Vec::new(),
            Some("Job completed before artifact registration."),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        write_test_job(
            team_dir,
            "job-1",
            Some("engineering"),
            Some("2"),
            TeamJobStatus::Completed,
            &old,
        );

        add_job_artifact(
            team_dir,
            JobArtifactArgs {
                id: "job-1".to_string(),
                path: "/tmp/report.txt".to_string(),
                note: "remote evidence".to_string(),
            },
        )
        .expect("add artifact");

        let task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("/tmp/report.txt"))
        );
        let config = load_config(team_dir).expect("config");
        let engineering = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(engineering.status, MemberStatus::Online);
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        assert!(engineering_messages.iter().any(|message| {
            message.message.contains("JOB_STATUS")
                && message.message.contains("確認してください")
                && message.message.contains("TEAM_COMPLETION_CHECKLIST")
        }));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "job_artifact_requires_owner_handoff")
        );
    }

    #[test]
    fn open_wait_blocks_task_completion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "7",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_wait(
            team_dir,
            "wait-1",
            Some("engineering"),
            Some("7"),
            TeamWaitStatus::Polling,
        );

        let changed = set_task_status_if_open(team_dir, "7", TaskStatus::Completed, Some("done"))
            .expect("set task");

        assert!(changed);
        let task = read_json::<TeamTask>(&task_path(team_dir, "7")).expect("task");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("open wait item"))
        );
    }

    #[test]
    fn wait_add_preserves_in_progress_task_status() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "7",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("working"),
        );

        add_team_wait(
            team_dir,
            WaitAddArgs {
                title: "handoff package".to_string(),
                owner: Some("engineering".to_string()),
                task: Some("7".to_string()),
                node: None,
                condition: "final handoff appears".to_string(),
                status: TeamWaitStatus::Waiting,
                progress: "owner is preparing artifacts".to_string(),
                evidence: Some("/tmp/final_handoff.md".to_string()),
            },
        )
        .expect("add wait");

        let task = read_json::<TeamTask>(&task_path(team_dir, "7")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("working") && result.contains("Waiting on `wait-1`")
        }));
        let wait = read_json::<TeamWait>(&wait_path(team_dir, "wait-1")).expect("wait");
        assert_eq!(wait.status, TeamWaitStatus::Waiting);

        auto_promote_dependency_waits(team_dir).expect("auto promote");
        let task = read_json::<TeamTask>(&task_path(team_dir, "7")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
    }

    #[test]
    fn wait_add_uses_state_lock_for_unique_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path().to_path_buf();
        write_test_config(&team_dir);

        let mut handles = Vec::new();
        for i in 0..8 {
            let team_dir = team_dir.clone();
            handles.push(std::thread::spawn(move || {
                add_team_wait(
                    &team_dir,
                    WaitAddArgs {
                        title: format!("concurrent wait {i}"),
                        owner: Some("lead".to_string()),
                        task: None,
                        node: None,
                        condition: "unique id".to_string(),
                        status: TeamWaitStatus::Waiting,
                        progress: String::new(),
                        evidence: None,
                    },
                )
                .expect("add wait");
            }));
        }
        for handle in handles {
            handle.join().expect("join");
        }

        let waits = load_waits(&team_dir).expect("waits");
        let ids = waits
            .iter()
            .map(|wait| wait.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(waits.len(), 8);
        assert_eq!(ids.len(), 8);
        assert!(ids.contains("wait-1"));
        assert!(ids.contains("wait-8"));
    }

    #[test]
    fn wait_completion_resumes_owner_for_handoff() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "8",
            Some("engineering"),
            TaskStatus::Waiting,
            Vec::new(),
            Some("Waiting on wait-1"),
        );
        write_test_wait(
            team_dir,
            "wait-1",
            Some("engineering"),
            Some("8"),
            TeamWaitStatus::Polling,
        );

        set_team_wait(
            team_dir,
            WaitSetArgs {
                id: "wait-1".to_string(),
                status: Some(TeamWaitStatus::Completed),
                progress: Some("finished".to_string()),
                evidence: Some("/tmp/result.json".to_string()),
                clear_evidence: false,
            },
        )
        .expect("set wait");

        let task = read_json::<TeamTask>(&task_path(team_dir, "8")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        let config = load_config(team_dir).expect("config");
        let engineering = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(engineering.status, MemberStatus::Online);
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("messages");
        assert!(messages.iter().any(|message| {
            message.message.contains("WAIT_STATUS") && message.message.contains("wait-1")
        }));
    }

    #[test]
    fn wait_auto_check_completes_file_and_log_conditions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "8",
            Some("engineering"),
            TaskStatus::Waiting,
            Vec::new(),
            Some("Waiting on wait-1"),
        );
        let evidence = team_dir.join("handoff.md");
        let log = team_dir.join("pytest.log");
        fs::write(&evidence, "handoff ready").expect("write evidence");
        fs::write(&log, "7 passed\nrc=0\n").expect("write log");
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        let now = now();
        write_json_atomic(
            &wait_path(team_dir, "wait-1"),
            &TeamWait {
                id: "wait-1".to_string(),
                title: "runtime evidence".to_string(),
                owner: Some("engineering".to_string()),
                task_id: Some("8".to_string()),
                node: None,
                condition: format!(
                    "AUTO_CHECK file_exists {}\nAUTO_CHECK log_contains {} :: 7 passed",
                    evidence.display(),
                    log.display()
                ),
                status: TeamWaitStatus::Polling,
                progress: "waiting for machine evidence".to_string(),
                evidence: Some(evidence.display().to_string()),
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write wait");

        let completed = auto_complete_wait_checks(team_dir).expect("auto checks");
        assert_eq!(completed, vec!["wait-1".to_string()]);
        let wait = read_json::<TeamWait>(&wait_path(team_dir, "wait-1")).expect("wait");
        assert_eq!(wait.status, TeamWaitStatus::Completed);
        assert!(wait.progress.contains("auto_completed"));
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("messages");
        assert!(
            messages
                .iter()
                .any(|message| message.message.contains("WAIT_STATUS"))
        );
    }

    #[test]
    fn wait_auto_check_leaves_wait_open_when_check_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        let now = now();
        write_json_atomic(
            &wait_path(team_dir, "wait-1"),
            &TeamWait {
                id: "wait-1".to_string(),
                title: "runtime evidence".to_string(),
                owner: Some("engineering".to_string()),
                task_id: None,
                node: None,
                condition: format!(
                    "AUTO_CHECK file_exists {}",
                    team_dir.join("missing.txt").display()
                ),
                status: TeamWaitStatus::Polling,
                progress: String::new(),
                evidence: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write wait");

        let completed = auto_complete_wait_checks(team_dir).expect("auto checks");
        assert!(completed.is_empty());
        let wait = read_json::<TeamWait>(&wait_path(team_dir, "wait-1")).expect("wait");
        assert_eq!(wait.status, TeamWaitStatus::Polling);
    }

    #[test]
    fn external_wait_cannot_fail_without_terminal_evidence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "9",
            Some("engineering"),
            TaskStatus::Waiting,
            Vec::new(),
            Some("Waiting on deep_thinker"),
        );
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        let now = now();
        write_json_atomic(
            &wait_path(team_dir, "wait-1"),
            &TeamWait {
                id: "wait-1".to_string(),
                title: "deep_thinker_strategy".to_string(),
                owner: Some("engineering".to_string()),
                task_id: Some("9".to_string()),
                node: None,
                condition: "MCP deep_thinker returns a PoC strategy".to_string(),
                status: TeamWaitStatus::Running,
                progress: "polling external tool".to_string(),
                evidence: Some("/tmp/missing-deep-thinker-result.md".to_string()),
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write wait");

        let err = set_team_wait(
            team_dir,
            WaitSetArgs {
                id: "wait-1".to_string(),
                status: Some(TeamWaitStatus::Failed),
                progress: Some("no usable response yet".to_string()),
                evidence: None,
                clear_evidence: false,
            },
        )
        .expect_err("missing terminal evidence should be rejected");

        assert!(
            err.to_string()
                .contains("refusing to mark external wait `wait-1` as failed")
        );
        let wait = read_json::<TeamWait>(&wait_path(team_dir, "wait-1")).expect("wait");
        assert_eq!(wait.status, TeamWaitStatus::Running);
    }

    #[test]
    fn external_wait_can_fail_with_terminal_progress_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "9",
            Some("engineering"),
            TaskStatus::Waiting,
            Vec::new(),
            Some("Waiting on deep_thinker"),
        );
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        let now = now();
        write_json_atomic(
            &wait_path(team_dir, "wait-1"),
            &TeamWait {
                id: "wait-1".to_string(),
                title: "deep_thinker_strategy".to_string(),
                owner: Some("engineering".to_string()),
                task_id: Some("9".to_string()),
                node: None,
                condition: "MCP deep_thinker returns a PoC strategy".to_string(),
                status: TeamWaitStatus::Running,
                progress: "polling external tool".to_string(),
                evidence: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write wait");

        set_team_wait(
            team_dir,
            WaitSetArgs {
                id: "wait-1".to_string(),
                status: Some(TeamWaitStatus::Failed),
                progress: Some("terminal_failure: MCP returned a final error".to_string()),
                evidence: None,
                clear_evidence: false,
            },
        )
        .expect("terminal marker permits failure");

        let wait = read_json::<TeamWait>(&wait_path(team_dir, "wait-1")).expect("wait");
        assert_eq!(wait.status, TeamWaitStatus::Failed);
    }

    #[test]
    fn task_watchdog_revives_blocked_task_with_existing_completed_job_artifact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            Vec::new(),
            Some("Waiting for artifact handoff."),
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("task");
        task.updated_at = old.clone();
        write_json_atomic(&task_path(team_dir, "2"), &task).expect("write task");
        write_test_job(
            team_dir,
            "job-1",
            Some("engineering"),
            Some("2"),
            TeamJobStatus::Completed,
            &old,
        );
        let mut job = load_job(team_dir, "job-1").expect("job");
        job.artifacts.push(TeamArtifact {
            path: "/tmp/report.txt".to_string(),
            note: "remote evidence".to_string(),
            created_at: old,
        });
        write_json_atomic(&job_path(team_dir, "job-1"), &job).expect("write job");

        let config = load_config(team_dir).expect("config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        let config = load_config(team_dir).expect("config");
        let engineering = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(engineering.status, MemberStatus::Online);
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "task_watchdog_completed_artifact_revival"
                && event.data.get("job").and_then(|value| value.as_str()) == Some("job-1")
        }));
    }

    #[test]
    fn task_status_counts_separate_completed_open_cancelled_and_failed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::Completed,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "3",
            Some("quality"),
            TaskStatus::Cancelled,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "4",
            Some("quality"),
            TaskStatus::Failed,
            Vec::new(),
            None,
        );

        let tasks = load_tasks(team_dir).expect("tasks");

        assert_eq!(
            format_task_status_counts(&tasks),
            "1 completed, 1 open, 1 cancelled, 1 failed, 4 total"
        );
    }

    #[test]
    fn failed_tasks_are_not_open_for_member_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::Failed,
            Vec::new(),
            None,
        );
        let tasks = load_tasks(team_dir).expect("tasks");

        assert!(!task_is_open(&tasks[0]));
        assert_eq!(
            member_task_status_summary(&tasks, "engineering"),
            "no_open_tasks"
        );
    }

    #[test]
    fn status_text_includes_node_last_seen_timestamp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_nodes(
            team_dir,
            &[TeamNode {
                id: "remote".to_string(),
                kind: TeamNodeKind::Ssh,
                url: Some("ws://127.0.0.1:9999".to_string()),
                host: Some("remote-host".to_string()),
                container: None,
                cwd: Some("/work".to_string()),
                status: TeamNodeStatus::Online,
                note: String::new(),
                created_at: "2026-05-01T00:00:00Z".to_string(),
                updated_at: "2026-05-08T06:41:31Z".to_string(),
            }],
        )
        .expect("write nodes");

        let status = format_status_text(team_dir).expect("status");

        assert!(status.contains(
            "remote Ssh Stale(raw=Online) url=ws://127.0.0.1:9999 last_seen=2026-05-08T06:41:31Z age="
        ));
        assert!(status.contains(" stale"));
    }

    #[test]
    fn connected_node_heartbeat_refreshes_last_seen_and_online_status() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_nodes(
            team_dir,
            &[TeamNode {
                id: "remote".to_string(),
                kind: TeamNodeKind::Ssh,
                url: Some("ws://127.0.0.1:9999".to_string()),
                host: Some("remote-host".to_string()),
                container: None,
                cwd: Some("/work".to_string()),
                status: TeamNodeStatus::Offline,
                note: String::new(),
                created_at: "2026-05-01T00:00:00Z".to_string(),
                updated_at: "2026-05-08T06:41:31Z".to_string(),
            }],
        )
        .expect("write nodes");

        heartbeat_connected_node_ids(team_dir, ["remote"]).expect("heartbeat");
        let nodes = load_nodes(team_dir).expect("nodes");
        let remote = nodes.iter().find(|node| node.id == "remote").unwrap();

        assert_eq!(remote.status, TeamNodeStatus::Online);
        assert_ne!(remote.updated_at, "2026-05-08T06:41:31Z");
        assert!(remote.updated_at.contains("+09:00") || remote.updated_at.ends_with('Z'));
    }

    #[test]
    fn local_node_is_not_blocked_by_node_availability_gate() {
        let nodes = vec![TeamNode {
            id: "local".to_string(),
            kind: TeamNodeKind::Local,
            url: Some("ws://127.0.0.1:9999".to_string()),
            host: None,
            container: None,
            cwd: None,
            status: TeamNodeStatus::Offline,
            note: String::new(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            updated_at: "2026-05-08T06:41:31Z".to_string(),
        }];

        assert!(node_unavailable_from_nodes("local", &nodes).is_none());
    }

    #[test]
    fn status_text_includes_member_unread_mail_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "user".to_string(),
                to: "lead".to_string(),
                message: "continue the loop".to_string(),
                timestamp: now(),
                read: false,
            },
        )
        .expect("unread message");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "system".to_string(),
                to: "lead".to_string(),
                message: "old tick".to_string(),
                timestamp: now(),
                read: true,
            },
        )
        .expect("read message");

        let status = format_status_text(team_dir).expect("status");

        assert!(status.contains(
            "lead (lead) session=Online tasks=no_open_tasks node=local unread=1 direct=1"
        ));
        assert!(status.contains(
            "engineering (engineering) session=Standby tasks=no_open_tasks node=local unread=0 direct=0"
        ));
    }

    #[test]
    fn compact_duration_formats_status_ages() {
        assert_eq!(format_compact_duration(42), "42s");
        assert_eq!(format_compact_duration(125), "2m5s");
        assert_eq!(format_compact_duration(3_900), "1h5m");
        assert_eq!(format_compact_duration(176_400), "2d1h");
    }

    #[test]
    fn status_text_includes_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let retry_at = (Local::now() + chrono::Duration::hours(2))
            .format("%B %-d, %Y %-I:%M %p")
            .to_string();
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "lead",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": format!("You've hit your usage limit. Visit settings or try again at {retry_at}."),
                "retry_after_sec": 2700,
            }),
        )
        .expect("usage event");

        let status = format_status_text(team_dir).expect("status");

        assert!(status.contains("Cooldowns:\n"));
        assert!(status.contains("lead usage_limit retry_in="));
    }

    #[test]
    fn status_text_warns_when_runtime_stopped_with_open_work() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);
        write_test_task(
            &team_dir,
            "1",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        fs::write(team_run_pid_path(&team_dir), "999999\n").expect("run pid");

        let status = format_status_text(&team_dir).expect("status");

        assert!(status.contains("Runtime: exiting run_pid=999999(dead)"));
        assert!(status.contains("open_tasks=1"));
        assert!(status.contains("Runtime warning: open work remains"));
        assert!(status.contains(
            "codex team resume --team team-task-test --dangerously-bypass-approvals-and-sandbox"
        ));
    }

    #[test]
    fn message_cli_allows_reserved_system_sender() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);

        send_message(
            root,
            MessageArgs {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                from: Some("system".to_string()),
                to: "lead".to_string(),
                message: "gate correction".to_string(),
            },
        )
        .expect("send system message");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(&team_dir, "lead")).expect("lead mailbox");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "system");
        assert_eq!(messages[0].message, "gate correction");
    }

    #[test]
    fn message_cli_allows_comma_separated_recipients() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);

        send_message(
            root,
            MessageArgs {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                from: Some("lead".to_string()),
                to: "engineering, quality,engineering".to_string(),
                message: "please coordinate".to_string(),
            },
        )
        .expect("send message");

        let engineering =
            read_jsonl::<MailMessage>(&mailbox_path(&team_dir, "engineering")).expect("mailbox");
        let quality =
            read_jsonl::<MailMessage>(&mailbox_path(&team_dir, "quality")).expect("mailbox");
        assert_eq!(engineering.len(), 1);
        assert_eq!(quality.len(), 1);
        assert_eq!(engineering[0].message, "please coordinate");
        assert_eq!(quality[0].message, "please coordinate");

        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        let event = events
            .iter()
            .find(|event| event.event == "message_sent")
            .expect("message_sent event");
        assert_eq!(
            event.data.get("to").expect("to"),
            &serde_json::json!(["engineering", "quality"])
        );
    }

    #[test]
    fn message_cli_rejects_all_mixed_with_explicit_recipient() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);

        let err = send_message(
            root,
            MessageArgs {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                from: Some("lead".to_string()),
                to: "all,quality".to_string(),
                message: "ambiguous".to_string(),
            },
        )
        .expect_err("mixed all should fail");

        assert!(
            err.to_string()
                .contains("recipient `all` cannot be combined")
        );
    }

    #[test]
    fn job_list_filters_by_owner_task_status_and_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_job(
            team_dir,
            "job-old",
            Some("engineering"),
            Some("34"),
            TeamJobStatus::Completed,
            "2026-05-08T08:00:00Z",
        );
        write_test_job(
            team_dir,
            "job-active",
            Some("engineering"),
            Some("34"),
            TeamJobStatus::Running,
            "2026-05-08T08:10:00Z",
        );
        write_test_job(
            team_dir,
            "job-other-task",
            Some("engineering"),
            Some("35"),
            TeamJobStatus::Running,
            "2026-05-08T08:20:00Z",
        );
        write_test_job(
            team_dir,
            "job-other-owner",
            Some("quality"),
            Some("34"),
            TeamJobStatus::Running,
            "2026-05-08T08:30:00Z",
        );

        let output = format_jobs_text_filtered(
            team_dir,
            &JobListArgs {
                owner: Some("engineering".to_string()),
                task: Some("34".to_string()),
                status: Some(TeamJobStatus::Running),
                limit: Some(1),
            },
        )
        .expect("format jobs");

        assert!(output.contains("job-active"));
        assert!(!output.contains("job-old"));
        assert!(!output.contains("job-other-task"));
        assert!(!output.contains("job-other-owner"));
    }

    #[test]
    fn remote_bootstrap_installs_codex_team_wrapper_on_path() {
        let script = remote_app_server_bootstrap_script(
            "team-test",
            "http://127.0.0.1:12345",
            "ws://127.0.0.1:23456",
        );

        assert!(script.contains("$HOME/bin/.codex-team-real"));
        assert!(script.contains("export CODEX_TEAM_ID="));
        assert!(script.contains("CODEX_TEAM_ID:-"));
        assert!(script.contains("team-test"));
        assert!(script.contains("export CODEX_TEAM_RELAY_URL="));
        assert!(script.contains("CODEX_TEAM_RELAY_URL:-"));
        assert!(script.contains("http://127.0.0.1:12345"));
        assert!(
            script.contains("install -m 0755 \"$helper_real\" /usr/local/bin/.codex-team-real")
        );
        assert!(
            script.contains("install -m 0755 \"$HOME/bin/codex-team\" /usr/local/bin/codex-team")
        );
        assert!(script.contains("export PATH=\"$HOME/bin:/usr/local/bin:/root/bin:$PATH\""));
        assert!(script.contains("CODEX_TEAM_HELPER_TIMEOUT:-30s"));
        assert!(script.contains("$CODEX_TEAM_RELAY_URL/wait/list"));
        assert!(script.contains("$CODEX_TEAM_RELAY_URL/wait/add"));
        assert!(script.contains("$CODEX_TEAM_RELAY_URL/wait/set"));
    }

    #[test]
    fn codex_device_auth_parser_extracts_url_and_code() {
        let log =
            "Open https://auth.openai.com/codex/device and enter this one-time code: ABCD-EFGHI";
        let parsed = parse_codex_device_auth_from_log(log)
            .expect("parse")
            .expect("auth");
        assert_eq!(parsed.0, "https://auth.openai.com/codex/device");
        assert_eq!(parsed.1, "ABCDEFGHI");

        let default_url = parse_codex_device_auth_from_log("device code is ZYXW98765")
            .expect("parse")
            .expect("auth");
        assert_eq!(default_url.0, "https://auth.openai.com/codex/device");
        assert_eq!(default_url.1, "ZYXW98765");
    }

    #[test]
    fn auth_browser_helpers_parse_endpoint_and_code() {
        let log = "noise\nDevTools listening on ws://127.0.0.1:35833/devtools/browser/abc\n";
        let ws = parse_auth_browser_ws_from_log(log)
            .expect("parse")
            .expect("ws");
        assert_eq!(ws, "ws://127.0.0.1:35833/devtools/browser/abc".to_string());
        assert_eq!(
            cdp_http_from_ws(&ws).expect("http"),
            "http://127.0.0.1:35833".to_string()
        );
        assert_eq!(
            normalize_codex_device_code("abcd-efghi").expect("code"),
            "ABCDEFGHI".to_string()
        );
    }

    #[test]
    fn dependency_soft_wait_auto_promotes_to_ready_and_notifies_owner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            vec!["1"],
            None,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert_eq!(promoted.len(), 1);
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Ready);
        assert_eq!(task.owner.as_deref(), Some("engineering"));
        assert_eq!(task.depends_on, vec!["1".to_string()]);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("Dependency gate cleared automatically"))
        );
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].message.contains("READY_TO_START: task 2"));
    }

    #[test]
    fn dependency_auto_promote_does_not_ready_task_with_open_wait() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            vec!["1"],
            Some("dependency task 1 is complete, but wait-1 is still open"),
        );
        write_test_wait(
            team_dir,
            "wait-1",
            Some("engineering"),
            Some("2"),
            TeamWaitStatus::Waiting,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("wait-1") && result.contains("Do not READY_TO_START")
        }));
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].message.contains("WAIT_STILL_OPEN: task 2"));
        assert!(!messages[0].message.contains("READY_TO_START"));
    }

    #[test]
    fn ready_dependency_task_with_open_wait_is_demoted_to_waiting() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Ready,
            vec!["1"],
            Some("premature ready"),
        );
        write_test_wait(
            team_dir,
            "wait-1",
            Some("engineering"),
            Some("2"),
            TeamWaitStatus::Polling,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("wait-1") && result.contains("Do not READY_TO_START")
        }));
    }

    #[test]
    fn cancelled_duplicate_wait_preserves_task_when_other_wait_is_open() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("runtime is producing artifacts"),
        );
        write_test_wait(
            team_dir,
            "wait-duplicate",
            Some("engineering"),
            Some("2"),
            TeamWaitStatus::Cancelled,
        );
        write_test_wait(
            team_dir,
            "wait-canonical",
            Some("engineering"),
            Some("2"),
            TeamWaitStatus::Running,
        );
        let wait = read_json::<TeamWait>(&wait_path(team_dir, "wait-duplicate")).expect("wait");

        handle_wait_status_change(team_dir, &wait, TeamWaitStatus::Waiting).expect("handle wait");

        let task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("wait-duplicate") && result.contains("obsolete/duplicate")
        }));
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert!(messages.iter().any(|message| {
            message.message.contains("wait `wait-duplicate`")
                && message.message.contains("cancelled")
        }));
    }

    #[test]
    fn task_add_records_created_before_auto_unblocked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);
        write_test_task(
            &team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );

        run_task(
            root,
            TaskCli {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                subcommand: TaskSubcommand::Add(TaskAddArgs {
                    subject: "dependent task".to_string(),
                    description: String::new(),
                    owner: Some("engineering".to_string()),
                    depends_on: vec!["1".to_string()],
                }),
            },
        )
        .expect("task add");

        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        let task_created_index = events
            .iter()
            .position(|event| event.event == "task_created")
            .expect("task_created event");
        let task_unblocked_index = events
            .iter()
            .position(|event| event.event == "task_dependency_unblocked")
            .expect("task_dependency_unblocked event");
        assert!(
            task_created_index < task_unblocked_index,
            "task_created should be recorded before dependency unblocking"
        );
    }

    #[test]
    fn task_add_reuses_similar_open_task_with_same_owner_and_dependencies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);
        write_test_task(
            &team_dir,
            "36",
            Some("quality"),
            TaskStatus::Completed,
            Vec::new(),
            Some("evaluation complete"),
        );
        write_test_task(
            &team_dir,
            "37",
            Some("quality"),
            TaskStatus::Completed,
            Vec::new(),
            Some("audit complete"),
        );
        let now = now();
        write_json_atomic(
            &task_path(&team_dir, "38"),
            &TeamTask {
                id: "38".to_string(),
                subject: "post task35 preparation review reconciliation and next gate decision"
                    .to_string(),
                description: String::new(),
                owner: Some("engineering".to_string()),
                status: TaskStatus::Waiting,
                depends_on: vec!["36".to_string(), "37".to_string()],
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task 38");

        run_task(
            root,
            TaskCli {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                subcommand: TaskSubcommand::Add(TaskAddArgs {
                    subject: "task35 preparation preflight review reconciliation".to_string(),
                    description: String::new(),
                    owner: Some("engineering".to_string()),
                    depends_on: vec!["36".to_string(), "37".to_string()],
                }),
            },
        )
        .expect("task add should reuse");

        let tasks = load_tasks(&team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 3);
        assert!(tasks.iter().any(|task| task.id == "38"));
        assert!(!task_path(&team_dir, "39").exists());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "task_add_reused_similar_open_task")
        );
    }

    #[test]
    fn pending_dependency_task_soft_waits_until_dependencies_complete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Pending,
            vec!["1"],
            None,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("Soft-waiting"))
        );
    }

    #[test]
    fn task_set_replaces_dependencies_and_waits_when_gate_incomplete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );

        update_task(
            team_dir,
            TaskSetArgs {
                id: "2".to_string(),
                status: None,
                owner: None,
                clear_owner: false,
                depends_on: vec!["1".to_string()],
                clear_depends: false,
                result: None,
            },
        )
        .expect("update dependency");

        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.depends_on, vec!["1".to_string()]);
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("Waiting for dependency task(s): 1"))
        );
    }

    #[test]
    fn task_add_rejects_self_dependency() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);

        let err = create_task(
            team_dir,
            TaskAddArgs {
                subject: "bad task".to_string(),
                description: String::new(),
                owner: Some("engineering".to_string()),
                depends_on: vec!["1".to_string()],
            },
        )
        .expect_err("self dependency should be rejected");

        assert!(err.to_string().contains("task 1 cannot depend on itself"));
        assert!(!task_path(team_dir, "1").exists());
    }

    #[test]
    fn task_add_rejects_unknown_dependency() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(team_dir, "1", None, TaskStatus::Completed, Vec::new(), None);

        let err = create_task(
            team_dir,
            TaskAddArgs {
                subject: "bad future dependency".to_string(),
                description: String::new(),
                owner: Some("engineering".to_string()),
                depends_on: vec!["3".to_string()],
            },
        )
        .expect_err("unknown dependency should be rejected");

        assert!(err.to_string().contains("dependency task 3 not found"));
        assert!(!task_path(team_dir, "2").exists());
    }

    #[test]
    fn dependency_auto_promote_preserves_hard_block_reason() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            vec!["1"],
            Some("Waiting for user decision."),
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert_eq!(promoted.len(), 0);
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert_eq!(task.result.as_deref(), Some("Waiting for user decision."));
    }

    #[test]
    fn dependency_auto_promote_preserves_manual_reopen_wait() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "51",
            Some("method_schema"),
            TaskStatus::Completed,
            Vec::new(),
            Some("method handoff complete"),
        );
        write_test_task(
            team_dir,
            "52",
            Some("runtime_cycle2"),
            TaskStatus::Waiting,
            vec!["51"],
            Some(
                "Await lead sync/verification of input and method package, then explicit reopen. Runtime must not execute from READY_TO_START alone.",
            ),
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "52").expect("task 52");
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("explicit reopen") && !result.contains("Dependency gate cleared")
        }));
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "runtime_cycle2")).expect("mailbox");
        assert!(messages.is_empty());
    }

    #[test]
    fn dependency_auto_promote_holds_remote_contract_inputs_for_lead_clearance() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("load config");
        let engineering = config
            .members
            .iter_mut()
            .find(|member| member.name == "engineering")
            .expect("engineering member");
        engineering.status = MemberStatus::Online;
        engineering.node = Some("remote-runtime".to_string());
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let contract_dir = team_dir.join("method_schema").join("cycle23");
        fs::create_dir_all(&contract_dir).expect("contract dir");
        fs::write(
            contract_dir.join("runtime_contract.yaml"),
            format!(
                r#"runtime_task: 2
method_package:
  host_path: {}
  expected_container_input_root: /workspace/inputs/method_schema/cycle23
"#,
                contract_dir.display()
            ),
        )
        .expect("contract");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: contract_dir.display().to_string(),
                owner: "method_schema".to_string(),
                note: "Task1 Cycle23 contract".to_string(),
                updated_at: now(),
            }],
        )
        .expect("ownerships");
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Waiting,
            vec!["1"],
            Some("Waiting for dependency task(s): 1"),
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("contract-declared inputs")
                && result.contains("explicit lead root-correct verification clearance")
                && !result.contains("Dependency gate cleared automatically")
        }));
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(engineering_messages.len(), 1);
        assert_eq!(lead_messages.len(), 1);
        assert!(
            engineering_messages[0]
                .message
                .contains("AWAITING_LEAD_CLEARANCE: task 2")
        );
        assert!(!engineering_messages[0].message.contains("READY_TO_START"));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.event == "task_contract_input_clearance_required"
                        && event.data.get("task").is_some()
                })
                .count(),
            1
        );

        let promoted_again = auto_promote_dependency_waits(team_dir).expect("auto promote again");
        assert!(promoted_again.is_empty());
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox after second pass");
        let lead_messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead"))
            .expect("lead mailbox after second pass");
        assert_eq!(engineering_messages.len(), 1);
        assert_eq!(lead_messages.len(), 1);
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "task_contract_input_clearance_required")
                .count(),
            1
        );
    }

    #[test]
    fn claim_ready_task_self_assigns_first_unowned_ready_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(team_dir, "1", None, TaskStatus::Pending, Vec::new(), None);

        claim_ready_task(
            team_dir,
            TaskClaimArgs {
                id: None,
                owner: Some("quality".to_string()),
            },
        )
        .expect("claim ready task");

        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "1").expect("task 1");
        assert_eq!(task.owner.as_deref(), Some("quality"));
        assert_eq!(task.status, TaskStatus::InProgress);
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert!(lead_messages[0].message.contains("@quality claimed task 1"));
    }

    #[test]
    fn round_robin_does_not_assign_unowned_ready_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::Ready,
            Vec::new(),
            Some("ready for claim"),
        );

        assign_unowned_tasks_round_robin(team_dir).expect("assign tasks");

        let task = read_json::<TeamTask>(&task_path(team_dir, "1")).expect("task");
        assert_eq!(task.owner, None);
        assert_eq!(task.status, TaskStatus::Ready);
    }

    #[test]
    fn round_robin_still_assigns_unowned_pending_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(team_dir, "1", None, TaskStatus::Pending, Vec::new(), None);

        assign_unowned_tasks_round_robin(team_dir).expect("assign tasks");

        let task = read_json::<TeamTask>(&task_path(team_dir, "1")).expect("task");
        assert_eq!(task.owner.as_deref(), Some("engineering"));
        assert_eq!(task.status, TaskStatus::Pending);
    }

    #[test]
    fn owned_pending_task_reactivates_completed_member() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("load config");
        let member = config
            .members
            .iter_mut()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        member.status = MemberStatus::Completed;
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::Pending,
            Vec::new(),
            None,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let config = load_config(team_dir).expect("reload config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(member.status, MemberStatus::Online);
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].message.contains("READY_TO_START: task 1"));
    }

    #[test]
    fn inactive_member_mailbox_poll_does_not_advance_seen_count() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        send_team_message_to_dir(team_dir, "lead", "engineering", "START_NOW: task 1")
            .expect("send message");
        let config = load_config(team_dir).expect("load config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        let mut mailbox_counts = HashMap::new();

        let inactive_messages =
            collect_new_active_mailbox_messages(team_dir, member, false, &mut mailbox_counts)
                .expect("inactive poll");

        assert!(inactive_messages.is_none());
        assert!(mailbox_counts.is_empty());

        let active_messages =
            collect_new_active_mailbox_messages(team_dir, member, true, &mut mailbox_counts)
                .expect("active poll")
                .expect("pending delivery");

        assert_eq!(active_messages.messages.len(), 1);
        assert!(mailbox_counts.is_empty());
        assert!(active_messages.messages[0].message.contains("START_NOW"));
    }

    #[test]
    fn mailbox_counts_resume_from_first_unread_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mailbox = mailbox_path(team_dir, "engineering");
        write_jsonl_atomic(
            &mailbox,
            &[
                MailMessage {
                    from: "lead".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "already handled".to_string(),
                    read: true,
                },
                MailMessage {
                    from: "lead".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "resume this after restart".to_string(),
                    read: false,
                },
            ],
        )
        .expect("write mailbox");
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let config = load_config(team_dir).expect("load config");
        let tasks = load_tasks(team_dir).expect("tasks");

        let counts = current_mailbox_counts(team_dir, &config.members, &tasks).expect("counts");

        assert_eq!(counts.get("engineering"), Some(&1));
    }

    #[test]
    fn mailbox_counts_compact_large_resume_backlog_for_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mailbox = mailbox_path(team_dir, "engineering");
        let messages = (0..30)
            .map(|idx| MailMessage {
                from: "lead".to_string(),
                to: "engineering".to_string(),
                timestamp: now(),
                message: format!("old unread backlog message {idx}"),
                read: false,
            })
            .collect::<Vec<_>>();
        write_jsonl_atomic(&mailbox, &messages).expect("write mailbox");
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let config = load_config(team_dir).expect("load config");
        let tasks = load_tasks(team_dir).expect("tasks");

        let counts = current_mailbox_counts(team_dir, &config.members, &tasks).expect("counts");

        let compacted_until = 30 - MAX_RUNTIME_START_UNREAD_MAILBOX_TAIL_MESSAGES;
        assert_eq!(counts.get("engineering"), Some(&compacted_until));
        let compacted =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert_eq!(compacted.len(), 31);
        assert!(
            compacted
                .iter()
                .take(compacted_until)
                .all(|message| message.read)
        );
        assert!(
            compacted
                .iter()
                .skip(compacted_until)
                .all(|message| !message.read)
        );
        assert!(
            compacted
                .last()
                .expect("summary")
                .message
                .contains("Mailbox resume compaction")
        );
        let mut mailbox_counts = counts;
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("member");
        let pending =
            collect_new_active_mailbox_messages(team_dir, member, true, &mut mailbox_counts)
                .expect("collect")
                .expect("pending");
        assert_eq!(
            pending.messages.len(),
            MAX_RUNTIME_START_UNREAD_MAILBOX_TAIL_MESSAGES + 1
        );
    }

    #[test]
    fn mailbox_counts_skip_old_worker_mail_when_no_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mailbox = mailbox_path(team_dir, "engineering");
        write_jsonl_atomic(
            &mailbox,
            &[
                MailMessage {
                    from: "lead".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "old handoff".to_string(),
                    read: false,
                },
                MailMessage {
                    from: "system".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "old heartbeat".to_string(),
                    read: false,
                },
            ],
        )
        .expect("write mailbox");
        let config = load_config(team_dir).expect("load config");
        let tasks = load_tasks(team_dir).expect("tasks");

        let counts = current_mailbox_counts(team_dir, &config.members, &tasks).expect("counts");

        assert_eq!(counts.get("engineering"), Some(&2));
        assert_eq!(counts.get("lead"), Some(&0));
    }

    #[test]
    fn next_action_signals_include_owned_final_audit_recommendation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let audit_dir = team_dir.join("audit");
        fs::create_dir_all(&audit_dir).expect("audit dir");
        fs::write(
            audit_dir.join("cycle_final_audit.md"),
            "Verdict: PASS_WITH_WARNINGS\n\nRecommended next action: fix the runtime dataset layout and rerun the smoke test.\n",
        )
        .expect("write audit");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: audit_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "audit artifacts".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");

        let signals =
            collect_recent_next_action_signals(team_dir, 4).expect("collect next actions");

        assert_eq!(signals.len(), 1);
        assert!(signals[0].contains("cycle_final_audit.md"));
        assert!(signals[0].contains("fix the runtime dataset layout"));
    }

    #[test]
    fn next_action_signals_ignore_live_message_transcripts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let live_dir = team_dir.join("live_messages");
        fs::create_dir_all(&live_dir).expect("live dir");
        fs::write(
            live_dir.join("audit.side.md"),
            "Next action: stale side-channel conversation should not drive lead ticks.\n",
        )
        .expect("write side transcript");

        let audit_dir = team_dir.join("audit");
        fs::create_dir_all(&audit_dir).expect("audit dir");
        fs::write(
            audit_dir.join("final_audit.md"),
            "Recommended next action: evaluate the fresh runtime package.\n",
        )
        .expect("write audit");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: audit_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "audit artifacts".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");

        let signals =
            collect_recent_next_action_signals(team_dir, 4).expect("collect next actions");

        assert_eq!(signals.len(), 1);
        assert!(signals[0].contains("final_audit.md"));
        assert!(!signals[0].contains("audit.side.md"));
    }

    #[test]
    fn lead_proposal_resolution_accepts_japanese_adoption_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_jsonl_atomic(
            &mailbox_path(team_dir, "lead"),
            &[
                MailMessage {
                    from: "quality".to_string(),
                    to: "lead".to_string(),
                    message:
                        "LEAD_PROPOSAL: task 7 の成果物を pull して評価 task を作ってください。"
                            .to_string(),
                    timestamp: "2026-05-13T16:18:01+09:00".to_string(),
                    read: false,
                },
                MailMessage {
                    from: "engineering".to_string(),
                    to: "lead".to_string(),
                    message: "LEAD_PROPOSAL: task 8 完了後に audit task を作ってください。"
                        .to_string(),
                    timestamp: "2026-05-13T16:18:03+09:00".to_string(),
                    read: false,
                },
            ],
        )
        .expect("write mailbox");
        let config = load_config(team_dir).expect("load config");
        append_jsonl(
            &team_dir.join("events.jsonl"),
            &Event {
                event: "message_sent",
                timestamp: "2026-05-13T16:18:02+09:00".to_string(),
                team: &config.id,
                data: serde_json::json!({
                "from": "lead",
                "to": ["evaluation"],
                "message": "LEAD_PROPOSAL は採用しました。task 8 と wait-11 を作成します。",
                "source": "team_relay",
                }),
            },
        )
        .expect("append resolution");

        let proposals =
            collect_recent_lead_proposals(team_dir, "lead", 4).expect("collect proposals");

        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].contains("@engineering"));
        assert!(!proposals[0].contains("@quality"));
    }

    #[test]
    fn remote_codex_dest_assignment_expands_home_on_remote() {
        let assignment = remote_codex_dest_assignment("$HOME/.codex");

        assert_eq!(assignment, "dest=\"${HOME:-/root}/.codex\"");
        assert!(!assignment.contains("'$HOME/.codex'"));
    }

    #[test]
    fn remote_codex_dest_assignment_quotes_literal_paths() {
        let assignment = remote_codex_dest_assignment("/root/.codex");

        assert_eq!(assignment, "dest='/root/.codex'");
    }

    #[test]
    fn remote_path_dest_assignment_expands_home_on_remote() {
        let assignment = remote_path_dest_assignment("$HOME/project/schema");

        assert_eq!(assignment, "dest=\"${HOME:-/root}/project/schema\"");
        assert!(!assignment.contains("'$HOME/project/schema'"));
    }

    #[test]
    fn path_sync_command_uses_node_destination_and_replace_guard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("method_schema");
        fs::create_dir_all(&src).expect("src dir");
        fs::write(src.join("schema.json"), "{}").expect("src file");
        let node = TeamNode {
            id: "runtime".to_string(),
            kind: TeamNodeKind::SshDocker,
            status: TeamNodeStatus::Online,
            url: None,
            host: Some("saitou".to_string()),
            container: Some("runtime-container".to_string()),
            cwd: Some("/workspace".to_string()),
            note: String::new(),
            created_at: now(),
            updated_at: now(),
        };

        let (command, src_kind) =
            build_path_sync_command(&node, &src, "/workspace/inputs/method_schema", true)
                .expect("build command");

        assert_eq!(src_kind, "directory");
        assert!(command.contains("ssh 'saitou'"));
        assert!(command.contains("docker exec -i"));
        assert!(command.contains("runtime-container"));
        assert!(command.contains("/workspace/inputs/method_schema"));
        assert!(command.contains("replace=1"));
        assert!(command.contains(".codex-team-handoff-backups"));
    }

    #[test]
    fn path_pull_command_uses_node_source_and_local_replace_guard() {
        let node = TeamNode {
            id: "runtime".to_string(),
            kind: TeamNodeKind::SshDocker,
            status: TeamNodeStatus::Online,
            url: None,
            host: Some("saitou".to_string()),
            container: Some("runtime-container".to_string()),
            cwd: Some("/workspace".to_string()),
            note: String::new(),
            created_at: now(),
            updated_at: now(),
        };

        let (command, src_name) = build_path_pull_command(
            &node,
            "/workspace/team-1/audit_package",
            Path::new("/tmp/local-audit-package"),
            true,
        )
        .expect("build command");

        assert_eq!(src_name, "audit_package");
        assert!(command.contains("ssh 'saitou'"));
        assert!(command.contains("docker exec"));
        assert!(command.contains("runtime-container"));
        assert!(command.contains("/workspace/team-1/audit_package"));
        assert!(command.contains("/tmp/local-audit-package"));
        assert!(command.contains("replace=1"));
        assert!(command.contains(".codex-team-handoff-backups"));
    }

    #[test]
    fn pull_path_from_local_node_copies_artifact_package() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path().join("team");
        write_test_config(&team_dir);
        let src = tmp.path().join("remote_artifact");
        fs::create_dir_all(&src).expect("src dir");
        fs::write(src.join("report.md"), "# report\n").expect("report");
        let dest = tmp.path().join("pulled_artifact");

        pull_node_path(
            &team_dir,
            NodePullPathArgs {
                id: "local".to_string(),
                src: src.display().to_string(),
                dest: dest.clone(),
                replace: false,
                dry_run: false,
            },
        )
        .expect("pull path");

        assert_eq!(
            fs::read_to_string(dest.join("report.md")).expect("pulled report"),
            "# report\n"
        );
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| event.event == "node_path_pulled"));
    }

    #[test]
    fn runtime_contract_inputs_extract_host_to_container_pairs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let contract_dir = team_dir.join("method_schema").join("cycle20");
        let audit_dir = team_dir.join("audit").join("cycle19");
        fs::create_dir_all(&contract_dir).expect("contract dir");
        fs::create_dir_all(&audit_dir).expect("audit dir");
        fs::write(audit_dir.join("sha256_manifest.txt"), "").expect("manifest");
        fs::write(
            contract_dir.join("runtime_contract.yaml"),
            format!(
                r#"runtime_task: 95
method_package:
  host_path: {}
  expected_container_input_root: /workspace/inputs/method_schema/cycle20
authoritative_predecessor:
  audit_root_host: {}
  expected_container_root: /workspace/inputs/audit/cycle19
"#,
                contract_dir.display(),
                audit_dir.display()
            ),
        )
        .expect("contract");

        let map = load_contract_declared_inputs(&[FileOwnership {
            path: contract_dir.display().to_string(),
            owner: "method_schema".to_string(),
            note: "Task94 Cycle20 contract".to_string(),
            updated_at: now(),
        }])
        .expect("load inputs");
        let inputs = map.get("95").expect("runtime task inputs");

        assert!(inputs.iter().any(|input| {
            input.src == contract_dir
                && input.dest == "/workspace/inputs/method_schema/cycle20"
                && input.label == "host_path"
        }));
        assert!(inputs.iter().any(|input| {
            input.src == audit_dir
                && input.dest == "/workspace/inputs/audit/cycle19"
                && input.label == "audit_root_host"
        }));
    }

    #[test]
    fn contract_input_sync_warns_when_declared_source_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let contract_dir = team_dir.join("method_schema").join("cycle20");
        fs::create_dir_all(&contract_dir).expect("contract dir");
        let missing_audit = team_dir.join("audit").join("cycle19");
        fs::write(
            contract_dir.join("runtime_contract.yaml"),
            format!(
                r#"runtime_task: 95
authoritative_predecessor:
  audit_root_host: {}
  expected_container_root: /workspace/inputs/audit/cycle19
"#,
                missing_audit.display()
            ),
        )
        .expect("contract");
        let now_stamp = now();
        let config = TeamConfig {
            version: 1,
            id: "team-contract-sync".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now_stamp.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "runtime".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now_stamp.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: Some("remote-runtime".to_string()),
                },
            ],
            language: None,
            created_at: now_stamp.clone(),
            updated_at: now_stamp.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "95"),
            &TeamTask {
                id: "95".to_string(),
                subject: "runtime gate".to_string(),
                description: String::new(),
                owner: Some("runtime".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now_stamp.clone(),
                updated_at: now_stamp,
            },
        )
        .expect("write task");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: contract_dir.display().to_string(),
                owner: "method_schema".to_string(),
                note: "Task94 Cycle20 contract".to_string(),
                updated_at: now(),
            }],
        )
        .expect("ownerships");
        let nodes = vec![TeamNode {
            id: "remote-runtime".to_string(),
            kind: TeamNodeKind::SshDocker,
            url: None,
            host: Some("saitou".to_string()),
            container: Some("runtime-container".to_string()),
            cwd: Some("/workspace".to_string()),
            status: TeamNodeStatus::Online,
            note: String::new(),
            created_at: now(),
            updated_at: now(),
        }];
        let mut attempts = HashSet::new();

        maybe_sync_contract_declared_inputs(team_dir, &config, &nodes, &mut attempts)
            .expect("sync warning");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let runtime_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "runtime")).expect("runtime inbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(runtime_messages.len(), 1);
        assert!(
            lead_messages[0]
                .message
                .contains("Contract-declared input sync warning")
        );
        assert!(lead_messages[0].message.contains("missing local source"));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "contract_declared_input_sync_missing_source"
                && event.data.get("task").and_then(|value| value.as_str()) == Some("95")
        }));
    }

    #[test]
    fn member_turn_has_completion_checklist_requires_all_fields() {
        let mut buffers = HashMap::new();
        buffers.insert(
            "ops".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /tmp/out
- verification: smoke ok
- messages_sent: lead and audit
- consumers_notified: audit
- blockers_or_limits: none"#
                .to_string(),
        );

        assert!(member_turn_has_completion_checklist(&buffers, "ops"));

        buffers.insert(
            "ops".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /tmp/out
- verification: smoke ok"#
                .to_string(),
        );

        assert!(!member_turn_has_completion_checklist(&buffers, "ops"));
    }

    #[test]
    fn checklist_field_value_accepts_multiline_yaml_lists() {
        let checklist = r#"TEAM_COMPLETION_CHECKLIST:
- artifacts:
  - /tmp/report.md
  - /tmp/result.json
- verification:
  - sha256sum -c sha256_manifest.txt rc=0
- messages_sent:
  - lead
- consumers_notified:
  - lead
- blockers_or_limits:
  - none"#;

        assert_eq!(
            checklist_field_value(&checklist.to_ascii_lowercase(), "artifacts:").as_deref(),
            Some("/tmp/report.md\n/tmp/result.json")
        );
        assert_eq!(
            checklist_field_value(&checklist.to_ascii_lowercase(), "verification:").as_deref(),
            Some("sha256sum -c sha256_manifest.txt rc=0")
        );
    }

    #[test]
    fn active_task_completion_rejects_empty_artifacts_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "84",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut buffers = HashMap::new();
        buffers.insert(
            "engineering".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: none
- verification: checked status only
- messages_sent: lead
- consumers_notified: lead
- blockers_or_limits: none"#
                .to_string(),
        );

        let issue = member_turn_active_task_completion_issue(team_dir, &buffers, "engineering")
            .expect("completion issue")
            .expect("empty artifacts should be rejected");

        assert!(issue.contains("artifacts is empty"));
    }

    #[test]
    fn active_task_completion_requires_declared_remote_handoff_path_in_checklist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        write_json_atomic(
            &task_path(team_dir, "84"),
            &TeamTask {
                id: "84".to_string(),
                subject: "remote validation".to_string(),
                description: "Produce report/json/checklist under /workspace/runtime/schema_handoff_validation_cycle17.".to_string(),
                owner: Some("engineering".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
        let mut buffers = HashMap::new();
        buffers.insert(
            "engineering".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /workspace/runtime/other_validation
- verification: sha256sum -c rc=0
- messages_sent: lead and audit
- consumers_notified: audit
- blockers_or_limits: none"#
                .to_string(),
        );

        let issue = member_turn_active_task_completion_issue(team_dir, &buffers, "engineering")
            .expect("completion issue")
            .expect("missing required path should be rejected");

        assert!(issue.contains("schema_handoff_validation_cycle17"));
    }

    #[test]
    fn active_task_completion_accepts_declared_remote_handoff_path_in_checklist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        write_json_atomic(
            &task_path(team_dir, "84"),
            &TeamTask {
                id: "84".to_string(),
                subject: "remote validation".to_string(),
                description: "Produce report/json/checklist under /workspace/runtime/schema_handoff_validation_cycle17.".to_string(),
                owner: Some("engineering".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
        let mut buffers = HashMap::new();
        buffers.insert(
            "engineering".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /workspace/runtime/schema_handoff_validation_cycle17/{report.md,validation.json,TEAM_COMPLETION_CHECKLIST.md,sha256_manifest.txt}
- verification: cd /workspace/runtime/schema_handoff_validation_cycle17 && sha256sum -c sha256_manifest.txt rc=0
- messages_sent: lead and audit
- consumers_notified: audit
- blockers_or_limits: bounded validation only"#
                .to_string(),
        );

        let issue = member_turn_active_task_completion_issue(team_dir, &buffers, "engineering")
            .expect("completion issue");

        assert_eq!(issue, None);
    }

    #[test]
    fn new_app_server_turn_resets_stale_assistant_buffer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_test_config(tmp.path());
        let config = load_config(tmp.path()).expect("config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering")
            .clone();
        let mut run = AppServerMemberRun {
            member,
            node_id: "local".to_string(),
            cwd: tmp.path().to_path_buf(),
            thread_id: "thread-1".to_string(),
            turn_id: "turn-old".to_string(),
            completed: true,
            failed: false,
            standby_after_turn: false,
            usage_category: "test".to_string(),
            team_message_scan_offset: 42,
            last_activity_at: Instant::now(),
            last_activity_kind: "turn_completed".to_string(),
            last_stale_notice_at: None,
            retry_not_before: None,
            side_context_ids: Vec::new(),
        };
        let mut buffers = HashMap::from([(
            "engineering".to_string(),
            "old blocked output that must not affect the next turn".to_string(),
        )]);

        assert!(reset_member_turn_buffer_if_new(
            &mut run,
            &mut buffers,
            "engineering",
            "turn-new"
        ));

        assert_eq!(buffers.get("engineering").map(String::as_str), Some(""));
        assert_eq!(run.team_message_scan_offset, 0);
    }

    #[test]
    fn new_app_server_turn_rotates_live_message_to_last_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let live_path = team_dir.join("live_messages").join("engineering.md");
        let last_path = team_dir.join("last_messages").join("engineering.md");
        write_text_atomic(&live_path, "old live output\nold checklist\n").expect("write live");

        reset_member_live_message_for_new_turn(team_dir, "engineering", "turn-new")
            .expect("reset live");

        let live = fs::read_to_string(&live_path).expect("live");
        let last = fs::read_to_string(&last_path).expect("last");
        assert!(live.contains("turn-new"));
        assert!(!live.contains("old checklist"));
        assert_eq!(last, "old live output\nold checklist\n");
    }

    #[test]
    fn prompts_require_current_manifest_hashes_in_handoffs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_test_config(tmp.path());
        let config = load_config(tmp.path()).expect("config");
        let worker = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("worker");
        let lead = config
            .members
            .iter()
            .find(|member| member.name == "lead")
            .expect("lead");

        let worker_prompt = build_worker_prompt(&config, &[], worker);
        assert!(worker_prompt.contains("team task --team \"$CODEX_TEAM_ID\" claim"));
        assert!(worker_prompt.contains("self-claim an unassigned `ready` task"));
        assert!(worker_prompt.contains("re-read the current manifest files from disk"));
        assert!(worker_prompt.contains("stale handoff hash"));
        assert!(
            worker_prompt.contains("write and hash or timestamp the frozen configuration/plan")
        );
        assert!(worker_prompt.contains("heldout/test/evaluation-only"));
        assert!(worker_prompt.contains("Broad inventory commands"));
        assert!(worker_prompt.contains("reveal heldout/test/evaluation-only path names"));
        assert!(worker_prompt.contains("guard bootstrap seed outside the protected input root"));
        assert!(worker_prompt.contains("never \"locate\" a seed"));
        assert!(worker_prompt.contains("ask lead for the path instead of discovering it"));
        assert!(worker_prompt.contains("an exact file path exception is not permission"));
        assert!(worker_prompt.contains("Do not run `cat`, `sed`, `head`, `tail`"));
        assert!(worker_prompt.contains("python -c open(...)"));
        assert!(worker_prompt.contains("pre_guard_allowed_exact_commands"));
        assert!(worker_prompt.contains("seed-local schema-valid fail-closed writer or template"));
        assert!(worker_prompt.contains("legacy or best-effort `outcome.json`"));
        assert!(worker_prompt.contains("frozen command transcript"));
        assert!(worker_prompt.contains("explicit shell exit code"));
        assert!(
            worker_prompt.contains("event ledger, or summarized outcome alone is not sufficient")
        );

        let lead_prompt = build_app_server_lead_prompt(
            &config,
            &[],
            lead,
            Path::new("codex"),
            TeamPromptLanguage::En,
        );
        assert!(lead_prompt.contains("stale manifest hashes written only in a handoff message"));
        assert!(lead_prompt.contains("current manifest-file hashes"));
        assert!(lead_prompt.contains("every named predecessor package"));
        assert!(lead_prompt.contains("not only the immediate method package"));
        assert!(lead_prompt.contains("root-correct verify every required prior audit"));
        assert!(lead_prompt.contains("cannot legally read it before the guard is active"));
        assert!(lead_prompt.contains("give the executor exact bootstrap paths"));
        assert!(lead_prompt.contains("parent-directory inventory over protected roots"));
        assert!(lead_prompt.contains("exact file path exception alone is not enough"));
        assert!(lead_prompt.contains("file reader/probe/command variant"));
        assert!(lead_prompt.contains("`cat`, `sed`, `head`, `tail`"));
        assert!(lead_prompt.contains("seed-local schema-valid early fail-closed writer/template"));
        assert!(lead_prompt.contains("keep runtime blocked and route the contract back"));
        assert!(lead_prompt.contains("artifact-level command transcript"));
        assert!(lead_prompt.contains("Event-level ledgers, mailbox summaries, or outcome JSON"));
        assert!(lead_prompt.contains("job id/log path and observed exit code"));
    }

    #[test]
    fn team_signal_ingest_streams_complete_lines_without_losing_partial_tail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("research"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );

        let member = TeamMember {
            name: "engineering".to_string(),
            role: "engineering".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: Some("thread".to_string()),
            workspace_path: None,
            node: None,
        };
        let mut active = HashMap::from([(
            "engineering".to_string(),
            AppServerMemberRun {
                member,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "test".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        )]);
        let mut buffers = HashMap::from([(
            "engineering".to_string(),
            "TEAM_MESSAGE to=lead: hel".to_string(),
        )]);

        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, false)
            .expect("partial ingest");
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(messages.is_empty());

        buffers.insert(
            "engineering".to_string(),
            "TEAM_MESSAGE to=lead: hello\nTEAM_TASK id=1 status=blocked result=waiting for data\n"
                .to_string(),
        );
        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, false)
            .expect("complete ingest");
        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, true)
            .expect("final ingest");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "hello");
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "1")
            .expect("task");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert_eq!(task.result.as_deref(), Some("waiting for data"));
    }

    #[test]
    fn team_signal_ingest_allows_comma_separated_message_recipients() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);

        let member = TeamMember {
            name: "engineering".to_string(),
            role: "engineering".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: Some("thread".to_string()),
            workspace_path: None,
            node: None,
        };
        let mut active = HashMap::from([(
            "engineering".to_string(),
            AppServerMemberRun {
                member,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "test".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        )]);
        let buffers = HashMap::from([(
            "engineering".to_string(),
            "TEAM_MESSAGE to=lead, quality: corrected verification passed\n".to_string(),
        )]);

        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, false)
            .expect("ingest");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(quality_messages.len(), 1);
        assert_eq!(lead_messages[0].message, "corrected verification passed");
        assert_eq!(quality_messages[0].message, "corrected verification passed");
    }

    #[test]
    fn completed_tracked_job_with_artifacts_keeps_task_in_progress_and_resumes_owner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "30",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        set_member_status(team_dir, "engineering", MemberStatus::Failed).expect("failed status");
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let job = TeamJob {
            id: "job-1".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("30".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: vec![TeamArtifact {
                path: "runtime/outcome.json".to_string(),
                note: "final handoff".to_string(),
                created_at: now(),
            }],
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-1"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "job-1").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Completed);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "30")
            .expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("completed with registered artifacts")
                && result.contains("final report/json/manifest/checklist")
        }));
        let config = load_config(team_dir).expect("config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(member.status, MemberStatus::Online);
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(owner_messages.iter().any(|message| {
            message
                .message
                .contains("JOB_STATUS: job `job-1` for your task 30")
                && message
                    .message
                    .contains("Registered job artifacts are not sufficient")
                && message.message.contains("TEAM_COMPLETION_CHECKLIST")
        }));
        assert!(lead_messages.iter().any(|message| {
            message
                .message
                .contains("@engineering's job `job-1` for task 30")
                && message.message.contains("task is now `in_progress`")
        }));
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.get("event").and_then(|value| value.as_str())
                == Some("job_completed_requires_owner_handoff")
        }));
    }

    #[test]
    fn stale_job_status_does_not_reopen_or_message_completed_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "35",
            Some("engineering"),
            TaskStatus::Completed,
            Vec::new(),
            Some("superseding retry completed and handoff accepted"),
        );
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "143").expect("exit code");
        let job = TeamJob {
            id: "stale-failed-job".to_string(),
            node: "local".to_string(),
            command: "false".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("35".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "stale-failed-job"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "stale-failed-job").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Failed);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "35")
            .expect("task");
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(
            task.result.as_deref(),
            Some("superseding retry completed and handoff accepted")
        );
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(owner_messages.is_empty());
        assert!(lead_messages.is_empty());
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.get("event").and_then(|value| value.as_str())
                == Some("job_status_ignored_closed_task")
        }));
    }

    #[test]
    fn completed_job_without_artifacts_keeps_task_in_progress() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "34",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let job = TeamJob {
            id: "job-no-artifacts".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("34".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-no-artifacts"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "job-no-artifacts").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Completed);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "34")
            .expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(
            task.result.as_deref().is_some_and(|result| {
                result.contains("completed without registered artifacts")
            })
        );
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert!(owner_messages.iter().any(|message| {
            message.message.contains("do not register fake artifacts")
                && message
                    .message
                    .contains("write the task's real final report/json/manifest/checklist")
        }));
        assert!(lead_messages.iter().any(|message| {
            message
                .message
                .contains("task 34 ended with status Completed")
        }));
    }

    #[test]
    fn completed_job_status_notification_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "34",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let mut job = TeamJob {
            id: "job-repeat".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("34".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-repeat"), &job).expect("write job");

        refresh_job_status(team_dir, "job-repeat").expect("first refresh");

        // Simulate a concurrent refresher that loaded the old Running state before
        // another refresher persisted the terminal status and sent notifications.
        job.status = TeamJobStatus::Running;
        job.exit_code = None;
        write_json_atomic(&job_path(team_dir, "job-repeat"), &job).expect("rewrite stale job");

        refresh_job_status(team_dir, "job-repeat").expect("second refresh");

        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let completed_without_artifacts_count = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("job_completed_without_artifacts")
            })
            .count();
        assert_eq!(completed_without_artifacts_count, 1);

        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        let owner_status_count = owner_messages
            .iter()
            .filter(|message| {
                message
                    .message
                    .contains("JOB_STATUS: job `job-repeat` for your task 34")
            })
            .count();
        assert_eq!(owner_status_count, 1);

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let lead_status_count = lead_messages
            .iter()
            .filter(|message| {
                message
                    .message
                    .contains("@engineering's job `job-repeat` for task 34")
            })
            .count();
        assert_eq!(lead_status_count, 1);
    }

    #[test]
    fn newly_registered_job_without_pid_stays_running_during_start_grace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        let job = TeamJob {
            id: "job-starting".to_string(),
            node: "local".to_string(),
            command: "sleep 1".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: None,
            status: TeamJobStatus::Running,
            pid: None,
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: team_dir.join("exit.code").display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-starting"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "job-starting").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Running);
        assert!(refreshed.pid.is_none());
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().all(|event| {
            event.get("event").and_then(|value| value.as_str()) != Some("job_unknown")
        }));
    }

    #[test]
    fn job_with_dead_pid_and_missing_exit_file_is_failed_not_unknown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "88",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let job = TeamJob {
            id: "job-missing-exit".to_string(),
            node: "local".to_string(),
            command: "false".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("88".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: team_dir.join("missing.exit").display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-missing-exit"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "job-missing-exit").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Failed);
        assert_eq!(refreshed.exit_code, None);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "88")
            .expect("task");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("job-missing-exit") && result.contains("Failed")
        }));
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert!(owner_messages.iter().any(|message| {
            message
                .message
                .contains("JOB_STATUS: job `job-missing-exit`")
                && message.message.contains("status Failed")
        }));
    }

    #[test]
    fn job_without_pid_and_recent_log_stays_running_after_start_grace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        let log_path = team_dir.join("job.log");
        fs::write(&log_path, "still downloading\n").expect("log");
        let old_created_at = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
        let job = TeamJob {
            id: "job-no-pid-recent-log".to_string(),
            node: "local".to_string(),
            command: "sleep 300".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: None,
            status: TeamJobStatus::Running,
            pid: None,
            log_path: log_path.display().to_string(),
            exit_path: team_dir.join("missing.exit").display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: old_created_at,
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-no-pid-recent-log"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "job-no-pid-recent-log").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Running);
        assert_eq!(refreshed.exit_code, None);
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().all(|event| {
            event.get("event").and_then(|value| value.as_str()) != Some("job_unknown")
        }));
    }

    #[test]
    fn auxiliary_job_does_not_overwrite_task_status_or_result() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "79",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("runtime is producing final package"),
        );
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let job = TeamJob {
            id: "lead-verify".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("lead".to_string()),
            task_id: Some("79".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: "read-only verification".to_string(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "lead-verify"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "lead-verify").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Completed);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "79")
            .expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(
            task.result.as_deref(),
            Some("runtime is producing final package")
        );
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.get("event").and_then(|value| value.as_str())
                == Some("auxiliary_job_status_no_task_update")
        }));
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert!(
            lead_messages
                .iter()
                .any(|message| message.message.contains("AUX_JOB_STATUS"))
        );
        assert!(
            owner_messages
                .iter()
                .any(|message| message.message.contains("AUX_JOB_STATUS"))
        );
    }

    #[test]
    fn idle_outreach_sends_from_free_department_to_active_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-test".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "helper".to_string(),
                    role: "review".to_string(),
                    status: MemberStatus::Completed,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "target".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "1"),
            &TeamTask {
                id: "1".to_string(),
                subject: "active work".to_string(),
                description: String::new(),
                owner: Some("target".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");

        let mut last = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0;
        maybe_send_idle_department_outreach(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("outreach");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "target")).expect("target mailbox");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "helper");
        assert!(messages[0].message.contains("Periodic idle outreach"));
    }

    #[test]
    fn idle_outreach_skips_unavailable_remote_helpers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let stale = (Utc::now() - chrono::Duration::seconds(11 * 60))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let config = TeamConfig {
            version: 1,
            id: "team-test".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "remote_helper".to_string(),
                    role: "review".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: Some("remote".to_string()),
                },
                TeamMember {
                    name: "target".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_nodes(
            team_dir,
            &[TeamNode {
                id: "remote".to_string(),
                kind: TeamNodeKind::Ssh,
                url: Some("ws://127.0.0.1:9999".to_string()),
                host: Some("remote-host".to_string()),
                container: None,
                cwd: Some("/work".to_string()),
                status: TeamNodeStatus::Online,
                note: String::new(),
                created_at: now.clone(),
                updated_at: stale,
            }],
        )
        .expect("write nodes");
        write_json_atomic(
            &task_path(team_dir, "1"),
            &TeamTask {
                id: "1".to_string(),
                subject: "active work".to_string(),
                description: String::new(),
                owner: Some("target".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");

        let mut last = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0;
        maybe_send_idle_department_outreach(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("outreach");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "target")).unwrap_or_default();
        assert!(messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "idle_outreach_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("no_idle_departments")
        }));
    }

    #[test]
    fn idle_wakeup_is_batched_and_rotates_idle_departments() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let mut members = vec![TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Online,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        }];
        for name in ["research", "capture", "method", "audit"] {
            members.push(TeamMember {
                name: name.to_string(),
                role: "worker".to_string(),
                status: MemberStatus::Completed,
                joined_at: now.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            });
        }
        let config = TeamConfig {
            version: 1,
            id: "team-idle".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members,
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let mut idle_since = HashMap::new();
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;
        let old = Instant::now() - Duration::from_secs(601);
        for name in ["research", "capture", "method", "audit"] {
            idle_since.insert(name.to_string(), old);
        }

        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("first wakeup batch");

        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research"))
                .expect("research")
                .len(),
            1
        );
        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "capture"))
                .expect("capture")
                .len(),
            1
        );
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "method"))
                .expect("method")
                .is_empty()
        );
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "audit"))
                .expect("audit")
                .is_empty()
        );

        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("suppressed immediate batch");
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "method"))
                .expect("method")
                .is_empty()
        );

        last_batch = Instant::now() - Duration::from_secs(601);
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("second wakeup batch");

        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "method"))
                .expect("method")
                .len(),
            1
        );
        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "audit"))
                .expect("audit")
                .len(),
            1
        );
    }

    #[test]
    fn idle_wakeup_backs_off_after_stay_without_direct_work() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-idle-stay".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "research".to_string(),
                    role: "research".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        append_event(
            team_dir,
            "message_sent",
            serde_json::json!({
                "from": "research",
                "to": ["lead"],
                "message": "STAY: no action needed",
                "source": "team_relay",
            }),
        )
        .expect("stay event");

        let mut idle_since = HashMap::from([(
            "research".to_string(),
            Instant::now() - Duration::from_secs(601),
        )]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("idle wakeup");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert!(messages.is_empty());
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.get("event").and_then(|value| value.as_str())
                == Some("department_idle_wakeup_skipped")
                && event
                    .get("data")
                    .and_then(|data| data.get("reason"))
                    .and_then(|value| value.as_str())
                    == Some("recent_stay_backoff")
        }));
    }

    #[test]
    fn heartbeat_skips_department_recently_woken_by_idle_wakeup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-idle-heartbeat".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "research".to_string(),
                    role: "research".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let mut idle_since = HashMap::from([(
            "research".to_string(),
            Instant::now() - Duration::from_secs(601),
        )]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("idle wakeup");

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &HashMap::new(),
            &mut heartbeats,
            &last_wakeup,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("heartbeat");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].message.contains("Department idle wakeup"));
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let skipped_count = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("department_heartbeat_skipped")
                    && event
                        .get("data")
                        .and_then(|data| data.get("reason"))
                        .and_then(|value| value.as_str())
                        == Some("recent_idle_wakeup")
            })
            .count();
        assert_eq!(skipped_count, 1);

        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &HashMap::new(),
            &mut heartbeats,
            &last_wakeup,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("second heartbeat");
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let skipped_count = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("department_heartbeat_skipped")
                    && event
                        .get("data")
                        .and_then(|data| data.get("reason"))
                        .and_then(|value| value.as_str())
                        == Some("recent_idle_wakeup")
            })
            .count();
        assert_eq!(skipped_count, 1);
    }

    #[test]
    fn heartbeat_suppresses_empty_standby_department_during_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let research = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-heartbeat-cooldown".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), research],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        let mut active = HashMap::new();
        active.insert(
            "lead".to_string(),
            AppServerMemberRun {
                member: lead,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: true,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "usage_limited".to_string(),
                last_stale_notice_at: None,
                retry_not_before: Some(Instant::now() + Duration::from_secs(300)),
                side_context_ids: Vec::new(),
            },
        );

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &active,
            &mut heartbeats,
            &HashMap::new(),
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("heartbeat");

        let research_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert!(research_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "department_heartbeat_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("usage_limit_cooldown")
        }));
    }

    #[test]
    fn heartbeat_skips_completed_department_without_open_tasks_even_if_turn_is_active() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let audit = TeamMember {
            name: "audit".to_string(),
            role: "audit".to_string(),
            status: MemberStatus::Completed,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-completed-heartbeat".to_string(),
            goal: "keep finished reviewers quiet unless assigned".to_string(),
            lead: "lead".to_string(),
            members: vec![lead, audit.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        let mut active = HashMap::new();
        active.insert(
            "audit".to_string(),
            AppServerMemberRun {
                member: audit,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread-audit".to_string(),
                turn_id: "turn-audit".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "agent_message_delta".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &active,
            &mut heartbeats,
            &HashMap::new(),
            Duration::from_secs(60),
            TeamPromptLanguage::Ja,
        )
        .expect("heartbeat");

        let audit_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "audit")).expect("mailbox");
        assert!(audit_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "department_heartbeat_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("completed_no_open_tasks")
                && event
                    .data
                    .get("active_turn")
                    .and_then(|value| value.as_bool())
                    == Some(true)
        }));
    }

    #[test]
    fn heartbeat_skips_busy_department_with_active_turn_and_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let audit = TeamMember {
            name: "audit".to_string(),
            role: "audit".to_string(),
            status: MemberStatus::Online,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-busy-heartbeat".to_string(),
            goal: "avoid interrupting active department work".to_string(),
            lead: "lead".to_string(),
            members: vec![lead, audit.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "9",
            Some("audit"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("final audit running"),
        );
        let mut active = HashMap::new();
        active.insert(
            "audit".to_string(),
            AppServerMemberRun {
                member: audit,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread-audit".to_string(),
                turn_id: "turn-audit".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "agent_message_delta".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &active,
            &mut heartbeats,
            &HashMap::new(),
            Duration::from_secs(60),
            TeamPromptLanguage::Ja,
        )
        .expect("heartbeat");

        let audit_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "audit")).expect("mailbox");
        assert!(audit_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "department_heartbeat_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("active_turn_in_progress")
                && event
                    .data
                    .get("owned_open_tasks")
                    .and_then(|value| value.as_u64())
                    == Some(1)
        }));
    }

    #[test]
    fn heartbeat_skips_standby_department_without_open_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-standby-heartbeat".to_string(),
            goal: "avoid heartbeat noise for idle departments".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "evaluation".to_string(),
                    role: "quality".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &HashMap::new(),
            &mut heartbeats,
            &HashMap::new(),
            Duration::from_secs(60),
            TeamPromptLanguage::Ja,
        )
        .expect("heartbeat");

        let evaluation_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "evaluation")).expect("mailbox");
        assert!(evaluation_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "department_heartbeat_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("no_open_tasks")
                && event.data.get("status").and_then(|value| value.as_str()) == Some("Standby")
        }));
    }

    #[test]
    fn idle_wakeup_suppresses_empty_standby_department_during_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let research = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-idle-cooldown".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), research],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        let mut active = HashMap::new();
        active.insert(
            "lead".to_string(),
            AppServerMemberRun {
                member: lead,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: true,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "usage_limited".to_string(),
                last_stale_notice_at: None,
                retry_not_before: Some(Instant::now() + Duration::from_secs(300)),
                side_context_ids: Vec::new(),
            },
        );
        let mut idle_since = HashMap::from([(
            "research".to_string(),
            Instant::now() - Duration::from_secs(601),
        )]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;

        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &active,
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("idle wakeup");
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &active,
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("second idle wakeup");

        let research_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert!(research_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        let skipped_count = events
            .iter()
            .filter(|event| {
                event.event == "department_idle_wakeup_skipped"
                    && event.data.get("reason").and_then(|value| value.as_str())
                        == Some("usage_limit_cooldown")
            })
            .count();
        assert_eq!(skipped_count, 1);
    }

    #[test]
    fn department_pings_suppress_open_task_owner_during_member_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-open-task-cooldown".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "research".to_string(),
                    role: "research".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("research"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "research",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Try again later.",
                "retry_after_sec": 600,
            }),
        )
        .expect("usage event");

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &HashMap::new(),
            &mut heartbeats,
            &HashMap::new(),
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("heartbeat");

        let mut idle_since = HashMap::from([(
            "research".to_string(),
            Instant::now() - Duration::from_secs(601),
        )]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("idle wakeup");

        let research_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert!(research_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "department_heartbeat_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("usage_limit_cooldown")
                && event
                    .data
                    .get("cooldown_source")
                    .and_then(|value| value.as_str())
                    == Some("member")
                && event
                    .data
                    .get("owned_open_tasks")
                    .and_then(|value| value.as_u64())
                    == Some(1)
        }));
        assert!(events.iter().any(|event| {
            event.event == "department_idle_wakeup_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("usage_limit_cooldown")
                && event
                    .data
                    .get("cooldown_source")
                    .and_then(|value| value.as_str())
                    == Some("member")
                && event
                    .data
                    .get("owned_open_tasks")
                    .and_then(|value| value.as_u64())
                    == Some(1)
        }));
    }

    #[test]
    fn department_pings_skip_stale_remote_nodes_before_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-stale-node".to_string(),
            goal: "remote runtime work".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "ops".to_string(),
                    role: "ops".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: Some("remote".to_string()),
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_nodes(
            team_dir,
            &[TeamNode {
                id: "remote".to_string(),
                kind: TeamNodeKind::Ssh,
                url: Some("ws://127.0.0.1:9999".to_string()),
                host: Some("remote-host".to_string()),
                container: None,
                cwd: Some("/work".to_string()),
                status: TeamNodeStatus::Online,
                note: String::new(),
                created_at: "2026-05-01T00:00:00Z".to_string(),
                updated_at: "2026-05-08T06:41:31Z".to_string(),
            }],
        )
        .expect("write nodes");
        write_test_task(
            team_dir,
            "1",
            Some("ops"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "lead",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Try again later.",
                "retry_after_sec": 600,
            }),
        )
        .expect("usage event");

        let mut idle_since =
            HashMap::from([("ops".to_string(), Instant::now() - Duration::from_secs(601))]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("idle wakeup");

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &HashMap::new(),
            &mut heartbeats,
            &HashMap::new(),
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("heartbeat");

        let ops_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "ops")).unwrap_or_default();
        assert!(ops_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "department_idle_wakeup_skipped"
                && event.data.get("reason").and_then(|value| value.as_str()) == Some("node_stale")
                && event.data.get("node").and_then(|value| value.as_str()) == Some("remote")
        }));
        assert!(events.iter().any(|event| {
            event.event == "department_heartbeat_skipped"
                && event.data.get("reason").and_then(|value| value.as_str()) == Some("node_stale")
                && event.data.get("node").and_then(|value| value.as_str()) == Some("remote")
        }));
    }

    #[test]
    fn task_watchdog_warns_about_open_wait_on_stale_node() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_nodes(
            team_dir,
            &[TeamNode {
                id: "remote".to_string(),
                kind: TeamNodeKind::Ssh,
                url: Some("ws://127.0.0.1:9999".to_string()),
                host: Some("remote-host".to_string()),
                container: None,
                cwd: Some("/work".to_string()),
                status: TeamNodeStatus::Online,
                note: String::new(),
                created_at: "2026-05-01T00:00:00Z".to_string(),
                updated_at: "2026-05-08T06:41:31Z".to_string(),
            }],
        )
        .expect("write nodes");
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "1")).expect("task");
        task.updated_at = "2026-05-08T06:41:31Z".to_string();
        write_json_atomic(&task_path(team_dir, "1"), &task).expect("rewrite task");
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        write_json_atomic(
            &wait_path(team_dir, "wait-1"),
            &TeamWait {
                id: "wait-1".to_string(),
                title: "remote runtime package".to_string(),
                owner: Some("engineering".to_string()),
                task_id: Some("1".to_string()),
                node: Some("remote".to_string()),
                condition: "remote node writes manifest".to_string(),
                status: TeamWaitStatus::Waiting,
                progress: "/work/package/MANIFEST.sha256".to_string(),
                evidence: Some("/work/package".to_string()),
                created_at: "2026-05-08T06:41:31Z".to_string(),
                updated_at: "2026-05-08T06:41:31Z".to_string(),
            },
        )
        .expect("write wait");

        let config = load_config(team_dir).expect("config");
        let mut last_watchdog = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last_watchdog,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Wait node watchdog")
                && message.message.contains("wait-1")
                && message.message.contains("node `remote`")
        }));
        let owner_messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
            .expect("engineering mailbox");
        assert!(
            owner_messages
                .iter()
                .any(|message| message.message.contains("Wait node watchdog"))
        );
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "wait_node_unavailable_attention"
                && event.data.get("wait").and_then(|value| value.as_str()) == Some("wait-1")
                && event.data.get("reason").and_then(|value| value.as_str()) == Some("node_stale")
        }));
    }

    #[test]
    fn runtime_attach_normalizes_stale_running_member_without_open_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-stale-running".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "validator".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let normalized =
            normalize_stale_running_members_without_active_turns(team_dir, &HashMap::new())
                .expect("normalize");

        assert_eq!(normalized, vec!["validator".to_string()]);
        assert_eq!(
            member_status(team_dir, "validator").expect("status"),
            Some(MemberStatus::Standby)
        );
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "member_status_normalized"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("no active app-server turn or open owned task after runtime attach")
        }));
    }

    #[test]
    fn runtime_attach_keeps_running_member_with_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-running-task".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "validator".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("validator"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );

        let normalized =
            normalize_stale_running_members_without_active_turns(team_dir, &HashMap::new())
                .expect("normalize");

        assert!(normalized.is_empty());
        assert_eq!(
            member_status(team_dir, "validator").expect("status"),
            Some(MemberStatus::Running)
        );
    }

    #[test]
    fn idle_wakeup_cooldown_is_restored_from_recent_events_after_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-idle-restart".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "research".to_string(),
                    role: "research".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "runtime".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        append_event(
            team_dir,
            "department_idle_wakeup_sent",
            serde_json::json!({
                "member": "research",
                "role": "research",
                "node": "local",
                "idle_for_sec": 900,
                "owned_open_tasks": 0,
            }),
        )
        .expect("recent idle wakeup event");

        let mut idle_since = HashMap::from([
            (
                "research".to_string(),
                Instant::now() - Duration::from_secs(601),
            ),
            (
                "runtime".to_string(),
                Instant::now() - Duration::from_secs(601),
            ),
        ]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        seed_department_idle_wakeup_cooldowns(
            team_dir,
            &mut last_wakeup,
            &mut last_batch,
            Duration::from_secs(600),
        )
        .expect("seed cooldowns");
        let mut cursor = 0_usize;
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("suppressed restart batch");

        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research"))
                .expect("research mailbox")
                .is_empty()
        );
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "runtime"))
                .expect("runtime mailbox")
                .is_empty()
        );
    }

    #[test]
    fn mark_mailbox_messages_read_updates_unread_tail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-read".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![TeamMember {
                name: "lead".to_string(),
                role: "lead".to_string(),
                status: MemberStatus::Online,
                joined_at: now.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            }],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "user".to_string(),
                to: "lead".to_string(),
                message: "old".to_string(),
                timestamp: now.clone(),
                read: false,
            },
        )
        .expect("append old");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "user".to_string(),
                to: "lead".to_string(),
                message: "new".to_string(),
                timestamp: now,
                read: false,
            },
        )
        .expect("append new");

        mark_mailbox_messages_read(team_dir, "lead", 1).expect("mark read");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("read mailbox");
        assert!(!messages[0].read);
        assert!(messages[1].read);
    }

    #[test]
    fn acknowledge_mailbox_delivery_marks_only_delivered_messages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        let path = mailbox_path(team_dir, "worker");
        for message in ["old", "delivered", "arrived_later"] {
            append_jsonl(
                &path,
                &MailMessage {
                    from: "lead".to_string(),
                    to: "worker".to_string(),
                    message: message.to_string(),
                    timestamp: now.clone(),
                    read: false,
                },
            )
            .expect("append message");
        }
        let mut counts = HashMap::from([("worker".to_string(), 1)]);

        acknowledge_mailbox_delivery(team_dir, &mut counts, "worker", 1, 1).expect("ack delivered");

        let messages = read_jsonl::<MailMessage>(&path).expect("read mailbox");
        assert!(!messages[0].read);
        assert!(messages[1].read);
        assert!(!messages[2].read);
        assert_eq!(counts.get("worker"), Some(&2));
    }

    #[test]
    fn side_channel_context_reinjects_until_acknowledged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-side-context".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "worker".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        append_jsonl(
            &side_channel_context_path(team_dir, "worker"),
            &SideChannelContextRecord {
                id: "ctx-1".to_string(),
                member: "worker".to_string(),
                node: "local".to_string(),
                source_thread: "thread-main".to_string(),
                side_thread: "thread-side".to_string(),
                side_turn: "turn-side".to_string(),
                recipients: vec!["reviewer".to_string()],
                incoming_summary: "reviewer asked a question".to_string(),
                reply: "Quick side-channel reply from @worker while my main turn continues:\n\nI will check it.".to_string(),
                created_at: now,
                status: SideChannelContextStatus::Pending,
                injected_turns: Vec::new(),
                injected_at: None,
                acknowledged_at: None,
            },
        )
        .expect("append context");

        let (prompt, ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-1",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("append context prompt");
        assert_eq!(ids, vec!["ctx-1".to_string()]);
        assert!(prompt.contains("Pending side-channel context"));
        assert!(prompt.contains("machine-readable artifacts/manifests"));
        assert!(prompt.contains("Do not hand off, complete a task, or rely on stale artifacts"));
        mark_side_channel_contexts_injected(team_dir, "worker", &ids, "turn-main-1")
            .expect("mark injected");

        let (_, same_turn_ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-1",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("same turn prompt");
        assert!(same_turn_ids.is_empty());

        let (_, next_turn_ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-2",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("next turn prompt");
        assert_eq!(next_turn_ids, vec!["ctx-1".to_string()]);

        acknowledge_side_channel_contexts(team_dir, "worker", &ids, "turn-main-1").expect("ack");
        let (_, after_ack_ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-3",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("after ack prompt");
        assert!(after_ack_ids.is_empty());
    }

    #[test]
    fn side_channel_context_prompt_caps_reinjection_volume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let now = now();
        for idx in 0..(MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT + 5) {
            append_jsonl(
                &side_channel_context_path(team_dir, "worker"),
                &SideChannelContextRecord {
                    id: format!("ctx-{idx:02}"),
                    member: "worker".to_string(),
                    node: "local".to_string(),
                    source_thread: "thread-main".to_string(),
                    side_thread: format!("thread-side-{idx}"),
                    side_turn: format!("turn-side-{idx}"),
                    recipients: vec!["reviewer".to_string()],
                    incoming_summary: format!("incoming summary {idx}"),
                    reply: format!("reply {idx}"),
                    created_at: now.clone(),
                    status: SideChannelContextStatus::Pending,
                    injected_turns: Vec::new(),
                    injected_at: None,
                    acknowledged_at: None,
                },
            )
            .expect("append context");
        }

        let (prompt, ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("append context prompt");

        assert_eq!(ids.len(), MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT);
        assert_eq!(ids.first().map(String::as_str), Some("ctx-05"));
        assert_eq!(ids.last().map(String::as_str), Some("ctx-12"));
        assert!(prompt.contains("5 older side-channel context record(s) are omitted"));
        assert!(!prompt.contains("[ctx-00]"));
        assert!(prompt.contains("[ctx-12]"));
    }

    #[test]
    fn active_external_wait_detection_is_limited_to_running_external_waits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        let timestamp = now();
        for wait in [
            TeamWait {
                id: "wait-deep".to_string(),
                title: "deep_thinker strategy".to_string(),
                owner: Some("research".to_string()),
                task_id: Some("1".to_string()),
                node: None,
                condition: "MCP result returns".to_string(),
                status: TeamWaitStatus::Running,
                progress: "polling external tool".to_string(),
                evidence: None,
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            },
            TeamWait {
                id: "wait-complete".to_string(),
                title: "deep_thinker completed".to_string(),
                owner: Some("research".to_string()),
                task_id: Some("1".to_string()),
                node: None,
                condition: "done".to_string(),
                status: TeamWaitStatus::Completed,
                progress: "artifact saved".to_string(),
                evidence: None,
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            },
            TeamWait {
                id: "wait-local".to_string(),
                title: "local review".to_string(),
                owner: Some("research".to_string()),
                task_id: Some("1".to_string()),
                node: None,
                condition: "review finishes".to_string(),
                status: TeamWaitStatus::Running,
                progress: "reading notes".to_string(),
                evidence: None,
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            },
        ] {
            write_json_atomic(&wait_path(team_dir, &wait.id), &wait).expect("write wait");
        }

        let waits =
            active_external_wait_ids_for_member(team_dir, "research").expect("active waits");
        assert_eq!(waits, vec!["wait-deep".to_string()]);
    }

    #[test]
    fn deferred_active_turn_context_is_reinjected_later() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let timestamp = now();
        let run = AppServerMemberRun {
            member: TeamMember {
                name: "research".to_string(),
                role: "research".to_string(),
                status: MemberStatus::Running,
                joined_at: timestamp.clone(),
                thread_id: Some("thread-main".to_string()),
                workspace_path: None,
                node: None,
            },
            node_id: "local".to_string(),
            cwd: team_dir.to_path_buf(),
            thread_id: "thread-main".to_string(),
            turn_id: "turn-main".to_string(),
            completed: false,
            failed: false,
            standby_after_turn: false,
            usage_category: "test".to_string(),
            team_message_scan_offset: 0,
            last_activity_at: Instant::now(),
            last_activity_kind: "test".to_string(),
            last_stale_notice_at: None,
            retry_not_before: None,
            side_context_ids: Vec::new(),
        };
        let messages = vec![MailMessage {
            from: "lead".to_string(),
            to: "research".to_string(),
            message: "deep result が返ったら Docker 条件も見てください".to_string(),
            timestamp,
            read: false,
        }];

        let context_id = record_deferred_active_turn_context(
            team_dir,
            &run,
            &messages,
            &["wait-deep".to_string()],
            TeamPromptLanguage::Ja,
        )
        .expect("defer")
        .expect("context id");
        let (prompt, ids) = append_side_channel_context_prompt(
            team_dir,
            "research",
            "turn-next",
            "次の turn".to_string(),
            TeamPromptLanguage::Ja,
        )
        .expect("append context");

        assert_eq!(ids, vec![context_id]);
        assert!(prompt.contains("実行中 turn へ直接 steer せず保留されました"));
        assert!(prompt.contains("Docker 条件"));
    }

    #[test]
    fn active_turn_system_nudges_are_deferrable() {
        let timestamp = now();
        let system_messages = vec![
            MailMessage {
                from: "system".to_string(),
                to: "lead".to_string(),
                message: "Lead autonomy tick".to_string(),
                timestamp: timestamp.clone(),
                read: false,
            },
            MailMessage {
                from: "system".to_string(),
                to: "lead".to_string(),
                message: "Department heartbeat".to_string(),
                timestamp: timestamp.clone(),
                read: false,
            },
        ];
        assert!(active_turn_messages_are_deferrable_system_nudges(
            &system_messages
        ));

        let user_messages = vec![MailMessage {
            from: "user".to_string(),
            to: "lead".to_string(),
            message: "今すぐ方針を変えて".to_string(),
            timestamp,
            read: false,
        }];
        assert!(!active_turn_messages_are_deferrable_system_nudges(
            &user_messages
        ));
    }

    #[test]
    fn ui_debug_timeline_collects_messages_events_side_context_and_buffers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let timestamp = now();
        append_event(
            team_dir,
            "message_sent",
            serde_json::json!({
                "from": "system",
                "to": "engineering",
                "message": "Automated lead status check",
                "source": "idle_wakeup",
            }),
        )
        .expect("append event");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "engineering".to_string(),
                to: "lead".to_string(),
                message: "Need review".to_string(),
                timestamp: timestamp.clone(),
                read: false,
            },
        )
        .expect("append mailbox");
        append_jsonl(
            &side_channel_context_path(team_dir, "engineering"),
            &SideChannelContextRecord {
                id: "ctx-ui".to_string(),
                member: "engineering".to_string(),
                node: "local".to_string(),
                source_thread: "thread-main".to_string(),
                side_thread: "thread-side".to_string(),
                side_turn: "turn-side".to_string(),
                recipients: vec!["quality".to_string()],
                incoming_summary: "quality asked for status".to_string(),
                reply: "Runtime evidence is pending.".to_string(),
                created_at: timestamp,
                status: SideChannelContextStatus::Pending,
                injected_turns: Vec::new(),
                injected_at: None,
                acknowledged_at: None,
            },
        )
        .expect("append side context");
        fs::create_dir_all(team_dir.join("live_messages")).expect("live dir");
        fs::write(
            team_dir.join("live_messages").join("engineering.md"),
            "currently building",
        )
        .expect("write live");

        let timeline = collect_ui_debug_timeline(team_dir, 100).expect("timeline");
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "system"
                    && item.body.contains("Automated lead status check"))
        );
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "message" && item.body == "Need review")
        );
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "side"
                    && item.body.contains("Runtime evidence is pending."))
        );
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "live" && item.body.contains("currently building"))
        );

        let json = render_team_debug_json(team_dir).expect("debug json");
        assert!(json.contains("team-task-test"));
        assert!(json.contains("ctx-ui"));
    }

    #[test]
    fn ui_timestamps_are_rendered_in_tokyo_offset() {
        assert_eq!(
            timestamp_for_ui("2026-05-10T00:00:00Z"),
            "2026-05-10T09:00:00+09:00"
        );
        assert!(now().ends_with("+09:00"));
    }

    #[test]
    fn task_watchdog_warns_about_unattended_active_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        let now = now();
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let config = TeamConfig {
            version: 1,
            id: "team-watch".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "worker".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Completed,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "42"),
            &TeamTask {
                id: "42".to_string(),
                subject: "needs work".to_string(),
                description: String::new(),
                owner: Some("worker".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: old.clone(),
                updated_at: old,
            },
        )
        .expect("write task");

        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let worker_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "worker")).expect("worker mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(worker_messages.len(), 1);
        assert!(lead_messages[0].message.contains("Task watchdog"));
        let config = load_config(team_dir).expect("reload config");
        let worker = config
            .members
            .iter()
            .find(|member| member.name == "worker")
            .expect("worker");
        assert_eq!(worker.status, MemberStatus::Online);
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "task_watchdog_attention"
                && event
                    .data
                    .get("owner_reactivated")
                    .and_then(|value| value.as_bool())
                    == Some(true)
        }));
    }

    #[test]
    fn task_watchdog_does_not_reactivate_usage_limited_owner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        let now = now();
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let config = TeamConfig {
            version: 1,
            id: "team-watch-cooldown".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "worker".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Completed,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "42"),
            &TeamTask {
                id: "42".to_string(),
                subject: "needs work".to_string(),
                description: String::new(),
                owner: Some("worker".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: old.clone(),
                updated_at: old,
            },
        )
        .expect("write task");
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "worker",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Try again later.",
                "retry_after_sec": 600,
            }),
        )
        .expect("usage event");

        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let worker_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "worker")).expect("worker mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert!(lead_messages[0].message.contains("usage-limit cooldown"));
        assert!(worker_messages.is_empty());
        let config = load_config(team_dir).expect("reload config");
        let worker = config
            .members
            .iter()
            .find(|member| member.name == "worker")
            .expect("worker");
        assert_eq!(worker.status, MemberStatus::Completed);
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "task_watchdog_attention"
                && event
                    .data
                    .get("owner_reactivated")
                    .and_then(|value| value.as_bool())
                    == Some(false)
                && event
                    .data
                    .get("owner_usage_limit_cooldown_sec")
                    .and_then(|value| value.as_u64())
                    .is_some()
        }));
    }

    #[test]
    fn task_watchdog_ignores_tasks_waiting_on_incomplete_dependencies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_config(team_dir);
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "1")).expect("read task");
        task.updated_at = old.clone();
        write_json_atomic(&task_path(team_dir, "1"), &task).expect("write parent task");
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            vec!["1"],
            Some("Waiting for task 1 handoff."),
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "2"), &task).expect("write waiting task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let worker_messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
            .expect("worker mailbox");
        assert!(lead_messages.is_empty());
        assert!(worker_messages.is_empty());
    }

    #[test]
    fn review_handoff_watchdog_warns_when_task_artifact_directory_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("cycle8");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 Cycle 8 final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("semantic verification passed; final package pending"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "44"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message
                    .message
                    .contains("directory exists but contains no files")
        }));
        assert!(quality_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("TEAM_COMPLETION_CHECKLIST")
        }));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "review_handoff_artifact_attention")
        );
    }

    #[test]
    fn review_handoff_watchdog_accepts_complete_local_artifact_package() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("cycle8");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("cycle8_audit_report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("cycle8_audit.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: cycle8\n- verification: sha256sum -c sha256_manifest.txt rc=0\n- messages_sent: lead and quality\n- consumers_notified: lead and quality\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "cycle8_audit_report.md",
                "cycle8_audit.json",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 Cycle 8 final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "44"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert!(
            !lead_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
        assert!(
            !quality_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
    }

    #[test]
    fn completion_blocker_accepts_named_manifest_and_checklist_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("evaluation");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(
            artifact_dir.join("claim_evidence_matrix.md"),
            "# claim matrix\n",
        )
        .expect("matrix");
        fs::write(artifact_dir.join("evaluation_status.md"), "# status\n").expect("status");
        fs::write(artifact_dir.join("evaluation_plan.yaml"), "version: 1\n").expect("yaml");
        let manifest = Command::new("sha256sum")
            .args([
                "claim_evidence_matrix.md",
                "evaluation_status.md",
                "evaluation_plan.yaml",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(
            artifact_dir.join("evaluation_manifest.sha256"),
            manifest.stdout,
        )
        .expect("manifest");
        send_team_message_to_dir(
            team_dir,
            "quality",
            "lead",
            "Final handoff\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: evaluation\n- verification: sha256sum -c evaluation_manifest.sha256 rc=0",
        )
        .expect("message");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 evaluation handoff".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_blocker_accepts_nested_checksums_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("evaluation").join("cycle1_review");
        let manifest_dir = artifact_dir.join("manifest");
        fs::create_dir_all(&manifest_dir).expect("manifest dir");
        fs::write(
            artifact_dir.join("claim_evidence_matrix.md"),
            "# claim matrix\n",
        )
        .expect("matrix");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: cycle1_review\n- verification: sha256sum -c manifest/checksums.sha256 rc=0\n- messages_sent: lead and quality\n- consumers_notified: lead and quality\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        fs::write(
            artifact_dir.join("validation_summary.json"),
            "{\"ok\":true}\n",
        )
        .expect("json");
        let manifest = Command::new("sha256sum")
            .args([
                "claim_evidence_matrix.md",
                "TEAM_COMPLETION_CHECKLIST.md",
                "validation_summary.json",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(manifest_dir.join("checksums.sha256"), manifest.stdout).expect("manifest");
        send_team_message_to_dir(
            team_dir,
            "quality",
            "lead",
            "Final handoff\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: evaluation/cycle1_review\n- verification: sha256sum -c manifest/checksums.sha256 rc=0",
        )
        .expect("message");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task46 evaluation handoff".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "46",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "46")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_blocker_accepts_embedded_checklist_in_validation_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("evaluation").join("final_runtime_validation");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(
            artifact_dir.join("validation_report.md"),
            "# Runtime package validation report\n\nverdict: PASS_WITH_WARNINGS\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: validation_report.md; claim_evidence_review.md; manifest_validation_log.md; sha256_manifest.txt\n- verification: sha256sum -c sha256_manifest.txt rc=0; canonical evidence job-13 Completed exit=0\n- messages_sent: lead and audit\n- consumers_notified: lead and audit\n- blockers_or_limits: no active blocker; warnings recorded\n",
        )
        .expect("validation report");
        fs::write(
            artifact_dir.join("claim_evidence_review.md"),
            "# claim evidence review\n",
        )
        .expect("claim evidence");
        fs::write(
            artifact_dir.join("manifest_validation_log.md"),
            "# manifest validation\nrc=0\n",
        )
        .expect("manifest validation");
        let manifest = Command::new("sha256sum")
            .args([
                "validation_report.md",
                "claim_evidence_review.md",
                "manifest_validation_log.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 final runtime validation artifacts".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_blocker_rejects_formal_handoff_task_without_output_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "52",
            Some("audit"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("Worker exited successfully."),
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "52")).expect("task");
        task.subject = "Audit next-cycle plan and produce PASS/WARN/FAIL, TEAM_COMPLETION_CHECKLIST, sha256_manifest.txt, and manifest check log.".to_string();
        write_json_atomic(&task_path(team_dir, "52"), &task).expect("write task");
        let task = read_json::<TeamTask>(&task_path(team_dir, "52")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker")
            .expect("missing output path should block formal handoff completion");

        assert!(issue.contains("no task-specific local or node-side output package path"));
    }

    #[test]
    fn completion_blocker_accepts_owner_level_artifact_ownership_for_formal_handoff() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("artifacts").join("audit");
        fs::create_dir_all(artifact_dir.join("evidence")).expect("artifact dir");
        fs::write(artifact_dir.join("audit_report.md"), "# audit\nPASS\n").expect("report");
        fs::write(artifact_dir.join("audit_ledger.json"), "{\"pass\":true}\n").expect("ledger");
        fs::write(
            artifact_dir.join("completion-checklist.md"),
            "# checklist\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: artifacts/audit\n- verification: sha256sum -c audit_manifest.sha256 rc=0\n- messages_sent: lead and all\n- consumers_notified: lead and all\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        fs::write(
            artifact_dir
                .join("evidence")
                .join("host_18081_curl_final.transcript.txt"),
            "SSH_SCRIPT_RC=0\n",
        )
        .expect("evidence");
        let manifest = Command::new("sha256sum")
            .args([
                "audit_report.md",
                "audit_ledger.json",
                "completion-checklist.md",
                "evidence/host_18081_curl_final.transcript.txt",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("audit_manifest.sha256"), manifest.stdout).expect("manifest");
        send_team_message_to_dir(
            team_dir,
            "quality",
            "lead",
            "audit final handoff\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: artifacts/audit\n- verification: sha256sum -c artifacts/audit/audit_manifest.sha256 rc=0\n- messages_sent: lead and all\n- consumers_notified: lead and all\n- blockers_or_limits: none\n",
        )
        .expect("message");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note:
                    "レビュー、動作確認、token usage 所見、TEAM_COMPLETION_CHECKLIST の一次所有。"
                        .to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "4",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some(
                "audit final PASS. TEAM_COMPLETION_CHECKLIST sent; sha256sum -c artifacts/audit/audit_manifest.sha256 rc=0.",
            ),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "4")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_blocker_accepts_subject_declared_remote_output_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "10",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            Some(
                "Candidate package complete. TEAM_COMPLETION_CHECKLIST present; sha256_manifest.txt verified with sha256sum -c rc=0.",
            ),
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "10")).expect("task");
        task.subject = "Produce final handoff with TEAM_COMPLETION_CHECKLIST, sha256_manifest.txt, and sha256sum -c. 出力 root は /workspace/team-20260511115343/candidate1_visual_state_consistency とし、最終 handoff に含める。".to_string();
        write_json_atomic(&task_path(team_dir, "10"), &task).expect("write task");
        let task = read_json::<TeamTask>(&task_path(team_dir, "10")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_blocker_rejects_pending_checklist_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("phase0_second_pass");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("audit_report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("findings.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: audit/phase0_second_pass\n- verification: output manifest check pending until manifest generation\n- messages_sent: final handoff to lead/evaluation pending until manifest generation\n- consumers_notified: pending final handoff\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "audit_report.md",
                "findings.json",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("manifest.sha256"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task47 phase0 second pass final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "47",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "47")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker")
            .expect("pending checklist should block completion");

        assert!(issue.contains("pending/unresolved"));
    }

    #[test]
    fn completion_blocker_accepts_stale_verification_text_when_manifest_verifies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("evaluation").join("iteration2");
        fs::create_dir_all(artifact_dir.join("reports")).expect("artifact dir");
        fs::write(
            artifact_dir.join("reports").join("evaluation_report.md"),
            "# evaluation\n",
        )
        .expect("report");
        fs::write(artifact_dir.join("summary.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: evaluation/iteration2\n- verification: evaluation manifest check to be recorded in final handoff after manifest generation\n- messages_sent: lead and audit\n- consumers_notified: lead and audit\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "reports/evaluation_report.md",
                "summary.json",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::create_dir_all(artifact_dir.join("manifests")).expect("manifests dir");
        fs::write(
            artifact_dir.join("manifests").join("MANIFEST.sha256"),
            manifest.stdout,
        )
        .expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task48 iteration2 evaluation package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "48",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "48")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_blocker_does_not_treat_transcript_mentions_as_checklists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("iteration2");
        fs::create_dir_all(artifact_dir.join("reports")).expect("reports dir");
        fs::create_dir_all(artifact_dir.join("ledgers")).expect("ledgers dir");
        fs::create_dir_all(artifact_dir.join("transcripts")).expect("transcripts dir");
        fs::write(
            artifact_dir
                .join("reports")
                .join("iteration2_final_audit_report.md"),
            "# final audit\n",
        )
        .expect("report");
        fs::write(
            artifact_dir
                .join("ledgers")
                .join("iteration2_gate_ledger.json"),
            "{}\n",
        )
        .expect("ledger");
        fs::write(
            artifact_dir
                .join("transcripts")
                .join("audit_iteration2_command_transcript.md"),
            "# command transcript\n\nquoted output mentioned TEAM_COMPLETION_CHECKLIST:\n- artifacts: none\n",
        )
        .expect("transcript");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: audit/iteration2\n- verification: sha256sum -c manifests/MANIFEST.sha256 rc=0\n- messages_sent: lead and all\n- consumers_notified: lead and all\n- blockers_or_limits: PASS_WITH_LIMITS\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "reports/iteration2_final_audit_report.md",
                "ledgers/iteration2_gate_ledger.json",
                "transcripts/audit_iteration2_command_transcript.md",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::create_dir_all(artifact_dir.join("manifests")).expect("manifests dir");
        fs::write(
            artifact_dir.join("manifests").join("MANIFEST.sha256"),
            manifest.stdout,
        )
        .expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "audit".to_string(),
                note: "Task9 final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "9",
            Some("audit"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "9")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn completion_blocker_allows_pending_language_in_blockers_or_limits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("task9_next_cycle_plan_audit");
        fs::create_dir_all(artifact_dir.join("logs")).expect("artifact dir");
        fs::write(
            artifact_dir.join("reports_next_cycle_plan_audit.md"),
            "# audit\n",
        )
        .expect("report");
        fs::write(artifact_dir.join("audit_status.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("logs").join("sha256_manifest_check.log"),
            "sha256sum -c rc=0\n",
        )
        .expect("check log");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: reports_next_cycle_plan_audit.md; audit_status.json; logs/sha256_manifest_check.log\n- verification: sha256sum -c sha256_manifest.txt rc=0\n- messages_sent: lead and all\n- consumers_notified: lead and all\n- blockers_or_limits: Candidate 2 remains blocked pending separate authorization; no active blocker for this handoff\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "reports_next_cycle_plan_audit.md",
                "audit_status.json",
                "logs/sha256_manifest_check.log",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "audit".to_string(),
                note: "Task52 repair audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "52",
            Some("audit"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "52")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
        assert!(handoff_file_kind("sha256_manifest_check.log").is_some());
    }

    #[test]
    fn completion_blocker_ignores_side_channel_empty_checklist_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("evaluation");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(
            artifact_dir.join("claim_evidence_matrix.md"),
            "# claim matrix\n",
        )
        .expect("matrix");
        fs::write(artifact_dir.join("evaluation_status.md"), "# status\n").expect("status");
        fs::write(artifact_dir.join("evaluation_plan.yaml"), "version: 1\n").expect("yaml");
        let manifest = Command::new("sha256sum")
            .args([
                "claim_evidence_matrix.md",
                "evaluation_status.md",
                "evaluation_plan.yaml",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(
            artifact_dir.join("evaluation_manifest.sha256"),
            manifest.stdout,
        )
        .expect("manifest");
        send_team_message_to_dir(
            team_dir,
            "quality",
            "lead",
            "side-channel ack only\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: none\n- verification: team message sent rc=0\n- blockers_or_limits: side-channel only",
        )
        .expect("message");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task45 evaluation handoff".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "45",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "45")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert!(
            issue
                .expect("empty checklist should not satisfy blocker")
                .contains("TEAM_COMPLETION_CHECKLIST.md")
        );
    }

    #[test]
    fn review_handoff_watchdog_ignores_stale_empty_placeholder_when_complete_package_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let placeholder_dir = team_dir.join("audit").join("cycle10_placeholder");
        let artifact_dir = team_dir.join("audit").join("cycle10_final");
        fs::create_dir_all(&placeholder_dir).expect("placeholder dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("cycle10_audit_report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("cycle10_audit.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: cycle10_final\n- verification: sha256sum -c sha256_manifest.txt rc=0\n- messages_sent: lead and quality\n- consumers_notified: lead and quality\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "cycle10_audit_report.md",
                "cycle10_audit.json",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[
                FileOwnership {
                    path: placeholder_dir.display().to_string(),
                    owner: "quality".to_string(),
                    note: "Task50 initial placeholder package root".to_string(),
                    updated_at: now(),
                },
                FileOwnership {
                    path: artifact_dir.display().to_string(),
                    owner: "quality".to_string(),
                    note: "Task50 final audit package".to_string(),
                    updated_at: now(),
                },
            ],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "50",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "50")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "50"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert!(
            !lead_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
        assert!(
            !quality_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
    }

    #[test]
    fn review_handoff_watchdog_warns_when_manifest_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("cycle8");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("cycle8_audit_report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("cycle8_audit.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: cycle8\n- verification: sha256sum -c sha256_manifest.txt rc=0\n- messages_sent: lead and quality\n- consumers_notified: lead and quality\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        fs::write(
            artifact_dir.join("sha256_manifest.txt"),
            "0000000000000000000000000000000000000000000000000000000000000000  cycle8_audit_report.md\n",
        )
        .expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 Cycle 8 final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "44"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("failed sha256 verification")
        }));
    }

    #[test]
    fn review_handoff_watchdog_warns_when_manifest_contains_volatile_transcript() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("runtime").join("cycle12");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("outcome.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "TEAM_COMPLETION_CHECKLIST:\n- artifacts: cycle12\n- verification: sha256sum -c sha256_manifest.txt rc=0\n- messages_sent: lead and engineering\n- consumers_notified: lead and engineering\n- blockers_or_limits: none\n",
        )
        .expect("checklist");
        fs::write(
            artifact_dir.join("command_transcript.log"),
            "still being appended\n",
        )
        .expect("transcript");
        let manifest = Command::new("sha256sum")
            .args([
                "report.md",
                "outcome.json",
                "TEAM_COMPLETION_CHECKLIST.md",
                "command_transcript.log",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "engineering".to_string(),
                note: "Task60 Cycle 12 final runtime package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "60",
            Some("engineering"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "60")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "60"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("volatile entry")
                && message.message.contains("command_transcript.log")
        }));
        assert!(engineering_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("exclude active transcripts")
        }));
    }

    #[test]
    fn review_handoff_watchdog_warns_for_remote_only_artifact_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: "/__codex_remote__/workspace/runtime/cycle12_gate".to_string(),
                owner: "engineering".to_string(),
                note: "Task60 container runtime final package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "60",
            Some("engineering"),
            TaskStatus::Review,
            Vec::new(),
            Some("container package ready for review"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "60")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "60"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("not locally inspectable")
                && message
                    .message
                    .contains("node-side manifest/checklist verification")
        }));
        assert!(engineering_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("not locally inspectable")
        }));
    }

    #[test]
    fn completed_task_with_missing_declared_local_package_does_not_unblock_dependency() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let missing_dir = team_dir.join("method_schema").join("cycle9_contract");
        write_test_task(
            team_dir,
            "46",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut producer = read_json::<TeamTask>(&task_path(team_dir, "46")).expect("producer");
        producer.subject = "Cycle 9 method/schema".to_string();
        producer.description = format!(
            "Produce the contract package at {} before runtime starts.",
            missing_dir.display()
        );
        write_json_atomic(&task_path(team_dir, "46"), &producer).expect("write producer");
        write_test_task(
            team_dir,
            "47",
            Some("quality"),
            TaskStatus::Waiting,
            vec!["46"],
            Some("Soft-waiting for dependency task(s)."),
        );

        let changed = set_task_status_if_open(team_dir, "46", TaskStatus::Completed, Some("Done."))
            .expect("set task");
        assert!(changed);

        let producer = read_json::<TeamTask>(&task_path(team_dir, "46")).expect("producer");
        let consumer = read_json::<TeamTask>(&task_path(team_dir, "47")).expect("consumer");
        assert_eq!(producer.status, TaskStatus::Blocked);
        assert!(
            producer
                .result
                .as_deref()
                .unwrap_or_default()
                .contains("Completion rejected")
        );
        assert_eq!(consumer.status, TaskStatus::Waiting);
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.iter().any(|message| {
            message
                .message
                .contains("Task completion rejected: task 46")
        }));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "task_completion_rejected_missing_artifacts")
        );
    }

    #[test]
    fn completed_task_sends_freeze_notice_to_owner_and_lead_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "86",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "86")).expect("task");
        task.subject = "Cycle 18 method package".to_string();
        write_json_atomic(&task_path(team_dir, "86"), &task).expect("write task");

        let changed = set_task_status_if_open(
            team_dir,
            "86",
            TaskStatus::Completed,
            Some("final handoff manifest hash abc123"),
        )
        .expect("set completed");
        assert!(changed);
        let changed_again =
            set_task_status_if_open(team_dir, "86", TaskStatus::Completed, Some("duplicate"))
                .expect("set completed again");
        assert!(!changed_again);

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let owner_messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
            .expect("owner mailbox");
        let lead_freezes = lead_messages
            .iter()
            .filter(|message| {
                message.message.contains("TASK_COMPLETION_FREEZE")
                    && message.message.contains("task 86")
                    && message.message.contains("completed by @engineering")
                    && message.message.contains("reopen the task before")
                    && message.message.contains("silent post-completion mutation")
            })
            .count();
        let owner_freezes = owner_messages
            .iter()
            .filter(|message| {
                message.message.contains("TASK_COMPLETION_FREEZE")
                    && message.message.contains("Cycle 18 method package")
                    && message
                        .message
                        .contains("Do not mutate completed task artifacts")
                    && message.message.contains("LEAD_PROPOSAL")
                    && message.message.contains("old/new hashes")
                    && message.message.contains("failed-attempt provenance")
            })
            .count();
        assert_eq!(lead_freezes, 1);
        assert_eq!(owner_freezes, 1);

        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let freeze_events = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("task_completion_freeze_notified")
                    && event
                        .get("data")
                        .and_then(|data| data.get("task"))
                        .and_then(|task| task.as_str())
                        == Some("86")
            })
            .count();
        assert_eq!(freeze_events, 1);
    }

    #[test]
    fn member_resume_reuses_existing_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-resume".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "engineering".to_string(),
                    role: "engineering".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "3"),
            &TeamTask {
                id: "3".to_string(),
                subject: "Department mission for engineering: Implement app.\n\nOperate as one department-level Codex session.".to_string(),
                description: String::new(),
                owner: Some("engineering".to_string()),
                status: TaskStatus::Blocked,
                depends_on: Vec::new(),
                result: Some("Waiting for schema handoff.".to_string()),
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");

        resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "engineering".to_string(),
                mission: Some("Implement app after schema handoff.".to_string()),
            },
        )
        .expect("resume");

        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "3");
        assert_eq!(tasks[0].status, TaskStatus::InProgress);
        assert!(
            tasks[0]
                .result
                .as_deref()
                .unwrap_or_default()
                .contains("Resumed without creating a duplicate task")
        );
        let events =
            read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("read events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "task_reused_for_resume")
        );
        assert!(events.iter().any(|event| {
            event.event == "member_resumed"
                && event
                    .data
                    .get("reused_task")
                    .and_then(|value| value.as_bool())
                    == Some(true)
        }));
    }

    #[test]
    fn member_resume_with_multiple_open_tasks_still_does_not_create_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let older =
            (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let config = TeamConfig {
            version: 1,
            id: "team-resume-multi".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "quality".to_string(),
                    role: "quality".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        for (id, updated_at) in [("4", older.clone()), ("6", now.clone())] {
            write_json_atomic(
                &task_path(team_dir, id),
                &TeamTask {
                    id: id.to_string(),
                    subject: format!("quality task {id}"),
                    description: String::new(),
                    owner: Some("quality".to_string()),
                    status: TaskStatus::Blocked,
                    depends_on: Vec::new(),
                    result: None,
                    created_at: updated_at.clone(),
                    updated_at,
                },
            )
            .expect("write task");
        }

        resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "quality".to_string(),
                mission: Some("Run QA after engineering handoff.".to_string()),
            },
        )
        .expect("resume");

        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 2);
        let latest = tasks.iter().find(|task| task.id == "6").expect("task 6");
        assert_eq!(latest.status, TaskStatus::InProgress);
        assert!(
            latest
                .result
                .as_deref()
                .unwrap_or_default()
                .contains("Resumed without creating a duplicate task")
        );
    }

    #[test]
    fn member_resume_reuses_task_id_named_in_mission() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "131",
            Some("engineering"),
            TaskStatus::Blocked,
            Vec::new(),
            Some("Waiting for explicit lead clearance."),
        );

        resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "engineering".to_string(),
                mission: Some(
                    "Task131 CLEARANCE: execute exactly the cleared command.".to_string(),
                ),
            },
        )
        .expect("resume referenced task");

        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 1);
        let task = tasks.iter().find(|task| task.id == "131").expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(
            task.result
                .as_deref()
                .unwrap_or_default()
                .contains("Resumed referenced task without creating a duplicate")
        );
    }

    #[test]
    fn member_resume_refuses_to_duplicate_completed_referenced_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "131",
            Some("engineering"),
            TaskStatus::Completed,
            Vec::new(),
            Some("Frozen handoff."),
        );

        let err = resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "engineering".to_string(),
                mission: Some("Task131 CLEARANCE: run the repair job.".to_string()),
            },
        )
        .expect_err("completed referenced task should require explicit reopen");

        assert!(err.to_string().contains("already completed"));
        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 1);
        assert!(!task_path(team_dir, "132").exists());
    }

    #[test]
    fn worker_output_waiting_for_lead_clearance_counts_as_blocked() {
        let mut buffers = HashMap::new();
        buffers.insert(
            "runtime".to_string(),
            "Task131 remains blocked until explicit lead clearance with exact command.".to_string(),
        );

        assert!(member_turn_reports_blocked(&buffers, "runtime"));
    }

    #[test]
    fn lead_autonomy_tick_reports_open_tasks_even_with_active_turns() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let worker = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-tick".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), worker.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "1"),
            &TeamTask {
                id: "1".to_string(),
                subject: "research cycle".to_string(),
                description: String::new(),
                owner: Some("research".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
        send_team_message_to_dir(
            team_dir,
            "research",
            "lead",
            "LEAD_PROPOSAL: task 1 appears ready for review; resume audit with the current evidence.",
        )
        .expect("send proposal");
        send_team_message_to_dir(
            team_dir,
            "@system",
            "lead",
            "Lead autonomy tick instructions mention `LEAD_PROPOSAL:` and task 1, but this is not a teammate proposal.",
        )
        .expect("send system instruction");
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "research",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Try again later.",
                "retry_after_sec": 1200,
            }),
        )
        .expect("usage event");

        let mut active = HashMap::new();
        active.insert(
            "research".to_string(),
            AppServerMemberRun {
                member: worker,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );
        let mut last_tick = Instant::now() - Duration::from_secs(181);
        maybe_send_lead_autonomy_tick(
            team_dir,
            &config,
            &active,
            &mut last_tick,
            Duration::from_secs(180),
            TeamPromptLanguage::En,
        )
        .expect("lead tick");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(lead_messages.len(), 3);
        let tick = lead_messages.last().expect("tick message");
        assert!(tick.message.contains("Lead autonomy tick"));
        assert!(tick.message.contains("research cycle"));
        assert!(tick.message.contains("Open task owner cooldowns"));
        assert!(
            tick.message
                .contains("@research owns open task(s) #1 but is in usage-limit cooldown")
        );
        assert!(tick.message.contains("Recent LEAD_PROPOSAL signals"));
        assert!(tick.message.contains("task 1 appears ready for review"));
        assert!(!tick.message.contains("this is not a teammate proposal"));

        let task_proposals =
            collect_recent_lead_proposals_for_task(team_dir, "lead", "1", 5).expect("proposals");
        assert_eq!(task_proposals.len(), 1);
        assert!(task_proposals[0].contains("@research"));
    }

    #[test]
    fn lead_autonomy_tick_surfaces_next_action_when_goal_continues() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let audit_dir = team_dir.join("audit");
        fs::create_dir_all(&audit_dir).expect("audit dir");
        fs::write(
            audit_dir.join("cycle0_audit.md"),
            "Verdict: PASS_WITH_WARNINGS\n\nRecommended next action: launch cycle1 with a public articulated-object sequence and frozen evaluation gates.\n",
        )
        .expect("audit");
        let created = now();
        let config = TeamConfig {
            version: 1,
            id: "team-continuation".to_string(),
            goal: "Keep iterating: research -> docker -> runtime -> audit -> next cycle until blocker.".to_string(),
            lead: "lead".to_string(),
            members: vec![TeamMember {
                name: "lead".to_string(),
                role: "lead".to_string(),
                status: MemberStatus::Running,
                joined_at: created.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            }],
            language: None,
            created_at: created.clone(),
            updated_at: created,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: audit_dir.display().to_string(),
                owner: "audit".to_string(),
                note: "cycle0 final audit".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");

        let active = HashMap::new();
        let mut last_tick = Instant::now() - Duration::from_secs(181);
        maybe_send_lead_autonomy_tick(
            team_dir,
            &config,
            &active,
            &mut last_tick,
            Duration::from_secs(180),
            TeamPromptLanguage::En,
        )
        .expect("lead tick");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let tick = lead_messages.last().expect("tick message");
        assert!(tick.message.contains("Recent artifact next-action signals"));
        assert!(tick.message.contains("public articulated-object sequence"));
        assert!(
            tick.message
                .contains("explicitly requests continuation/iteration")
        );
        assert!(tick.message.contains("Open tasks:\n- none"));
    }

    #[test]
    fn continuation_detection_accepts_continue_iterating_prompt() {
        assert!(team_goal_requests_continuation(
            "Continue iterating: research/planning -> Docker/build -> container experiment -> evaluation/audit -> next action."
        ));
        assert!(team_goal_requests_continuation(
            "Keep iterating until a real blocker requiring user input appears."
        ));
        assert!(team_goal_requests_continuation(
            "自動研究として phase0.md と phase1.md を使い、deep_thinker の結果を待ってから ssh saitou 上で Dockerfile を作り、container 内で実験を繰り返す。"
        ));
        assert!(!team_goal_requests_continuation(
            "Run one bounded review and stop for user approval."
        ));
    }

    #[test]
    fn autoresearch_goal_gets_explicit_loop_policy_in_prompts() {
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Online,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let research = TeamMember {
            name: "research_planning".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Online,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-autoresearch-policy".to_string(),
            goal: "自動研究: /home/yukimaru/research_prompt/phase0.md と /home/yukimaru/research_prompt/phase1.md を使い、deep_thinker/deep_researcher、ssh saitou Dockerfile build、container 実験、結果を見て次実験を永遠に繰り返す。".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), research.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let task = TeamTask {
            id: "1".to_string(),
            subject: "phase0 scans".to_string(),
            description: "Run Fixed-4 + Flexible-2 scans and save prompt/result artifacts."
                .to_string(),
            owner: Some("research_planning".to_string()),
            status: TaskStatus::InProgress,
            depends_on: Vec::new(),
            result: None,
            created_at: now.clone(),
            updated_at: now,
        };

        let worker_prompt = build_worker_prompt(&config, std::slice::from_ref(&task), &research);
        assert!(worker_prompt.contains("Autoresearch loop policy"));
        assert!(worker_prompt.contains("Fixed-4 + Flexible-2"));
        assert!(worker_prompt.contains("container-internal department"));
        assert!(worker_prompt.contains("Do not treat the loop as done"));

        let lead_prompt = build_app_server_lead_prompt(
            &config,
            &[task],
            &lead,
            Path::new("/tmp/codex"),
            TeamPromptLanguage::En,
        );
        assert!(lead_prompt.contains("Autoresearch loop policy"));
        assert!(lead_prompt.contains("Environment phase"));
        assert!(lead_prompt.contains("Runtime phase"));
        assert!(lead_prompt.contains("Reflection phase"));

        let design_prompt =
            build_lead_department_design_prompt(&config.goal, &[], TeamPromptLanguage::En);
        assert!(design_prompt.contains("Additional constraints for autoresearch goals"));
        assert!(design_prompt.contains("local research/planning department"));
        assert!(design_prompt.contains("saitou"));
        assert!(design_prompt.contains("Do not create local placeholder"));
        assert!(design_prompt.contains("Do not read or invoke ordinary user-facing team-launch"));
        assert!(!design_prompt.contains("codex-team-secretary"));
        assert!(!design_prompt.to_lowercase().contains("secretary"));
    }

    #[test]
    fn department_design_prompt_masks_user_facing_team_secretary_trigger_words() {
        let prompt = build_lead_department_design_prompt(
            "Use codex-team-secretary style secretary routing for this team.",
            &[],
            TeamPromptLanguage::En,
        );
        let lower = prompt.to_lowercase();
        assert!(!lower.contains("codex-team-secretary"));
        assert!(!lower.contains("secretary"));
        assert!(lower.contains("coordinator"));
    }

    #[test]
    fn department_design_drops_future_container_local_placeholders() {
        let mut args = StartArgs {
            goal: "Build a Docker image on ssh saitou, create a container, then do implementation and tests inside the container.".to_string(),
            id: None,
            members: Vec::new(),
            nodes: Vec::new(),
            tasks: Vec::new(),
            language: None,
        };
        let design = LeadDepartmentDesign {
            nodes: vec![LeadNodeDesign {
                id: "saitou".to_string(),
                kind: TeamNodeKind::Ssh,
                host: Some("saitou".to_string()),
                container: None,
                cwd: None,
                note: "remote build host".to_string(),
            }],
            departments: vec![
                LeadDepartment {
                    name: "ops".to_string(),
                    role: "ops".to_string(),
                    mission: "Build the Docker image on saitou and report TEAM_NODE when the container exists.".to_string(),
                    node: Some("saitou".to_string()),
                },
                LeadDepartment {
                    name: "implementation".to_string(),
                    role: "engineering".to_string(),
                    mission: "Wait for the future container and implement the API inside the container.".to_string(),
                    node: Some("local".to_string()),
                },
                LeadDepartment {
                    name: "quality".to_string(),
                    role: "quality".to_string(),
                    mission: "Run tests inside the Docker container after it is created.".to_string(),
                    node: Some("local".to_string()),
                },
            ],
        };

        apply_department_design(&mut args, design);

        assert_eq!(args.members, vec!["ops:ops@saitou"]);
        assert_eq!(args.tasks.len(), 1);
        assert!(args.tasks[0].contains("Build the Docker image"));
    }

    #[test]
    fn department_design_keeps_local_audit_of_container_results() {
        let mut args = StartArgs {
            goal: "Build a Docker image on ssh saitou, create a container, then implement and test the app inside the container.".to_string(),
            id: None,
            members: Vec::new(),
            nodes: Vec::new(),
            tasks: Vec::new(),
            language: None,
        };
        let design = LeadDepartmentDesign {
            nodes: vec![LeadNodeDesign {
                id: "saitou".to_string(),
                kind: TeamNodeKind::Ssh,
                host: Some("saitou".to_string()),
                container: None,
                cwd: None,
                note: "remote build host".to_string(),
            }],
            departments: vec![
                LeadDepartment {
                    name: "ops".to_string(),
                    role: "ops".to_string(),
                    mission: "Build the Docker image on saitou and report TEAM_NODE when the container exists.".to_string(),
                    node: Some("saitou".to_string()),
                },
                LeadDepartment {
                    name: "quality_audit".to_string(),
                    role: "quality".to_string(),
                    mission: "Review the container-internal department's implementation, pytest logs, HTTP smoke test results, artifacts, and completion evidence from local.".to_string(),
                    node: Some("local".to_string()),
                },
            ],
        };

        apply_department_design(&mut args, design);

        assert_eq!(
            args.members,
            vec!["ops:ops@saitou", "quality_audit:quality"]
        );
        assert_eq!(args.tasks.len(), 2);
        assert!(args.tasks[1].contains("pytest logs"));
    }

    #[test]
    fn reactive_prompt_compacts_large_mailbox_messages() {
        let member = TeamMember {
            name: "worker".to_string(),
            role: "engineering".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let messages = (0..(MAX_REACTIVE_PROMPT_MESSAGES + 3))
            .map(|idx| MailMessage {
                from: "lead".to_string(),
                to: "worker".to_string(),
                message: format!("message-{idx} {}", "x".repeat(2_000)),
                timestamp: now(),
                read: false,
            })
            .collect::<Vec<_>>();

        let prompt = build_reactive_steer_prompt(&member, &messages, TeamPromptLanguage::En);

        assert!(prompt.contains("older message(s) are omitted"));
        assert!(!prompt.contains("message-0"));
        assert!(prompt.contains("message-14"));
        assert!(prompt.contains("..."));
        assert!(prompt.len() < 20_000);
    }

    #[test]
    fn lead_proposal_collection_ignores_resolved_old_signals() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let created = now();
        let config = TeamConfig {
            version: 1,
            id: "team-proposals".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: created.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "runtime".to_string(),
                    role: "runtime".to_string(),
                    status: MemberStatus::Online,
                    joined_at: created.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: created.clone(),
            updated_at: created,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        send_team_message_to_dir(
            team_dir,
            "runtime",
            "lead",
            "LEAD_PROPOSAL: task 10 is ready; resume runtime after sync.",
        )
        .expect("send old proposal");
        send_team_message_to_dir(
            team_dir,
            "lead",
            "runtime",
            "Earlier LEAD_PROPOSAL items are accepted/addressed by the current dependency graph; no separate action needed.",
        )
        .expect("send resolution");
        std::thread::sleep(Duration::from_secs(1));
        send_team_message_to_dir(
            team_dir,
            "runtime",
            "lead",
            "LEAD_PROPOSAL: task 11 is ready; resume validator after runtime handoff.",
        )
        .expect("send new proposal");

        let proposals =
            collect_recent_lead_proposals(team_dir, "lead", 5).expect("collect proposals");
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].contains("task 11"));
        assert!(!proposals[0].contains("task 10"));
    }

    #[test]
    fn lead_autonomy_tick_is_suppressed_during_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let worker = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-cooldown".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), worker],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut active = HashMap::new();
        active.insert(
            "lead".to_string(),
            AppServerMemberRun {
                member: lead,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: true,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "usage_limited".to_string(),
                last_stale_notice_at: None,
                retry_not_before: Some(Instant::now() + Duration::from_secs(300)),
                side_context_ids: Vec::new(),
            },
        );
        let mut last_tick = Instant::now() - Duration::from_secs(181);

        maybe_send_lead_autonomy_tick(
            team_dir,
            &config,
            &active,
            &mut last_tick,
            Duration::from_secs(180),
            TeamPromptLanguage::En,
        )
        .expect("lead tick");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "lead_autonomy_tick_suppressed")
        );
    }

    #[test]
    fn lead_autonomy_tick_skips_when_lead_turn_is_active() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-active-lead".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut active = HashMap::new();
        active.insert(
            "lead".to_string(),
            AppServerMemberRun {
                member: lead,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "lead_tick".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );
        let mut last_tick = Instant::now() - Duration::from_secs(181);

        maybe_send_lead_autonomy_tick(
            team_dir,
            &config,
            &active,
            &mut last_tick,
            Duration::from_secs(180),
            TeamPromptLanguage::En,
        )
        .expect("lead tick");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "lead_autonomy_tick_skipped"
                    && event.data.get("reason").and_then(|value| value.as_str())
                        == Some("lead_turn_active"))
        );
    }

    #[test]
    fn token_usage_panel_groups_latest_turn_usage_by_feature() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let member = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: Some("thread-1".to_string()),
            workspace_path: None,
            node: None,
        };
        let run = AppServerMemberRun {
            member: member.clone(),
            node_id: "local".to_string(),
            cwd: team_dir.to_path_buf(),
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            completed: false,
            failed: false,
            standby_after_turn: false,
            usage_category: "lead_tick".to_string(),
            team_message_scan_offset: 0,
            last_activity_at: Instant::now(),
            last_activity_kind: "turn_started".to_string(),
            last_stale_notice_at: None,
            retry_not_before: None,
            side_context_ids: Vec::new(),
        };
        let mut active = HashMap::new();
        active.insert(member.name.clone(), run);
        let mut thread_to_member = HashMap::new();
        thread_to_member.insert(thread_key("local", "thread-1"), member.name.clone());
        let side_replies = HashMap::new();

        for total in [10, 15, 15] {
            record_token_usage_update(
                team_dir,
                "local",
                ThreadTokenUsageUpdatedNotification {
                    thread_id: "thread-1".to_string(),
                    turn_id: "turn-1".to_string(),
                    token_usage: codex_app_server_protocol::ThreadTokenUsage {
                        total: TokenUsageBreakdown {
                            total_tokens: total,
                            input_tokens: total - 4,
                            cached_input_tokens: 2,
                            output_tokens: 4,
                            reasoning_output_tokens: 0,
                        },
                        last: TokenUsageBreakdown {
                            total_tokens: total,
                            input_tokens: total - 4,
                            cached_input_tokens: 2,
                            output_tokens: 4,
                            reasoning_output_tokens: 0,
                        },
                        model_context_window: Some(128_000),
                    },
                },
                &active,
                &side_replies,
                &thread_to_member,
            )
            .expect("record usage");
        }
        for idx in 0..MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT {
            append_jsonl(
                &side_channel_context_path(team_dir, "research"),
                &SideChannelContextRecord {
                    id: format!("ctx-{idx}"),
                    member: "research".to_string(),
                    node: "local".to_string(),
                    source_thread: "thread-1".to_string(),
                    side_thread: format!("side-thread-{idx}"),
                    side_turn: format!("side-turn-{idx}"),
                    recipients: vec!["lead".to_string()],
                    incoming_summary: "please confirm".to_string(),
                    reply: "confirmed".to_string(),
                    created_at: now(),
                    status: SideChannelContextStatus::Pending,
                    injected_turns: Vec::new(),
                    injected_at: None,
                    acknowledged_at: None,
                },
            )
            .expect("append context");
        }

        let html = render_token_usage_panel(team_dir);
        assert!(html.contains("lead_tick"));
        assert!(html.contains("research"));
        assert!(html.contains("Token Bottlenecks"));
        assert!(html.contains("Side-channel Context Pressure"));
        assert!(html.contains("capped at 8"));
        assert!(html.contains(">25</"));
        assert!(!html.contains(">40</"));
    }

    #[test]
    fn resume_runtime_base_cwd_prefers_saved_lead_workspace() {
        let fallback = PathBuf::from("/tmp/current-shell");
        let config = TeamConfig {
            version: 1,
            id: "team-resume-cwd".to_string(),
            goal: "resume cwd test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now(),
                    thread_id: None,
                    workspace_path: Some("/home/yukimaru/research/project".to_string()),
                    node: Some("local".to_string()),
                },
                TeamMember {
                    name: "evaluation".to_string(),
                    role: "quality".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now(),
                    thread_id: None,
                    workspace_path: Some("/home/yukimaru/research/project/eval".to_string()),
                    node: Some("local".to_string()),
                },
            ],
            language: None,
            created_at: now(),
            updated_at: now(),
        };

        assert_eq!(
            resume_runtime_base_cwd(&config, &fallback),
            PathBuf::from("/home/yukimaru/research/project")
        );
    }

    #[test]
    fn thread_usage_rotation_limit_uses_total_and_context_ratio() {
        let mut record = TeamTokenUsageRecord {
            timestamp: now(),
            member: "lead".to_string(),
            role: "lead".to_string(),
            node: "local".to_string(),
            thread: "thread-1".to_string(),
            turn: "turn-1".to_string(),
            category: "lead_tick".to_string(),
            source: "active_turn".to_string(),
            total: TeamTokenUsageBreakdown {
                total_tokens: MAX_APP_SERVER_THREAD_TOTAL_TOKENS - 1,
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
            },
            last: TeamTokenUsageBreakdown::default(),
            model_context_window: Some(1_000_000),
        };
        assert!(!thread_usage_exceeds_rotation_limit(&record));

        record.total.total_tokens = MAX_APP_SERVER_THREAD_TOTAL_TOKENS;
        assert!(thread_usage_exceeds_rotation_limit(&record));

        record.total.total_tokens = 700;
        record.model_context_window = Some(1_000);
        assert!(thread_usage_exceeds_rotation_limit(&record));
    }

    #[test]
    fn active_turn_token_pressure_detects_oversized_thread_before_steering() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let member = TeamMember {
            name: "worker".to_string(),
            role: "engineering".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: Some("thread-1".to_string()),
            workspace_path: None,
            node: None,
        };
        let run = AppServerMemberRun {
            member: member.clone(),
            node_id: "local".to_string(),
            cwd: team_dir.to_path_buf(),
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            completed: false,
            failed: false,
            standby_after_turn: false,
            usage_category: "team_message".to_string(),
            team_message_scan_offset: 0,
            last_activity_at: Instant::now(),
            last_activity_kind: "turn_started".to_string(),
            last_stale_notice_at: None,
            retry_not_before: None,
            side_context_ids: Vec::new(),
        };
        let mut active = HashMap::new();
        active.insert(member.name.clone(), run.clone());
        let mut thread_to_member = HashMap::new();
        thread_to_member.insert(thread_key("local", "thread-1"), member.name);

        record_token_usage_update(
            team_dir,
            "local",
            ThreadTokenUsageUpdatedNotification {
                thread_id: "thread-1".to_string(),
                turn_id: "turn-1".to_string(),
                token_usage: codex_app_server_protocol::ThreadTokenUsage {
                    total: TokenUsageBreakdown {
                        total_tokens: MAX_APP_SERVER_THREAD_TOTAL_TOKENS + 1,
                        input_tokens: MAX_APP_SERVER_THREAD_TOTAL_TOKENS + 1,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_output_tokens: 0,
                    },
                    last: TokenUsageBreakdown {
                        total_tokens: 0,
                        input_tokens: 0,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_output_tokens: 0,
                    },
                    model_context_window: Some(258_400),
                },
            },
            &active,
            &HashMap::new(),
            &thread_to_member,
        )
        .expect("record usage");

        assert_eq!(
            active_turn_token_pressure(team_dir, &run).expect("token pressure"),
            Some(MAX_APP_SERVER_THREAD_TOTAL_TOKENS + 1)
        );
    }

    #[test]
    fn active_turn_recent_steer_rate_limit_detects_hot_thread() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let member = TeamMember {
            name: "worker".to_string(),
            role: "engineering".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: Some("thread-1".to_string()),
            workspace_path: None,
            node: None,
        };
        let run = AppServerMemberRun {
            member,
            node_id: "local".to_string(),
            cwd: team_dir.to_path_buf(),
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            completed: false,
            failed: false,
            standby_after_turn: false,
            usage_category: "team_message".to_string(),
            team_message_scan_offset: 0,
            last_activity_at: Instant::now(),
            last_activity_kind: "turn_started".to_string(),
            last_stale_notice_at: None,
            retry_not_before: None,
            side_context_ids: Vec::new(),
        };
        append_event(
            team_dir,
            "app_server_turn_steered",
            serde_json::json!({
                "member": "worker",
                "node": "local",
                "thread": "thread-1",
                "turn": "turn-1",
                "messages": 1,
            }),
        )
        .expect("steer event");

        let remaining = active_turn_recently_steered(team_dir, &run, Duration::from_secs(30))
            .expect("rate limit")
            .expect("recent steer");
        assert!(remaining <= Duration::from_secs(30));
    }

    #[test]
    fn usage_category_for_messages_identifies_system_triggers() {
        let base = MailMessage {
            from: "system".to_string(),
            to: "lead".to_string(),
            message: "Lead autonomy tick: inspect work".to_string(),
            timestamp: now(),
            read: false,
        };
        assert_eq!(
            usage_category_for_messages("member_reactive", &[base]),
            "lead_tick"
        );

        let user = MailMessage {
            from: "user".to_string(),
            to: "lead".to_string(),
            message: "please continue".to_string(),
            timestamp: now(),
            read: false,
        };
        assert_eq!(
            usage_category_for_messages("lead_reactive", &[user]),
            "user_message"
        );

        let stay = MailMessage {
            from: "research".to_string(),
            to: "lead".to_string(),
            message: "STAY: no open task or blocker.".to_string(),
            timestamp: now(),
            read: false,
        };
        assert_eq!(
            usage_category_for_messages("member_reactive", &[stay]),
            "team_noop_stay"
        );

        let handoff = MailMessage {
            from: "runtime".to_string(),
            to: "lead".to_string(),
            message: "TEAM_COMPLETION_CHECKLIST: final handoff ready".to_string(),
            timestamp: now(),
            read: false,
        };
        assert_eq!(
            usage_category_for_messages("member_reactive", &[handoff]),
            "team_handoff"
        );
    }

    #[test]
    fn usage_limit_cooldown_is_restored_from_recent_event_after_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let retry_at = (Local::now() + chrono::Duration::hours(2))
            .format("%B %-d, %Y %-I:%M %p")
            .to_string();
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "lead",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": format!("You've hit your usage limit. Visit settings or try again at {retry_at}."),
                "retry_after_sec": 2700,
            }),
        )
        .expect("usage event");

        let remaining = recent_usage_limit_retry_remaining_with_auth(team_dir, "lead", None)
            .expect("restore cooldown");

        assert!(remaining.is_some_and(|remaining| remaining > Duration::from_secs(90 * 60)));
    }

    #[test]
    fn usage_limit_cooldown_is_ignored_after_auth_refresh() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let event_time = Utc::now() - chrono::Duration::hours(1);
        let event = Event {
            event: "app_server_member_usage_limited",
            timestamp: event_time.to_rfc3339_opts(SecondsFormat::Secs, true),
            team: "team-task-test",
            data: serde_json::json!({
                "member": "lead",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Visit settings or try again at May 12th, 2026 7:43 AM.",
                "retry_after_sec": 7200,
            }),
        };
        append_jsonl(&team_dir.join("events.jsonl"), &event).expect("usage event");
        let auth_json = team_dir.join("auth.json");
        fs::write(&auth_json, "{}").expect("auth refresh");

        let remaining =
            recent_usage_limit_retry_remaining_with_auth(team_dir, "lead", Some(&auth_json))
                .expect("cooldown check");

        assert!(remaining.is_none());
    }

    #[test]
    fn usage_limit_cooldown_is_ignored_after_node_device_auth_refresh() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("config");
        config.members.push(TeamMember {
            name: "docker_build".to_string(),
            role: "ops".to_string(),
            status: MemberStatus::Standby,
            joined_at: now(),
            thread_id: None,
            workspace_path: None,
            node: Some("saitou".to_string()),
        });
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        let event_time = Utc::now() - chrono::Duration::hours(1);
        let event = Event {
            event: "app_server_member_usage_limited",
            timestamp: event_time.to_rfc3339_opts(SecondsFormat::Secs, true),
            team: "team-task-test",
            data: serde_json::json!({
                "member": "docker_build",
                "node": "saitou",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Visit settings or try again at May 12th, 2026 7:43 AM.",
                "retry_after_sec": 7200,
            }),
        };
        append_jsonl(&team_dir.join("events.jsonl"), &event).expect("usage event");
        append_event(
            team_dir,
            "node_direct_device_auth_completed",
            serde_json::json!({
                "node": "saitou",
                "url": "https://auth.openai.com/codex/device",
                "log": "/tmp/node-saitou.log",
            }),
        )
        .expect("auth event");

        let remaining =
            recent_usage_limit_retry_remaining_with_auth(team_dir, "docker_build", None)
                .expect("cooldown check");

        assert!(remaining.is_none());
    }

    #[test]
    fn usage_limit_cooldown_uses_retry_at_time_when_present() {
        let error = "You've hit your usage limit. Visit settings or try again at 4:43 PM.";
        let now_secs = 16 * 60 * 60 + 16 * 60;

        let cooldown = usage_limit_cooldown_from_error(error, &[now_secs]);

        assert_eq!(cooldown, Duration::from_secs(27 * 60));
    }

    #[test]
    fn usage_limit_cooldown_uses_full_retry_datetime_when_present() {
        let error = "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at May 12th, 2026 7:43 AM.";
        let now = Local
            .with_ymd_and_hms(2026, 5, 9, 4, 23, 0)
            .single()
            .expect("local datetime");

        let cooldown = usage_limit_cooldown_from_error_at(error, now, &[4 * 60 * 60 + 23 * 60]);

        assert_eq!(
            cooldown,
            Duration::from_secs((3 * 24 * 60 * 60) + (3 * 60 * 60) + (20 * 60))
        );
    }

    #[test]
    fn usage_limit_cooldown_rolls_retry_time_to_next_day() {
        let error = "You've hit your usage limit. Please try again at 7:43 AM.";
        let now_secs = 23 * 60 * 60 + 50 * 60;

        let cooldown = usage_limit_cooldown_from_error(error, &[now_secs]);

        assert_eq!(cooldown, Duration::from_secs(7 * 60 * 60 + 53 * 60));
    }

    #[test]
    fn usage_limit_cooldown_chooses_shortest_wall_clock_candidate() {
        let error = "You've hit your usage limit. Please try again at 7:43 AM.";
        let jst_now_secs = 16 * 60 * 60 + 21 * 60;
        let utc_now_secs = 7 * 60 * 60 + 21 * 60;

        let cooldown = usage_limit_cooldown_from_error(error, &[jst_now_secs, utc_now_secs]);

        assert_eq!(cooldown, Duration::from_secs(22 * 60));
    }

    #[test]
    fn usage_limit_cooldown_short_backs_off_when_retry_minute_just_passed() {
        let error = "You've hit your usage limit. Please try again at 7:43 AM.";
        let utc_now_secs = 7 * 60 * 60 + 43 * 60 + 3;

        let cooldown = usage_limit_cooldown_from_error(error, &[utc_now_secs]);

        assert_eq!(cooldown, Duration::from_secs(5 * 60));
    }

    #[test]
    fn usage_limit_cooldown_falls_back_without_retry_time() {
        let error = "You've hit your usage limit. Visit settings to purchase more credits.";

        let cooldown = usage_limit_cooldown_from_error(error, &[0]);

        assert_eq!(cooldown, Duration::from_secs(45 * 60));
    }

    #[test]
    fn stale_active_turn_warns_lead_and_member() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let worker = TeamMember {
            name: "capture".to_string(),
            role: "capture".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-stale".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![lead, worker.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "3"),
            &TeamTask {
                id: "3".to_string(),
                subject: "capture plan".to_string(),
                description: String::new(),
                owner: Some("capture".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");

        let mut active = HashMap::new();
        active.insert(
            "capture".to_string(),
            AppServerMemberRun {
                member: worker,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                usage_category: "test".to_string(),
                team_message_scan_offset: 0,
                last_activity_at: Instant::now() - Duration::from_secs(900),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );

        let mut last_check = Instant::now() - Duration::from_secs(31);
        maybe_warn_stale_active_turns(
            team_dir,
            &config,
            &mut active,
            &mut last_check,
            Duration::from_secs(30),
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("stale warning");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let worker_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "capture")).expect("worker mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(worker_messages.len(), 1);
        assert!(
            lead_messages[0]
                .message
                .contains("Stale active turn attention")
        );
        assert_eq!(worker_messages[0].from, "lead");
        assert!(
            worker_messages[0]
                .message
                .contains("Automated lead status check")
        );

        active
            .get_mut("capture")
            .expect("active capture")
            .last_stale_notice_at = Some(Instant::now() - Duration::from_secs(601));
        let mut last_check = Instant::now() - Duration::from_secs(31);
        maybe_warn_stale_active_turns(
            team_dir,
            &config,
            &mut active,
            &mut last_check,
            Duration::from_secs(30),
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("repeated stale warning");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(lead_messages.len(), 2);
        assert!(lead_messages[1].message.contains("Escalation:"));
        assert!(
            lead_messages[1]
                .message
                .contains("cancel/reassign/recover the task")
        );
    }
}
