//! Keyboard shortcuts and the keymap, after photocraft's `shortcuts.rs`:
//! shortcuts are written `Ctrl+Shift+P` (Ctrl is the command modifier) and
//! the user's choices in preferences replace a command's default keys.

use std::collections::{BTreeMap, HashMap};

use egui::{Key, Modifiers};

use crate::commands::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Shortcut {
    /// Parses `Ctrl+Alt+Shift+Key` in any modifier order; the key is an
    /// egui key name or its symbol (`Ctrl++` is Ctrl and Plus).
    pub fn parse(text: &str) -> Option<Self> {
        let mut shortcut = Self {
            ctrl: false,
            alt: false,
            shift: false,
            key: Key::Escape,
        };
        let text = text.trim();
        // The key `+` itself: `+`, `Ctrl++`.
        let text = match text.strip_suffix('+') {
            Some(rest) if rest.is_empty() || rest.ends_with('+') => format!("{rest}Plus"),
            _ => text.to_string(),
        };
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (name, modifiers) = parts.split_last()?;
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "cmd" => shortcut.ctrl = true,
                "alt" => shortcut.alt = true,
                "shift" => shortcut.shift = true,
                _ => return None,
            }
        }
        shortcut.key = Key::from_name(name)?;
        (!is_modifier(shortcut.key)).then_some(shortcut)
    }

    /// The shortcut a key press makes, for recording; `None` for a lone
    /// modifier. Shift is dropped from symbols, which layouts may need
    /// Shift to type.
    pub fn from_key(key: Key, modifiers: Modifiers) -> Option<Self> {
        if is_modifier(key) {
            return None;
        }
        Some(Self {
            ctrl: modifiers.command || modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift && !is_symbol(key),
            key,
        })
    }

    /// How preferences keep it: `Ctrl+Shift+Comma`.
    pub fn notation(self) -> String {
        self.join(self.key.name())
    }

    /// How menus and tooltips show it: `Ctrl+,`.
    pub fn label(self) -> String {
        let key = match self.key {
            Key::Minus => "-",
            key if is_symbol(key) => key.symbol_or_name(),
            key => key.name(),
        };
        self.join(key)
    }

    fn join(self, key: &str) -> String {
        let mut text = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                text.push_str(name);
            }
        }
        text.push_str(key);
        text
    }

    /// Whether typing text can't produce it, so it may fire while a text
    /// field has focus.
    pub fn works_in_text(self) -> bool {
        self.ctrl || self.alt || is_function_key(self.key)
    }

    fn modifier_count(self) -> usize {
        usize::from(self.ctrl) + usize::from(self.alt) + usize::from(self.shift)
    }

    /// Whether a key press is this shortcut. Ctrl and Alt must match; so
    /// must Shift, except on a symbol the shortcut doesn't shift.
    pub fn matches(self, key: Key, modifiers: Modifiers) -> bool {
        key == self.key
            && (modifiers.command || modifiers.ctrl) == self.ctrl
            && modifiers.alt == self.alt
            && (modifiers.shift == self.shift || (!self.shift && is_symbol(key)))
    }
}

fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

fn is_symbol(key: Key) -> bool {
    matches!(
        key,
        Key::Colon
            | Key::Comma
            | Key::Minus
            | Key::Period
            | Key::Plus
            | Key::Equals
            | Key::Semicolon
            | Key::Backslash
            | Key::Slash
            | Key::Pipe
            | Key::Questionmark
            | Key::Exclamationmark
            | Key::OpenBracket
            | Key::CloseBracket
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
            | Key::Backtick
            | Key::Quote
    )
}

