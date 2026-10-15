//! Keyboard shortcuts and the keymap, after photocraft's `shortcuts.rs`:
//! shortcuts are written `Ctrl+Shift+P` (Ctrl is the command modifier) and
//! the user's choices in preferences replace a command's default keys.
//!
//! A binding is a sequence of up to three shortcuts, written with spaces
//! (`G G`, `Ctrl+W V`). No binding is the start of another, so a sequence
//! runs as soon as its last key is pressed, without vim's timeout. The Vim
//! preset also takes a count typed first (`42 Shift+G`).

use std::collections::{BTreeMap, HashMap};

use egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};

use crate::commands::Command;

/// Which default keys commands have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeymapPreset {
    #[default]
    Standard,
    /// After vim, zathura and sioyek: hjkl, `gg`, counts, `Ctrl+W` panes.
    Vim,
}

impl KeymapPreset {
    pub const ALL: [Self; 2] = [Self::Standard, Self::Vim];

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Vim => "Vim",
        }
    }
}

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

/// The most shortcuts a binding's sequence has.
pub const MAX_KEYS: usize = 3;

/// A binding: shortcuts pressed one after another.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Keys(Vec<Shortcut>);

impl Keys {
    /// Up to `MAX_KEYS` shortcuts, at least one.
    pub fn new(strokes: Vec<Shortcut>) -> Option<Self> {
        (1..=MAX_KEYS)
            .contains(&strokes.len())
            .then_some(Self(strokes))
    }

    /// Shortcuts separated by spaces: `Ctrl+W V`.
    pub fn parse(text: &str) -> Option<Self> {
        let strokes: Option<Vec<Shortcut>> = text.split_whitespace().map(Shortcut::parse).collect();
        Self::new(strokes?)
    }

    pub fn strokes(&self) -> &[Shortcut] {
        &self.0
    }

    /// How preferences keep it: `Ctrl+W Shift+V`.
    pub fn notation(&self) -> String {
        self.join(Shortcut::notation)
    }

    /// How menus and tooltips show it: `Ctrl+W V`.
    pub fn label(&self) -> String {
        self.join(Shortcut::label)
    }

    fn join(&self, show: fn(Shortcut) -> String) -> String {
        self.0
            .iter()
            .map(|&stroke| show(stroke))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether one of the two starts the other (or they are the same):
    /// both can't be bound.
    pub fn overlaps(&self, other: &Keys) -> bool {
        self.0.starts_with(&other.0) || other.0.starts_with(&self.0)
    }
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
            let mut parsed: Vec<Keys> = Vec::new();
            for binding in keys.iter().filter_map(|key| Keys::parse(key)) {
                if !parsed.contains(&binding) {
                    parsed.push(binding);
                }
            }
            // Keys that are all unreadable keep the defaults rather than
            // unbinding the command.
            if parsed.is_empty() && !keys.is_empty() {
                return None;
            }
            Some((id.clone(), parsed.iter().map(Keys::notation).collect()))
        })
        .collect()
}

/// The keys in effect: each command's defaults in the preset, or the
/// user's keys instead. A binding runs one command and starts no other
/// binding: the user's choice takes it from a default.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    preset: KeymapPreset,
    keys: HashMap<Command, Vec<Keys>>,
    bindings: Vec<(Keys, Command)>,
}

/// A command a key press ran, with the count typed before its keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pressed {
    pub command: Command,
    pub count: Option<u32>,
}

