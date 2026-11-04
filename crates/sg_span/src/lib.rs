//! ソース位置（FileId, Span, SourceMap）

/// 0始まりの UTF-8 バイト範囲。`end` は含まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }
}

/// 1始まりの行・列（列は Unicode スカラー値単位）と、0始まりのバイトオフセット。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
    pub byte: u32,
}

/// 1ファイル分のソース。バイトオフセットから行・列への変換を行う。
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(u32::try_from(i + 1).expect("source file too large"));
            }
        }
        Self {
            name: name.into(),
            text,
            line_starts,
        }
    }

    /// バイトオフセットを行・列に変換する。文字境界でないオフセットはパニックする。
    pub fn position(&self, byte: u32) -> Position {
        let line_idx = match self.line_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx] as usize;
        let column = self.text[line_start..byte as usize].chars().count() + 1;
        Position {
            line: line_idx as u32 + 1,
            column: column as u32,
            byte,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_one_based_and_count_chars() {
        let f = SourceFile::new("a.sg", "x = 1\ny = \"あい\"\n");
        assert_eq!(
            f.position(0),
            Position {
                line: 1,
                column: 1,
                byte: 0
            }
        );
        // 2行目の先頭
        assert_eq!(f.position(6).line, 2);
        assert_eq!(f.position(6).column, 1);
        // "あ" は3バイトだが1列として数える
        let after_a = 6 + "y = \"あ".len() as u32;
        assert_eq!(f.position(after_a).column, 7);
    }

    #[test]
    #[should_panic]
    fn span_rejects_reversed_range() {
        Span::new(3, 1);
    }
}
