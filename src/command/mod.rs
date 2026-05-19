pub mod builtins;

pub struct BuiltinCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub admin_only: bool,
}

pub struct CommandRegistry {
    builtins: Vec<BuiltinCommand>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            builtins: vec![
                BuiltinCommand {
                    name: "new",
                    description: "Create a new session",
                    admin_only: false,
                },
                BuiltinCommand {
                    name: "doctor",
                    description: "Run system diagnostics",
                    admin_only: false,
                },
                BuiltinCommand {
                    name: "help",
                    description: "List available commands",
                    admin_only: false,
                },
            ],
        }
    }

    pub fn is_command(body: &str) -> bool {
        body.trim_start().starts_with('/')
    }

    pub fn parse_command(body: &str) -> Option<(&str, &str)> {
        let trimmed = body.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let without_slash = &trimmed[1..];
        let (cmd, args) = match without_slash.find(char::is_whitespace) {
            Some(pos) => (&without_slash[..pos], without_slash[pos..].trim()),
            None => (without_slash, ""),
        };
        if cmd.is_empty() {
            return None;
        }
        Some((cmd, args))
    }

    pub fn find_command(&self, name: &str) -> Option<&BuiltinCommand> {
        self.builtins.iter().find(|c| c.name == name)
    }

    pub fn list_commands(&self) -> &[BuiltinCommand] {
        &self.builtins
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn detect_and_parse_command(body: &str) -> Option<(String, String)> {
    CommandRegistry::parse_command(body).map(|(cmd, args)| (cmd.to_string(), args.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_command() {
        assert!(CommandRegistry::is_command("/help"));
        assert!(CommandRegistry::is_command("  /help"));
        assert!(!CommandRegistry::is_command("hello"));
        assert!(!CommandRegistry::is_command(""));
    }

    #[test]
    fn test_parse_command() {
        let (cmd, args) = CommandRegistry::parse_command("/help").unwrap();
        assert_eq!(cmd, "help");
        assert_eq!(args, "");

        let (cmd, args) = CommandRegistry::parse_command("/switch my-session").unwrap();
        assert_eq!(cmd, "switch");
        assert_eq!(args, "my-session");

        let (cmd, args) = CommandRegistry::parse_command("/name Project Alpha").unwrap();
        assert_eq!(cmd, "name");
        assert_eq!(args, "Project Alpha");

        assert!(CommandRegistry::parse_command("not a command").is_none());
        assert!(CommandRegistry::parse_command("/").is_none());
    }

    #[test]
    fn test_find_command() {
        let registry = CommandRegistry::new();
        assert!(registry.find_command("help").is_some());
        assert!(registry.find_command("new").is_some());
        assert!(registry.find_command("nonexistent").is_none());
    }
}
