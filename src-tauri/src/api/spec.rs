//! Single source of truth for API documentation: the OpenAPI document, the
//! `/llms.txt` reference and the HTML docs page are all generated from this table.

use serde_json::{Map, Value, json};

pub struct Param {
    pub name: &'static str,
    /// "path" or "query"
    pub location: &'static str,
    pub ty: &'static str,
    pub description: &'static str,
}

pub struct Endpoint {
    pub method: &'static str,
    pub path: &'static str,
    pub tag: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub params: &'static [Param],
    /// Example JSON request body.
    pub body: Option<&'static str>,
    /// Example JSON response (or a note for non-JSON responses).
    pub response: Option<&'static str>,
    pub auth: bool,
}

const fn p(name: &'static str, location: &'static str, ty: &'static str, description: &'static str) -> Param {
    Param {
        name,
        location,
        ty,
        description,
    }
}

const ID: Param = p("id", "path", "string", "Connection id");
const GID: Param = p("id", "path", "string", "Group id (\"all\" is the built-in group)");

pub const OVERVIEW: &str = r#"Local REST API for RDP App. Everything a user can do in the window can be done here: manage groups
and connections, connect/disconnect sessions, see live session state, take screenshots, send keyboard
and mouse input, change settings, import/export, and drive the UI.

Base URL: http://127.0.0.1:<port>/api/v1 (loopback only; the port is in Settings → Automation, default 47123).

Authentication: every /api/v1 request except /health needs the token from Settings → Automation:
  Authorization: Bearer <token>        (or ?token=<token> for clients that cannot set headers)

Conventions:
- JSON in, JSON out (camelCase). Errors: {"error": "message"} with a 4xx/5xx status.
- Connection ids double as session ids. The built-in group id is "all".
- Passwords are write-only: the only way to read them back is an export, which always includes them in plain text.
- Connection settings are layered: the app settings (/settings), overridden by a group's "settings", overridden by a
  connection's "settings". Overridable keys: audio, microphone, clipboardSync, driveRedirection (none|all|custom),
  drivePaths (folders shared when custom), printerRedirection, smartcardRedirection, autoReconnect, codec,
  h264Quality, videoDecoder, remoteWallpaper, companion, hostStatsInterval, directTransfers. A "settings" object
  holds only the overrides; {} inherits everything.
- The companion (setting "companion") is a small agent in the remote session: it reports host load (/stats) and
  receives files (/files/send).
- Connecting is asynchronous. Add ?wait=true to block until sessions are connected or failed (timeout ?timeout=30 s),
  or follow GET /api/v1/events (Server-Sent Events).
- Coordinates for mouse input are remote desktop pixels; GET /sessions/{id} reports the current width/height.

Typical agent flow:
  1. GET  /api/v1/state                         → groups, connections, sessions
  2. POST /api/v1/sessions/{id}/connect?wait=true
  3. GET  /api/v1/sessions/{id}/screenshot?maxWidth=1280   → PNG to look at
  4. POST /api/v1/sessions/{id}/mouse {"action":"click","x":640,"y":400}
     POST /api/v1/sessions/{id}/keys  {"keys":"win+r"}
     POST /api/v1/sessions/{id}/type  {"text":"notepad\n"}
  5. POST /api/v1/sessions/{id}/disconnect"#;

