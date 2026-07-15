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