/// The start of a sequence and a count typed so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    keys: Vec<Shortcut>,
    count: Option<u32>,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.count.is_none()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// What vim's showcmd shows: `42`, `G`, `3 Ctrl+W`.
    pub fn indicator(&self) -> Option<String> {
        let parts: Vec<String> = self
            .count
            .map(|count| count.to_string())
            .into_iter()
            .chain(self.keys.iter().map(|stroke| stroke.label()))
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// What a key press does to the pending sequence.
enum Step {
    Run(Command),
    Continue(Vec<Shortcut>),
    Nothing,
}

impl Keymap {
    /// The bindings `pending` starts, as the pending-keys chip lists them:
    /// `gg top`, `gt tab`. Single unmodified keys run together, as vim
    /// writes them.
    pub fn continuations(&self, pending: &Pending) -> Vec<(String, Command)> {
        if pending.keys.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<(String, Command)> = self
            .bindings
            .iter()
            .filter(|(keys, _)| {
                keys.0.len() > pending.keys.len() && keys.0.starts_with(&pending.keys)
            })
            .map(|(keys, command)| {
                let plain = keys.0.iter().all(|stroke| !stroke.ctrl && !stroke.alt);
                let label = if plain {
                    keys.0
                        .iter()
                        .map(|&stroke| stroke.label().to_lowercase())
                        .collect::<String>()
                } else {
                    keys.label().to_lowercase()
                };
                (label, *command)
            })
            .collect();
        found.sort();
        found
    }

    pub fn new(preset: KeymapPreset, overrides: &Overrides) -> Self {
        let chosen: HashMap<Command, Vec<Keys>> = overrides
            .iter()
            .filter_map(|(id, keys)| {
                let command = Command::from_id(id)?;
                Some((
                    command,
                    keys.iter().filter_map(|key| Keys::parse(key)).collect(),
                ))
            })
            .collect();
        let mut keymap = Self {
            preset,
            ..Self::default()
        };
        // The user's keys claim their bindings first, then defaults take
        // what is left.
        for pass_chosen in [true, false] {
            for command in Command::ALL {
                let keys: Vec<Keys> = match (pass_chosen, chosen.get(&command)) {
                    (true, Some(keys)) => keys.clone(),
                    (false, None) => defaults(preset, command),
                    _ => continue,
                };
                let entry = keymap.keys.entry(command).or_default();
                for binding in keys {
                    if !keymap
                        .bindings
                        .iter()
                        .any(|(bound, _)| bound.overlaps(&binding))
                    {
                        entry.push(binding.clone());
                        keymap.bindings.push((binding, command));
                    }
                }
            }
        }
        keymap
    }

    pub fn preset(&self) -> KeymapPreset {
        self.preset
    }

    /// Whether digits typed first are a count (the Vim preset).
    pub fn counts(&self) -> bool {
        self.preset == KeymapPreset::Vim
    }

    pub fn keys(&self, command: Command) -> &[Keys] {
        self.keys.get(&command).map_or(&[], Vec::as_slice)
    }

    /// The command bound to exactly `keys`.
    #[cfg(test)]
    pub fn owner(&self, keys: &Keys) -> Option<Command> {
        self.bindings
            .iter()
            .find(|(bound, _)| bound == keys)
            .map(|(_, command)| *command)
    }

    /// The bindings `keys` can't live with: the same keys, ones it starts
    /// and ones that start it.
    pub fn conflicts(&self, keys: &Keys) -> Vec<(Keys, Command)> {
        self.bindings
            .iter()
            .filter(|(bound, _)| bound.overlaps(keys))
            .cloned()
            .collect()
    }

    /// The first key of a command, as menus show it.
    pub fn label(&self, command: Command) -> Option<String> {
        self.keys(command).first().map(Keys::label)
    }

    /// A tooltip naming the command's key: "Find (Ctrl+F)".
    pub fn tip(&self, text: &str, command: Command) -> String {
        match self.label(command) {
            Some(key) => format!("{text} ({key})"),
            None => text.to_string(),
        }
    }

    /// The commands whose keys were pressed this frame, taking those key
    /// presses from egui's input. A press that starts or continues a
    /// sequence, or adds to a count, waits in `pending`; Escape drops what
    /// is pending. While `typing`, only single shortcuts typing can't
    /// produce fire, and only for commands that allow it.
    pub fn take_pressed(
        &self,
        ctx: &egui::Context,
        typing: bool,
        pending: &mut Pending,
    ) -> Vec<Pressed> {
        if typing {
            pending.clear();
        }
        let mut pressed = Vec::new();
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
                if is_modifier(*key) {
                    return true;
                }
                if typing {
                    let found = self.bindings.iter().find(|(binding, command)| {
                        let [shortcut] = binding.strokes() else {
                            return false;
                        };
                        shortcut.matches(*key, *modifiers)
                            && command.in_text()
                            && shortcut.works_in_text()
                    });
                    return match found {
                        Some((_, command)) => {
                            pressed.push(Pressed {
                                command: *command,
                                count: None,
                            });
                            false
                        }
                        None => true,
                    };
                }
                if *key == Key::Escape && modifiers.is_none() && !pending.is_empty() {
                    pending.clear();
                    return false;
                }
                if self.counts()
                    && pending.keys.is_empty()
                    && modifiers.is_none()
                    && let Some(digit) = digit(*key)
                    && (digit != 0 || pending.count.is_some())
                {
                    let count = pending.count.unwrap_or(0);
                    pending.count = Some(
                        count
                            .saturating_mul(10)
                            .saturating_add(digit)
                            .min(MAX_COUNT),
                    );
                    return false;
                }
                match self.step(&pending.keys, *key, *modifiers) {
                    Step::Run(command) => {
                        pressed.push(Pressed {
                            command,
                            count: pending.count,
                        });
                        pending.clear();
                        false
                    }
                    Step::Continue(keys) => {
                        pending.keys = keys;
                        false
                    }
                    // A press that ends no sequence drops it, like vim.
                    Step::Nothing if !pending.is_empty() => {
                        pending.clear();
                        false
                    }
                    Step::Nothing => true,
                }
            });
        });
        pressed
    }

    /// Where pressing `key` after `pending` leads. Of the shortcuts the
    /// press matches, the one with the most modifiers wins.
    fn step(&self, pending: &[Shortcut], key: Key, modifiers: Modifiers) -> Step {
        let next = self
            .bindings
            .iter()
            .filter_map(|(binding, _)| {
                let strokes = binding.strokes();
                strokes
                    .starts_with(pending)
                    .then(|| strokes.get(pending.len()))
                    .flatten()
                    .filter(|stroke| stroke.matches(key, modifiers))
            })
            .max_by_key(|stroke| stroke.modifier_count());
        let Some(&next) = next else {
            return Step::Nothing;
        };
        let mut keys = pending.to_vec();
        keys.push(next);
        match self
            .bindings
            .iter()
            .find(|(binding, _)| binding.strokes() == keys.as_slice())
        {
            Some((_, command)) => Step::Run(*command),
            None => Step::Continue(keys),
        }
    }
}

