//! Alacritty - The GPU Enhanced Terminal.

#![warn(rust_2018_idioms, future_incompatible)]
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
#![cfg_attr(clippy, deny(warnings))]

pub mod event;
/// pty を読み書きする係。WASI には pty も epoll も無いので外す
#[cfg(not(target_os = "wasi"))]
pub mod event_loop;
pub mod grid;
pub mod index;
pub mod selection;
pub mod sync;
pub mod term;
pub mod thread;
#[cfg(not(target_os = "wasi"))]
pub mod tty;
pub mod vi_mode;

pub use crate::grid::Grid;
pub use crate::term::Term;
pub use vte;
