use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct SubmoduleListParams {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubmoduleOpenParams {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub path: String,
    #[serde(default)]
    pub focus: bool,
    #[serde(default = "sharing_default")]
    pub share_skills: bool,
}

fn sharing_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubmoduleContextParams {
    pub workspace_id: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub codex_sources: Option<Vec<String>>,
    #[serde(default)]
    pub claude_sources: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubmoduleInfo {
    pub path: String,
    pub checkout_path: String,
    pub initialized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubmoduleContextInfo {
    pub workspace_id: String,
    pub parent_path: String,
    pub submodule_path: String,
    pub checkout_path: String,
    pub repo_key: String,
    pub parent_workspace_id: Option<String>,
    pub sharing_enabled: bool,
    pub status: String,
    pub messages: Vec<String>,
}
