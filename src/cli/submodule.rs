use crate::api::schema::{
    EmptyParams, Method, Request, SubmoduleContextParams, SubmoduleListParams, SubmoduleOpenParams,
};

pub(super) fn run_submodule_command(args: &[String]) -> std::io::Result<i32> {
    let method = match parse_method(args) {
        Ok(method) => method,
        Err(error) => {
            eprintln!("{error}");
            return Ok(2);
        }
    };
    super::print_response(&super::send_request(&Request {
        id: "cli:submodule".into(),
        method,
    })?)
}

fn parse_method(args: &[String]) -> Result<Method, String> {
    let matches = super::spec::submodule_command()
        .no_binary_name(true)
        .try_get_matches_from(args)
        .map_err(|error| error.to_string())?;
    let (command, args) = matches
        .subcommand()
        .ok_or_else(|| "a submodule command is required".to_owned())?;
    match command {
        "list" | "open" => {
            let workspace_id = args
                .get_one::<String>("workspace")
                .map(|value| super::normalize_workspace_id(value));
            let cwd = args
                .get_one::<String>("cwd")
                .map(|value| super::worktree::normalize_path_arg(value))
                .transpose()
                .map_err(|error| error.to_string())?;
            if command == "list" {
                Ok(Method::SubmoduleList(SubmoduleListParams {
                    workspace_id,
                    cwd,
                }))
            } else {
                Ok(Method::SubmoduleOpen(SubmoduleOpenParams {
                    workspace_id,
                    cwd,
                    path: args
                        .get_one::<String>("path")
                        .cloned()
                        .ok_or_else(|| "submodule path is required".to_owned())?,
                    focus: args.get_flag("focus"),
                    share_skills: !args.get_flag("no-share-skills"),
                }))
            }
        }
        "contexts" => Ok(Method::SubmoduleContexts(EmptyParams::default())),
        "context" => {
            let sources = |name| {
                if args.get_flag("clear-sources") {
                    Some(Vec::new())
                } else {
                    args.get_many::<String>(name)
                        .map(|values| values.cloned().collect())
                }
            };
            let workspace_id = args
                .get_one::<String>("workspace_id")
                .ok_or_else(|| "workspace ID is required".to_owned())?;
            Ok(Method::SubmoduleContextRefresh(SubmoduleContextParams {
                workspace_id: super::normalize_workspace_id(workspace_id),
                enabled: if args.get_flag("enable") {
                    Some(true)
                } else if args.get_flag("disable") {
                    Some(false)
                } else {
                    None
                },
                codex_sources: sources("codex-source"),
                claude_sources: sources("claude-source"),
            }))
        }
        _ => Err("unknown submodule command".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Method, String> {
        parse_method(
            &args
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn opening_uses_parent_relative_path_and_defaults_to_sharing_without_focus() {
        let Method::SubmoduleOpen(params) =
            parse(&["open", "modules/rebuild", "--workspace", "w1"]).unwrap()
        else {
            panic!("expected open");
        };
        assert_eq!(params.workspace_id.as_deref(), Some("w1"));
        assert_eq!(params.path, "modules/rebuild");
        assert!(params.share_skills);
        assert!(!params.focus);
    }

    #[test]
    fn context_preserves_unspecified_sources_and_can_clear_them() {
        let Method::SubmoduleContextRefresh(refresh) = parse(&["context", "w2"]).unwrap() else {
            panic!("expected refresh");
        };
        assert_eq!(refresh.enabled, None);
        assert_eq!(refresh.codex_sources, None);
        let Method::SubmoduleContextRefresh(clear) =
            parse(&["context", "w2", "--clear-sources", "--disable"]).unwrap()
        else {
            panic!("expected refresh");
        };
        assert_eq!(clear.enabled, Some(false));
        assert_eq!(clear.codex_sources, Some(vec![]));
        assert_eq!(clear.claude_sources, Some(vec![]));
    }

    #[test]
    fn parser_rejects_conflicting_or_incomplete_options() {
        for args in [
            vec!["list", "--workspace", "w1", "--cwd", "/repo"],
            vec!["open"],
            vec!["open", "child", "--focus", "--no-focus"],
            vec!["context", "w2", "--enable", "--disable"],
            vec![
                "context",
                "w2",
                "--clear-sources",
                "--codex-source",
                "skills",
            ],
            vec!["contexts", "unexpected"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}