pub const ENDPOINTS: &[Endpoint] = &[
    // ── Meta ────────────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/health",
        tag: "Meta",
        summary: "Health check (no auth)",
        description: "Returns ok and the app version. Useful to discover whether the API is running.",
        params: &[],
        body: None,
        response: Some(r#"{"ok": true, "version": "0.1.0"}"#),
        auth: false,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/state",
        tag: "Meta",
        summary: "Everything at once",
        description: "Groups, connections, settings, every session's state and the UI state. Start here.",
        params: &[],
        body: None,
        response: Some(
            r#"{"library": {"groups": [{"id":"all","name":"All","collapsed":false,"builtin":true,"gateway":null,"settings":{}}], "connections": [{"id":"9f1c…","name":"","host":"10.0.0.5","port":3389,"username":"admin","domain":"","hasPassword":true,"groupId":"all","notes":"","createdAt":1760857200000,"gateway":null,"settings":{}}], "settings": {…}}, "sessions": [{"id":"9f1c…","status":"connected","width":1920,"height":1080,"codec":"H.264 · AVC420"}], "ui": {"selectedId":"9f1c…","view":"session","fullscreen":false}}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/events",
        tag: "Meta",
        summary: "Live event stream (Server-Sent Events)",
        description: "Each event's data is JSON with a \"type\": status (id, status, message), resolution (id, width, height), codec (id, codec), certificate (id, endpoint, fingerprint, reason), libraryChanged, uiState (selectedId, view, fullscreen). Statuses: idle, queued, connecting, connected, reconnecting, error.",
        params: &[],
        body: None,
        response: Some("text/event-stream — data: {\"type\":\"status\",\"id\":\"9f1c…\",\"status\":\"connected\"}"),
        auth: true,
    },
    // ── Connections ─────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/connections",
        tag: "Connections",
        summary: "List connections",
        description: "In display order. Filter by group, by a search string (name/host/user) or by session status.",
        params: &[
            p("group", "query", "string", "Group id"),
            p(
                "q",
                "query",
                "string",
                "Case-insensitive search in name, host and username",
            ),
            p(
                "status",
                "query",
                "string",
                "idle | queued | connecting | connected | reconnecting | error",
            ),
        ],
        body: None,
        response: Some(
            r#"[{"id":"9f1c…","name":"web-01","host":"10.0.0.5","port":3389,"username":"admin","domain":"","hasPassword":true,"groupId":"all","notes":"","createdAt":1760857200000,"gateway":null,"settings":{}}]"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/connections",
        tag: "Connections",
        summary: "Add one or many connections",
        description: "Body is one object or an array. Only host and username are required; port defaults to 3389 and groupId to \"all\". Nothing connects until you ask.",
        params: &[],
        body: Some(
            r#"[{"name":"web-01","host":"10.0.0.5","port":3389,"username":"admin","domain":"CORP","password":"secret","groupId":"all","notes":"","gateway":null}]"#,
        ),
        response: Some(r#"[{"id":"9f1c…","name":"web-01","host":"10.0.0.5", "…": "…"}]"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/connections/bulk",
        tag: "Connections",
        summary: "Add connections from bulk text",
        description: "One connection per line: host:user:pass, host:port:user:pass, DOMAIN\\user, user@domain, [ipv6]:port:user:pass. Passwords may contain ':'. Lines starting with # are comments. mode \"replace\" makes the group contain exactly these connections (matching host+port+user keep their identity and sessions).",
        params: &[],
        body: Some(
            r#"{"text": "10.0.0.5:admin:secret\n10.0.0.6:3390:CORP\\jdoe:p@ss", "groupId": "all", "mode": "append"}"#,
        ),
        response: Some(
            r#"{"added": 2, "kept": 0, "removed": 0, "errors": [{"line": 3, "error": "Missing username"}]}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/connections/{id}",
        tag: "Connections",
        summary: "Get a connection",
        description: "",
        params: &[ID],
        body: None,
        response: Some(
            r#"{"id":"9f1c…","name":"web-01","host":"10.0.0.5","port":3389,"username":"admin","domain":"","hasPassword":true,"groupId":"all","notes":"","createdAt":1760857200000,"gateway":null,"settings":{}}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "PATCH",
        path: "/api/v1/connections/{id}",
        tag: "Connections",
        summary: "Update a connection",
        description: "Only the fields you send change. password: \"\" clears it. gateway: null means \"use the group's gateway\"; {\"enabled\": false} forces a direct connection. settings: the connection's complete set of setting overrides (see Connection settings); send {} to follow its group and the app.",
        params: &[ID],
        body: Some(
            r#"{"name":"web-01 (new)","password":"new-secret","groupId":"<group id>","gateway":{"enabled":true,"host":"rdg.example.com","sameCredentials":true,"username":"","domain":""}}"#,
        ),
        response: Some(r#"{"id":"9f1c…", "…": "…"}"#),
        auth: true,
    },
    Endpoint {
        method: "DELETE",
        path: "/api/v1/connections/{id}",
        tag: "Connections",
        summary: "Delete a connection",
        description: "Disconnects its session first.",
        params: &[ID],
        body: None,
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/connections/delete",
        tag: "Connections",
        summary: "Delete many connections",
        description: "",
        params: &[],
        body: Some(r#"{"ids": ["9f1c…", "2b7a…"]}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/connections/{id}/duplicate",
        tag: "Connections",
        summary: "Duplicate a connection",
        description: "Copies every field, including the stored password, into a new connection in the same group.",
        params: &[ID],
        body: None,
        response: Some(r#"{"id":"<new id>", "…": "…"}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/connections/reorder",
        tag: "Connections",
        summary: "Set display order and group membership",
        description: "The full desired order. Connections you leave out keep their relative order at the end.",
        params: &[],
        body: Some(r#"{"order": [{"id": "9f1c…", "groupId": "all"}, {"id": "2b7a…", "groupId": "<group id>"}]}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    // ── Groups ──────────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/groups",
        tag: "Groups",
        summary: "List groups",
        description: "In display order; the built-in \"All\" group (id \"all\") is always first. Each group includes its connection count.",
        params: &[],
        body: None,
        response: Some(
            r#"[{"id":"all","name":"All","collapsed":false,"builtin":true,"gateway":null,"connections":12}]"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/groups",
        tag: "Groups",
        summary: "Create a group",
        description: "Optionally with an RD Gateway used by its connections.",
        params: &[],
        body: Some(
            r#"{"name":"Production","gateway":{"enabled":true,"host":"rdg.example.com","sameCredentials":false,"username":"gwuser","domain":"CORP","password":"gw-secret"}}"#,
        ),
        response: Some(
            r#"{"id":"<group id>","name":"Production","collapsed":false,"builtin":false,"gateway":{"enabled":true,"host":"rdg.example.com","sameCredentials":false,"username":"gwuser","domain":"CORP","hasPassword":true}}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "PATCH",
        path: "/api/v1/groups/{id}",
        tag: "Groups",
        summary: "Rename / collapse / set gateway or setting overrides",
        description: "gateway: null removes the group's gateway. settings replaces the group's overrides for its connections ({} clears them). The built-in group cannot be renamed.",
        params: &[GID],
        body: Some(
            r#"{"name":"Prod","collapsed":true,"settings":{"driveRedirection":"custom","drivePaths":["D:\\Shared"]}}"#,
        ),
        response: Some(r#"{"id":"<group id>","name":"Prod", "…": "…"}"#),
        auth: true,
    },
    Endpoint {
        method: "DELETE",
        path: "/api/v1/groups/{id}",
        tag: "Groups",
        summary: "Delete a group",
        description: "Its connections move to \"All\" unless ?connections=delete.",
        params: &[GID, p("connections", "query", "string", "move (default) | delete")],
        body: None,
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/groups/reorder",
        tag: "Groups",
        summary: "Reorder groups",
        description: "Ids in the desired order; \"all\" always stays first.",
        params: &[],
        body: Some(r#"{"ids": ["<group id>", "<group id>"]}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/groups/{id}/replace",
        tag: "Groups",
        summary: "Replace all connections of a group",
        description: "The group ends up containing exactly these connections, in order. Send bulk text or a connections array. Entries matching an existing connection (same host, port, username — case-insensitive) keep it and its live session, updating name/password/domain when given; the rest are added; connections not in the list are disconnected and deleted.",
        params: &[GID],
        body: Some(r#"{"text": "10.0.0.5:admin:secret\n10.0.0.7:admin:secret"}"#),
        response: Some(r#"{"kept": 1, "added": 1, "removed": 3, "errors": []}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/groups/{id}/connect",
        tag: "Groups",
        summary: "Connect every connection in a group",
        description: "Queued through the parallel-connect limit. ?wait=true blocks until all are connected or failed.",
        params: &[
            GID,
            p("wait", "query", "boolean", "Block until settled"),
            p("timeout", "query", "number", "Seconds (default 30)"),
        ],
        body: None,
        response: Some(r#"{"sessions": [{"id":"9f1c…","status":"connected"}]}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/groups/{id}/disconnect",
        tag: "Groups",
        summary: "Disconnect every connection in a group",
        description: "",
        params: &[GID],
        body: None,
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/groups/{id}/export",
        tag: "Groups",
        summary: "Export one group with its connections",
        description: "Same options as GET /export.",
        params: &[GID, p("format", "query", "string", "json (default) | text")],
        body: None,
        response: Some("The export document (application/json) or bulk text (text/plain)."),
        auth: true,
    },
    // ── Sessions ────────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/sessions",
        tag: "Sessions",
        summary: "State of every session",
        description: "One entry per connection. status: idle | queued | connecting | connected | reconnecting | error (message explains).",
        params: &[],
        body: None,
        response: Some(
            r#"[{"id":"9f1c…","status":"connected","width":1920,"height":1080,"codec":"H.264 · AVC420"},{"id":"2b7a…","status":"error","message":"Authentication failed — check username and password"}]"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/sessions/{id}",
        tag: "Sessions",
        summary: "State of one session",
        description: "",
        params: &[ID],
        body: None,
        response: Some(r#"{"id":"9f1c…","status":"connected","width":1920,"height":1080,"codec":"H.264 · AVC420"}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/connect",
        tag: "Sessions",
        summary: "Connect many (or all)",
        description: "Body: {\"ids\": [...]} or {\"all\": true}. ?wait=true blocks until settled.",
        params: &[
            p("wait", "query", "boolean", "Block until settled"),
            p("timeout", "query", "number", "Seconds (default 30)"),
        ],
        body: Some(r#"{"ids": ["9f1c…", "2b7a…"]}"#),
        response: Some(
            r#"{"sessions": [{"id":"9f1c…","status":"connected"},{"id":"2b7a…","status":"error","message":"…"}]}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/disconnect",
        tag: "Sessions",
        summary: "Disconnect many (or all)",
        description: "Body: {\"ids\": [...]} or {\"all\": true}.",
        params: &[],
        body: Some(r#"{"all": true}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/connect",
        tag: "Sessions",
        summary: "Connect a session",
        description: "?wait=true blocks until connected or failed and returns the final state. If it fails with a certificate event, trust it via POST /certificates/trust and connect again.",
        params: &[
            ID,
            p("wait", "query", "boolean", "Block until settled"),
            p("timeout", "query", "number", "Seconds (default 30)"),
        ],
        body: None,
        response: Some(r#"{"id":"9f1c…","status":"connected","width":1920,"height":1080,"codec":"H.264 · AVC420"}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/disconnect",
        tag: "Sessions",
        summary: "Disconnect a session",
        description: "The remote session stays signed in (like closing mstsc).",
        params: &[ID],
        body: None,
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/reconnect",
        tag: "Sessions",
        summary: "Reconnect a session",
        description: "",
        params: &[
            ID,
            p("wait", "query", "boolean", "Block until settled"),
            p("timeout", "query", "number", "Seconds (default 30)"),
        ],
        body: None,
        response: Some(r#"{"id":"9f1c…","status":"connected"}"#),
        auth: true,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/sessions/{id}/screenshot",
        tag: "Sessions",
        summary: "Screenshot of a session (PNG)",
        description: "Returns image/png of the current remote desktop. Paused (off-screen) sessions are briefly resumed to get a fresh frame. maxWidth downscales (keeps aspect ratio); coordinates for /mouse are always in full-resolution remote pixels (see width/height of GET /sessions/{id}).",
        params: &[ID, p("maxWidth", "query", "number", "Downscale to at most this width")],
        body: None,
        response: Some("image/png"),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/keys",
        tag: "Input",
        summary: "Press key combinations",
        description: "One combination or a list, pressed in order. Names: a-z 0-9 f1-f12 enter tab esc space backspace delete insert home end pageup pagedown up down left right ctrl shift alt win menu printscreen capslock numlock, numpad0-9, volumeup/down/mute, and punctuation. Example: \"ctrl+shift+esc\". delayMs pauses between combinations.",
        params: &[ID],
        body: Some(r#"{"keys": ["win+r"], "delayMs": 300}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/type",
        tag: "Input",
        summary: "Type text",
        description: "Unicode typing, independent of the remote keyboard layout. \\n presses Enter, \\t presses Tab.",
        params: &[ID],
        body: Some(r#"{"text": "notepad\n"}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/mouse",
        tag: "Input",
        summary: "Mouse action",
        description: "action: move | click | double_click | down | up | scroll. x/y in remote pixels. button: left (default) | right | middle. scroll uses delta (120 = one notch up; negative = down) and horizontal: true for sideways.",
        params: &[ID],
        body: Some(r#"{"action": "click", "x": 640, "y": 400, "button": "left"}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/input",
        tag: "Input",
        summary: "Raw input events",
        description: "Low-level events in order. t: key (code = set-1 scancode, extended keys | 0xE000, down) | unicode (ch, down) | move (x, y) | button (button 0 left 1 middle 2 right, down) | wheel (delta, horizontal) | releaseAll | sync (caps, num, scroll: lock key states).",
        params: &[ID],
        body: Some(
            r#"{"events": [{"t":"move","x":100,"y":100},{"t":"button","button":0,"down":true},{"t":"button","button":0,"down":false},{"t":"key","code":57373,"down":true}]}"#,
        ),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/sessions/{id}/ctrl-alt-del",
        tag: "Input",
        summary: "Send Ctrl+Alt+Del",
        description: "",
        params: &[ID],
        body: None,
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    // ── Companion ───────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/stats",
        tag: "Companion",
        summary: "Host load of every session",
        description: "Latest sample per session whose companion reports it (setting hostStatsInterval, seconds; 0 = off). cpu, diskBusy are 0-100; rates are bytes per second averaged over span seconds; memory and volume sizes in bytes. diskRead/diskWrite/diskBusy are absent when the host doesn't expose disk counters.",
        params: &[],
        body: None,
        response: Some(
            r#"{"9f1c…":{"at":1760857200000,"span":30.0,"cpu":12.5,"memUsed":6871947673,"memTotal":17179869184,"netRx":81920,"netTx":20480,"diskRead":1048576,"diskWrite":524288,"diskBusy":4.2,"volumes":[{"name":"C:","free":107374182400,"total":274877906944}]}}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/sessions/{id}/stats",
        tag: "Companion",
        summary: "Host load of one session",
        description: "404 when there is none (no companion attached, or host stats are off).",
        params: &[ID],
        body: None,
        response: Some(
            r#"{"at":1760857200000,"span":30.0,"cpu":12.5,"memUsed":6871947673,"memTotal":17179869184,"netRx":81920,"netTx":20480,"volumes":[]}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/files/send",
        tag: "Companion",
        summary: "Send files and folders to connections",
        description: "Copies local paths (files or whole folders) to each connection, into folder on the remote: Desktop (default), Downloads, Documents, one of those with a subfolder (Desktop\\Drop), or a full path (%VARS% expanded). Names already there are kept and the new ones numbered. Connections that aren't connected are connected first; each send starts once its companion attaches (the companion setting must be on). Returns transfer ids; follow GET /transfers.",
        params: &[],
        body: Some(
            r#"{"ids": ["9f1c…", "2b7a…"], "paths": ["C:\Tools\setup.exe", "C:\Tools\config"], "folder": "Desktop"}"#,
        ),
        response: Some(r#"{"transfers": [2, 4]}"#),
        auth: true,
    },
    Endpoint {
        method: "GET",
        path: "/api/v1/transfers",
        tag: "Companion",
        summary: "Sends in progress or recently finished",
        description: "state: waiting (connecting / companion attaching) | sending | done | failed (error explains) | cancelled. sent and total are bytes.",
        params: &[],
        body: None,
        response: Some(
            r#"[{"session":"9f1c…","id":2,"label":"2 items","folder":"Desktop","files":14,"total":52428800,"sent":31457280,"state":"sending"}]"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/transfers/{id}/cancel",
        tag: "Companion",
        summary: "Cancel a send",
        description: "The remote deletes the partly written file.",
        params: &[p("id", "path", "number", "Transfer id")],
        body: None,
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    // ── Certificates ────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/certificates",
        tag: "Certificates",
        summary: "Trusted (pinned) server certificates",
        description: "",
        params: &[],
        body: None,
        response: Some(
            r#"[{"endpoint":"10.0.0.5:3389","fingerprint":"AB:CD:…","subject":"CN=web-01","firstSeen":1760857200000}]"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/certificates/trust",
        tag: "Certificates",
        summary: "Trust a server certificate",
        description: "Use the endpoint and fingerprint from a certificate event, then connect again.",
        params: &[],
        body: Some(r#"{"endpoint": "10.0.0.5:3389", "fingerprint": "AB:CD:…"}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    Endpoint {
        method: "DELETE",
        path: "/api/v1/certificates/{endpoint}",
        tag: "Certificates",
        summary: "Forget a pinned certificate",
        description: "URL-encode the endpoint (host:port).",
        params: &[p("endpoint", "path", "string", "host:port")],
        body: None,
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
    // ── Settings ────────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/settings",
        tag: "Settings",
        summary: "Current settings",
        description: "Keys: theme (dark|light|system), topTabs (active: tabs only for connecting/connected sessions | opened: any opened connection), connectOnSelect, maxConcurrentConnects (1-32), pauseHiddenSessions, codec (auto|h264|remotefx), h264Quality (balanced|sharp: 4:4:4 when the host offers it), videoDecoder (auto|hardware|software), remoteWallpaper (false: plain background, applies on the next connect), autoReconnect, clipboardSync, audio (off|active|all), microphone, driveRedirection (none|all|custom), drivePaths, printerRedirection, smartcardRedirection, certificatePolicy (tofu|prompt|ignore), apiEnabled, apiPort, plus UI layout values.",
        params: &[],
        body: None,
        response: Some(
            r#"{"theme":"dark","codec":"auto","audio":"active","certificatePolicy":"tofu","apiEnabled":true,"apiPort":47123, "…": "…"}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "PATCH",
        path: "/api/v1/settings",
        tag: "Settings",
        summary: "Change settings",
        description: "Only the keys you send change; null resets a key to its default. Invalid values are rejected with 400 and nothing is saved. Session options apply to new connections. Changing apiPort or apiEnabled restarts the API (the response is sent first).",
        params: &[],
        body: Some(r#"{"theme": "light", "audio": "off", "maxConcurrentConnects": 10}"#),
        response: Some(r#"{"theme":"light", "…": "…"}"#),
        auth: true,
    },
    // ── Import & export ─────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/export",
        tag: "Import & export",
        summary: "Export groups and connections",
        description: "format json keeps everything (groups, gateways, notes, passwords); text is bulk lines (host:port:user:pass) with \"# Group: name\" headers. Passwords are always included in plain text. Also available as POST with the same fields in a JSON body.",
        params: &[
            p(
                "groups",
                "query",
                "string",
                "Comma-separated group ids (default: all groups)",
            ),
            p("format", "query", "string", "json (default) | text"),
        ],
        body: None,
        response: Some(
            r#"{"format":"rdp-app/export","version":1,"exportedAt":"2026-10-19T07:00:00Z","passwords":"plain","groups":[{"name":"Production","connections":[{"name":"","host":"10.0.0.5","port":3389,"username":"admin","domain":"","password":"…"}]}]}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/import/preview",
        tag: "Import & export",
        summary: "Preview an import",
        description: "text is an export document or bulk text. target: \"byName\" (into groups with matching names, creating missing ones) or a group id.",
        params: &[],
        body: Some(r#"{"text": "…", "mode": "merge", "target": "byName"}"#),
        response: Some(
            r#"{"format":"json","groups":[{"name":"Production","connections":12,"existingGroupId":null,"duplicates":0}],"total":12,"errors":[]}"#,
        ),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/import",
        tag: "Import & export",
        summary: "Import",
        description: "mode merge adds new connections and updates matching ones (same host, port, user); replace makes each target group contain exactly the imported connections.",
        params: &[],
        body: Some(r##"{"text": "# Group: Lab\n10.0.1.5:admin:secret", "mode": "merge", "target": "byName"}"##),
        response: Some(r#"{"added": 1, "updated": 0, "removed": 0, "groupsCreated": 1}"#),
        auth: true,
    },
    // ── UI ──────────────────────────────────────────────────────────────────
    Endpoint {
        method: "GET",
        path: "/api/v1/ui",
        tag: "UI",
        summary: "What the window shows",
        description: "",
        params: &[],
        body: None,
        response: Some(r#"{"selectedId":"9f1c…","view":"session","fullscreen":false}"#),
        auth: true,
    },
    Endpoint {
        method: "POST",
        path: "/api/v1/ui",
        tag: "UI",
        summary: "Drive the window",
        description: "Any of: select (connection id to show), view (session | grid), fullscreen (bool), focus (bring the window to the front).",
        params: &[],
        body: Some(r#"{"select": "9f1c…", "view": "session", "focus": true}"#),
        response: Some(r#"{"ok": true}"#),
        auth: true,
    },
];

/// OpenAPI 3.1 document built from [`ENDPOINTS`].
pub fn openapi(base_url: &str) -> Value {
    let mut paths = Map::new();
    for e in ENDPOINTS {
        let mut op = Map::new();
        op.insert("tags".into(), json!([e.tag]));
        op.insert("summary".into(), json!(e.summary));
        if !e.description.is_empty() {
            op.insert("description".into(), json!(e.description));
        }
        let params: Vec<Value> = e
            .params
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "in": p.location,
                    "required": p.location == "path",
                    "description": p.description,
                    "schema": {"type": p.ty},
                })
            })
            .collect();
        if !params.is_empty() {
            op.insert("parameters".into(), Value::Array(params));
        }
        if let Some(body) = e.body {
            let example: Value = serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.into()));
            op.insert(
                "requestBody".into(),
                json!({"content": {"application/json": {"schema": {"type": "object"}, "example": example}}}),
            );
        }
        let response = match e.response {
            Some(r) if r.trim_start().starts_with(['{', '[']) => {
                let example: Value = serde_json::from_str(r).unwrap_or_else(|_| Value::String(r.into()));
                json!({"200": {"description": "OK", "content": {"application/json": {"example": example}}}})
            }
            Some(r) => json!({"200": {"description": r}}),
            None => json!({"200": {"description": "OK"}}),
        };
        op.insert("responses".into(), response);
        if !e.auth {
            op.insert("security".into(), json!([]));
        }
        let entry = paths.entry(e.path.to_owned()).or_insert_with(|| json!({}));
        entry
            .as_object_mut()
            .expect("path item is an object")
            .insert(e.method.to_lowercase(), Value::Object(op));
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "RDP App local API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": OVERVIEW,
        },
        "servers": [{"url": base_url}],
        "security": [{"bearer": []}],
        "components": {"securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}}},
        "paths": paths,
    })
}

/// Plain-text reference for language models.
pub fn llms_txt(base_url: &str) -> String {
    let mut out = String::new();
    out.push_str("# RDP App — local API\n\n");
    out.push_str(&OVERVIEW.replace("<port>", base_url.rsplit(':').next().unwrap_or("47123")));
    out.push_str("\n\n");
    let mut tag = "";
    for e in ENDPOINTS {
        if e.tag != tag {
            tag = e.tag;
            out.push_str(&format!("\n## {tag}\n"));
        }
        out.push_str(&format!("\n### {} {}\n{}\n", e.method, e.path, e.summary));
        if !e.description.is_empty() {
            out.push_str(e.description);
            out.push('\n');
        }
        for p in e.params {
            out.push_str(&format!("- {} ({}, {}): {}\n", p.name, p.location, p.ty, p.description));
        }
        if let Some(body) = e.body {
            out.push_str(&format!("Body: {body}\n"));
        }
        if let Some(r) = e.response {
            out.push_str(&format!("Returns: {r}\n"));
        }
        if !e.auth {
            out.push_str("(no auth)\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_are_valid_json_where_they_look_like_json() {
        for e in ENDPOINTS {
            if let Some(body) = e.body {
                assert!(
                    serde_json::from_str::<Value>(body).is_ok() || body.contains('…'),
                    "{} {} body",
                    e.method,
                    e.path
                );
            }
        }
        let doc = openapi("http://127.0.0.1:47123");
        assert!(doc["paths"]["/api/v1/sessions/{id}/screenshot"]["get"].is_object());
        assert!(llms_txt("http://127.0.0.1:47123").contains("POST /api/v1/groups/{id}/replace"));
    }
}
