//! Key chords and text → protocol input frames.

use iced_mcp_protocol::{Input, Modifiers};

/// `"Ctrl+Shift+P"`, `"Cmd+S"`, `"Enter"`, `"a"`. `Mod` is Cmd on macOS and
/// Ctrl elsewhere, for shortcuts written once for both.
pub(crate) fn chord(spec: &str) -> Result<(Modifiers, String), String> {
    let parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    // "Ctrl++" → modifiers then the "+" key.
    let (mods, key) = match parts.as_slice() {
        [.., "", ""] => (&parts[..parts.len() - 2], "+"),
        [rest @ .., key] => (rest, *key),
        [] => return Err("empty key".into()),
    };
    if key.is_empty() {
        return Err(format!("no key in `{spec}`"));
    }
    let mut modifiers = Modifiers::default();
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "shift" => modifiers.shift = true,
            "ctrl" | "control" => modifiers.ctrl = true,
            "alt" | "option" | "opt" => modifiers.alt = true,
            "cmd" | "command" | "meta" | "super" | "win" | "logo" => modifiers.logo = true,
            "mod" | "primary" => {
                if cfg!(target_os = "macos") {
                    modifiers.logo = true;
                } else {
                    modifiers.ctrl = true;
                }
            }
            other => return Err(format!("unknown modifier `{other}` in `{spec}`")),
        }
    }
    Ok((modifiers, key.to_owned()))
}

/// The frames of pressing a chord once: modifiers down, key down and up,
/// modifiers released — each its own frame so shortcut handlers see the
/// modifier state the way they would from a keyboard.
pub(crate) fn press(modifiers: Modifiers, key: &str) -> Vec<Vec<Input>> {
    let any_modifier = modifiers != Modifiers::default();
    // A bare printable key types itself; a chord with Ctrl/Cmd/Alt does not.
    let types = key.chars().count() == 1 && !modifiers.ctrl && !modifiers.logo && !modifiers.alt;
    let text = types.then(|| {
        if modifiers.shift {
            key.to_uppercase()
        } else {
            key.to_owned()
        }
    });
    let mut frames = Vec::new();
    if any_modifier {
        frames.push(vec![Input::Modifiers { modifiers }]);
    }
    frames.push(vec![
        Input::KeyDown {
            key: key.to_owned(),
            text,
            modifiers,
        },
        Input::KeyUp {
            key: key.to_owned(),
            modifiers,
        },
    ]);
    if any_modifier {
        frames.push(vec![Input::Modifiers {
            modifiers: Modifiers::default(),
        }]);
    }
    frames
}

/// One frame per character, so each keystroke is applied before the next.
pub(crate) fn typing(text: &str) -> Vec<Vec<Input>> {
    text.chars()
        .map(|c| {
            let (key, typed) = match c {
                '\n' => ("Enter".to_owned(), None),
                '\t' => ("Tab".to_owned(), None),
                c => (c.to_string(), Some(c.to_string())),
            };
            vec![
                Input::KeyDown {
                    key: key.clone(),
                    text: typed,
                    modifiers: Modifiers::default(),
                },
                Input::KeyUp {
                    key,
                    modifiers: Modifiers::default(),
                },
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse() {
        let (m, k) = chord("Ctrl+Shift+P").unwrap();
        assert!(m.ctrl && m.shift && !m.alt);
        assert_eq!(k, "P");
        let (m, k) = chord("Enter").unwrap();
        assert_eq!(m, Modifiers::default());
        assert_eq!(k, "Enter");
        let (m, k) = chord("Ctrl++").unwrap();
        assert!(m.ctrl);
        assert_eq!(k, "+");
        assert!(chord("Hyper+A").is_err());
    }

    #[test]
    fn shortcuts_do_not_type_but_plain_keys_do() {
        let frames = press(
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
            "s",
        );
        assert_eq!(frames.len(), 3);
        assert!(matches!(&frames[1][0], Input::KeyDown { text: None, .. }));
        let frames = press(Modifiers::default(), "a");
        assert!(matches!(&frames[0][0], Input::KeyDown { text: Some(t), .. } if t == "a"));
    }
}
