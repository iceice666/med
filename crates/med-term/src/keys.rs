//! What a key sends to the program in a terminal, as xterm sends it.

/// The modifiers held with a key. The platform key (cmd) is not here: it
/// belongs to med, not to the program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    /// xterm's number for them, in `CSI 1 ; m X`: one more than the sum
    /// of shift 1, alt 2, control 4. `None` with none held.
    fn param(self) -> Option<u8> {
        let m = self.shift as u8 | (self.alt as u8) << 1 | (self.control as u8) << 2;
        (m != 0).then_some(m + 1)
    }
}

/// The bytes `key` (a key name, as GPUI gives it: `a`, `enter`, `up`,
/// `f5`…) sends with `mods` held. `None` for a key that types text as it
/// is, which comes by the input method instead, so that it can compose.
/// `app_cursor`: the program asked for the arrows in their other form.
pub fn encode_key(key: &str, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let esc = |rest: &[u8]| [b"\x1b".as_slice(), rest].concat();
    // With alt held, what the key sends by itself, after an escape.
    let meta = |bytes: Vec<u8>| if mods.alt { esc(&bytes) } else { bytes };
    let named = match key {
        "enter" => Some(meta(b"\r".to_vec())),
        "escape" => Some(meta(b"\x1b".to_vec())),
        "tab" if mods.shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(meta(b"\t".to_vec())),
        "backspace" if mods.control => Some(meta(b"\x08".to_vec())),
        "backspace" => Some(meta(b"\x7f".to_vec())),
        "space" if mods.control => Some(meta(b"\0".to_vec())),
        "space" if mods.alt => Some(esc(b" ")),
        "up" | "down" | "right" | "left" | "home" | "end" => {
            let letter = match key {
                "up" => 'A',
                "down" => 'B',
                "right" => 'C',
                "left" => 'D',
                "home" => 'H',
                _ => 'F',
            };
            Some(match mods.param() {
                Some(m) => format!("\x1b[1;{m}{letter}").into_bytes(),
                None if app_cursor => format!("\x1bO{letter}").into_bytes(),
                None => format!("\x1b[{letter}").into_bytes(),
            })
        }
        "insert" | "delete" | "pageup" | "pagedown" => {
            let n = match key {
                "insert" => 2,
                "delete" => 3,
                "pageup" => 5,
                _ => 6,
            };
            Some(tilde(n, mods))
        }
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return function_key(n, mods);
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if mods.control {
        return control_char(c).map(|byte| meta(vec![byte]));
    }
    if mods.alt {
        let c = if mods.shift {
            c.to_ascii_uppercase()
        } else {
            c
        };
        return Some(esc(c.to_string().as_bytes()));
    }
    None
}

/// `CSI n ~`, or `CSI n ; m ~` with modifiers.
fn tilde(n: u8, mods: Modifiers) -> Vec<u8> {
    match mods.param() {
        Some(m) => format!("\x1b[{n};{m}~").into_bytes(),
        None => format!("\x1b[{n}~").into_bytes(),
    }
}

fn function_key(n: u8, mods: Modifiers) -> Option<Vec<u8>> {
    let tilde_number = match n {
        1..=4 => {
            let letter = (b'P' + n - 1) as char;
            return Some(match mods.param() {
                Some(m) => format!("\x1b[1;{m}{letter}").into_bytes(),
                None => format!("\x1bO{letter}").into_bytes(),
            });
        }
        5 => 15,
        6..=10 => n + 11,
        11 | 12 => n + 12,
        _ => return None,
    };
    Some(tilde(tilde_number, mods))
}

/// What control and `c` send: `ctrl-a` is 1, `ctrl-[` escape.
fn control_char(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}
