//! Fixed sandbox flags handed to `ls-containers` so no call site can pick
//! its own "safe enough" settings. MVP: one hardcoded policy, no config
//! system — there's exactly one set of defaults, so a config layer would
//! just be indirection around a constant.

pub struct SandboxPolicy {
    pub read_only_rootfs: bool,
    pub memory_limit: &'static str,
    pub cpu_limit: &'static str,
    /// "bridge": containers can reach each other and published host ports,
    /// but not the sender's machine.
    pub network_mode: &'static str,
}

pub fn default_policy() -> SandboxPolicy {
    SandboxPolicy {
        read_only_rootfs: true,
        memory_limit: "1g",
        cpu_limit: "1.0",
        network_mode: "bridge",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_locked_down() {
        let policy = default_policy();
        assert!(policy.read_only_rootfs);
        assert_eq!(policy.network_mode, "bridge");
    }
}
