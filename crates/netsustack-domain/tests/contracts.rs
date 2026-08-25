use netsustack_domain::{
    ConfigValidationError, MemoryLimitMode, NetsuStackConfig, NetsuStackStatus, PortOccupant,
    PreferredShell, Project, ServerAction, ServerConfig, ServerState, TemporaryJobState,
    TemporaryJobStatus, is_project_id, is_server_id, is_temporary_id, new_project_id,
    new_server_id, new_temporary_id, parse_memory_size, parse_timeout,
};
use serde_json::{Value, json};

fn config_with_server(server: ServerConfig) -> NetsuStackConfig {
    let mut project = Project::new("Project", r"S:\project");
    project.servers = vec![server];
    NetsuStackConfig {
        projects: vec![project],
        ..NetsuStackConfig::default()
    }
}

#[test]
fn config_defaults_match_the_v1_contract() {
    let config = NetsuStackConfig::default();

    assert_eq!(config.version, 1);
    assert_eq!(config.api_port, 7737);
    assert_eq!(config.health_interval_seconds, 10);
    assert_eq!(config.max_restart_attempts, 5);
    assert_eq!(config.log_buffer_lines, 5_000);
    assert_eq!(config.log_file_max_mb, 10);
    assert_eq!(config.global_memory_limit_bytes, None);
    assert_eq!(config.preferred_shell, PreferredShell::Auto);
    assert!(config.projects.is_empty());
}

#[test]
fn config_deserialization_requires_version_api_port_and_projects() {
    let missing_required_fields = [
        json!({}),
        json!({ "apiPort": 7737, "projects": [] }),
        json!({ "version": 1, "projects": [] }),
        json!({ "version": 1, "apiPort": 7737 }),
    ];

    for input in missing_required_fields {
        assert!(
            serde_json::from_value::<NetsuStackConfig>(input.clone()).is_err(),
            "config unexpectedly decoded: {input}"
        );
    }
}

#[test]
fn legacy_config_receives_defaults_without_losing_known_fields() {
    let legacy = json!({
        "version": 1,
        "apiPort": 8811,
        "projects": [{
            "name": "Legacy",
            "root": "S:\\legacy",
            "servers": [{
                "name": "web",
                "command": "npm run dev"
            }]
        }],
        "retiredField": "ignored"
    });

    let config: NetsuStackConfig = serde_json::from_value(legacy).expect("legacy config decodes");
    let project = &config.projects[0];
    let server = &project.servers[0];

    assert_eq!(config.api_port, 8811);
    assert_eq!(config.health_interval_seconds, 10);
    assert_eq!(config.preferred_shell, PreferredShell::Auto);
    assert!(project.id.starts_with("prj_"));
    assert_eq!(project.icon, "shippingbox.fill");
    assert_eq!(project.color, "#8E8E93");
    assert_eq!(project.memory_limit_mode, MemoryLimitMode::Inherit);
    assert!(server.id.starts_with("srv_"));
    assert!(server.env.is_empty());
    assert!(server.auto_restart);
    assert!(server.actions.is_empty());
}

#[test]
fn legacy_empty_ids_and_null_collections_are_repaired() {
    let config: NetsuStackConfig = serde_json::from_value(json!({
        "version": 1,
        "apiPort": 7737,
        "projects": [
            {
                "id": "",
                "name": "Empty",
                "icon": "",
                "color": "",
                "root": "S:\\empty",
                "servers": null
            },
            {
                "name": "Server",
                "root": "S:\\server",
                "servers": [{
                    "id": "",
                    "name": "web",
                    "command": "npm run dev",
                    "env": null,
                    "actions": null
                }]
            }
        ]
    }))
    .expect("legacy null collections decode");

    assert!(is_project_id(&config.projects[0].id));
    assert_eq!(config.projects[0].icon, "shippingbox.fill");
    assert_eq!(config.projects[0].color, "#8E8E93");
    assert!(config.projects[0].servers.is_empty());
    assert!(is_server_id(&config.projects[1].servers[0].id));
    assert!(config.projects[1].servers[0].env.is_empty());
    assert!(config.projects[1].servers[0].actions.is_empty());
}

