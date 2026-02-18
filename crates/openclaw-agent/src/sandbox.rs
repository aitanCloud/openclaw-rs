use std::path::Path;

/// Security sandbox policy for tool execution
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Directories the agent is allowed to read from (empty = unrestricted)
    pub read_allow: Vec<String>,
    /// Directories the agent is allowed to write to (empty = unrestricted)
    pub write_allow: Vec<String>,
    /// Shell commands/patterns that are blocked
    pub command_blocklist: Vec<String>,
    /// Maximum exec timeout in seconds (overrides tool-requested timeout)
    pub max_exec_timeout_secs: u64,
    /// Maximum output bytes from exec
    pub max_output_bytes: usize,
    /// Whether network access is allowed for tools
    pub network_allowed: bool,
    /// Per-turn timeout in seconds (0 = no limit)
    pub turn_timeout_secs: u64,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            read_allow: Vec::new(),
            write_allow: Vec::new(),
            command_blocklist: default_command_blocklist(),
            max_exec_timeout_secs: 60,
            max_output_bytes: 64 * 1024,
            network_allowed: true,
            turn_timeout_secs: 120,
        }
    }
}

impl SandboxPolicy {
    /// Check if a path is allowed for reading
    pub fn can_read(&self, path: &str) -> bool {
        if self.read_allow.is_empty() {
            return true;
        }
        let path = Path::new(path);
        self.read_allow.iter().any(|allowed| {
            let allowed = Path::new(allowed);
            path.starts_with(allowed)
        })
    }

    /// Check if a path is allowed for writing
    pub fn can_write(&self, path: &str) -> bool {
        if self.write_allow.is_empty() {
            return true;
        }
        let path = Path::new(path);
        self.write_allow.iter().any(|allowed| {
            let allowed = Path::new(allowed);
            path.starts_with(allowed)
        })
    }

    /// Check if a command is blocked by the blocklist
    pub fn is_command_blocked(&self, command: &str) -> Option<&str> {
        let cmd_lower = command.to_lowercase();
        for pattern in &self.command_blocklist {
            let pattern_lower = pattern.to_lowercase();
            // Check if the blocked pattern appears as a word boundary in the command
            if cmd_lower.contains(&pattern_lower) {
                return Some(pattern);
            }
        }
        None
    }

    /// Clamp a requested timeout to the policy maximum
    pub fn clamp_timeout(&self, requested: u64) -> u64 {
        if self.max_exec_timeout_secs == 0 {
            requested
        } else {
            requested.min(self.max_exec_timeout_secs)
        }
    }
}

/// Default dangerous commands that should be blocked
fn default_command_blocklist() -> Vec<String> {
    vec![
        // Destructive filesystem operations
        "rm -rf /".to_string(),
        "rm -rf /*".to_string(),
        "mkfs".to_string(),
        "dd if=".to_string(),
        // System modification
        "shutdown".to_string(),
        "reboot".to_string(),
        "halt".to_string(),
        "poweroff".to_string(),
        "init 0".to_string(),
        "init 6".to_string(),
        // User/permission escalation
        "passwd".to_string(),
        "useradd".to_string(),
        "userdel".to_string(),
        "usermod".to_string(),
        "visudo".to_string(),
        "chown -R /".to_string(),
        "chmod -R 777 /".to_string(),
        // Network attacks
        "nmap".to_string(),
        "masscan".to_string(),
        // Crypto mining
        "xmrig".to_string(),
        "minerd".to_string(),
        "cpuminer".to_string(),
        // Reverse shells
        "/dev/tcp/".to_string(),
        "nc -e".to_string(),
        "nc -l".to_string(),
        "ncat -e".to_string(),
        // Fork bombs
        ":(){ :|:& };:".to_string(),
        // Credential theft
        "/etc/shadow".to_string(),
        ".ssh/id_".to_string(),
        "ssh-keygen".to_string(),
        // Package manager abuse (in Docker context)
        "apt install".to_string(),
        "apt-get install".to_string(),
        "yum install".to_string(),
        "pip install".to_string(),
        "npm install -g".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = SandboxPolicy::default();
        assert!(!policy.command_blocklist.is_empty());
        assert_eq!(policy.max_exec_timeout_secs, 60);
        assert!(policy.network_allowed);
    }

    #[test]
    fn test_command_blocklist() {
        let policy = SandboxPolicy::default();
        assert!(policy.is_command_blocked("rm -rf /").is_some());
        assert!(policy.is_command_blocked("shutdown -h now").is_some());
        assert!(policy.is_command_blocked("echo hello").is_none());
        assert!(policy.is_command_blocked("ls -la").is_none());
        assert!(policy.is_command_blocked("cat /etc/shadow").is_some());
    }

    #[test]
    fn test_path_allow() {
        let policy = SandboxPolicy {
            read_allow: vec!["/home/user/workspace".to_string()],
            write_allow: vec!["/home/user/workspace".to_string()],
            ..Default::default()
        };
        assert!(policy.can_read("/home/user/workspace/file.txt"));
        assert!(!policy.can_read("/etc/passwd"));
        assert!(policy.can_write("/home/user/workspace/out.txt"));
        assert!(!policy.can_write("/tmp/evil.sh"));
    }

    #[test]
    fn test_unrestricted_paths() {
        let policy = SandboxPolicy::default();
        // Empty allow lists = unrestricted
        assert!(policy.can_read("/anything"));
        assert!(policy.can_write("/anything"));
    }

    #[test]
    fn test_clamp_timeout() {
        let policy = SandboxPolicy {
            max_exec_timeout_secs: 30,
            ..Default::default()
        };
        assert_eq!(policy.clamp_timeout(10), 10);
        assert_eq!(policy.clamp_timeout(60), 30);
        assert_eq!(policy.clamp_timeout(30), 30);
    }
}
