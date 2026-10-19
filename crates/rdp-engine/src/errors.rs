//! Turns protocol-level failures into messages a person can act on.

pub(crate) fn friendly(err: &anyhow::Error) -> String {
    let chain: Vec<String> = err.chain().map(|e| e.to_string()).collect();
    let all = chain.join(": ").to_lowercase();

    let pick = |needle: &[&str]| needle.iter().any(|n| all.contains(n));

    if pick(&["timed out", "timeout", "deadline"]) {
        return "Connection timed out — host unreachable or port 3389 filtered".into();
    }
    if pick(&["connection refused", "actively refused", "os error 10061"]) {
        return "Connection refused — is Remote Desktop enabled on the host?".into();
    }
    if pick(&[
        "no such host",
        "failed to lookup",
        "name or service not known",
        "os error 11001",
        "lookup",
    ]) {
        return "Host name could not be resolved".into();
    }
    if pick(&["os error 10065", "os error 10051", "unreachable"]) {
        return "Network unreachable".into();
    }
    if pick(&[
        "logon_failure",
        "logon failure",
        "status_logon_failure",
        "0xc000006d",
        "wrong password",
        "authentication failed",
    ]) {
        return "Authentication failed — check username and password".into();
    }
    if pick(&[
        "account_restriction",
        "0xc000006e",
        "password_expired",
        "0xc0000071",
        "password must change",
        "0xc0000224",
    ]) {
        return "Account restriction — password expired or must be changed".into();
    }
    if pick(&["account_locked", "0xc0000234"]) {
        return "Account locked out".into();
    }
    if pick(&["account_disabled", "0xc0000072"]) {
        return "Account disabled".into();
    }
    if pick(&[
        "access denied",
        "access_denied",
        "0xc0000022",
        "early user authorization",
    ]) {
        return "Access denied — user is not allowed to sign in remotely".into();
    }
    if pick(&["credssp", "nla", "sspi", "ntlm", "kerberos"]) {
        return format!("Network Level Authentication failed ({})", short(&chain));
    }
    if pick(&["tls", "certificate", "handshake"]) {
        return format!("Secure channel failed ({})", short(&chain));
    }
    if pick(&[
        "connection reset",
        "os error 10054",
        "broken pipe",
        "unexpected eof",
        "eof",
    ]) {
        return "Connection dropped by the remote host".into();
    }
    short(&chain)
}

fn short(chain: &[String]) -> String {
    let mut msg = chain.last().cloned().unwrap_or_else(|| "Unknown error".into());
    if msg.len() > 160 {
        msg.truncate(157);
        msg.push('…');
    }
    let mut chars = msg.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => msg,
    }
}
