//! Bulk connection text: one connection per line (mirror of `src/lib/bulkParse.ts`).
//!
//! ```text
//! 10.0.0.5:admin:hunter2                 host, user, pass
//! 10.0.0.5:3390:admin:hunter2            host, port, user, pass (≥4 fields, field 2 = 1–65535)
//! srv01.corp.local:CORP\jdoe:p@ss:word   domain "CORP", user "jdoe", pass "p@ss:word"
//! srv01:jdoe@corp.local:secret           UPN kept as the username
//! [fe80::1]:3389:admin:secret            IPv6 host in brackets
//! 10.0.0.5  admin  secret                whitespace / tab / comma / semicolon separated
//! # Group: Production                    starts a group (used by text exports/imports)
//! # anything else                        comment
//! ```

use serde::Serialize;

pub const DEFAULT_PORT: u16 = 3389;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineError {
    pub line: usize,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct Parsed {
    /// (group name from a `# Group:` header, if any, entry)
    pub entries: Vec<(Option<String>, Entry)>,
    pub errors: Vec<LineError>,
}

/// Identity used for duplicate detection and replace matching.
pub fn key(host: &str, port: u16, username: &str) -> String {
    format!("{}|{}|{}", host.to_lowercase(), port, username.to_lowercase())
}

pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut group: Option<String> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            if let Some(name) = comment
                .strip_prefix("Group:")
                .or_else(|| comment.strip_prefix("group:"))
                .map(str::trim)
            {
                group = (!name.is_empty()).then(|| name.to_owned());
            }
            continue;
        }
        match parse_line(line) {
            Ok(entry) => out.entries.push((group.clone(), entry)),
            Err(error) => out.errors.push(LineError { line: i + 1, error }),
        }
    }
    out
}