/// The largest count kept; more digits don't grow it.
const MAX_COUNT: u32 = 99_999;

fn digit(key: Key) -> Option<u32> {
    let keys = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    keys.iter()
        .position(|&digit| digit == key)
        .map(|at| at as u32)
}

fn defaults(preset: KeymapPreset, command: Command) -> Vec<Keys> {
    command
        .default_keys(preset)
        .iter()
        .filter_map(|key| Keys::parse(key))
        .collect()
}

/// Gives `keys` to `command`, taking it from the bindings it conflicts
/// with (the same keys, or ones that start or continue them).
pub fn assign(overrides: &mut Overrides, keymap: &Keymap, command: Command, keys: Keys) {
    let conflicts = keymap.conflicts(&keys);
    if conflicts
        .iter()
        .any(|(bound, owner)| *owner == command && *bound == keys)
    {
        return;
    }
    let mut owners: Vec<Command> = conflicts.iter().map(|(_, owner)| *owner).collect();
    owners.sort_unstable();
    owners.dedup();
    for owner in owners.iter().copied().filter(|owner| *owner != command) {
        let kept = without(keymap.keys(owner), &conflicts);
        set(overrides, keymap.preset, owner, kept);
    }
    let mut kept = without(keymap.keys(command), &conflicts);
    kept.push(keys);
    set(overrides, keymap.preset, command, kept);
}

/// Takes `keys` away from `command`.
pub fn unbind(overrides: &mut Overrides, keymap: &Keymap, command: Command, keys: &Keys) {
    let kept = keymap
        .keys(command)
        .iter()
        .filter(|bound| *bound != keys)
        .cloned()
        .collect();
    set(overrides, keymap.preset, command, kept);
}

/// Back to the command's default keys.
pub fn reset(overrides: &mut Overrides, command: Command) {
    overrides.remove(command.id());
}

fn without(keys: &[Keys], conflicts: &[(Keys, Command)]) -> Vec<Keys> {
    keys.iter()
        .filter(|bound| !conflicts.iter().any(|(conflict, _)| conflict == *bound))
        .cloned()
        .collect()
}

