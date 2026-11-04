//! Protocol input → iced events, tracking where the synthetic cursor is.

use iced::keyboard::{self, key};
use iced::mouse::{self, Cursor};
use iced::{Event, Point};
use iced_mcp_protocol::{Input, Modifiers, MouseButton, ScrollUnit};

/// The events of one frame, each with the cursor it is delivered at.
pub(crate) fn events(inputs: Vec<Input>, cursor: &mut Option<Point>) -> Vec<(Event, Cursor)> {
    inputs
        .into_iter()
        .map(|input| {
            let event = event(input, cursor);
            let at = cursor.map_or(Cursor::Unavailable, Cursor::Available);
            (event, at)
        })
        .collect()
}

fn event(input: Input, cursor: &mut Option<Point>) -> Event {
    match input {
        Input::Move { x, y } => {
            let position = Point::new(x, y);
            *cursor = Some(position);
            Event::Mouse(mouse::Event::CursorMoved { position })
        }
        Input::Press { button } => Event::Mouse(mouse::Event::ButtonPressed(button_of(button))),
        Input::Release { button } => Event::Mouse(mouse::Event::ButtonReleased(button_of(button))),
        Input::Scroll { x, y, unit } => Event::Mouse(mouse::Event::WheelScrolled {
            delta: match unit {
                ScrollUnit::Lines => mouse::ScrollDelta::Lines { x, y },
                ScrollUnit::Pixels => mouse::ScrollDelta::Pixels { x, y },
            },
        }),
        Input::KeyDown {
            key,
            text,
            modifiers,
        } => {
            let key = parse_key(&key);
            Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: unidentified(),
                location: keyboard::Location::Standard,
                modifiers: modifiers_of(modifiers),
                text: text.map(Into::into),
                repeat: false,
            })
        }
        Input::KeyUp { key, modifiers } => {
            let key = parse_key(&key);
            Event::Keyboard(keyboard::Event::KeyReleased {
                modified_key: key.clone(),
                key,
                physical_key: unidentified(),
                location: keyboard::Location::Standard,
                modifiers: modifiers_of(modifiers),
            })
        }
        Input::Modifiers { modifiers } => {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers_of(modifiers)))
        }
        Input::Leave => {
            *cursor = None;
            Event::Mouse(mouse::Event::CursorLeft)
        }
    }
}

fn unidentified() -> key::Physical {
    key::Physical::Unidentified(key::NativeCode::Unidentified)
}

fn button_of(button: MouseButton) -> mouse::Button {
    match button {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Middle => mouse::Button::Middle,
        MouseButton::Back => mouse::Button::Back,
        MouseButton::Forward => mouse::Button::Forward,
    }
}

fn modifiers_of(modifiers: Modifiers) -> keyboard::Modifiers {
    let mut out = keyboard::Modifiers::empty();
    out.set(keyboard::Modifiers::SHIFT, modifiers.shift);
    out.set(keyboard::Modifiers::CTRL, modifiers.ctrl);
    out.set(keyboard::Modifiers::ALT, modifiers.alt);
    out.set(keyboard::Modifiers::LOGO, modifiers.logo);
    out
}

/// A key by its W3C name (`Enter`, `ArrowDown`, `F5`…) or a single
/// character (`a`, `/`). Names are case-insensitive; anything else is sent
/// as a character key.
pub(crate) fn parse_key(name: &str) -> keyboard::Key {
    use key::Named;
    let named = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(Named::Enter),
        "tab" => Some(Named::Tab),
        "space" | " " => Some(Named::Space),
        "escape" | "esc" => Some(Named::Escape),
        "backspace" => Some(Named::Backspace),
        "delete" | "del" => Some(Named::Delete),
        "insert" => Some(Named::Insert),
        "home" => Some(Named::Home),
        "end" => Some(Named::End),
        "pageup" => Some(Named::PageUp),
        "pagedown" => Some(Named::PageDown),
        "arrowup" | "up" => Some(Named::ArrowUp),
        "arrowdown" | "down" => Some(Named::ArrowDown),
        "arrowleft" | "left" => Some(Named::ArrowLeft),
        "arrowright" | "right" => Some(Named::ArrowRight),
        "shift" => Some(Named::Shift),
        "control" | "ctrl" => Some(Named::Control),
        "alt" | "option" => Some(Named::Alt),
        "super" | "meta" | "cmd" | "command" | "logo" => Some(Named::Super),
        "f1" => Some(Named::F1),
        "f2" => Some(Named::F2),
        "f3" => Some(Named::F3),
        "f4" => Some(Named::F4),
        "f5" => Some(Named::F5),
        "f6" => Some(Named::F6),
        "f7" => Some(Named::F7),
        "f8" => Some(Named::F8),
        "f9" => Some(Named::F9),
        "f10" => Some(Named::F10),
        "f11" => Some(Named::F11),
        "f12" => Some(Named::F12),
        _ => None,
    };
    match named {
        Some(named) if name != " " => keyboard::Key::Named(named),
        Some(_) => keyboard::Key::Named(Named::Space),
        None => keyboard::Key::Character(name.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_by_name_or_character() {
        assert_eq!(parse_key("Enter"), keyboard::Key::Named(key::Named::Enter));
        assert_eq!(
            parse_key("arrowdown"),
            keyboard::Key::Named(key::Named::ArrowDown)
        );
        assert_eq!(parse_key("a"), keyboard::Key::Character("a".into()));
        assert_eq!(parse_key("/"), keyboard::Key::Character("/".into()));
    }

    #[test]
    fn moves_carry_the_cursor_forward() {
        let mut cursor = None;
        let frame = events(
            vec![
                Input::Move { x: 10.0, y: 20.0 },
                Input::Press {
                    button: MouseButton::Left,
                },
            ],
            &mut cursor,
        );
        assert_eq!(cursor, Some(Point::new(10.0, 20.0)));
        assert!(frame
            .iter()
            .all(|(_, at)| *at == Cursor::Available(Point::new(10.0, 20.0))));
    }
}