fn is_function_key(key: Key) -> bool {
    key.name()
        .strip_prefix('F')
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// The user's shortcut choices: command id to its keys in notation; an
/// empty list unbinds the command.
pub type Overrides = BTreeMap<String, Vec<String>>;

/// Drops what storage shouldn't hold: unknown commands and keys that
/// don't parse. Keys are rewritten in notation, without repeats.
pub fn sanitize(overrides: &Overrides) -> Overrides {
    overrides
        .iter()
        .filter(|(id, _)| Command::from_id(id).is_some())
        .filter_map(|(id, keys)| {
            let mut parsed: Vec<Shortcut> = Vec::new();
            for shortcut in keys.iter().filter_map(|key| Shortcut::parse(key)) {
                if !parsed.contains(&shortcut) {
                    parsed.push(shortcut);
                }
            }
            // Keys that are all unreadable keep the defaults rather than
            // unbinding the command.
            if parsed.is_empty() && !keys.is_empty() {
                return None;
            }
            Some((
                id.clone(),
                parsed.into_iter().map(Shortcut::notation).collect(),
            ))
        })
        .collect()
}

/// The keys in effect: each command's defaults, or the user's keys
/// instead. A shortcut runs one command: the user's choice takes it from
/// a default.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    keys: HashMap<Command, Vec<Shortcut>>,
    /// Every binding, most modifiers first.
    bindings: Vec<(Shortcut, Command)>,
}

impl Keymap {
    pub fn new(overrides: &Overrides) -> Self {
        let chosen: HashMap<Command, Vec<Shortcut>> = overrides
            .iter()
            .filter_map(|(id, keys)| {
                let command = Command::from_id(id)?;
                Some((
                    command,
                    keys.iter().filter_map(|key| Shortcut::parse(key)).collect(),
                ))
            })
            .collect();
        let mut keymap = Self::default();
        let mut taken: HashMap<Shortcut, Command> = HashMap::new();
        // The user's keys claim their shortcuts first, then defaults take
        // what is left.
        for pass_chosen in [true, false] {
            for command in Command::ALL {
                let keys: Vec<Shortcut> = match (pass_chosen, chosen.get(&command)) {
                    (true, Some(keys)) => keys.clone(),
                    (false, None) => command
                        .default_keys()
                        .iter()
                        .filter_map(|key| Shortcut::parse(key))
                        .collect(),
                    _ => continue,
                };
                let entry = keymap.keys.entry(command).or_default();
                for shortcut in keys {
                    if let std::collections::hash_map::Entry::Vacant(vacant) = taken.entry(shortcut)
                    {
                        vacant.insert(command);
                        entry.push(shortcut);
                    }
                }
            }
        }
        keymap.bindings = taken.into_iter().collect();
        keymap.bindings.sort_by_key(|(shortcut, command)| {
            (std::cmp::Reverse(shortcut.modifier_count()), *command)
        });
        keymap
    }

    pub fn keys(&self, command: Command) -> &[Shortcut] {
        self.keys.get(&command).map_or(&[], Vec::as_slice)
    }

    pub fn owner(&self, shortcut: Shortcut) -> Option<Command> {
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == shortcut)
            .map(|(_, command)| *command)
    }

    /// The first key of a command, as menus show it.
    pub fn label(&self, command: Command) -> Option<String> {
        self.keys(command).first().map(|shortcut| shortcut.label())
    }

    /// A tooltip naming the command's key: "Find (Ctrl+F)".
    pub fn tip(&self, text: &str, command: Command) -> String {
        match self.label(command) {
            Some(key) => format!("{text} ({key})"),
            None => text.to_string(),
        }
    }

    /// The commands whose keys were pressed this frame, taking those key
    /// presses from egui's input. While `typing`, only shortcuts typing
    /// can't produce fire, and only for commands that allow it.
    pub fn take_pressed(&self, ctx: &egui::Context, typing: bool) -> Vec<Command> {
        let mut commands = Vec::new();
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    return true;
                };
                let found = self.bindings.iter().find(|(shortcut, command)| {
                    shortcut.matches(*key, *modifiers)
                        && (!typing || (command.in_text() && shortcut.works_in_text()))
                });
                match found {
                    Some((_, command)) => {
                        commands.push(*command);
                        false
                    }
                    None => true,
                }
            });
        });
        commands
    }
}

/// Gives `shortcut` to `command`, taking it from the command that has it.
pub fn assign(overrides: &mut Overrides, keymap: &Keymap, command: Command, shortcut: Shortcut) {
    if let Some(owner) = keymap.owner(shortcut) {
        if owner == command {
            return;
        }
        let keys = without(keymap.keys(owner), shortcut);
        set(overrides, owner, keys);
    }
    let mut keys = keymap.keys(command).to_vec();
    keys.push(shortcut);
    set(overrides, command, keys);
}