#[test]
fn minimal_config_fixture_round_trips_its_normative_fields() {
    let source: Value = serde_json::from_str(include_str!(
        "../../../Tests/contracts/config-v1-minimal.json"
    ))
    .expect("fixture is JSON");
    let config: NetsuStackConfig = serde_json::from_value(source.clone()).expect("fixture decodes");
    let encoded = serde_json::to_value(config).expect("config encodes");

    assert_eq!(encoded, source);
}

#[test]
fn config_validation_rejects_case_insensitive_name_conflicts() {
    let config = NetsuStackConfig {
        projects: vec![
            Project::new("NetsuStack", r"S:\one"),
            Project::new("netsustack", r"S:\two"),
        ],
        ..NetsuStackConfig::default()
    };
    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::DuplicateProjectName { .. })
    ));

    let mut project = Project::new("NetsuStack", r"S:\one");
    project.servers = vec![
        ServerConfig::new("web", "npm run dev"),
        ServerConfig::new("WEB", "npm run preview"),
    ];
    let config = NetsuStackConfig {
        projects: vec![project],
        ..NetsuStackConfig::default()
    };
    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::DuplicateServerName { .. })
    ));

    let mut project = Project::new("NetsuStack", r"S:\one");
    project.servers = vec![ServerConfig {
        actions: vec![
            ServerAction::new("clear-cache", "clear one"),
            ServerAction::new("CLEAR-CACHE", "clear two"),
        ],
        ..ServerConfig::new("api", "cargo run")
    }];
    let config = NetsuStackConfig {
        projects: vec![project],
        ..NetsuStackConfig::default()
    };
    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::DuplicateActionName { .. })
    ));
}

#[test]
fn config_validation_rejects_invalid_ports_and_empty_commands() {
    let mut project = Project::new("NetsuStack", r"S:\one");
    project.servers = vec![ServerConfig {
        port: Some(7737),
        ..ServerConfig::new("web", "npm run dev")
    }];
    let mut config = NetsuStackConfig {
        projects: vec![project],
        ..NetsuStackConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::ApiPortConflict { port: 7737, .. })
    ));

    config.projects[0].servers[0].port = Some(0);
    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::InvalidServerPort { port: 0, .. })
    ));

    config.projects[0].servers[0].port = Some(5173);
    config.projects[0].servers[0].command.clear();
    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::EmptyServerCommand { .. })
    ));
}

#[test]
fn config_validation_rejects_health_intervals_below_two_seconds() {
    let config = NetsuStackConfig {
        health_interval_seconds: 1,
        ..NetsuStackConfig::default()
    };

    assert!(config.validate().is_err());
}

#[test]
fn config_validation_rejects_unsupported_schema_versions_with_typed_error() {
    let config = NetsuStackConfig {
        version: 2,
        ..NetsuStackConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::UnsupportedConfigVersion {
            found: 2,
            supported: 1
        })
    ));
}

#[test]
fn operational_config_bounds_match_documented_portly_parity() {
    for health_interval_seconds in [2, 120] {
        let config = NetsuStackConfig {
            health_interval_seconds,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_ok());
    }
    for health_interval_seconds in [1, 121] {
        let config = NetsuStackConfig {
            health_interval_seconds,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_err());
    }

    for max_restart_attempts in [1, 20] {
        let config = NetsuStackConfig {
            max_restart_attempts,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_ok());
    }
    for max_restart_attempts in [0, 21] {
        let config = NetsuStackConfig {
            max_restart_attempts,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_err());
    }

    for log_buffer_lines in [500, 50_000] {
        let config = NetsuStackConfig {
            log_buffer_lines,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_ok());
    }
    for log_buffer_lines in [0, 499, 50_001] {
        let config = NetsuStackConfig {
            log_buffer_lines,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_err());
    }

    for log_file_max_mb in [1, 100] {
        let config = NetsuStackConfig {
            log_file_max_mb,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_ok());
    }
    for log_file_max_mb in [0, 101] {
        let config = NetsuStackConfig {
            log_file_max_mb,
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_err());
    }
}

