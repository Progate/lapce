//! evdev のキー番号（`linux/input-event-codes.h` の `KEY_*`）から winit のキーへ。
//!
//! windowserver が送るのは**物理キーの番号だけ**で、文字は送らない（打った文字が届くのは
//! IME で確定したときだけ。→ `text` / `preedit`）。だから文字は US 配列として
//! ここで決める。Linux の端末（`loadkeys us`）が同じことをしている。

use crate::keyboard::{KeyCode, NamedKey};

pub(super) struct KeyInfo {
    pub code: KeyCode,
    pub named: Option<NamedKey>,
    /// 何も押さずに打った文字と、Shift を押して打った文字
    pub chars: Option<(char, char)>,
}

const fn key(code: KeyCode, named: Option<NamedKey>, chars: Option<(char, char)>) -> KeyInfo {
    KeyInfo { code, named, chars }
}

pub(super) fn lookup(evdev: u16) -> Option<KeyInfo> {
    use KeyCode as K;
    use NamedKey as N;
    let c = |lower: char, upper: char| Some((lower, upper));
    Some(match evdev {
        1 => key(K::Escape, Some(N::Escape), None),
        2 => key(K::Digit1, None, c('1', '!')),
        3 => key(K::Digit2, None, c('2', '@')),
        4 => key(K::Digit3, None, c('3', '#')),
        5 => key(K::Digit4, None, c('4', '$')),
        6 => key(K::Digit5, None, c('5', '%')),
        7 => key(K::Digit6, None, c('6', '^')),
        8 => key(K::Digit7, None, c('7', '&')),
        9 => key(K::Digit8, None, c('8', '*')),
        10 => key(K::Digit9, None, c('9', '(')),
        11 => key(K::Digit0, None, c('0', ')')),
        12 => key(K::Minus, None, c('-', '_')),
        13 => key(K::Equal, None, c('=', '+')),
        14 => key(K::Backspace, Some(N::Backspace), None),
        15 => key(K::Tab, Some(N::Tab), None),
        16 => key(K::KeyQ, None, c('q', 'Q')),
        17 => key(K::KeyW, None, c('w', 'W')),
        18 => key(K::KeyE, None, c('e', 'E')),
        19 => key(K::KeyR, None, c('r', 'R')),
        20 => key(K::KeyT, None, c('t', 'T')),
        21 => key(K::KeyY, None, c('y', 'Y')),
        22 => key(K::KeyU, None, c('u', 'U')),
        23 => key(K::KeyI, None, c('i', 'I')),
        24 => key(K::KeyO, None, c('o', 'O')),
        25 => key(K::KeyP, None, c('p', 'P')),
        26 => key(K::BracketLeft, None, c('[', '{')),
        27 => key(K::BracketRight, None, c(']', '}')),
        28 => key(K::Enter, Some(N::Enter), None),
        29 => key(K::ControlLeft, Some(N::Control), None),
        30 => key(K::KeyA, None, c('a', 'A')),
        31 => key(K::KeyS, None, c('s', 'S')),
        32 => key(K::KeyD, None, c('d', 'D')),
        33 => key(K::KeyF, None, c('f', 'F')),
        34 => key(K::KeyG, None, c('g', 'G')),
        35 => key(K::KeyH, None, c('h', 'H')),
        36 => key(K::KeyJ, None, c('j', 'J')),
        37 => key(K::KeyK, None, c('k', 'K')),
        38 => key(K::KeyL, None, c('l', 'L')),
        39 => key(K::Semicolon, None, c(';', ':')),
        40 => key(K::Quote, None, c('\'', '"')),
        41 => key(K::Backquote, None, c('`', '~')),
        42 => key(K::ShiftLeft, Some(N::Shift), None),
        43 => key(K::Backslash, None, c('\\', '|')),
        44 => key(K::KeyZ, None, c('z', 'Z')),
        45 => key(K::KeyX, None, c('x', 'X')),
        46 => key(K::KeyC, None, c('c', 'C')),
        47 => key(K::KeyV, None, c('v', 'V')),
        48 => key(K::KeyB, None, c('b', 'B')),
        49 => key(K::KeyN, None, c('n', 'N')),
        50 => key(K::KeyM, None, c('m', 'M')),
        51 => key(K::Comma, None, c(',', '<')),
        52 => key(K::Period, None, c('.', '>')),
        53 => key(K::Slash, None, c('/', '?')),
        54 => key(K::ShiftRight, Some(N::Shift), None),
        56 => key(K::AltLeft, Some(N::Alt), None),
        57 => key(K::Space, Some(N::Space), c(' ', ' ')),
        58 => key(K::CapsLock, Some(N::CapsLock), None),
        59 => key(K::F1, Some(N::F1), None),
        60 => key(K::F2, Some(N::F2), None),
        61 => key(K::F3, Some(N::F3), None),
        62 => key(K::F4, Some(N::F4), None),
        63 => key(K::F5, Some(N::F5), None),
        64 => key(K::F6, Some(N::F6), None),
        65 => key(K::F7, Some(N::F7), None),
        66 => key(K::F8, Some(N::F8), None),
        67 => key(K::F9, Some(N::F9), None),
        68 => key(K::F10, Some(N::F10), None),
        87 => key(K::F11, Some(N::F11), None),
        88 => key(K::F12, Some(N::F12), None),
        97 => key(K::ControlRight, Some(N::Control), None),
        100 => key(K::AltRight, Some(N::Alt), None),
        102 => key(K::Home, Some(N::Home), None),
        103 => key(K::ArrowUp, Some(N::ArrowUp), None),
        104 => key(K::PageUp, Some(N::PageUp), None),
        105 => key(K::ArrowLeft, Some(N::ArrowLeft), None),
        106 => key(K::ArrowRight, Some(N::ArrowRight), None),
        107 => key(K::End, Some(N::End), None),
        108 => key(K::ArrowDown, Some(N::ArrowDown), None),
        109 => key(K::PageDown, Some(N::PageDown), None),
        110 => key(K::Insert, Some(N::Insert), None),
        111 => key(K::Delete, Some(N::Delete), None),
        125 => key(K::SuperLeft, Some(N::Super), None),
        126 => key(K::SuperRight, Some(N::Super), None),
        _ => return None,
    })
}
