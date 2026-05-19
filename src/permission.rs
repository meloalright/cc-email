use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum PermissionDecision {
    Allow,
    Deny,
    AllowAll,
}

#[derive(Debug)]
pub struct PendingPermission {
    pub request: PermissionRequest,
    pub asked_at: DateTime<Utc>,
}

pub fn match_permission_response(body: &str) -> Option<PermissionDecision> {
    let first_line = body.lines().next().unwrap_or("").trim().to_lowercase();
    if is_approve_all(&first_line) {
        Some(PermissionDecision::AllowAll)
    } else if is_allow(&first_line) {
        Some(PermissionDecision::Allow)
    } else if is_deny(&first_line) {
        Some(PermissionDecision::Deny)
    } else {
        None
    }
}

fn is_approve_all(s: &str) -> bool {
    matches!(
        s,
        "allow all" | "allowall" | "approve all" | "yes all" | "allow_all"
    )
}

fn is_allow(s: &str) -> bool {
    matches!(s, "allow" | "yes" | "y" | "ok" | "approve" | "granted")
}

fn is_deny(s: &str) -> bool {
    matches!(s, "deny" | "no" | "n" | "reject" | "denied" | "block")
}

pub fn format_permission_email(req: &PermissionRequest) -> String {
    let input_preview = if let Some(cmd) = req.tool_input.get("command").and_then(|c| c.as_str()) {
        format!("  {}", cmd)
    } else {
        serde_json::to_string_pretty(&req.tool_input)
            .unwrap_or_else(|_| format!("{}", req.tool_input))
    };

    format!(
        "Permission Request\n\n\
         Claude wants to use: {}\n\n\
         Input:\n{}\n\n\
         Reply with one of:\n\
         - allow     — approve this tool use\n\
         - deny      — block this tool use\n\
         - allow all — auto-approve all remaining permissions\n\n\
         This request will timeout in 5 minutes.\n\
         If no response, the tool use will be denied.\n\n\
         --\nSent by cc-email\n",
        req.tool_name, input_preview
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allow_responses() {
        assert!(matches!(
            match_permission_response("allow"),
            Some(PermissionDecision::Allow)
        ));
        assert!(matches!(
            match_permission_response("yes"),
            Some(PermissionDecision::Allow)
        ));
        assert!(matches!(
            match_permission_response("y"),
            Some(PermissionDecision::Allow)
        ));
        assert!(matches!(
            match_permission_response("ok"),
            Some(PermissionDecision::Allow)
        ));
        assert!(matches!(
            match_permission_response("  YES  "),
            Some(PermissionDecision::Allow)
        ));
    }

    #[test]
    fn test_deny_responses() {
        assert!(matches!(
            match_permission_response("deny"),
            Some(PermissionDecision::Deny)
        ));
        assert!(matches!(
            match_permission_response("no"),
            Some(PermissionDecision::Deny)
        ));
        assert!(matches!(
            match_permission_response("n"),
            Some(PermissionDecision::Deny)
        ));
    }

    #[test]
    fn test_allow_all_responses() {
        assert!(matches!(
            match_permission_response("allow all"),
            Some(PermissionDecision::AllowAll)
        ));
        assert!(matches!(
            match_permission_response("approve all"),
            Some(PermissionDecision::AllowAll)
        ));
    }

    #[test]
    fn test_non_permission_response() {
        assert!(match_permission_response("hello world").is_none());
        assert!(match_permission_response("fix the bug").is_none());
    }

    #[test]
    fn test_multiline_only_first_line() {
        assert!(matches!(
            match_permission_response("allow\nsome other text"),
            Some(PermissionDecision::Allow)
        ));
    }

    #[test]
    fn test_format_permission_email() {
        let req = PermissionRequest {
            id: "req_1".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: serde_json::json!({"command": "rm -rf /tmp/old"}),
        };
        let email = format_permission_email(&req);
        assert!(email.contains("Bash"));
        assert!(email.contains("rm -rf /tmp/old"));
        assert!(email.contains("allow"));
        assert!(email.contains("deny"));
    }
}