/// Takes `shortcut` away from `command`.
pub fn unbind(overrides: &mut Overrides, keymap: &Keymap, command: Command, shortcut: Shortcut) {
    let keys = without(keymap.keys(command), shortcut);
    set(overrides, command, keys);
}

/// Back to the command's default keys.
pub fn reset(overrides: &mut Overrides, command: Command) {
    overrides.remove(command.id());
}

fn without(keys: &[Shortcut], shortcut: Shortcut) -> Vec<Shortcut> {
    keys.iter()
        .copied()
        .filter(|key| *key != shortcut)
        .collect()
}

/// Keeps `keys` for `command`, or forgets the choice when they are the
/// defaults.
fn set(overrides: &mut Overrides, command: Command, keys: Vec<Shortcut>) {
    let defaults: Vec<Shortcut> = command
        .default_keys()
        .iter()
        .filter_map(|key| Shortcut::parse(key))
        .collect();
    if keys == defaults {
        overrides.remove(command.id());
    } else {
        overrides.insert(
            command.id().to_string(),
            keys.into_iter().map(Shortcut::notation).collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(text: &str) -> Shortcut {
        Shortcut::parse(text).unwrap_or_else(|| panic!("{text} parses"))
    }

    #[test]
    fn shortcuts_parse_and_print_in_notation_and_as_labels() {
        for (text, notation, label) in [
            ("Ctrl+Shift+P", "Ctrl+Shift+P", "Ctrl+Shift+P"),
            ("shift+ctrl+p", "Ctrl+Shift+P", "Ctrl+Shift+P"),
            ("+", "Plus", "+"),
            ("Shift+F3", "Shift+F3", "Shift+F3"),
            ("Ctrl+Shift+Tab", "Ctrl+Shift+Tab", "Ctrl+Shift+Tab"),
            ("Ctrl++", "Ctrl+Plus", "Ctrl++"),
            ("Plus", "Plus", "+"),
            ("-", "Minus", "-"),
            ("Ctrl+Comma", "Ctrl+Comma", "Ctrl+,"),
            ("Ctrl+,", "Ctrl+Comma", "Ctrl+,"),
            ("Alt+Left", "Alt+Left", "Alt+Left"),
            ("0", "0", "0"),
            ("Space", "Space", "Space"),
        ] {
            let parsed = Shortcut::parse(text);
            if notation.is_empty() {
                assert_eq!(parsed, None, "{text} is not a shortcut");
                continue;
            }
            let parsed = parsed.unwrap_or_else(|| panic!("{text} parses"));
            assert_eq!(parsed.notation(), notation, "{text}");
            assert_eq!(parsed.label(), label, "{text}");
            assert_eq!(Shortcut::parse(&parsed.notation()), Some(parsed));
        }
        for bad in ["", "Ctrl+", "Ctrl+Nope", "Hyper+A", "A+B", "ShiftLeft"] {
            assert_eq!(Shortcut::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn default_keys_parse_and_no_shortcut_runs_two_commands() {
        let mut seen: HashMap<Shortcut, Command> = HashMap::new();
        for command in Command::ALL {
            for key in command.default_keys() {
                let parsed = shortcut(key);
                if let Some(other) = seen.insert(parsed, command) {
                    panic!("{key} runs both {other:?} and {command:?}");
                }
            }
        }
        let keymap = Keymap::new(&Overrides::new());
        for command in Command::ALL {
            assert_eq!(keymap.keys(command).len(), command.default_keys().len());
        }
    }

    #[test]
    fn the_users_keys_replace_defaults_and_take_shortcuts_from_them() {
        let mut overrides = Overrides::new();
        overrides.insert(Command::Find.id().into(), vec!["Ctrl+G".into()]);
        overrides.insert(Command::Present.id().into(), vec![]);
        // Takes F9 from the sidebar.
        overrides.insert(Command::Assistant.id().into(), vec!["F9".into()]);
        let keymap = Keymap::new(&overrides);
        assert_eq!(keymap.keys(Command::Find), &[shortcut("Ctrl+G")]);
        assert!(keymap.keys(Command::Present).is_empty());
        assert_eq!(keymap.owner(shortcut("F9")), Some(Command::Assistant));
        assert!(keymap.keys(Command::ToggleSidebar).is_empty());
        assert_eq!(keymap.owner(shortcut("Ctrl+F")), None);
        assert_eq!(keymap.tip("Find", Command::Find), "Find (Ctrl+G)");
        assert_eq!(keymap.tip("Present", Command::Present), "Present");
    }

    #[test]
    fn stored_choices_are_sanitized() {
        let mut stored = Overrides::new();
        stored.insert("no.such.command".into(), vec!["Ctrl+X".into()]);
        stored.insert(Command::Present.id().into(), vec!["Hyper+P".into()]);
        stored.insert(Command::Assistant.id().into(), vec![]);
        stored.insert(
            Command::Find.id().into(),
            vec!["ctrl+g".into(), "Nonsense".into(), "Ctrl+G".into()],
        );
        let clean = sanitize(&stored);
        assert_eq!(clean.len(), 2);
        assert_eq!(clean[Command::Find.id()], vec!["Ctrl+G".to_string()]);
        assert!(clean[Command::Assistant.id()].is_empty(), "still unbound");
    }

    #[test]
    fn assigning_a_shortcut_moves_it_and_resetting_restores_defaults() {
        let mut overrides = Overrides::new();
        let keymap = Keymap::new(&overrides);
        assign(&mut overrides, &keymap, Command::Assistant, shortcut("P"));
        let keymap = Keymap::new(&overrides);
        assert_eq!(keymap.owner(shortcut("P")), Some(Command::Assistant));
        assert!(keymap.keys(Command::Present).is_empty());
        assert_eq!(
            keymap.keys(Command::Assistant),
            &[shortcut("Ctrl+L"), shortcut("P")]
        );

        unbind(&mut overrides, &keymap, Command::Assistant, shortcut("P"));
        let keymap = Keymap::new(&overrides);
        assert!(
            !overrides.contains_key(Command::Assistant.id()),
            "back to the defaults, nothing kept"
        );
        assert_eq!(keymap.owner(shortcut("P")), None);

        reset(&mut overrides, Command::Present);
        let keymap = Keymap::new(&overrides);
        assert_eq!(keymap.owner(shortcut("P")), Some(Command::Present));
        assert!(overrides.is_empty());
    }

    fn press(keymap: &Keymap, key: Key, modifiers: Modifiers, typing: bool) -> Vec<Command> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        let mut pressed = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            pressed = keymap.take_pressed(ui.ctx(), typing);
        });
        output.textures_delta.clear();
        pressed
    }

    #[test]
    fn key_presses_run_the_command_they_are_bound_to() {
        let keymap = Keymap::new(&Overrides::new());
        let none = Modifiers::NONE;
        assert_eq!(press(&keymap, Key::F3, none, false), [Command::FindNext]);
        assert_eq!(
            press(&keymap, Key::F3, Modifiers::SHIFT, false),
            [Command::FindPrevious]
        );
        assert_eq!(press(&keymap, Key::Space, none, false), [Command::NextPage]);
        assert_eq!(
            press(&keymap, Key::Plus, Modifiers::SHIFT, false),
            [Command::ZoomIn],
            "layouts type + with Shift"
        );
        assert_eq!(
            press(&keymap, Key::P, Modifiers::ALT, false),
            [],
            "Alt+P is not P"
        );
        assert_eq!(press(&keymap, Key::P, none, true), [], "typing a P");
        assert_eq!(
            press(&keymap, Key::F, Modifiers::COMMAND, true),
            [Command::Find]
        );
        assert_eq!(
            press(&keymap, Key::A, Modifiers::COMMAND, true),
            [],
            "Ctrl+A selects the field's text"
        );
        assert_eq!(
            press(
                &keymap,
                Key::Tab,
                Modifiers::COMMAND | Modifiers::SHIFT,
                false
            ),
            [Command::PreviousTab]
        );
    }

    #[test]
    fn recorded_presses_ignore_modifiers_alone_and_shift_on_symbols() {
        assert_eq!(Shortcut::from_key(Key::ShiftLeft, Modifiers::SHIFT), None);
        assert_eq!(
            Shortcut::from_key(Key::Plus, Modifiers::COMMAND | Modifiers::SHIFT),
            Some(shortcut("Ctrl+Plus"))
        );
        assert_eq!(
            Shortcut::from_key(Key::G, Modifiers::COMMAND | Modifiers::SHIFT),
            Some(shortcut("Ctrl+Shift+G"))
        );
    }
}