#[test]
fn health_status_and_url_are_syntactically_valid() {
    for health_status in [100, 599] {
        let config = config_with_server(ServerConfig {
            health_status: Some(health_status),
            ..ServerConfig::new("web", "npm run dev")
        });
        assert!(config.validate().is_ok());
    }
    for health_status in [0, 99, 600] {
        let config = config_with_server(ServerConfig {
            health_status: Some(health_status),
            ..ServerConfig::new("web", "npm run dev")
        });
        assert!(config.validate().is_err());
    }

    for health_url in [
        "/api/health",
        "/health?target=localhost:3000",
        "health?target=http://localhost:3000",
        "api/health",
        "http://localhost:5173/health",
        "https://example.com/health",
    ] {
        let config = config_with_server(ServerConfig {
            port: Some(5173),
            health_url: Some(health_url.into()),
            ..ServerConfig::new("web", "npm run dev")
        });
        assert!(config.validate().is_ok(), "URL rejected: {health_url}");
    }

    for health_url in [
        "",
        " /health",
        "health ",
        "ftp://localhost/health",
        "http://",
        "//example.com/health",
    ] {
        let config = config_with_server(ServerConfig {
            port: Some(5173),
            health_url: Some(health_url.into()),
            ..ServerConfig::new("web", "npm run dev")
        });
        assert!(config.validate().is_err(), "URL accepted: {health_url}");
    }

    let relative_without_port = config_with_server(ServerConfig {
        health_url: Some("/health".into()),
        ..ServerConfig::new("web", "npm run dev")
    });
    assert!(relative_without_port.validate().is_err());
}

#[test]
fn custom_shell_must_be_a_trimmed_absolute_windows_path() {
    for shell in [
        "",
        " ",
        "pwsh.exe",
        r"C:\tools\shell.exe ",
        r"C:\",
        r"\\server\share",
    ] {
        let config = NetsuStackConfig {
            preferred_shell: PreferredShell::Custom(shell.into()),
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_err(), "shell accepted: {shell:?}");
    }

    for shell in [r"C:\tools\shell.exe", r"\\server\tools\shell.exe"] {
        let config = NetsuStackConfig {
            preferred_shell: PreferredShell::Custom(shell.into()),
            ..NetsuStackConfig::default()
        };
        assert!(config.validate().is_ok(), "shell rejected: {shell:?}");
    }
}

#[test]
fn config_validation_rejects_non_trimmed_visual_name_duplicates() {
    let config = NetsuStackConfig {
        projects: vec![
            Project::new("Project", r"S:\one"),
            Project::new(" Project ", r"S:\two"),
        ],
        ..NetsuStackConfig::default()
    };
    assert!(config.validate().is_err());

    let config = config_with_server(ServerConfig::new(" web ", "npm run dev"));
    assert!(config.validate().is_err());

    let config = config_with_server(ServerConfig {
        actions: vec![ServerAction::new(" clear-cache ", "clear")],
        ..ServerConfig::new("web", "npm run dev")
    });
    assert!(config.validate().is_err());
}

#[test]
fn config_validation_rejects_unaddressable_names_containing_separator() {
    let config = NetsuStackConfig {
        projects: vec![Project::new("Group/Project", r"S:\project")],
        ..NetsuStackConfig::default()
    };
    assert!(config.validate().is_err());

    let config = config_with_server(ServerConfig::new("api/v1", "npm run dev"));
    assert!(config.validate().is_err());
}

#[test]
fn action_names_may_contain_separator_and_remain_case_insensitively_unique() {
    let config = config_with_server(ServerConfig {
        actions: vec![ServerAction::new("cache/clear", "clear")],
        ..ServerConfig::new("web", "npm run dev")
    });
    assert!(config.validate().is_ok());

    let config = config_with_server(ServerConfig {
        actions: vec![
            ServerAction::new("cache/clear", "clear one"),
            ServerAction::new("CACHE/CLEAR", "clear two"),
        ],
        ..ServerConfig::new("web", "npm run dev")
    });
    assert!(config.validate().is_err());
}

