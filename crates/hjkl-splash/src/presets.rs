//! Built-in presets for the hjkl-splash animation.

pub mod hjkl {
    /// The HJKL ASCII art block (5 rows × 32 cols).
    pub const ART: &str = include_str!("../art/hjkl.txt");

    /// Number of rows in the art block.
    pub const ROWS: u16 = 5;

    /// Number of columns in the art block.
    pub const COLS: u16 = 32;

    /// The cursor path tracing the H, J, K, L letterforms.
    // Art is 5 rows tall, 32 cols wide. Each entry is (row, col, segment_char).
    // The cursor traces: H left-vert → crossbar → right-vert, J top→down→hook,
    // K right-vert bottom-to-top → upper arm → lower arm, L vert → bottom bar.
    #[rustfmt::skip]
    pub const PATH: &[(u8, u8, char)] = &[
        // H: left vertical top→bottom
        (0, 0, 'h'), (1, 0, 'h'), (2, 0, 'h'), (3, 0, 'h'), (4, 0, 'h'),
        // H: crossbar left→right (row 2)
        (2, 1, 'h'), (2, 2, 'h'), (2, 3, 'h'), (2, 4, 'h'), (2, 5, 'h'), (2, 6, 'h'), (2, 7, 'h'),
        // H: right vertical bottom→top
        (4, 5, 'h'), (3, 5, 'h'), (1, 5, 'h'), (0, 5, 'h'),
        // J: main vertical top→bottom
        (0, 13, 'j'), (1, 13, 'j'), (2, 13, 'j'), (3, 13, 'j'), (4, 13, 'j'),
        // J: hook — row 3 leftward then row 4 leftward
        (3, 9, 'j'), (3, 8, 'j'),
        (4, 12, 'j'), (4, 11, 'j'), (4, 10, 'j'), (4, 9, 'j'), (4, 8, 'j'),
        // K: left vertical bottom→top
        (4, 13, 'k'), (3, 13, 'k'), (2, 13, 'k'), (1, 13, 'k'), (0, 13, 'k'),
        // K: upper arm row 0→2 going right (diagonal)
        (0, 21, 'k'), (0, 22, 'k'), (0, 23, 'k'), (0, 24, 'k'), (0, 25, 'k'), (0, 26, 'k'),
        (1, 20, 'k'), (1, 21, 'k'), (1, 22, 'k'), (1, 23, 'k'), (1, 24, 'k'), (1, 25, 'k'), (1, 26, 'k'),
        (2, 19, 'k'), (2, 20, 'k'), (2, 21, 'k'), (2, 22, 'k'),
        // K: lower arm rows 3→4 going right
        (3, 16, 'k'), (3, 17, 'k'), (3, 18, 'k'), (3, 19, 'k'), (3, 20, 'k'), (3, 21, 'k'), (3, 22, 'k'),
        (4, 16, 'k'), (4, 17, 'k'), (4, 18, 'k'), (4, 21, 'k'), (4, 22, 'k'), (4, 23, 'k'), (4, 24, 'k'), (4, 25, 'k'),
        // L: vertical top→bottom
        (0, 24, 'l'), (1, 24, 'l'), (2, 24, 'l'), (3, 24, 'l'), (4, 24, 'l'),
        // L: bottom stroke left→right (row 4)
        (4, 25, 'l'), (4, 26, 'l'), (4, 27, 'l'), (4, 28, 'l'), (4, 29, 'l'), (4, 30, 'l'), (4, 31, 'l'),
    ];
}