/// Keeps `keys` for `command`, or forgets the choice when they are the
/// preset's defaults.
fn set(overrides: &mut Overrides, preset: KeymapPreset, command: Command, keys: Vec<Keys>) {
    if keys == defaults(preset, command) {
        overrides.remove(command.id());
    } else {
        overrides.insert(
            command.id().to_string(),
            keys.iter().map(Keys::notation).collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(text: &str) -> Shortcut {
        Shortcut::parse(text).unwrap_or_else(|| panic!("{text} parses"))
    }

    fn keys(text: &str) -> Keys {
        Keys::parse(text).unwrap_or_else(|| panic!("{text} parses"))
    }

    fn standard(overrides: &Overrides) -> Keymap {
        Keymap::new(KeymapPreset::Standard, overrides)
    }

    fn vim() -> Keymap {
        Keymap::new(KeymapPreset::Vim, &Overrides::new())
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
    fn sequences_parse_and_print_with_spaces() {
        for (text, notation, label) in [
            ("g g", "G G", "G G"),
            ("Ctrl+W  shift+v", "Ctrl+W Shift+V", "Ctrl+W Shift+V"),
            ("Ctrl+W Ctrl+,", "Ctrl+W Ctrl+Comma", "Ctrl+W Ctrl+,"),
            ("Colon", "Colon", ":"),
            ("Ctrl+F", "Ctrl+F", "Ctrl+F"),
        ] {
            let parsed = keys(text);
            assert_eq!(parsed.notation(), notation, "{text}");
            assert_eq!(parsed.label(), label, "{text}");
            assert_eq!(Keys::parse(&parsed.notation()), Some(parsed));
        }
        for bad in ["", "  ", "G G G G", "G Nope"] {
            assert_eq!(Keys::parse(bad), None, "{bad}");
        }
        assert!(keys("G G").overlaps(&keys("G")));
        assert!(keys("G").overlaps(&keys("G T")));
        assert!(!keys("G G").overlaps(&keys("G T")));
    }

    #[test]
    fn default_keys_parse_and_no_binding_runs_or_starts_another() {
        for preset in KeymapPreset::ALL {
            let mut seen: Vec<(Keys, Command)> = Vec::new();
            for command in Command::ALL {
                for key in command.default_keys(preset) {
                    let parsed = keys(key);
                    if let Some((other, owner)) =
                        seen.iter().find(|(bound, _)| bound.overlaps(&parsed))
                    {
                        panic!("{preset:?}: {key} meets {other:?} of {owner:?} in {command:?}");
                    }
                    seen.push((parsed, command));
                }
            }
            let keymap = Keymap::new(preset, &Overrides::new());
            for command in Command::ALL {
                assert_eq!(
                    keymap.keys(command).len(),
                    command.default_keys(preset).len()
                );
            }
        }
    }

    #[test]
    fn the_users_keys_replace_defaults_and_take_shortcuts_from_them() {
        let mut overrides = Overrides::new();
        overrides.insert(Command::Find.id().into(), vec!["Ctrl+G".into()]);
        overrides.insert(Command::Present.id().into(), vec![]);
        // Takes F9 from the sidebar.
        overrides.insert(Command::Assistant.id().into(), vec!["F9".into()]);
        let keymap = standard(&overrides);
        assert_eq!(keymap.keys(Command::Find), &[keys("Ctrl+G")]);
        assert!(keymap.keys(Command::Present).is_empty());
        assert_eq!(keymap.owner(&keys("F9")), Some(Command::Assistant));
        assert!(keymap.keys(Command::ToggleSidebar).is_empty());
        assert_eq!(keymap.owner(&keys("Ctrl+F")), None);
        assert_eq!(keymap.tip("Find", Command::Find), "Find (Ctrl+G)");
        assert_eq!(keymap.tip("Present", Command::Present), "Present");
    }

    #[test]
    fn a_users_key_takes_the_sequences_it_starts() {
        let mut overrides = Overrides::new();
        overrides.insert(Command::Zen.id().into(), vec!["G".into()]);
        let keymap = Keymap::new(KeymapPreset::Vim, &overrides);
        assert_eq!(keymap.owner(&keys("G")), Some(Command::Zen));
        assert_eq!(keymap.keys(Command::FirstPage), &[keys("Home")]);
        assert_eq!(keymap.keys(Command::NextTab), &[keys("Ctrl+Tab")]);
        assert_eq!(keymap.keys(Command::LastPage)[0], keys("Shift+G"));
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
        stored.insert(
            Command::SplitDown.id().into(),
            vec!["ctrl+w  s".into(), "Ctrl+W S".into()],
        );
        let clean = sanitize(&stored);
        assert_eq!(clean.len(), 3);
        assert_eq!(clean[Command::Find.id()], vec!["Ctrl+G".to_string()]);
        assert_eq!(clean[Command::SplitDown.id()], vec!["Ctrl+W S".to_string()]);
        assert!(clean[Command::Assistant.id()].is_empty(), "still unbound");
    }

    #[test]
    fn assigning_a_shortcut_moves_it_and_resetting_restores_defaults() {
        let mut overrides = Overrides::new();
        let keymap = standard(&overrides);
        assign(&mut overrides, &keymap, Command::Assistant, keys("P"));
        let keymap = standard(&overrides);
        assert_eq!(keymap.owner(&keys("P")), Some(Command::Assistant));
        assert!(keymap.keys(Command::Present).is_empty());
        assert_eq!(
            keymap.keys(Command::Assistant),
            &[keys("Ctrl+L"), keys("P")]
        );

        unbind(&mut overrides, &keymap, Command::Assistant, &keys("P"));
        let keymap = standard(&overrides);
        assert!(
            !overrides.contains_key(Command::Assistant.id()),
            "back to the defaults, nothing kept"
        );
        assert_eq!(keymap.owner(&keys("P")), None);

        reset(&mut overrides, Command::Present);
        let keymap = standard(&overrides);
        assert_eq!(keymap.owner(&keys("P")), Some(Command::Present));
        assert!(overrides.is_empty());
    }

    #[test]
    fn assigning_a_prefix_takes_every_sequence_it_starts() {
        let mut overrides = Overrides::new();
        let keymap = vim();
        let conflicts = keymap.conflicts(&keys("G"));
        assert_eq!(conflicts.len(), 3, "G G, G T and G Shift+T");
        assign(&mut overrides, &keymap, Command::Zen, keys("G"));
        let keymap = Keymap::new(KeymapPreset::Vim, &overrides);
        assert_eq!(keymap.owner(&keys("G")), Some(Command::Zen));
        assert_eq!(keymap.keys(Command::FirstPage), &[keys("Home")]);
        assert_eq!(keymap.keys(Command::PreviousTab), &[keys("Ctrl+Shift+Tab")]);
        assert_eq!(keymap.keys(Command::Zen), &[keys("Z"), keys("G")]);
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// Runs one frame of `events`; returns the commands and what is left
    /// of the events.
    fn frame(
        keymap: &Keymap,
        events: Vec<egui::Event>,
        typing: bool,
        pending: &mut Pending,
    ) -> (Vec<Pressed>, usize) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut pressed = Vec::new();
        let mut left = 0;
        let mut output = ctx.run_ui(input, |ui| {
            pressed = keymap.take_pressed(ui.ctx(), typing, pending);
            left = ui.input(|input| input.events.len());
        });
        output.textures_delta.clear();
        (pressed, left)
    }

    fn press(keymap: &Keymap, key: Key, modifiers: Modifiers, typing: bool) -> Vec<Command> {
        let (pressed, _) = frame(
            keymap,
            vec![self::key(key, modifiers)],
            typing,
            &mut Pending::default(),
        );
        pressed.into_iter().map(|pressed| pressed.command).collect()
    }

    /// Presses `text`'s keys (`keys` notation) one a frame.
    fn sequence(keymap: &Keymap, text: &str, pending: &mut Pending) -> Vec<Pressed> {
        let mut pressed = Vec::new();
        for stroke in text.split_whitespace() {
            let stroke = shortcut(stroke);
            let mut modifiers = Modifiers::NONE;
            modifiers.ctrl = stroke.ctrl;
            modifiers.command = stroke.ctrl;
            modifiers.alt = stroke.alt;
            modifiers.shift = stroke.shift;
            pressed.extend(frame(keymap, vec![key(stroke.key, modifiers)], false, pending).0);
        }
        pressed
    }

    fn ran(command: Command, count: Option<u32>) -> Vec<Pressed> {
        vec![Pressed { command, count }]
    }

    #[test]
    fn key_presses_run_the_command_they_are_bound_to() {
        let keymap = standard(&Overrides::new());
        let none = Modifiers::NONE;
        assert_eq!(press(&keymap, Key::F3, none, false), [Command::FindNext]);
        assert_eq!(
            press(&keymap, Key::F3, Modifiers::SHIFT, false),
            [Command::FindPrevious]
        );
        assert_eq!(press(&keymap, Key::Space, none, false), [Command::NextPage]);
        assert_eq!(
            press(&keymap, Key::ArrowDown, none, false),
            [Command::ScrollDown]
        );
        assert_eq!(press(&keymap, Key::ArrowDown, none, true), [], "in a field");
        assert_eq!(
            press(&keymap, Key::PageDown, Modifiers::SHIFT, false),
            [Command::NextHalfPage]
        );
        assert_eq!(
            press(&keymap, Key::PageUp, Modifiers::SHIFT, false),
            [Command::PreviousHalfPage]
        );
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
    fn a_pending_key_lists_what_can_follow_it() {
        let keymap = vim();
        let mut pending = Pending::default();
        assert!(keymap.continuations(&pending).is_empty());
        assert!(sequence(&keymap, "G", &mut pending).is_empty());
        let next = keymap.continuations(&pending);
        assert!(
            next.iter()
                .any(|(keys, command)| keys == "gg" && *command == Command::FirstPage),
            "{next:?}"
        );
        assert!(next.iter().all(|(keys, _)| keys.starts_with('g')));
    }

    #[test]
    fn vim_sequences_and_counts_run_their_commands() {
        let keymap = vim();
        let mut pending = Pending::default();
        assert!(sequence(&keymap, "G", &mut pending).is_empty());
        assert_eq!(pending.indicator().as_deref(), Some("G"));
        assert_eq!(
            sequence(&keymap, "G", &mut pending),
            ran(Command::FirstPage, None)
        );
        assert!(pending.is_empty());
        assert_eq!(
            sequence(&keymap, "4 2 Shift+G", &mut pending),
            ran(Command::LastPage, Some(42))
        );
        assert_eq!(
            sequence(&keymap, "0", &mut pending),
            ran(Command::FitWidth, None),
            "0 without a count fits the width"
        );
        assert_eq!(
            sequence(&keymap, "1 0 J", &mut pending),
            ran(Command::ScrollDown, Some(10))
        );
        assert_eq!(
            sequence(&keymap, "Ctrl+W V", &mut pending),
            ran(Command::SplitRight, None)
        );
        assert_eq!(
            sequence(&keymap, "Ctrl+W Ctrl+W", &mut pending),
            ran(Command::FocusNextPane, None)
        );
        assert_eq!(
            sequence(&keymap, "G Shift+T", &mut pending),
            ran(Command::PreviousTab, None)
        );
        assert_eq!(
            sequence(&keymap, "Colon", &mut pending),
            ran(Command::Palette, None)
        );
        assert_eq!(
            sequence(&keymap, "Shift+N", &mut pending),
            ran(Command::FindPrevious, None)
        );
    }

    #[test]
    fn modifier_presses_leave_a_sequence_waiting() {
        let keymap = vim();
        let mut pending = Pending::default();
        sequence(&keymap, "G", &mut pending);
        let (pressed, left) = frame(
            &keymap,
            vec![key(Key::ShiftLeft, Modifiers::SHIFT)],
            false,
            &mut pending,
        );
        assert!(pressed.is_empty());
        assert_eq!(left, 1, "the modifier stays in egui's input");
        assert_eq!(
            sequence(&keymap, "Shift+T", &mut pending),
            ran(Command::PreviousTab, None)
        );
    }

    #[test]
    fn escape_an_unknown_key_or_typing_drops_what_is_pending() {
        let keymap = vim();
        let mut pending = Pending::default();
        sequence(&keymap, "5 G", &mut pending);
        assert_eq!(pending.indicator().as_deref(), Some("5 G"));
        let (pressed, left) = frame(
            &keymap,
            vec![key(Key::Escape, Modifiers::NONE)],
            false,
            &mut pending,
        );
        assert!(pressed.is_empty() && pending.is_empty());
        assert_eq!(left, 0, "Escape cancels nothing else");
        let (_, left) = frame(
            &keymap,
            vec![key(Key::Escape, Modifiers::NONE)],
            false,
            &mut pending,
        );
        assert_eq!(left, 1, "with nothing pending, Escape is the view's");

        sequence(&keymap, "G", &mut pending);
        assert!(sequence(&keymap, "X", &mut pending).is_empty());
        assert!(pending.is_empty(), "G X is no sequence");
        assert_eq!(
            sequence(&keymap, "J", &mut pending),
            ran(Command::ScrollDown, None)
        );

        sequence(&keymap, "3 Ctrl+W", &mut pending);
        frame(&keymap, Vec::new(), true, &mut pending);
        assert!(pending.is_empty(), "focusing a field drops it");
    }

    #[test]
    fn the_standard_preset_takes_no_counts() {
        let keymap = standard(&Overrides::new());
        let mut pending = Pending::default();
        let (pressed, left) = frame(
            &keymap,
            vec![key(Key::Num5, Modifiers::NONE)],
            false,
            &mut pending,
        );
        assert!(pressed.is_empty() && pending.is_empty());
        assert_eq!(left, 1);
        assert_eq!(
            press(&keymap, Key::Num0, Modifiers::NONE, false),
            [Command::FitWidth]
        );
        assert_eq!(press(&keymap, Key::J, Modifiers::NONE, false), []);
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
