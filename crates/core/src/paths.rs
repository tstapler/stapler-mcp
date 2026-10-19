//! `~/.stapler-mcp/{daemon.sock,daemon.lock,daemon.log}`, overridable via
//! `STAPLER_MCP_HOME` (how the test suite gets full isolation from any real
//! daemon on the machine).

use crate::ports::EnvPort;

const ENV_HOME_OVERRIDE: &str = "STAPLER_MCP_HOME";
const ENV_BROWSER_PROFILE_DIR: &str = "STAPLER_MCP_BROWSER_PROFILE_DIR";

pub fn base_dir<E: EnvPort>(env: &E) -> String {
    if let Some(v) = env.var(ENV_HOME_OVERRIDE) {
        if !v.is_empty() {
            return v;
        }
    }
    let home = env.home_dir().unwrap_or_default();
    format!("{home}/.stapler-mcp")
}

pub fn socket_path<E: EnvPort>(env: &E) -> String {
    format!("{}/daemon.sock", base_dir(env))
}

pub fn lock_path<E: EnvPort>(env: &E) -> String {
    format!("{}/daemon.lock", base_dir(env))
}

pub fn log_path<E: EnvPort>(env: &E) -> String {
    format!("{}/daemon.log", base_dir(env))
}

pub fn http_port_path<E: EnvPort>(env: &E) -> String {
    format!("{}/http-port", base_dir(env))
}

pub fn http_token_path<E: EnvPort>(env: &E) -> String {
    format!("{}/http-token", base_dir(env))
}

pub fn cache_dir<E: EnvPort>(env: &E) -> String {
    format!("{}/cache", base_dir(env))
}

pub fn docs_index_dir<E: EnvPort>(env: &E) -> String {
    format!("{}/docs-index", base_dir(env))
}

pub fn embedding_cache_dir<E: EnvPort>(env: &E) -> String {
    format!("{}/models", base_dir(env))
}

