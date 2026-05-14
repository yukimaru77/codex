fn create_team(root: &Path, args: StartArgs) -> Result<(String, PathBuf)> {
    fs::create_dir_all(root)?;
    let team_id = match args.id {
        Some(id) => sanitize_id(&id),
        None => format!("team-{}", tokyo_now().format("%Y%m%d%H%M%S")),
    };
    let team_dir = root.join(&team_id);
    if team_dir.exists() {
        bail!("team `{team_id}` already exists");
    }

    fs::create_dir_all(team_dir.join("tasks"))?;
    fs::create_dir_all(team_dir.join("mailboxes"))?;
    fs::create_dir_all(team_dir.join("logs"))?;
    fs::create_dir_all(team_dir.join("last_messages"))?;
    fs::create_dir_all(team_dir.join("live_messages"))?;
    fs::create_dir_all(team_dir.join("jobs"))?;
    fs::create_dir_all(team_dir.join("waits"))?;
    write_json_atomic(
        &team_dir.join("ownerships.json"),
        &Vec::<FileOwnership>::new(),
    )?;
    let mut nodes = Vec::<TeamNode>::new();
    for raw_node in args.nodes {
        nodes.push(parse_node_spec(&raw_node, &now())?);
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    write_json_atomic(&team_dir.join("nodes.json"), &nodes)?;

    let now = now();
    let mut members = vec![TeamMember {
        name: "lead".to_string(),
        role: "lead".to_string(),
        status: MemberStatus::Online,
        joined_at: now.clone(),
        thread_id: None,
        workspace_path: None,
        node: Some("local".to_string()),
    }];
    for raw_member in args.members {
        members.push(parse_member(&raw_member, &now)?);
    }

    let config = TeamConfig {
        version: 1,
        id: team_id.clone(),
        goal: args.goal.clone(),
        lead: "lead".to_string(),
        members,
        language: args.language,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    write_json_atomic(&team_dir.join("config.json"), &config)?;

    let initial_tasks = if args.tasks.is_empty() {
        vec![args.goal]
    } else {
        args.tasks
    };
    for subject in initial_tasks {
        create_task(
            &team_dir,
            TaskAddArgs {
                subject,
                description: String::new(),
                owner: None,
                depends_on: Vec::new(),
            },
        )?;
    }

    append_event(
        &team_dir,
        "team_started",
        serde_json::json!({ "goal": config.goal, "members": config.members }),
    )?;

    Ok((team_id, team_dir))
}

fn ensure_team_prompt_language(
    team_dir: &Path,
    override_language: Option<TeamPromptLanguage>,
) -> Result<TeamPromptLanguage> {
    let mut config = load_config(team_dir)?;
    let language = override_language.or(config.language).unwrap_or_default();
    if config.language != Some(language) {
        config.language = Some(language);
        config.updated_at = now();
        write_json_atomic(&team_dir.join("config.json"), &config)?;
        append_event(
            team_dir,
            "team_prompt_language_updated",
            serde_json::json!({ "language": language.cli_value() }),
        )?;
    }
    Ok(language)
}

fn apply_natural_language_defaults(args: &mut StartArgs) {
    if args.members.is_empty() {
        apply_department_design(args, fallback_department_design(&args.goal));
        return;
    }

    if args.tasks.is_empty() {
        let goal = args.goal.clone();
        args.tasks = args
            .members
            .iter()
            .map(|member| {
                let name = member.split(':').next().unwrap_or(member);
                format!("Department mission for {name}: contribute to {goal}")
            })
            .collect();
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LeadDepartmentDesign {
    #[serde(default)]
    nodes: Vec<LeadNodeDesign>,
    departments: Vec<LeadDepartment>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LeadDepartment {
    name: String,
    role: String,
    mission: String,
    #[serde(default)]
    node: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LeadNodeDesign {
    id: String,
    kind: TeamNodeKind,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    note: String,
}

fn should_use_lead_department_design(args: &StartArgs) -> bool {
    args.members.is_empty() && args.tasks.is_empty()
}

fn fallback_department_design(goal: &str) -> LeadDepartmentDesign {
    LeadDepartmentDesign {
        nodes: Vec::new(),
        departments: vec![
            LeadDepartment {
                name: "product".to_string(),
                role: "product".to_string(),
                mission: format!(
                    "Clarify the user goal, scope the deliverable, and make product decisions for: {goal}"
                ),
                node: None,
            },
            LeadDepartment {
                name: "engineering".to_string(),
                role: "engineering".to_string(),
                mission: format!(
                    "Implement the primary technical deliverable, using internal subagents or tools when useful, for: {goal}"
                ),
                node: None,
            },
            LeadDepartment {
                name: "quality".to_string(),
                role: "quality".to_string(),
                mission: format!(
                    "Review, test, and identify risks or missing behavior for: {goal}"
                ),
                node: None,
            },
        ],
    }
}

fn apply_department_design(args: &mut StartArgs, design: LeadDepartmentDesign) {
    merge_lead_node_designs(args, &design.nodes);
    let valid_node_ids = args
        .nodes
        .iter()
        .map(|raw| node_spec_id(raw))
        .collect::<HashSet<_>>();
    let mut seen_departments = HashSet::<String>::new();
    let mut departments = design
        .departments
        .into_iter()
        .filter(|department| {
            let name = sanitize_id(&department.name);
            let role = sanitize_role(&department.role);
            if name.is_empty() || name == "lead" || role == "lead" {
                return false;
            }
            if !seen_departments.insert(name) {
                return false;
            }
            let Some(node) = department.node.as_deref().map(sanitize_id) else {
                return true;
            };
            node.is_empty() || node == "local" || valid_node_ids.contains(&node)
        })
        .collect::<Vec<_>>();
    if goal_expects_future_container_work(&args.goal) {
        departments.retain(|department| !is_future_container_local_placeholder(department));
    }
    if departments.is_empty() {
        departments = fallback_department_design(&args.goal).departments;
    }
    departments.truncate(6);

    args.members = departments
        .iter()
        .map(|department| {
            let name = sanitize_id(&department.name);
            let role = sanitize_role(&department.role);
            let node = department.node.as_deref().map(sanitize_id).filter(|node| {
                !node.is_empty()
                    && (node == "local"
                        || args
                            .nodes
                            .iter()
                            .any(|raw| node_spec_id(raw) == node.as_str()))
            });
            match node {
                Some(node) if node != "local" => format!("{name}:{role}@{node}"),
                _ => format!("{name}:{role}"),
            }
        })
        .collect();
    args.tasks = departments
        .iter()
        .map(|department| {
            format!(
                "Department mission for {}: {}\n\nOperate as one department-level Codex session. Proactively coordinate with related departments: broadcast your initial plan, ask even small uncertainty questions before committing to a risky choice, report failures with proposed next steps, and hand off artifacts to their consumers. The user explicitly authorizes departments to use subagents, agent tools, parallel delegation, skills, MCP servers, and internal decomposition for substantial work. If the mission is broad, research-heavy, implementation-heavy, or review-heavy, actively use those available helpers inside this department instead of doing all work in one main thread or asking the lead to create duplicate peer departments for load balancing.",
                sanitize_id(&department.name),
                department.mission
            )
        })
        .collect();
}

fn goal_expects_future_container_work(goal: &str) -> bool {
    let lower = goal.to_ascii_lowercase();
    lower.contains("docker image")
        || lower.contains("dockerfile")
        || lower.contains("docker file")
        || lower.contains("docker build")
        || lower.contains("create a container")
        || lower.contains("build a container")
        || lower.contains("container を作")
        || lower.contains("コンテナを作")
        || lower.contains("コンテナ作成")
        || lower.contains("image を作")
        || lower.contains("イメージを作")
}

fn is_future_container_local_placeholder(department: &LeadDepartment) -> bool {
    let node = department
        .node
        .as_deref()
        .map(sanitize_id)
        .unwrap_or_default();
    if !(node.is_empty() || node == "local") {
        return false;
    }
    let name = sanitize_id(&department.name);
    let role = sanitize_role(&department.role);
    let text = format!(
        "{} {} {}",
        name,
        role,
        department.mission.to_ascii_lowercase()
    );
    let mentions_container_runtime = [
        "container-internal",
        "inside container",
        "inside the container",
        "in the container",
        "container 内",
        "コンテナ内",
        "docker container",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    if !mentions_container_runtime {
        return false;
    }
    if is_local_container_result_review_department(department, &text) {
        return false;
    }
    let placeholder_role = [
        "implementation",
        "implementer",
        "engineering",
        "runtime",
        "experiment",
        "evaluation",
        "quality",
        "qa",
        "test",
        "tester",
        "validation",
    ]
    .iter()
    .any(|needle| name.contains(needle) || role.contains(needle) || text.contains(needle));
    let host_bootstrap_role = ["ops", "bootstrap", "build", "docker", "infra"]
        .iter()
        .any(|needle| name.contains(needle) || role.contains(needle));
    placeholder_role && !host_bootstrap_role
}

fn is_local_container_result_review_department(department: &LeadDepartment, text: &str) -> bool {
    let name = sanitize_id(&department.name);
    let role = sanitize_role(&department.role);
    let reviewer_role = [
        "audit",
        "auditor",
        "review",
        "reviewer",
        "quality",
        "qa",
        "validation",
    ]
    .iter()
    .any(|needle| name.contains(needle) || role.contains(needle));
    if !reviewer_role {
        return false;
    }
    let consumes_outputs = [
        "review",
        "audit",
        "validate",
        "validation",
        "evidence",
        "artifact",
        "result",
        "output",
        "handoff",
        "レビュー",
        "監査",
        "確認",
        "検証",
        "証跡",
        "成果物",
        "結果",
        "完了条件",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    let claims_runtime_execution = [
        "implement inside",
        "develop inside",
        "execute inside",
        "run inside",
        "test inside",
        "container 内で実装",
        "container 内で開発",
        "container 内で実行",
        "container 内でテスト",
        "コンテナ内で実装",
        "コンテナ内で開発",
        "コンテナ内で実行",
        "コンテナ内でテスト",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    consumes_outputs && !claims_runtime_execution
}

#[allow(clippy::too_many_arguments)]
fn run_lead_department_design(
    codex_exe: &Path,
    cwd: &Path,
    goal: &str,
    placement_candidates: &[LeadNodeDesign],
    language: TeamPromptLanguage,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<LeadDepartmentDesign> {
    let output =
        tempfile::NamedTempFile::new().context("create lead department design temp file")?;
    let output_path = output.path().to_path_buf();
    let prompt = build_lead_department_design_prompt(goal, placement_candidates, language);
    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("-o")
        .arg(&output_path);
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
    let status = command
        .arg(prompt)
        .status()
        .context("run lead department design Codex turn")?;
    if !status.success() {
        bail!("lead department design failed with status {status}");
    }
    let raw = fs::read_to_string(&output_path)
        .with_context(|| format!("read {}", output_path.display()))?;
    parse_lead_department_design(&raw)
}

fn build_lead_department_design_prompt(
    goal: &str,
    placement_candidates: &[LeadNodeDesign],
    language: TeamPromptLanguage,
) -> String {
    let autoresearch_policy = build_autoresearch_department_design_policy(goal, language);
    let prompt_goal = sanitize_goal_for_internal_team_prompt(goal);
    let candidates = if placement_candidates.is_empty() {
        "(none explicitly provided by CLI/UI advanced options)".to_string()
    } else {
        placement_candidates
            .iter()
            .map(|node| {
                format!(
                    "- id={} kind={:?} host={} container={} cwd={} note={}",
                    sanitize_id(&node.id),
                    node.kind,
                    node.host.as_deref().unwrap_or(""),
                    node.container.as_deref().unwrap_or(""),
                    node.cwd.as_deref().unwrap_or(""),
                    node.note
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if language.is_ja() {
        return format!(
            r#"あなたは、ユーザーの依頼を直接聞く lead agent です。ユーザーは実質的に社長です。あなたの仕事は、依頼全体を理解し、運用計画を決め、正しい実行場所に部署を作ることです。あなたはオーケストレーターであり、実装作業者でも、単純な作業分配係でもありません。

これは Codex Teams 内部の部署設計プロンプトです。通常ユーザー向けの team 起動ワークフローを読んだり実行したりしてはいけません。既存 team の探索、team swarm/run の起動、status 確認は不要です。この turn では下記 goal を読んで JSON の部署設計だけを返してください。

ユーザーの goal:
{goal}

CLI/UI の advanced option から明示された配置候補だけ:
{candidates}

ユーザーの自然言語 goal を注意深く読み、配置を自分で推論してください:
- ユーザーが SSH host、remote machine、compute server、Docker container、その他 execution site を普通の文章で名指しした場合、それは運用要件として扱ってください。
- local/remote の境界を守ってください。ユーザーが「research は local、build/run は SSH 計算サーバー」と言った場合、research は local、build/run は SSH node の host-side department に分けてください。
- ユーザーに low-level flag、node spec、Docker mount、port、GPU flag、department placement を要求しないでください。それらは lead が決めることです。
- 明示的な CLI/UI 配置候補は advanced option 由来の hint にすぎません。自然言語 goal が authoritative です。
- host/container 名が明確な場合、ユーザーの表記をそのまま使ってください。日本語の助詞や周辺文を host 名に含めないでください。
- Docker node は具体的な既存 container を意味します。「Docker image を build」「container を作成」のような文言だけから Docker/container node を捏造しないでください。まず正しい host に planning/build/container-creation work を割り当ててください。container-internal department は、実 container が存在し node 登録された後に自動追加されます。
- future container 内で実行する implementation/test/experiment 部署を local placeholder として作らないでください。container がまだ存在しない場合は、host-side ops/bootstrap 部署に image/container 作成と TEAM_NODE 報告だけを担当させ、node 登録後に自動追加される container-internal department に runtime 実装・テスト・実験を任せてください。

小さな部署構成を設計してください:
- 部署は 2 から 5 個にしてください。
- 各部署は、明確な ownership domain を持つ peer Codex session です。
- `lead` 部署は含めないでください。live lead は部署一覧の外に既に存在します。
- workload balancing だけを目的に重複部署を作らないでください。
- ユーザーは、重い部署作業で subagent/agent tools、parallel delegation、skills、MCP servers、または内部分解を使うことを明示的に許可しています。部署の作業が重い場合、その部署は利用可能な helper を積極的に使うべきです。
- 部署は、自分の execution site で必要な task tool や library を install できます。環境セットアップだけを理由に peer department を増やさないでください。
- goal が public/open-source model、dataset、package、API、service に依存する場合、research/ops は transitive runtime artifact や model dependency が現在の環境で実際に access 可能か確認してください。未提供の gated credential が必要な新しい選択肢より、少し新規性が低くても end-to-end で動く選択肢を優先してください。
- product、engineering、design、quality、research、docs、ops、security、data などの domain ownership を優先してください。
- 自動研究 goal の追加制約:
{autoresearch_policy}
- department name と role は lowercase ASCII identifier にしてください。
- 配置も department design の一部として決めてください。local department は `"node": "local"` または省略を使ってください。ユーザー要求が到達可能な SSH site を明確に呼ぶ場合は SSH node を使ってください。
- ユーザー goal が到達可能な SSH host または既存 Docker/container execution site を名指しする場合、`nodes` に含めてください。
- SSH/Docker node に department を割り当てるのは、bootstrap に必要な情報が揃っている場合だけです。SSH には `host`、Docker には `container`、SSH Docker には `host` と `container` が必要です。

有効な JSON だけを返してください。Markdown や commentary は不要です:
{{
  "nodes": [
    {{
      "id": "saitou",
      "kind": "ssh",
      "host": "saitou",
      "container": null,
      "cwd": null,
      "note": "ユーザーが要求した remote SSH site。"
    }}
  ],
  "departments": [
    {{
      "name": "product",
      "role": "product",
      "mission": "Scope, requirements, product decisions を担当する。",
      "node": "local"
    }}
  ]
}}
"#,
            goal = prompt_goal,
            candidates = candidates,
            autoresearch_policy = autoresearch_policy,
        );
    }

    format!(
        r#"You are the lead agent directly listening to the user's request. The user is effectively the president/CEO. Your job is to understand the whole request, decide the operating plan, and create departments at the right execution sites. You are an orchestrator, not an implementation worker and not a simple worker balancer.

This is an internal Codex Teams department-design prompt. Do not read or invoke ordinary user-facing team-launch workflows. Do not inspect existing teams, start nested team swarm/run commands, or check team status. In this turn, read the goal below and return only the JSON department design.

User goal:
{goal}

Explicit placement candidates from CLI/UI advanced options only:
{candidates}

Read the user goal carefully and infer placement from the natural language yourself:
- If the user names an SSH host, remote machine, compute server, Docker container, or other execution site, treat that as an operational requirement even when it is written in ordinary prose.
- Preserve local/remote boundaries. If the user says research is local but build/run happens on an SSH compute server, keep research local and create host-side departments on that SSH node for build/run work.
- Do not require the user to provide low-level flags, node specs, Docker mount specs, ports, GPU flags, or department placement. Those are lead-owned decisions.
- Explicit CLI/UI placement candidates are only hints from advanced options. The natural-language goal is authoritative.
- Use the user's exact host/container names when they are clearly named. Do not parse Japanese particles or surrounding prose as part of a host name.
- A Docker node means a concrete, already-existing container. Do not invent Docker/container nodes from phrases like "build a Docker image" or "create a Docker container"; assign planning/build/container-creation work to the correct host first. Container-internal departments are added automatically only after the real container exists and has been registered as a node.
- Do not create local placeholder implementation/test/experiment departments for work that is explicitly supposed to run inside a future container. If the container does not exist yet, create a host-side ops/bootstrap department for image/container creation and TEAM_NODE reporting, then let the automatically added container-internal department own runtime implementation, tests, and experiments after node registration.

Design a small department structure:
- Create 2 to 5 departments.
- Each department is one peer Codex session with a clear ownership domain.
- Do not include a `lead` department. The live lead already exists outside your department list.
- Do not create duplicate departments just to balance workload.
- The user explicitly authorizes departments to use subagents, agent tools, parallel delegation, skills, MCP servers, and internal decomposition for substantial work. If a department's work is heavy, that department should proactively use available helpers.
- Departments are allowed to install missing task tools and libraries in their own execution site when that is the best way to complete or verify the work. Do not create extra peer departments just because an environment needs setup.
- If the goal depends on a public external model, dataset, package, or service, research/ops must verify that all required runtime artifacts and transitive model dependencies are actually accessible in the current environment. Prefer a slightly less novel option that can run end-to-end over a newer option that requires unprovided gated credentials.
- Prefer domain ownership such as product, engineering, design, quality, research, docs, ops, security, data, etc.
- Additional constraints for autoresearch goals:
{autoresearch_policy}
- Use lowercase ASCII identifiers for department names and roles.
- Decide placement as part of the department design. Use `"node": "local"` or omit `node` for local departments. Use an SSH node when the user's request clearly calls for a reachable SSH site.
- Include nodes in `nodes` when the user goal names a reachable SSH host or an already-existing Docker/container execution site.
- Do not assign a department to an SSH/Docker node unless the node has enough information to bootstrap: SSH needs `host`; Docker needs `container`; SSH Docker needs both `host` and `container`.

Return only valid JSON, no Markdown, no commentary:
{{
  "nodes": [
    {{
      "id": "saitou",
      "kind": "ssh",
      "host": "saitou",
      "container": null,
      "cwd": null,
      "note": "Remote SSH site requested by the user."
    }}
  ],
  "departments": [
    {{
      "name": "product",
      "role": "product",
      "mission": "Own scope, requirements, and product decisions.",
      "node": "local"
    }}
  ]
}}
"#,
        goal = prompt_goal,
        candidates = candidates,
        autoresearch_policy = autoresearch_policy,
    )
}

fn sanitize_goal_for_internal_team_prompt(goal: &str) -> String {
    let replacements = [
        ("codex-team-secretary", "ordinary user-facing team launcher"),
        ("Codex Team Secretary", "ordinary user-facing team launcher"),
        ("team secretary", "team coordinator"),
        ("Team Secretary", "team coordinator"),
        ("secretary skill", "launcher workflow"),
        ("secretary workflow", "launcher workflow"),
        ("secretary", "coordinator"),
        ("秘書スキル", "起動ワークフロー"),
        ("秘書ワークフロー", "起動ワークフロー"),
        ("秘書", "連絡役"),
    ];
    replacements
        .into_iter()
        .fold(goal.to_string(), |acc, (needle, replacement)| {
            acc.replace(needle, replacement)
        })
}

fn parse_lead_department_design(raw: &str) -> Result<LeadDepartmentDesign> {
    let json = extract_json_object(raw).unwrap_or(raw);
    let mut design: LeadDepartmentDesign =
        serde_json::from_str(json).context("parse lead department design JSON")?;
    design.departments.retain(|department| {
        !department.name.trim().is_empty()
            && !department.role.trim().is_empty()
            && !department.mission.trim().is_empty()
    });
    if design.departments.is_empty() {
        bail!("lead department design did not include any valid departments");
    }
    Ok(design)
}

fn lead_placement_candidates_from_start(args: &StartArgs) -> Result<Vec<LeadNodeDesign>> {
    let mut candidates = Vec::new();
    let now = now();
    for raw in &args.nodes {
        let node = parse_node_spec(raw, &now)?;
        candidates.push(LeadNodeDesign {
            id: node.id,
            kind: node.kind,
            host: node.host,
            container: node.container,
            cwd: node.cwd,
            note: node.note,
        });
    }
    Ok(candidates)
}

fn merge_lead_node_designs(args: &mut StartArgs, nodes: &[LeadNodeDesign]) {
    for node in nodes {
        let Some(spec) = lead_node_design_to_spec(node) else {
            continue;
        };
        let id = node_spec_id(&spec);
        if id.is_empty() || id == "local" {
            continue;
        }
        if !args
            .nodes
            .iter()
            .any(|existing| node_spec_id(existing) == id)
        {
            args.nodes.push(spec);
        }
    }
}

fn merge_lead_node_metadata(team_dir: &Path, designs: &[LeadNodeDesign]) -> Result<()> {
    if designs.is_empty() {
        return Ok(());
    }
    let mut nodes = load_nodes(team_dir)?;
    let mut changed = false;
    for design in designs {
        let id = sanitize_id(&design.id);
        let Some(node) = nodes.iter_mut().find(|node| node.id == id) else {
            continue;
        };
        if node.cwd.is_none() && design.cwd.is_some() {
            node.cwd = design.cwd.clone();
            changed = true;
        }
        if node.note.trim().is_empty() && !design.note.trim().is_empty() {
            node.note = design.note.clone();
            changed = true;
        }
        if changed {
            node.updated_at = now();
        }
    }
    if changed {
        write_nodes(team_dir, &nodes)?;
    }
    Ok(())
}

fn lead_node_design_to_spec(node: &LeadNodeDesign) -> Option<String> {
    let id = sanitize_id(&node.id);
    if id.is_empty() || id == "local" {
        return None;
    }
    match &node.kind {
        TeamNodeKind::Local => None,
        TeamNodeKind::Manual => None,
        TeamNodeKind::Ssh => node
            .host
            .as_deref()
            .filter(|host| !host.trim().is_empty())
            .map(|host| format!("{id}@ssh={}", host.trim())),
        TeamNodeKind::Docker => node
            .container
            .as_deref()
            .filter(|container| !container.trim().is_empty())
            .filter(|container| docker_container_exists(None, container.trim()))
            .map(|container| format!("{id}@docker={}", container.trim())),
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref()?.trim();
            let container = node.container.as_deref()?.trim();
            if host.is_empty() || container.is_empty() {
                None
            } else if !docker_container_exists(Some(host), container) {
                None
            } else {
                Some(format!("{id}@ssh-docker={host}:{container}"))
            }
        }
    }
}

fn docker_container_exists(host: Option<&str>, container: &str) -> bool {
    let command = format!("docker inspect {} >/dev/null 2>&1", shell_quote(container));
    let status = match host {
        Some(host) => Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(host)
            .arg(format!("bash -lc {}", shell_quote(&command)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
        None => Command::new("bash")
            .arg("-lc")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    };
    matches!(status, Ok(status) if status.success())
}

fn node_spec_id(raw: &str) -> String {
    let left = raw.split_once('=').map(|(left, _)| left).unwrap_or(raw);
    let id = left.split_once('@').map(|(id, _)| id).unwrap_or(left);
    sanitize_id(id)
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&raw[start..=end])
}