fn valid_hostname(h: &str) -> bool {
    let bytes = h.as_bytes();
    !h.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_ipv6(h: &str) -> bool {
    let (addr, _zone) = h.split_once('%').unwrap_or((h, ""));
    !addr.is_empty() && addr.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
}

pub fn parse_line(text: &str) -> Result<Entry, String> {
    let (host, mut rest): (String, Vec<String>) = if let Some(after_bracket) = text.strip_prefix('[') {
        let close = after_bracket.find(']').ok_or("Unclosed '[' in IPv6 host")?;
        let host = after_bracket[..close].to_owned();
        let after = &after_bracket[close + 1..];
        if !after.is_empty() && !after.starts_with(':') {
            return Err("Expected ':' after ']'".into());
        }
        if !valid_ipv6(&host) {
            return Err("Invalid IPv6 address".into());
        }
        let rest = if after.is_empty() {
            Vec::new()
        } else {
            after[1..].split(':').map(str::to_owned).collect()
        };
        (host, rest)
    } else if !text.contains(':') {
        // Spreadsheet-ish pastes: tab, comma, semicolon or whitespace separated.
        let parts: Vec<String> = if text.contains('\t') {
            text.split('\t').filter(|s| !s.is_empty()).map(str::to_owned).collect()
        } else if text.contains(',') {
            text.split(',').map(|s| s.trim().to_owned()).collect()
        } else if text.contains(';') {
            text.split(';').map(|s| s.trim().to_owned()).collect()
        } else {
            text.split_whitespace().map(str::to_owned).collect()
        };
        let host = parts.first().cloned().unwrap_or_default();
        let mut rest: Vec<String> = parts.into_iter().skip(1).collect();
        if rest.len() > 2 && !rest[0].chars().all(|c| c.is_ascii_digit()) {
            rest = vec![rest[0].clone(), rest[1..].join(" ")];
        }
        (host, rest)
    } else {
        let mut parts = text.split(':').map(str::to_owned);
        let host = parts.next().unwrap_or_default();
        (host, parts.collect())
    };

    let host = host.trim().to_owned();
    if host.is_empty() {
        return Err("Missing host".into());
    }
    if !text.starts_with('[') && !valid_hostname(&host) {
        return Err("Invalid host".into());
    }
    if rest.is_empty() || (rest.len() == 1 && rest[0].trim().is_empty()) {
        return Err("Missing username".into());
    }

    let mut port = DEFAULT_PORT;
    if rest.len() >= 3 && !rest[0].trim().is_empty() && rest[0].trim().chars().all(|c| c.is_ascii_digit()) {
        let p: u32 = rest[0].trim().parse().map_err(|_| format!("Invalid port {}", rest[0].trim()))?;
        if !(1..=65535).contains(&p) {
            return Err(format!("Invalid port {}", rest[0].trim()));
        }
        port = p as u16;
        rest.remove(0);
    }

    let user = rest[0].trim();
    if user.is_empty() {
        return Err("Missing username".into());
    }
    let password = rest[1..].join(":");
    let (domain, username) = match user.split_once('\\') {
        Some((d, u)) if !d.is_empty() => (d.to_owned(), u.to_owned()),
        _ => (String::new(), user.to_owned()),
    };
    if username.is_empty() {
        return Err("Missing username".into());
    }
    Ok(Entry {
        host,
        port,
        username,
        domain,
        password,
    })
}

/// Formats an entry back into a bulk line.
pub fn format_line(host: &str, port: u16, username: &str, domain: &str, password: Option<&str>) -> String {
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
    let user = if domain.is_empty() {
        username.to_owned()
    } else {
        format!("{domain}\\{username}")
    };
    match password {
        Some(p) => format!("{host}:{port}:{user}:{p}"),
        None => format!("{host}:{port}:{user}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(host: &str, port: u16, user: &str, domain: &str, pass: &str) -> Entry {
        Entry {
            host: host.into(),
            port,
            username: user.into(),
            domain: domain.into(),
            password: pass.into(),
        }
    }

    #[test]
    fn parses_all_shapes() {
        assert_eq!(parse_line("10.0.0.5:admin:hunter2"), Ok(e("10.0.0.5", 3389, "admin", "", "hunter2")));
        assert_eq!(parse_line("10.0.0.5:3390:admin:hunter2"), Ok(e("10.0.0.5", 3390, "admin", "", "hunter2")));
        assert_eq!(
            parse_line(r"srv01.corp.local:CORP\jdoe:p@ss:word"),
            Ok(e("srv01.corp.local", 3389, "jdoe", "CORP", "p@ss:word"))
        );
        assert_eq!(parse_line("srv01:jdoe@corp.local:secret"), Ok(e("srv01", 3389, "jdoe@corp.local", "", "secret")));
        assert_eq!(parse_line("[fe80::1]:3389:admin:secret"), Ok(e("fe80::1", 3389, "admin", "", "secret")));
        assert_eq!(parse_line("10.0.0.5  admin  secret"), Ok(e("10.0.0.5", 3389, "admin", "", "secret")));
        assert_eq!(parse_line("10.0.0.5\tadmin\tsecret"), Ok(e("10.0.0.5", 3389, "admin", "", "secret")));
        assert_eq!(parse_line("10.0.0.5:admin"), Ok(e("10.0.0.5", 3389, "admin", "", "")));
        // Two fields after the host: the second is a password even if numeric.
        assert_eq!(parse_line("h:admin:1234"), Ok(e("h", 3389, "admin", "", "1234")));
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(parse_line("a").is_err());
        assert_eq!(parse_line("a:99999:u:p"), Err("Invalid port 99999".into()));
        assert!(parse_line("bad host!:u:p").is_err());
        assert!(parse_line("[fe80::1:u:p").is_err());
    }

    #[test]
    fn groups_and_comments() {
        let p = parse("# hello\n# Group: Prod\na:u:p\n\nb:u:p\n# Group: Lab\nc:u:p\nbad\n");
        let groups: Vec<_> = p.entries.iter().map(|(g, e)| (g.clone(), e.host.clone())).collect();
        assert_eq!(
            groups,
            vec![
                (Some("Prod".into()), "a".into()),
                (Some("Prod".into()), "b".into()),
                (Some("Lab".into()), "c".into())
            ]
        );
        assert_eq!(p.errors.len(), 1);
        assert_eq!(p.errors[0].line, 8);
    }

    #[test]
    fn formats_round_trip() {
        let line = format_line("fe80::1", 3390, "jdoe", "CORP", Some("a:b"));
        assert_eq!(line, r"[fe80::1]:3390:CORP\jdoe:a:b");
        assert_eq!(parse_line(&line), Ok(e("fe80::1", 3390, "jdoe", "CORP", "a:b")));
    }
}