#[test]
fn config_validation_requires_syntactically_absolute_windows_project_roots() {
    for root in [
        "project",
        r".\project",
        r"C:project",
        r"\project",
        "/project",
    ] {
        let config = NetsuStackConfig {
            projects: vec![Project::new("Relative", root)],
            ..NetsuStackConfig::default()
        };
        assert!(
            config.validate().is_err(),
            "root unexpectedly accepted: {root}"
        );
    }

    for root in [r"C:\project", r"S:/project", r"\\server\share\project"] {
        let config = NetsuStackConfig {
            projects: vec![Project::new("Absolute", root)],
            ..NetsuStackConfig::default()
        };
        assert!(
            config.validate().is_ok(),
            "root unexpectedly rejected: {root}"
        );
    }
}

#[test]
fn generated_ids_have_the_required_prefix_and_shape() {
    let project = new_project_id();
    let server = new_server_id();
    let temporary = new_temporary_id();

    assert!(is_project_id(&project));
    assert!(is_server_id(&server));
    assert!(temporary.starts_with("tmp_"));
    assert!(is_temporary_id(&temporary));
    assert!(is_temporary_id("tmp_1234abcd"));
    assert!(!is_temporary_id("job_1234abcd"));
    assert!(!is_project_id("prj_ABCDEF12"));
    assert!(!is_server_id("srv_too-short"));
    assert_ne!(new_server_id(), server);
}

#[test]
fn config_validation_enforces_prefixed_globally_unique_ids() {
    let mut first = Project::new("One", r"S:\one");
    first.id = "project-one".into();
    first.servers = vec![ServerConfig {
        id: "srv_aaaaaaaa".into(),
        ..ServerConfig::new("web", "one")
    }];
    let mut second = Project::new("Two", r"S:\two");
    second.servers = vec![ServerConfig {
        id: "srv_aaaaaaaa".into(),
        ..ServerConfig::new("api", "two")
    }];
    let mut config = NetsuStackConfig {
        projects: vec![first, second],
        ..NetsuStackConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::InvalidProjectId { .. })
    ));
    config.projects[0].id = "prj_11111111".into();
    config.projects[0].servers[0].id = "server-one".into();
    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::InvalidServerId { .. })
    ));
    config.projects[0].servers[0].id = "srv_aaaaaaaa".into();
    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::DuplicateServerId { .. })
    ));
}

#[test]
fn projects_resolve_by_exact_id_then_case_insensitive_name() {
    let mut project = Project::new("NetsuStack", r"S:\one");
    project.id = "prj_a1b2c3d4".into();
    let config = NetsuStackConfig {
        projects: vec![project],
        ..NetsuStackConfig::default()
    };

    assert_eq!(
        config
            .resolve_project("prj_a1b2c3d4")
            .map(|item| item.name.as_str()),
        Some("NetsuStack")
    );
    assert_eq!(
        config
            .resolve_project("netsustack")
            .map(|item| item.id.as_str()),
        Some("prj_a1b2c3d4")
    );
    assert!(config.resolve_project("PRJ_A1B2C3D4").is_none());
}

#[test]
fn servers_resolve_by_exact_id_qualified_name_then_first_name() {
    let mut first = Project::new("One", r"S:\one");
    first.id = "prj_11111111".into();
    first.servers = vec![ServerConfig {
        id: "srv_11111111".into(),
        ..ServerConfig::new("web", "one")
    }];
    let mut second = Project::new("Two", r"S:\two");
    second.id = "prj_22222222".into();
    second.servers = vec![ServerConfig {
        id: "srv_22222222".into(),
        ..ServerConfig::new("WEB", "two")
    }];
    let config = NetsuStackConfig {
        projects: vec![first, second],
        ..NetsuStackConfig::default()
    };

    let exact = config.resolve_server("srv_22222222").expect("exact ID");
    assert_eq!(exact.project.name, "Two");
    let qualified = config.resolve_server("tWo/WeB").expect("qualified name");
    assert_eq!(qualified.server.id, "srv_22222222");
    let qualified_by_id = config
        .resolve_server("prj_11111111/WEB")
        .expect("qualified project ID");
    assert_eq!(qualified_by_id.server.id, "srv_11111111");
    let first_match = config.resolve_server("web").expect("first name");
    assert_eq!(first_match.project.id, "prj_11111111");
    assert!(config.resolve_server("missing/web").is_none());
}