/// Opt-in resolver for a durable Chrome `user_data_dir` that survives daemon
/// restarts. No computed default (see ADR-0001) — unset, empty, or a value
/// that isn't (or can't be resolved to) an absolute path all mean "use the
/// existing ephemeral pid+timestamp-scoped profile," never a value passed
/// through unchecked to `create_dir_all` at an unpredictable
/// daemon-cwd-relative location. A leading `~/` or bare `~` is expanded via
/// `EnvPort::home_dir()` first, since a value pasted into an MCP client's
/// JSON `env` config block is never shell-expanded.
pub fn browser_profile_dir<E: EnvPort>(env: &E) -> Option<String> {
    let v = env.var(ENV_BROWSER_PROFILE_DIR).filter(|v| !v.is_empty())?;

    let v = if v == "~" || v.starts_with("~/") {
        match env.home_dir() {
            Some(home) if v == "~" => home,
            Some(home) => format!("{home}{}", &v[1..]),
            None => v,
        }
    } else {
        v
    };

    if !v.starts_with('/') {
        eprintln!(
            "stapler-mcp: {ENV_BROWSER_PROFILE_DIR} must be an absolute path, got {v:?} — falling back to ephemeral profile"
        );
        return None;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEnv {
        home_override: Option<String>,
        browser_profile_override: Option<String>,
        home_dir_override: Option<String>,
    }

    impl EnvPort for MockEnv {
        fn var(&self, key: &str) -> Option<String> {
            if key == ENV_HOME_OVERRIDE {
                self.home_override.clone()
            } else if key == ENV_BROWSER_PROFILE_DIR {
                self.browser_profile_override.clone()
            } else {
                None
            }
        }

        fn home_dir(&self) -> Option<String> {
            self.home_dir_override.clone()
        }
    }

    #[test]
    fn should_build_docs_index_dir_under_home_override() {
        let env = MockEnv {
            home_override: Some("/tmp/test-home".to_string()),
            browser_profile_override: None,
            home_dir_override: Some("/home/testuser".to_string()),
        };

        let result = docs_index_dir(&env);

        assert_eq!(result, "/tmp/test-home/docs-index");
    }

    #[test]
    fn should_build_embedding_cache_dir_under_home_override() {
        let env = MockEnv {
            home_override: Some("/tmp/test-home".to_string()),
            browser_profile_override: None,
            home_dir_override: Some("/home/testuser".to_string()),
        };

        let result = embedding_cache_dir(&env);

        assert_eq!(result, "/tmp/test-home/models");
    }

    #[test]
    fn should_build_docs_index_dir_under_default_home() {
        let env = MockEnv {
            home_override: None,
            browser_profile_override: None,
            home_dir_override: Some("/home/testuser".to_string()),
        };

        let result = docs_index_dir(&env);

        assert_eq!(result, "/home/testuser/.stapler-mcp/docs-index");
    }

    #[test]
    fn should_build_http_port_path_under_home_override() {
        let env = MockEnv {
            home_override: Some("/tmp/test-home".to_string()),
            browser_profile_override: None,
            home_dir_override: Some("/home/testuser".to_string()),
        };

        let result = http_port_path(&env);

        assert_eq!(result, "/tmp/test-home/http-port");
    }

    #[test]
    fn should_build_http_token_path_under_home_override() {
        let env = MockEnv {
            home_override: Some("/tmp/test-home".to_string()),
            browser_profile_override: None,
            home_dir_override: Some("/home/testuser".to_string()),
        };

        let result = http_token_path(&env);

        assert_eq!(result, "/tmp/test-home/http-token");
    }

    #[test]
    fn should_return_none_for_browser_profile_dir_when_unset() {
        let env = MockEnv {
            home_override: None,
            browser_profile_override: None,
            home_dir_override: Some("/home/testuser".to_string()),
        };

        assert_eq!(browser_profile_dir(&env), None);
    }

    #[test]
    fn should_return_none_for_browser_profile_dir_when_empty() {
        let env = MockEnv {
            home_override: None,
            browser_profile_override: Some(String::new()),
            home_dir_override: Some("/home/testuser".to_string()),
        };

        assert_eq!(browser_profile_dir(&env), None);
    }

    #[test]
    fn should_return_configured_path_for_browser_profile_dir_when_set() {
        let env = MockEnv {
            home_override: None,
            browser_profile_override: Some(
                "/home/testuser/.stapler-mcp/browser-profile".to_string(),
            ),
            home_dir_override: Some("/home/testuser".to_string()),
        };

        assert_eq!(
            browser_profile_dir(&env),
            Some("/home/testuser/.stapler-mcp/browser-profile".to_string())
        );
    }

    #[test]
    fn should_return_none_for_browser_profile_dir_when_relative_path_given() {
        let env = MockEnv {
            home_override: None,
            browser_profile_override: Some("relative/profile-dir".to_string()),
            home_dir_override: Some("/home/testuser".to_string()),
        };

        assert_eq!(browser_profile_dir(&env), None);
    }

    #[test]
    fn should_expand_leading_tilde_for_browser_profile_dir_when_home_dir_available() {
        let env = MockEnv {
            home_override: None,
            browser_profile_override: Some("~/.stapler-mcp/browser-profile".to_string()),
            home_dir_override: Some("/home/testuser".to_string()),
        };

        assert_eq!(
            browser_profile_dir(&env),
            Some("/home/testuser/.stapler-mcp/browser-profile".to_string())
        );
    }

    #[test]
    fn should_return_none_for_browser_profile_dir_when_tilde_prefixed_and_home_dir_unavailable() {
        let env = MockEnv {
            home_override: None,
            browser_profile_override: Some("~/.stapler-mcp/browser-profile".to_string()),
            home_dir_override: None,
        };

        assert_eq!(browser_profile_dir(&env), None);
    }
}
