//! Key names → scancodes, for the REST API (`"ctrl+alt+del"`, `"win+r"`, `"f5"` …).

use anyhow::bail;
use rdp_engine::InputEvent;

/// Set-1 scancode for a key name (case-insensitive). Extended keys carry 0xE000.
pub fn scancode(name: &str) -> Option<u16> {
    let n = name.trim().to_ascii_lowercase();
    let code = match n.as_str() {
        "esc" | "escape" => 0x01,
        "1" => 0x02,
        "2" => 0x03,
        "3" => 0x04,
        "4" => 0x05,
        "5" => 0x06,
        "6" => 0x07,
        "7" => 0x08,
        "8" => 0x09,
        "9" => 0x0a,
        "0" => 0x0b,
        "-" | "minus" => 0x0c,
        "=" | "equal" | "equals" => 0x0d,
        "backspace" | "bksp" => 0x0e,
        "tab" => 0x0f,
        "q" => 0x10,
        "w" => 0x11,
        "e" => 0x12,
        "r" => 0x13,
        "t" => 0x14,
        "y" => 0x15,
        "u" => 0x16,
        "i" => 0x17,
        "o" => 0x18,
        "p" => 0x19,
        "[" | "bracketleft" => 0x1a,
        "]" | "bracketright" => 0x1b,
        "enter" | "return" => 0x1c,
        "ctrl" | "control" | "lctrl" | "leftctrl" => 0x1d,
        "a" => 0x1e,
        "s" => 0x1f,
        "d" => 0x20,
        "f" => 0x21,
        "g" => 0x22,
        "h" => 0x23,
        "j" => 0x24,
        "k" => 0x25,
        "l" => 0x26,
        ";" | "semicolon" => 0x27,
        "'" | "quote" => 0x28,
        "`" | "backquote" | "grave" => 0x29,
        "shift" | "lshift" | "leftshift" => 0x2a,
        "\\" | "backslash" => 0x2b,
        "z" => 0x2c,
        "x" => 0x2d,
        "c" => 0x2e,
        "v" => 0x2f,
        "b" => 0x30,
        "n" => 0x31,
        "m" => 0x32,
        "," | "comma" => 0x33,
        "." | "period" => 0x34,
        "/" | "slash" => 0x35,
        "rshift" | "rightshift" => 0x36,
        "numpad*" | "multiply" => 0x37,
        "alt" | "lalt" | "leftalt" => 0x38,
        "space" => 0x39,
        "capslock" => 0x3a,
        "f1" => 0x3b,
        "f2" => 0x3c,
        "f3" => 0x3d,
        "f4" => 0x3e,
        "f5" => 0x3f,
        "f6" => 0x40,
        "f7" => 0x41,
        "f8" => 0x42,
        "f9" => 0x43,
        "f10" => 0x44,
        "numlock" => 0x45,
        "scrolllock" => 0x46,
        "numpad7" => 0x47,
        "numpad8" => 0x48,
        "numpad9" => 0x49,
        "numpad-" | "subtract" => 0x4a,
        "numpad4" => 0x4b,
        "numpad5" => 0x4c,
        "numpad6" => 0x4d,
        "numpad+" | "add" => 0x4e,
        "numpad1" => 0x4f,
        "numpad2" => 0x50,
        "numpad3" => 0x51,
        "numpad0" => 0x52,
        "numpad." | "decimal" => 0x53,
        "f11" => 0x57,
        "f12" => 0x58,
        "numpadenter" => 0xe01c,
        "rctrl" | "rightctrl" => 0xe01d,
        "numpad/" | "divide" => 0xe035,
        "printscreen" | "prtsc" | "print" => 0xe037,
        "ralt" | "rightalt" | "altgr" => 0xe038,
        "home" => 0xe047,
        "up" | "arrowup" => 0xe048,
        "pageup" | "pgup" => 0xe049,
        "left" | "arrowleft" => 0xe04b,
        "right" | "arrowright" => 0xe04d,
        "end" => 0xe04f,
        "down" | "arrowdown" => 0xe050,
        "pagedown" | "pgdn" => 0xe051,
        "insert" | "ins" => 0xe052,
        "delete" | "del" => 0xe053,
        "win" | "windows" | "meta" | "super" | "cmd" | "lwin" => 0xe05b,
        "rwin" => 0xe05c,
        "menu" | "apps" | "contextmenu" => 0xe05d,
        "volumemute" => 0xe020,
        "volumedown" => 0xe02e,
        "volumeup" => 0xe030,
        "mediaplaypause" => 0xe022,
        "medianext" => 0xe019,
        "mediaprev" => 0xe010,
        _ => return None,
    };
    Some(code)
}

/// `"ctrl+shift+esc"` → press in order, release in reverse.
pub fn combo(spec: &str) -> anyhow::Result<Vec<InputEvent>> {
    let mut codes = Vec::new();
    // "+" alone (or trailing "++") means the plus key itself.
    let parts: Vec<&str> = if spec.trim() == "+" {
        vec!["equal"]
    } else {
        spec.split('+').filter(|p| !p.trim().is_empty()).collect()
    };
    if parts.is_empty() {
        bail!("empty key combination");
    }
    for part in parts {
        match scancode(part) {
            Some(c) => codes.push(c),
            None => bail!("unknown key {part:?}"),
        }
    }
    let mut events: Vec<InputEvent> = codes.iter().map(|&code| InputEvent::Key { code, down: true }).collect();
    events.extend(codes.iter().rev().map(|&code| InputEvent::Key { code, down: false }));
    Ok(events)
}

/// Types text as Unicode key events (layout independent). `\n` → Enter, `\t` → Tab.
pub fn text(s: &str) -> Vec<InputEvent> {
    let mut events = Vec::with_capacity(s.len() * 2);
    for ch in s.chars() {
        match ch {
            '\r' => {}
            '\n' => {
                events.push(InputEvent::Key { code: 0x1c, down: true });
                events.push(InputEvent::Key { code: 0x1c, down: false });
            }
            '\t' => {
                events.push(InputEvent::Key { code: 0x0f, down: true });
                events.push(InputEvent::Key { code: 0x0f, down: false });
            }
            ch => {
                events.push(InputEvent::Unicode { ch, down: true });
                events.push(InputEvent::Unicode { ch, down: false });
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(events: &[InputEvent]) -> Vec<(u16, bool)> {
        events
            .iter()
            .filter_map(|e| match e {
                InputEvent::Key { code, down } => Some((*code, *down)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_combos() {
        assert_eq!(
            codes(&combo("Ctrl+Alt+Del").unwrap()),
            vec![(0x1d, true), (0x38, true), (0xe053, true), (0xe053, false), (0x38, false), (0x1d, false)]
        );
        assert_eq!(codes(&combo("win+r").unwrap())[0], (0xe05b, true));
        assert!(combo("ctrl+nope").is_err());
        assert!(combo("").is_err());
    }

    #[test]
    fn types_text_with_newlines() {
        let ev = text("a\nb");
        assert_eq!(ev.len(), 6);
        assert!(matches!(ev[2], InputEvent::Key { code: 0x1c, down: true }));
    }
}