#[test]
fn timeout_parser_accepts_seconds_and_supported_suffixes() {
    let cases = [
        ("1", 1),
        ("1s", 1),
        ("1.1s", 2),
        ("0.5m", 30),
        (" 2H ", 7_200),
        ("168h", 604_800),
    ];

    for (input, expected) in cases {
        assert_eq!(parse_timeout(input), Ok(expected), "input {input:?}");
    }
}

#[test]
fn timeout_parser_rejects_values_outside_one_second_to_seven_days() {
    for input in ["", "0", "-1s", "604801", "1d", "NaN", "inf"] {
        assert!(parse_timeout(input).is_err(), "input {input:?}");
    }
}

#[test]
fn memory_parser_accepts_localized_binary_units() {
    let cases = [
        ("128 MB", 134_217_728),
        ("1,5 GB", 1_610_612_736),
        ("1 GiB", 1_073_741_824),
        ("0.5 To", 549_755_813_888),
        ("1 TB", 1_099_511_627_776),
        ("1 2 8 Mo", 134_217_728),
    ];

    for (input, expected) in cases {
        assert_eq!(parse_memory_size(input), Ok(expected), "input {input:?}");
    }
}

#[test]
fn memory_parser_rejects_missing_units_and_values_outside_the_contract() {
    for input in ["", "128", "127 MiB", "1.1 TiB", "-1GB", "NaN GB", "inf GB"] {
        assert!(parse_memory_size(input).is_err(), "input {input:?}");
    }
}

#[test]
fn complete_status_fixture_round_trips_all_normative_fields() {
    let source: Value = serde_json::from_str(include_str!(
        "../../../Tests/contracts/status-complete.json"
    ))
    .expect("fixture is JSON");
    let status: NetsuStackStatus =
        serde_json::from_value(source.clone()).expect("status fixture decodes");
    let encoded = serde_json::to_value(&status).expect("status encodes");

    assert_eq!(encoded, source);
    assert_eq!(status.revision, 42);
    assert_eq!(
        status.projects[0].servers[0]
            .started_at
            .expect("startedAt")
            .to_rfc3339(),
        "2026-08-25T10:15:30+00:00"
    );
}

#[test]
fn temporary_job_dto_uses_camel_case_iso_8601_and_stable_exit_codes() {
    let job: TemporaryJobStatus = serde_json::from_value(json!({
        "id": "tmp_a1b2c3d4",
        "name": "tests",
        "command": "cargo test",
        "directory": "S:\\project",
        "state": "timedOut",
        "pid": 1234,
        "startedAt": "2026-08-25T10:00:00Z",
        "finishedAt": "2026-08-25T10:30:00Z",
        "timeoutSeconds": 1800,
        "deadline": "2026-08-25T10:30:00Z",
        "exitCode": null,
        "error": "deadline reached"
    }))
    .expect("temporary job decodes");

    assert_eq!(job.state, TemporaryJobState::TimedOut);
    assert!(job.state.is_finished());
    assert_eq!(job.process_exit_code(), 124);
    let encoded = serde_json::to_value(job).expect("temporary job encodes");
    assert_eq!(encoded["timeoutSeconds"], 1800);
    assert_eq!(encoded["startedAt"], "2026-08-25T10:00:00Z");
    assert!(encoded.get("timeout_seconds").is_none());
}

#[test]
fn runtime_enums_and_port_occupant_use_wire_contract_names() {
    assert_eq!(
        serde_json::to_value(ServerState::Unhealthy).unwrap(),
        "unhealthy"
    );
    assert_eq!(
        serde_json::to_value(TemporaryJobState::TimedOut).unwrap(),
        "timedOut"
    );

    let occupant = PortOccupant {
        port: 5173,
        pid: 42,
        command: "node.exe".into(),
        user: "developer".into(),
        owned_by_netsustack: true,
        server_id: Some("srv_a1b2c3d4".into()),
        docker_container_id: Some("abc123".into()),
        docker_container_name: None,
        docker_compose_project: None,
        docker_compose_service: None,
    };
    let encoded = serde_json::to_value(occupant).expect("occupant encodes");
    assert_eq!(encoded["ownedByNetsuStack"], true);
    assert_eq!(encoded["serverID"], "srv_a1b2c3d4");
    assert_eq!(encoded["dockerContainerID"], "abc123");
}
