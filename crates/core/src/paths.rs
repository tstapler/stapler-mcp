//! `~/.stapler-mcp/{daemon.sock,daemon.lock,daemon.log}`, overridable via
//! `STAPLER_MCP_HOME` (how the test suite gets full isolation from any real
//! daemon on the machine).

use crate::ports::EnvPort;

const ENV_HOME_OVERRIDE: &str = "STAPLER_MCP_HOME";

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

pub fn cache_dir<E: EnvPort>(env: &E) -> String {
    format!("{}/cache", base_dir(env))
}

pub fn docs_index_dir<E: EnvPort>(env: &E) -> String {
    format!("{}/docs-index", base_dir(env))
}

pub fn embedding_cache_dir<E: EnvPort>(env: &E) -> String {
    format!("{}/models", base_dir(env))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEnv {
        home_override: Option<String>,
    }

    impl EnvPort for MockEnv {
        fn var(&self, key: &str) -> Option<String> {
            if key == ENV_HOME_OVERRIDE {
                self.home_override.clone()
            } else {
                None
            }
        }

        fn home_dir(&self) -> Option<String> {
            Some("/home/testuser".to_string())
        }
    }

    #[test]
    fn should_build_docs_index_dir_under_home_override() {
        let env = MockEnv {
            home_override: Some("/tmp/test-home".to_string()),
        };

        let result = docs_index_dir(&env);

        assert_eq!(result, "/tmp/test-home/docs-index");
    }

    #[test]
    fn should_build_embedding_cache_dir_under_home_override() {
        let env = MockEnv {
            home_override: Some("/tmp/test-home".to_string()),
        };

        let result = embedding_cache_dir(&env);

        assert_eq!(result, "/tmp/test-home/models");
    }

    #[test]
    fn should_build_docs_index_dir_under_default_home() {
        let env = MockEnv { home_override: None };

        let result = docs_index_dir(&env);

        assert_eq!(result, "/home/testuser/.stapler-mcp/docs-index");
    }
}
