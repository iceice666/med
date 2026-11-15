//! Pure text-buffer core for med: no UI, no async, fully unit-testable.

mod buffer;

pub use buffer::{Buffer, Edit, LineEnding, Point};
